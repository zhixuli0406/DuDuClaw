//! Inference engine — the main entry point coordinating backends, models, and routing.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, info, warn};

use crate::backend::InferenceBackend;
use crate::config::InferenceConfig;
use crate::error::{InferenceError, Result};
use crate::hardware::detect_hardware;
use crate::manager::{InferenceManager, InferenceMode};
use crate::model_manager::ModelManager;
use crate::openai_compat::OpenAiCompatBackend;
use crate::router::{ConfidenceRouter, RoutingDecision, RoutingTier};
use crate::types::*;
use crate::ucci::{Observation, UcciCascade};

/// The main inference engine — manages backends, models, routing, and multi-mode switching.
pub struct InferenceEngine {
    config: InferenceConfig,
    backend: RwLock<Option<Arc<dyn InferenceBackend>>>,
    model_manager: Arc<ModelManager>,
    hardware: RwLock<Option<HardwareInfo>>,
    router: Option<ConfidenceRouter>,
    ucci: Option<UcciCascade>,
    /// In-flight `ucci_shadow_strong` background generations. The shadow is a
    /// *collection* side effect, so it runs detached (the user-visible reply
    /// no longer waits for a second model call) and its observation row may
    /// land after the reply — `scripts/ucci_fit.py` reads the JSONL by
    /// `request_id`, not by line order, and the append itself already holds
    /// `duduclaw_core::with_file_lock`. Handles are kept only so
    /// [`InferenceEngine::flush_shadow_observations`] can drain them before a
    /// process exits; finished ones are pruned on every push.
    shadow_tasks: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Caps how many `ucci_shadow_strong` background generations may run at
    /// once (`[router] ucci_shadow_max_inflight`, default 1). A background
    /// shadow and the next request's foreground generation share the same
    /// backend/model slot, so leaving this unbounded risked a model-switch
    /// race once shadows were detached onto their own tasks. Acquired with
    /// `try_acquire_owned` — never awaited — so a saturated cap skips the
    /// spawn instead of blocking or queuing behind the foreground reply.
    shadow_inflight: Arc<Semaphore>,
    /// Count of shadow spawns skipped because `shadow_inflight` was
    /// saturated. Telemetry only (also `debug!`-logged at the skip site);
    /// no new persistence — this is an in-memory counter for this process.
    shadow_skipped_inflight: Arc<AtomicU64>,
    manager: InferenceManager,
    /// DuDuClaw home dir (`~/.duduclaw`), used to resolve encrypted config
    /// fields (e.g. `openai_compat.api_key_enc`) read-only at backend build.
    home_dir: std::path::PathBuf,
}

impl InferenceEngine {
    /// Create a new inference engine from config.
    pub async fn new(home_dir: &Path) -> Self {
        let config = InferenceConfig::load(home_dir).await;
        let models_dir = config.models_path();
        let model_manager = Arc::new(ModelManager::new(models_dir));

        let router = config.router.clone().map(ConfidenceRouter::new);
        let ucci = config
            .router
            .as_ref()
            .filter(|c| c.enabled)
            .map(|c| UcciCascade::load(c, home_dir));
        let manager = InferenceManager::new(&config);
        let shadow_max_inflight = config
            .router
            .as_ref()
            .map(|r| r.effective_ucci_shadow_max_inflight())
            .unwrap_or(1);

        Self {
            config,
            backend: RwLock::new(None),
            model_manager,
            hardware: RwLock::new(None),
            router,
            ucci,
            shadow_tasks: tokio::sync::Mutex::new(Vec::new()),
            shadow_inflight: Arc::new(Semaphore::new(shadow_max_inflight)),
            shadow_skipped_inflight: Arc::new(AtomicU64::new(0)),
            manager,
            home_dir: home_dir.to_path_buf(),
        }
    }

    /// Initialize the engine: detect hardware, select backend, optionally auto-load model.
    pub async fn init(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Local inference is disabled");
            return Ok(());
        }

        // Detect hardware
        let hw = detect_hardware().await;
        info!(
            gpu = %hw.gpu_name,
            gpu_type = ?hw.gpu_type,
            ram_mb = hw.ram_total_mb,
            recommended_backend = %hw.recommended_backend,
            "Hardware detected"
        );
        *self.hardware.write().await = Some(hw.clone());

        // Select and initialize backend
        let backend = self.create_backend(&hw).await?;
        if !backend.is_available().await {
            warn!(backend = backend.name(), "Backend not available, inference disabled");
            return Ok(());
        }
        info!(backend = backend.name(), "Inference backend ready");
        *self.backend.write().await = Some(Arc::from(backend));

        // Scan models
        let models = self.model_manager.scan().await?;
        info!(count = models.len(), "Models available");

        // Log router status
        if let Some(ref router) = self.router
            && router.is_enabled() {
                info!(
                    fast_model = ?router.config().fast_model,
                    strong_model = ?router.config().strong_model,
                    fast_threshold = router.config().fast_threshold,
                    strong_threshold = router.config().strong_threshold,
                    "Confidence router enabled"
                );
            }

        // Initialize InferenceManager (llamafile)
        let mgr_mode = self.manager.init().await.unwrap_or(InferenceMode::CloudOnly);
        if mgr_mode != InferenceMode::CloudOnly {
            info!(mode = %mgr_mode, "InferenceManager active");
            // If manager found Exo or llamafile, create an OpenAI-compat backend for it
            if let Some(url) = self.manager.get_api_base_url().await {
                let model = self.manager.get_model().await.unwrap_or_else(|| "default".to_string());
                info!(url = %url, model = %model, "Using manager-provided backend");
                let compat = crate::config::OpenAiCompatConfig {
                    base_url: url,
                    api_key: None,
                    api_key_enc: None,
                    model,
                };
                *self.backend.write().await =
                    Some(Arc::new(OpenAiCompatBackend::new_with_home(compat, &self.home_dir).await));
            }
        }

        // Auto-load default model if configured
        if self.config.auto_load
            && let Some(ref default_model) = self.config.default_model {
                match self.load_model(default_model).await {
                    Ok(info) => info!(model = %info.id, "Auto-loaded default model"),
                    Err(e) => warn!(model = default_model, error = %e, "Failed to auto-load model"),
                }
            }

        Ok(())
    }

    /// Create the appropriate backend based on config and hardware.
    async fn create_backend(&self, hw: &HardwareInfo) -> Result<Box<dyn InferenceBackend>> {
        // OpenAI-compat takes priority if configured
        if let Some(ref compat) = self.config.openai_compat {
            info!(url = %compat.base_url, "Using OpenAI-compatible backend");
            return Ok(Box::new(OpenAiCompatBackend::new_with_home(
                compat.clone(),
                &self.home_dir,
            ).await));
        }

        let backend_type = self.config.backend.unwrap_or(hw.recommended_backend);

        match backend_type {
            // Removed 2026-09-29 (`wiki/reports/feature-audit-2026-09-29.md`
            // T1-D3 / T3-S5): the in-process llama.cpp and mistral.rs backends
            // were never compiled into a shipped binary (`release.sh` builds
            // neither `metal`/`cuda`/`vulkan` nor `mistralrs`), and llama.cpp's
            // `generate()` was a stub. The variants stay so an existing
            // `inference.toml` carrying `backend = "llama_cpp"` still parses;
            // they now fail with a message that names the replacement.
            BackendType::LlamaCpp | BackendType::MistralRs => {
                Err(InferenceError::BackendUnavailable {
                    backend: backend_type.to_string(),
                    reason: "in-process backend removed in 2026-09 — run a local \
                             OpenAI-compatible server (llama-server / Ollama / vLLM) \
                             and point [openai_compat] base_url at it"
                        .to_string(),
                })
            }
            BackendType::OpenAiCompat => Err(InferenceError::Config(
                "OpenAI-compatible backend requires [openai_compat] config section".to_string(),
            )),
        }
    }

    /// Route a query through the confidence router and generate.
    ///
    /// Returns `Ok(Some(response))` if handled locally,
    /// `Ok(None)` if the router decided to escalate to Cloud API.
    ///
    /// With a fitted UCCI router, use top-2 token margins to decide whether
    /// to escalate LocalFast → LocalStrong → Cloud API. Without UCCI, the
    /// optional legacy post-hoc logistic gate retains its previous behavior.
    ///
    /// Takes `&Arc<Self>` so the opt-in `ucci_shadow_strong` collection call
    /// can be detached onto its own task instead of doubling the latency of
    /// the reply it is only *observing*. Call
    /// [`InferenceEngine::flush_shadow_observations`] before process exit to
    /// drain whatever is still in flight.
    pub async fn route_and_generate(
        self: &Arc<Self>,
        request: &InferenceRequest,
    ) -> Result<Option<InferenceResponse>> {
        let decision = self.route(&request.system_prompt, &request.user_prompt);

        let mut tier = decision.tier;
        let mut model_id = decision.model_id;
        let request_id = uuid::Uuid::new_v4().to_string();

        loop {
            if tier == RoutingTier::CloudApi {
                info!(reason = %decision.reason, "Escalating to Cloud API");
                return Ok(None); // Caller should fall back to Claude API
            }

            // Override model_id with the router's decision
            let mut routed_request = request.clone();
            let ucci_enabled = self
                .ucci
                .as_ref()
                .is_some_and(|u| u.requested() || u.collecting());
            if ucci_enabled {
                routed_request.params.capture_logprobs = true;
            }
            if ucci_enabled {
                // UCCI's token margin is defined for greedy generation.
                routed_request.params.temperature = 0.0;
                routed_request.params.capture_top_logprobs = true;
                routed_request.params.ucci_drop_stop_token = self
                    .router
                    .as_ref()
                    .is_some_and(|r| r.config().ucci_drop_stop_token);
            }
            if let Some(ref id) = model_id {
                routed_request.model_id = Some(id.clone());
            }
            let response = self.generate(&routed_request).await?;

            if ucci_enabled {
                let cascade = self.ucci.as_ref().expect("ucci_enabled implies cascade");
                let route = response
                    .margin_uncertainty
                    .and_then(|u| cascade.router(tier).and_then(|router| router.route(u).ok()));
                let escalated = route
                    .as_ref()
                    .map(|r| r.escalate)
                    .or_else(|| cascade.stage_configured(tier).then_some(true));
                cascade
                    .observe(&Observation::from_response(
                        &request_id,
                        tier,
                        &request.system_prompt,
                        &request.user_prompt,
                        &response,
                        route.as_ref().map(|r| r.p_hat),
                        escalated,
                    ))
                    .await;
                if cascade.requested() {
                    match route {
                        // The two escalating arms fall through to the
                        // tier-escalation block below.
                        Some(route) if route.escalate => {
                            info!(tier = %tier, u = ?response.margin_uncertainty,
                            p_hat = route.p_hat, "UCCI escalated local answer");
                        }
                        Some(route) => {
                            info!(tier = %tier, u = ?response.margin_uncertainty,
                            p_hat = route.p_hat, "UCCI accepted local answer");
                            self.spawn_strong_shadow(&request_id, tier, request).await;
                            return Ok(Some(response));
                        }
                        None if cascade.stage_configured(tier) => {
                            warn!(tier = %tier, "UCCI assessment unavailable; escalating local answer");
                        }
                        None => {
                            self.spawn_strong_shadow(&request_id, tier, request).await;
                            return Ok(Some(response));
                        }
                    }
                } else {
                    // Collection-only UCCI (`ucci_shadow_strong` / observation
                    // gathering without a fitted router): observe, then accept
                    // exactly as an ungated tier does.
                    self.spawn_strong_shadow(&request_id, tier, request).await;
                    return Ok(Some(response));
                }
            } else {
                // No calibration gate is active for this tier, so there is
                // nothing that could reject the answer — accept it. Until
                // 2026-09-29 this was the `assess_response() == None` branch
                // of the legacy post-hoc gate (`…feature-audit-2026-09-29.md`
                // T3-S7); with that gate removed the fail-safe has to be
                // stated explicitly, or an ungated tier would escalate every
                // single answer.
                self.spawn_strong_shadow(&request_id, tier, request).await;
                return Ok(Some(response));
            }

            // Low confidence — escalate to the next tier.
            let router = self
                .router
                .as_ref()
                .expect("a routed tier implies a configured router");
            let next = router.next_tier(tier).unwrap_or(RoutingTier::CloudApi);
            // Carry the margin UCCI actually decided on (`None` on a UCCI
            // escalation forced by a missing top-2 signal — exactly the case
            // worth spotting in a log).
            info!(
                router = "ucci",
                tier_from = %tier,
                tier_to = %next,
                margin = ?response.margin_uncertainty,
                // Kept so existing log filters on `from`/`to` still match.
                from = %tier,
                to = %next,
                "Confidence below threshold, escalating"
            );
            tier = next;
            model_id = match next {
                RoutingTier::LocalStrong => router.config().strong_model.clone(),
                _ => None,
            };
        }
    }

    /// Whether a fitted UCCI router was requested for either local tier.
    pub fn ucci_requested(&self) -> bool {
        self.ucci.as_ref().is_some_and(UcciCascade::requested)
    }

    /// Detach the opt-in `ucci_shadow_strong` collection call.
    ///
    /// Until 2026-09-28 this ran inline: every accepted Fast reply waited for
    /// a full Strong generation before the user saw it, doubling latency for
    /// a row that is only ever read offline by `scripts/ucci_fit.py`. The
    /// eligibility checks stay on the caller's task (they are pure config
    /// reads and cost nothing), so a disabled shadow spawns nothing at all;
    /// only the generation and the JSONL append move to a background task.
    ///
    /// The observation row is therefore allowed to lag the reply. That is
    /// safe for its one consumer — the fitter pairs rows by `request_id` and
    /// does not care about file order — and the append already serializes
    /// across processes through `duduclaw_core::with_file_lock`.
    ///
    /// Detaching the shadow (2026-09-28) removed the doubled-latency bug but
    /// opened a new one: nothing capped how many background shadows could run
    /// at once, so a burst of accepted Fast replies could pile up shadows
    /// that race the *next* request's foreground generation for the same
    /// backend/model slot. `shadow_inflight` bounds that (default 1 via
    /// `[router] ucci_shadow_max_inflight`); a saturated cap skips the spawn
    /// entirely rather than queuing or blocking — the foreground reply is
    /// unaffected either way, it simply loses that one observation row.
    async fn spawn_strong_shadow(
        self: &Arc<Self>,
        request_id: &str,
        tier: RoutingTier,
        request: &InferenceRequest,
    ) {
        if tier != RoutingTier::LocalFast {
            return;
        }
        let eligible = self
            .router
            .as_ref()
            .zip(self.ucci.as_ref())
            .is_some_and(|(router, cascade)| {
                router.config().ucci_shadow_strong
                    && cascade.collecting()
                    && router.config().strong_model.is_some()
            });
        if !eligible {
            return;
        }
        let permit = match Arc::clone(&self.shadow_inflight).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.shadow_skipped_inflight.fetch_add(1, Ordering::Relaxed);
                debug!(
                    request_id,
                    %tier,
                    "UCCI shadow strong generation skipped: max inflight reached"
                );
                return;
            }
        };
        let engine = Arc::clone(self);
        let request_id = request_id.to_string();
        let request = request.clone();
        let handle = tokio::spawn(async move {
            // Held until the shadow generation finishes (success, error, or
            // panic-unwind) so the permit count always tracks reality.
            let _permit = permit;
            engine
                .record_strong_shadow(&request_id, tier, &request)
                .await;
        });
        let mut tasks = self.shadow_tasks.lock().await;
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
    }

    /// Await every in-flight `ucci_shadow_strong` observation.
    ///
    /// Call this on the way out of a process that may exit while a shadow is
    /// still generating; without it the last row or two are simply lost (the
    /// rest of the file is already durable — each row is appended under a
    /// file lock as soon as its generation returns).
    pub async fn flush_shadow_observations(&self) {
        let pending: Vec<_> = std::mem::take(&mut *self.shadow_tasks.lock().await);
        for task in pending {
            if let Err(error) = task.await
                && !error.is_cancelled()
            {
                warn!(%error, "UCCI shadow observation task panicked");
            }
        }
    }

    async fn record_strong_shadow(
        &self,
        request_id: &str,
        tier: RoutingTier,
        request: &InferenceRequest,
    ) {
        if tier != RoutingTier::LocalFast {
            return;
        }
        let Some(router) = self.router.as_ref() else {
            return;
        };
        let Some(cascade) = self.ucci.as_ref() else {
            return;
        };
        if !router.config().ucci_shadow_strong || !cascade.collecting() {
            return;
        }
        let Some(model_id) = router.config().strong_model.as_ref() else {
            return;
        };
        let mut shadow = request.clone();
        shadow.model_id = Some(model_id.clone());
        shadow.params.temperature = 0.0;
        shadow.params.capture_logprobs = true;
        shadow.params.capture_top_logprobs = true;
        shadow.params.ucci_drop_stop_token = router.config().ucci_drop_stop_token;
        match self.generate(&shadow).await {
            Ok(response) => {
                cascade
                    .observe(&Observation::from_response(
                        request_id,
                        RoutingTier::LocalStrong,
                        &request.system_prompt,
                        &request.user_prompt,
                        &response,
                        None,
                        None,
                    ))
                    .await
            }
            Err(error) => warn!(%error, "UCCI LocalStrong shadow generation failed"),
        }
    }

    /// Get the routing decision for a query (without generating).
    pub fn route(&self, system_prompt: &str, user_prompt: &str) -> RoutingDecision {
        match &self.router {
            Some(router) => router.route(system_prompt, user_prompt),
            None => RoutingDecision {
                tier: RoutingTier::LocalStrong,
                confidence: 0.5,
                reason: "No router configured".to_string(),
                model_id: self.config.default_model.clone(),
            },
        }
    }

    /// Load a model by id or path.
    ///
    /// For in-process backends the id is resolved against
    /// `models_dir` and the resulting filesystem path is passed to the backend.
    /// For remote backends (OpenAI-compatible HTTP) the id is passed through
    /// unchanged because the model lives on a server — without this branch the
    /// engine would error with `ModelNotFound` before the backend ever sees
    /// the request, breaking remote-only setups (vLLM, SGLang, llamafile).
    pub async fn load_model(&self, model_id: &str) -> Result<ModelInfo> {
        let backend = self.get_backend().await?;
        let path_str = if backend.requires_local_file() {
            self.model_manager
                .resolve_path(model_id)
                .await?
                .to_string_lossy()
                .to_string()
        } else {
            model_id.to_string()
        };

        let info = backend.load_model(&path_str, &self.config.generation).await?;
        self.model_manager.set_loaded(&info.id, info.context_length).await;
        Ok(info)
    }

    /// Unload the current model.
    pub async fn unload_model(&self) -> Result<()> {
        let backend = self.get_backend().await?;
        backend.unload_model().await?;
        self.model_manager.set_unloaded().await;
        Ok(())
    }

    /// Generate text using the loaded model.
    pub async fn generate(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        let backend = self.get_backend().await?;

        // Auto-load model if specified in request but not yet loaded
        if let Some(ref model_id) = request.model_id {
            let current = self.model_manager.loaded_model_id().await;
            if current.as_deref() != Some(model_id) {
                self.load_model(model_id).await?;
            }
        }

        // Verify a model is loaded
        if backend.loaded_model().await.is_none() {
            if let Some(ref default_model) = self.config.default_model {
                self.load_model(default_model).await?;
            } else {
                return Err(InferenceError::NoModelLoaded);
            }
        }

        backend.generate(request).await
    }

    /// Generate text with a simple prompt (convenience method).
    pub async fn generate_simple(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String> {
        let request = InferenceRequest {
            system_prompt: system_prompt.to_string(),
            user_prompt: user_prompt.to_string(),
            params: self.config.generation.clone(),
            model_id: self.config.default_model.clone(),
        };
        let response = self.generate(&request).await?;
        Ok(response.text)
    }

    /// List available models.
    pub async fn list_models(&self) -> Vec<ModelInfo> {
        self.model_manager.list().await
    }

    /// Get model info by id.
    pub async fn get_model(&self, model_id: &str) -> Option<ModelInfo> {
        self.model_manager.get(model_id).await
    }

    /// Get detected hardware info.
    pub async fn hardware_info(&self) -> Option<HardwareInfo> {
        self.hardware.read().await.clone()
    }

    /// Check if inference is enabled and a backend is available.
    pub async fn is_available(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        let guard = self.backend.read().await;
        if let Some(ref backend) = *guard {
            backend.is_available().await
        } else {
            false
        }
    }

    /// Check if the confidence router is enabled.
    pub fn router_enabled(&self) -> bool {
        self.router.as_ref().is_some_and(|r| r.is_enabled())
    }

    /// Snapshot of the active OpenAI-compatible HTTP endpoint, if the current
    /// backend is HTTP-based: an `InferenceManager`-discovered server
    /// (Exo / llamafile — takes precedence, mirroring [`Self::init`]) or a
    /// configured `[openai_compat]` server. `None` for in-process backends
    /// (llama.cpp / mistral.rs) and when local inference is disabled.
    ///
    /// External adapters (the gateway's `LocalChatProvider`) use this to point
    /// a tool-calling-capable OpenAI-compat client at the same server.
    pub async fn compat_endpoint(&self) -> Option<crate::adapter::CompatEndpoint> {
        let manager_url = self.manager.get_api_base_url().await;
        let manager_model = self.manager.get_model().await;
        let config_compat = self
            .config
            .openai_compat
            .as_ref()
            .map(|c| (c.base_url.as_str(), c.model.as_str()));
        let (base_url, model, source) = crate::adapter::resolve_compat_endpoint(
            self.config.enabled,
            manager_url,
            manager_model,
            config_compat,
        )?;
        // Only the configured endpoint may carry a key; manager-discovered
        // llamafile/Exo servers are keyless local processes.
        let api_key = match source {
            crate::adapter::CompatSource::Config => match self.config.openai_compat.as_ref() {
                Some(c) => c.resolved_api_key(&self.home_dir).await,
                None => None,
            },
            crate::adapter::CompatSource::Manager => None,
        };
        Some(crate::adapter::CompatEndpoint { base_url, model, api_key })
    }

    /// Whether the operator allows an external adapter to run tool calling
    /// against the local endpoint (`[router] local_tools`, default `true`).
    pub fn local_tools_enabled(&self) -> bool {
        crate::adapter::local_tools_enabled(&self.config)
    }

    /// Get the active backend.
    async fn get_backend(&self) -> Result<Arc<dyn InferenceBackend>> {
        self.backend
            .read()
            .await
            .clone()
            .ok_or(InferenceError::BackendUnavailable {
                backend: "none".to_string(),
                reason: "No backend initialized. Is inference enabled in inference.toml?".to_string(),
            })
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &InferenceConfig {
        &self.config
    }

    /// Get the inference manager for multi-mode status.
    pub fn manager(&self) -> &InferenceManager {
        &self.manager
    }

    /// Get the current inference mode (llamafile / direct / cloud).
    pub async fn current_mode(&self) -> InferenceMode {
        self.manager.current_mode().await
    }

    #[cfg(test)]
    async fn set_backend_for_test(&self, backend: Arc<dyn InferenceBackend>) {
        *self.backend.write().await = Some(backend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    /// Stub backend used by tests to verify engine behavior without needing
    /// a real model file or network endpoint.
    struct StubBackend {
        requires_local: bool,
        load_called: AtomicBool,
        last_path: RwLock<Option<String>>,
    }

    impl StubBackend {
        fn new(requires_local: bool) -> Self {
            Self {
                requires_local,
                load_called: AtomicBool::new(false),
                last_path: RwLock::new(None),
            }
        }
    }

    #[async_trait]
    impl InferenceBackend for StubBackend {
        fn name(&self) -> &str {
            "stub"
        }

        fn requires_local_file(&self) -> bool {
            self.requires_local
        }

        async fn load_model(
            &self,
            model_path: &str,
            _params: &GenerationParams,
        ) -> Result<ModelInfo> {
            self.load_called.store(true, Ordering::SeqCst);
            *self.last_path.write().await = Some(model_path.to_string());
            Ok(ModelInfo {
                id: "stub-model".to_string(),
                path: model_path.to_string(),
                architecture: "stub".to_string(),
                parameter_count: "0".to_string(),
                quantization: "none".to_string(),
                file_size_bytes: 0,
                estimated_memory_mb: 0,
                kv_cache_mb: 0,
                is_loaded: true,
                context_length: 4096,
            })
        }

        async fn unload_model(&self) -> Result<()> {
            Ok(())
        }

        async fn loaded_model(&self) -> Option<ModelInfo> {
            None
        }

        async fn generate(&self, _request: &InferenceRequest) -> Result<InferenceResponse> {
            unreachable!("generate should not be called by load_model tests")
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    /// Regression test for v1.8.34: remote backends (OpenAI-compat) must skip
    /// `ModelManager::resolve_path` because the model lives on a server and
    /// there is no local GGUF file.
    #[tokio::test]
    async fn load_model_skips_path_resolution_for_remote_backends() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = InferenceEngine::new(tmp.path()).await;
        let backend = Arc::new(StubBackend::new(false));
        engine.set_backend_for_test(backend.clone()).await;

        let info = engine
            .load_model("qwen3.6-35b-a3b")
            .await
            .expect("remote backend load should not require a local file");

        assert!(backend.load_called.load(Ordering::SeqCst));
        assert_eq!(info.id, "stub-model");
        // Remote backends receive the raw model id, not a filesystem path.
        let last = backend.last_path.read().await.clone();
        assert_eq!(last.as_deref(), Some("qwen3.6-35b-a3b"));
    }

    /// Local backends must still go through `resolve_path` so missing files
    /// surface as `ModelNotFound` (preserves pre-v1.8.34 llama.cpp behavior).
    #[tokio::test]
    async fn load_model_still_resolves_path_for_local_backends() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = InferenceEngine::new(tmp.path()).await;
        let backend = Arc::new(StubBackend::new(true));
        engine.set_backend_for_test(backend.clone()).await;

        let err = engine
            .load_model("nonexistent-model")
            .await
            .expect_err("local backend with missing file should fail");

        assert!(matches!(err, InferenceError::ModelNotFound { .. }));
        assert!(!backend.load_called.load(Ordering::SeqCst));
    }

    // ── Calibrated cascade (post-hoc confidence) tests ─────────────────

    /// Stub backend that returns a configurable mean_logprob per model id and
    /// records which models were asked to generate.
    struct CascadeStub {
        /// model id → mean_logprob returned by generate()
        logprobs: std::collections::HashMap<String, Option<f32>>,
        margins: std::collections::HashMap<String, f64>,
        calls: std::sync::Mutex<Vec<String>>,
        capture_flags: std::sync::Mutex<Vec<bool>>,
        loaded: RwLock<Option<ModelInfo>>,
        /// model id → gate a `generate()` for that model waits on, and which
        /// is only recorded in `calls` once released. Lets a test prove that
        /// a caller did *not* wait for a particular model's generation.
        gate: Option<(String, Arc<tokio::sync::Notify>)>,
        /// Number of gated `generate()` calls currently parked on `gate`,
        /// incremented before the wait and decremented after. Lets a test
        /// prove two shadow generations are genuinely concurrent (both
        /// parked at once) rather than merely both eventually completing one
        /// after the other.
        gate_waiting: std::sync::atomic::AtomicUsize,
    }

    impl CascadeStub {
        fn new(logprobs: &[(&str, Option<f32>)]) -> Self {
            Self {
                logprobs: logprobs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                margins: std::collections::HashMap::new(),
                calls: std::sync::Mutex::new(Vec::new()),
                capture_flags: std::sync::Mutex::new(Vec::new()),
                loaded: RwLock::new(None),
                gate: None,
                gate_waiting: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn with_margins(mut self, margins: &[(&str, f64)]) -> Self {
            self.margins = margins.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            self
        }

        fn blocking_on(mut self, model_id: &str, gate: Arc<tokio::sync::Notify>) -> Self {
            self.gate = Some((model_id.to_string(), gate));
            self
        }

        fn stub_info(id: &str) -> ModelInfo {
            ModelInfo {
                id: id.to_string(),
                path: id.to_string(),
                architecture: "stub".to_string(),
                parameter_count: "0".to_string(),
                quantization: "none".to_string(),
                file_size_bytes: 0,
                estimated_memory_mb: 0,
                kv_cache_mb: 0,
                is_loaded: true,
                context_length: 4096,
            }
        }
    }

    #[async_trait]
    impl InferenceBackend for CascadeStub {
        fn name(&self) -> &str {
            "cascade-stub"
        }

        fn requires_local_file(&self) -> bool {
            false
        }

        async fn load_model(
            &self,
            model_path: &str,
            _params: &GenerationParams,
        ) -> Result<ModelInfo> {
            let info = Self::stub_info(model_path);
            *self.loaded.write().await = Some(info.clone());
            Ok(info)
        }

        async fn unload_model(&self) -> Result<()> {
            *self.loaded.write().await = None;
            Ok(())
        }

        async fn loaded_model(&self) -> Option<ModelInfo> {
            self.loaded.read().await.clone()
        }

        async fn generate(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
            let model = request.model_id.clone().unwrap_or_default();
            if let Some((gated, gate)) = self.gate.as_ref()
                && *gated == model
            {
                self.gate_waiting.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
                self.gate_waiting.fetch_sub(1, Ordering::SeqCst);
            }
            self.calls.lock().unwrap().push(model.clone());
            self.capture_flags
                .lock()
                .unwrap()
                .push(request.params.capture_logprobs);
            let mean_logprob = self.logprobs.get(&model).copied().flatten();
            let margin_uncertainty = self.margins.get(&model).copied();
            Ok(InferenceResponse {
                text: format!("answer from {model}"),
                tokens_generated: 2,
                tokens_prompt: 2,
                generation_time_ms: 1,
                tokens_per_second: 0.0,
                backend: BackendType::OpenAiCompat,
                model_id: model,
                mean_logprob,
                margin_uncertainty,
            })
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    /// Build an engine with a [router] section written to inference.toml.
    async fn cascade_engine(tmp: &TempDir) -> Arc<InferenceEngine> {
        let toml = r#"
enabled = true

[router]
enabled = true
fast_threshold = 0.7
strong_threshold = 0.35
fast_model = "fast-model"
strong_model = "strong-model"
"#;
        tokio::fs::write(tmp.path().join("inference.toml"), toml)
            .await
            .expect("write inference.toml");
        Arc::new(InferenceEngine::new(tmp.path()).await)
    }

    /// A prompt that the ex-ante router sends to LocalFast ("hello" keyword).
    fn fast_request() -> InferenceRequest {
        InferenceRequest {
            system_prompt: String::new(),
            user_prompt: "hello, how are you?".to_string(),
            params: GenerationParams::default(),
            model_id: None,
        }
    }

    /// Regression (2026-09-29, `wiki/reports/feature-audit-2026-09-29.md`
    /// T3-S7): removing the legacy post-hoc gate must NOT turn an ungated
    /// tier into an always-escalating one. With no UCCI router file and no
    /// post-hoc gate, the first local answer is returned as-is and the
    /// backend is called exactly once — the same behaviour
    /// `post_hoc_enabled = false` produced before the gate was deleted.
    #[tokio::test]
    async fn no_calibration_gate_accepts_the_first_local_answer() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp).await;
        // A very low mean logprob: under the old gate this would have been
        // rejected, and with no gate at all it must still be accepted.
        let backend = Arc::new(CascadeStub::new(&[("fast-model", Some(-4.0))]));
        engine.set_backend_for_test(backend.clone()).await;

        let response = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("an ungated tier must answer locally, not escalate");

        assert_eq!(response.model_id, "fast-model");
        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model"],
            "no second tier may be called when nothing rejected the answer"
        );
        assert!(
            backend.capture_flags.lock().unwrap().iter().all(|&f| !f),
            "an ungated tier must not request logprobs"
        );
    }

    #[tokio::test]
    async fn ucci_routes_both_local_tiers_and_logs_review_rows() {
        use ucci::{
            Router,
            calibration::IsotonicCalibrator,
            policy::{CostModel, Costs},
        };

        let tmp = TempDir::new().unwrap();
        let map = IsotonicCalibrator::fit(&[0.0, 1.0], &[0.0, 1.0]).unwrap();
        let costs = Costs::new(1.0, 3.0, CostModel::Sequential).unwrap();
        Router::new(map.clone(), 0.5, costs)
            .unwrap()
            .save(tmp.path().join("fast.json"))
            .unwrap();
        Router::new(map, 0.5, costs)
            .unwrap()
            .save(tmp.path().join("strong.json"))
            .unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_threshold = 0.7
strong_threshold = 0.35
fast_model = "fast-model"
strong_model = "strong-model"
ucci_fast_router = "fast.json"
ucci_strong_router = "strong.json"
ucci_observations = "observations.jsonl"
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        let backend = Arc::new(
            CascadeStub::new(&[]).with_margins(&[("fast-model", 0.9), ("strong-model", 0.8)]),
        );
        engine.set_backend_for_test(backend.clone()).await;
        assert!(
            engine
                .route_and_generate(&fast_request())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model", "strong-model"]
        );
        let rows = tokio::fs::read_to_string(tmp.path().join("observations.jsonl"))
            .await
            .unwrap();
        let records: Vec<serde_json::Value> = rows
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["stage"], "local_fast");
        assert_eq!(records[0]["u"], 0.9);
        assert_eq!(records[0]["escalated"], true);
        assert_eq!(records[0]["label_source"], serde_json::Value::Null);
        assert_eq!(records[0]["request_id"], records[1]["request_id"]);
    }

    #[tokio::test]
    async fn ucci_collection_can_shadow_strong_without_changing_fast_reply() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_model = "fast-model"
strong_model = "strong-model"
ucci_observations = "observations.jsonl"
ucci_shadow_strong = true
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        let backend = Arc::new(
            CascadeStub::new(&[]).with_margins(&[("fast-model", 0.2), ("strong-model", 0.1)]),
        );
        engine.set_backend_for_test(backend.clone()).await;
        let result = engine
            .route_and_generate(&fast_request())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.model_id, "fast-model");
        // The shadow now runs detached, so its call and its row are only
        // guaranteed once the engine has been flushed. Everything after the
        // flush is exactly what this test asserted while it ran inline.
        engine.flush_shadow_observations().await;
        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model", "strong-model"]
        );
        let rows = tokio::fs::read_to_string(tmp.path().join("observations.jsonl"))
            .await
            .unwrap();
        assert_eq!(rows.lines().count(), 2);
    }

    /// Regression: `ucci_shadow_strong` ran its Strong generation inline, so
    /// every accepted Fast reply paid for a second model call before the user
    /// saw anything — a collection side effect charging the reply path. The
    /// reply must now return without waiting for it.
    #[tokio::test]
    async fn ucci_shadow_strong_does_not_block_the_fast_reply() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_model = "fast-model"
strong_model = "strong-model"
ucci_observations = "observations.jsonl"
ucci_shadow_strong = true
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        // The Strong model is held until the test releases it; if the reply
        // waited for the shadow, `route_and_generate` could not return.
        let release = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(
            CascadeStub::new(&[])
                .with_margins(&[("fast-model", 0.2), ("strong-model", 0.1)])
                .blocking_on("strong-model", release.clone()),
        );
        engine.set_backend_for_test(backend.clone()).await;

        // Bounded so the pre-fix inline shadow fails this test instead of
        // hanging the suite: the gate is not released until after the
        // assertions below.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.route_and_generate(&fast_request()),
        )
        .await
        .expect("the reply must not wait for the shadow generation")
        .unwrap()
        .unwrap();

        assert_eq!(result.model_id, "fast-model");
        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model"],
            "the reply must not wait for the shadow generation"
        );
        release.notify_one();
        engine.flush_shadow_observations().await;
        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model", "strong-model"],
            "the shadow must still run, just not inline"
        );
        let rows = tokio::fs::read_to_string(tmp.path().join("observations.jsonl"))
            .await
            .unwrap();
        assert_eq!(rows.lines().count(), 2);
    }

    /// The flush must be a no-op when nothing was ever spawned: a disabled
    /// shadow spawns no task at all, so a shutdown hook cannot hang on it.
    #[tokio::test]
    async fn flush_shadow_observations_is_a_no_op_without_shadow_collection() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp).await;
        let backend = Arc::new(CascadeStub::new(&[("fast-model", Some(-0.05))]));
        engine.set_backend_for_test(backend.clone()).await;
        engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("answered locally");
        engine.flush_shadow_observations().await;
        assert_eq!(backend.calls.lock().unwrap().as_slice(), &["fast-model"]);
    }

    /// Regression: `spawn_strong_shadow` used to spawn an unbounded number of
    /// background shadow generations — a burst of accepted Fast replies could
    /// pile up shadows racing the *next* request's foreground generation for
    /// the same backend/model slot. With the default cap of 1, a second
    /// shadow attempted while the first is still in flight must be skipped
    /// (not queued, not blocking) and the skip must be counted; the
    /// foreground reply for the second request must not wait for it either.
    #[tokio::test]
    async fn ucci_shadow_max_inflight_default_skips_a_second_concurrent_shadow() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_model = "fast-model"
strong_model = "strong-model"
ucci_observations = "observations.jsonl"
ucci_shadow_strong = true
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        // Held until released below so the first shadow's permit stays taken
        // across the second `route_and_generate` call.
        let release = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(
            CascadeStub::new(&[])
                .with_margins(&[("fast-model", 0.2), ("strong-model", 0.1)])
                .blocking_on("strong-model", release.clone()),
        );
        engine.set_backend_for_test(backend.clone()).await;

        let first = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.route_and_generate(&fast_request()),
        )
        .await
        .expect("first reply must not wait for its own shadow")
        .unwrap()
        .unwrap();
        assert_eq!(first.model_id, "fast-model");

        // The first shadow's `try_acquire_owned` runs synchronously inside
        // `spawn_strong_shadow` before it ever spawns — by the time
        // `route_and_generate` above returned, the sole permit is already
        // held, so this second call deterministically observes it saturated.
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.route_and_generate(&fast_request()),
        )
        .await
        .expect("second reply must not wait for the (skipped) shadow")
        .unwrap()
        .unwrap();
        assert_eq!(second.model_id, "fast-model");

        assert_eq!(
            backend.calls.lock().unwrap().as_slice(),
            &["fast-model", "fast-model"],
            "no strong-model call yet: the first shadow is still gated, the second was skipped"
        );
        assert_eq!(
            engine.shadow_skipped_inflight.load(Ordering::Relaxed),
            1,
            "the second shadow attempt must be counted as skipped"
        );

        release.notify_one();
        engine.flush_shadow_observations().await;

        let calls = backend.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| c.as_str() == "strong-model").count(),
            1,
            "only the first request's shadow may have run"
        );
        drop(calls);

        let rows = tokio::fs::read_to_string(tmp.path().join("observations.jsonl"))
            .await
            .unwrap();
        // 2 local_fast rows (one per accepted reply) + 1 local_strong row
        // (only the first request's shadow completed).
        assert_eq!(rows.lines().count(), 3);
    }

    /// Companion to the default-cap test: raising `ucci_shadow_max_inflight`
    /// to 2 must let two shadow generations run genuinely concurrently
    /// (both parked on the blocking gate at once), with nothing skipped.
    #[tokio::test]
    async fn ucci_shadow_max_inflight_configured_allows_two_concurrent_shadows() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_model = "fast-model"
strong_model = "strong-model"
ucci_observations = "observations.jsonl"
ucci_shadow_strong = true
ucci_shadow_max_inflight = 2
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        let release = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(
            CascadeStub::new(&[])
                .with_margins(&[("fast-model", 0.2), ("strong-model", 0.1)])
                .blocking_on("strong-model", release.clone()),
        );
        engine.set_backend_for_test(backend.clone()).await;

        engine
            .route_and_generate(&fast_request())
            .await
            .unwrap()
            .unwrap();
        engine
            .route_and_generate(&fast_request())
            .await
            .unwrap()
            .unwrap();

        // Poll (bounded) until both shadows are genuinely parked at once —
        // proves the cap actually allows 2 concurrent generations, not just
        // 2 eventually-sequential ones.
        let mut both_waiting = false;
        for _ in 0..200 {
            if backend.gate_waiting.load(Ordering::SeqCst) == 2 {
                both_waiting = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            both_waiting,
            "both shadows must be concurrently in flight under a cap of 2"
        );
        assert_eq!(
            engine.shadow_skipped_inflight.load(Ordering::Relaxed),
            0,
            "neither shadow should be skipped under a cap of 2"
        );

        release.notify_one();
        release.notify_one();
        engine.flush_shadow_observations().await;

        let calls = backend.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| c.as_str() == "fast-model").count(),
            2
        );
        assert_eq!(
            calls.iter().filter(|c| c.as_str() == "strong-model").count(),
            2,
            "both shadows must have run to completion"
        );
        drop(calls);

        let rows = tokio::fs::read_to_string(tmp.path().join("observations.jsonl"))
            .await
            .unwrap();
        assert_eq!(rows.lines().count(), 4);
    }

    #[tokio::test]
    async fn configured_ucci_gate_escalates_when_top_two_signal_is_missing() {
        use ucci::{
            Router,
            calibration::IsotonicCalibrator,
            policy::{CostModel, Costs},
        };

        let tmp = TempDir::new().unwrap();
        let map = IsotonicCalibrator::fit(&[0.0, 1.0], &[0.0, 1.0]).unwrap();
        Router::new(
            map,
            0.5,
            Costs::new(1.0, 3.0, CostModel::Sequential).unwrap(),
        )
        .unwrap()
        .save(tmp.path().join("fast.json"))
        .unwrap();
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            r#"
enabled = true
[router]
enabled = true
fast_model = "fast-model"
strong_model = "strong-model"
ucci_fast_router = "fast.json"
"#,
        )
        .await
        .unwrap();
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        let backend = Arc::new(CascadeStub::new(&[]));
        engine.set_backend_for_test(backend.clone()).await;
        let result = engine
            .route_and_generate(&fast_request())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.model_id, "strong-model");
    }
}
