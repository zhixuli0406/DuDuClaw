//! Inference engine — the main entry point coordinating backends, models, and routing.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::backend::InferenceBackend;
use crate::config::InferenceConfig;
use crate::error::{InferenceError, Result};
use crate::hardware::detect_hardware;
use crate::manager::{InferenceManager, InferenceMode};
use crate::mlx_bridge::MlxBridge;
use crate::model_manager::ModelManager;
use crate::openai_compat::OpenAiCompatBackend;
use crate::router::{ConfidenceRouter, RoutingDecision, RoutingTier};
use crate::types::*;

/// The main inference engine — manages backends, models, routing, and multi-mode switching.
pub struct InferenceEngine {
    config: InferenceConfig,
    backend: RwLock<Option<Arc<dyn InferenceBackend>>>,
    model_manager: Arc<ModelManager>,
    hardware: RwLock<Option<HardwareInfo>>,
    router: Option<ConfidenceRouter>,
    manager: InferenceManager,
    mlx: Option<MlxBridge>,
    /// JitRL zero-gradient continual learning (arXiv:2601.18510, see
    /// [`crate::jitrl`]). `None` unless `[jitrl] enabled = true` — the
    /// disabled hot path carries zero JitRL code and requests are untouched.
    jitrl: Option<crate::jitrl::JitrlEngine>,
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
        let manager = InferenceManager::new(&config);
        let mlx = config.mlx.clone().map(MlxBridge::new);
        let jitrl = config
            .jitrl
            .clone()
            .and_then(|c| crate::jitrl::JitrlEngine::new(c, home_dir));

        Self {
            config,
            backend: RwLock::new(None),
            model_manager,
            hardware: RwLock::new(None),
            router,
            manager,
            mlx,
            jitrl,
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

        // Initialize InferenceManager (Exo, llamafile)
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
                    Some(Arc::new(OpenAiCompatBackend::new_with_home(compat, &self.home_dir)));
            }
        }

        // Check MLX availability
        if let Some(ref mlx) = self.mlx
            && mlx.is_available().await {
                info!("MLX bridge available for evolution reflections");
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
            )));
        }

        let backend_type = self.config.backend.unwrap_or(hw.recommended_backend);

        match backend_type {
            BackendType::LlamaCpp => {
                #[cfg(any(feature = "metal", feature = "cuda", feature = "vulkan"))]
                {
                    info!("Using llama.cpp backend");
                    return Ok(Box::new(crate::llama_cpp::LlamaCppBackend::new()));
                }
                #[cfg(not(any(feature = "metal", feature = "cuda", feature = "vulkan")))]
                {
                    Err(InferenceError::BackendUnavailable {
                        backend: "llama.cpp".to_string(),
                        reason: "Build with --features metal, cuda, or vulkan to enable llama.cpp".to_string(),
                    })
                }
            }
            BackendType::MistralRs => {
                #[cfg(feature = "mistralrs")]
                {
                    let mrs_config = self.config.mistralrs.clone().unwrap_or_default();
                    info!(isq = ?mrs_config.isq_bits, paged_attn = mrs_config.paged_attention, "Using mistral.rs backend");
                    return Ok(Box::new(crate::mistral_rs::MistralRsBackend::new(mrs_config)));
                }
                #[cfg(not(feature = "mistralrs"))]
                {
                    Err(InferenceError::BackendUnavailable {
                        backend: "mistral.rs".to_string(),
                        reason: "mistralrs feature not enabled at compile time. Build with --features mistralrs-metal or mistralrs-cuda".to_string(),
                    })
                }
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
    /// **Calibrated cascade** (when `router.post_hoc_enabled`): after a local
    /// tier answers, the mean token logprob is Platt-scaled into an acceptance
    /// probability `g`; a rejected answer escalates LocalFast → LocalStrong →
    /// Cloud API instead of being returned. When post-hoc is disabled or the
    /// backend returns no logprobs, behaviour is identical to the legacy path.
    pub async fn route_and_generate(
        &self,
        request: &InferenceRequest,
    ) -> Result<Option<InferenceResponse>> {
        let decision = self.route(&request.system_prompt, &request.user_prompt);

        let mut tier = decision.tier;
        let mut model_id = decision.model_id;

        loop {
            if tier == RoutingTier::CloudApi {
                info!(reason = %decision.reason, "Escalating to Cloud API");
                return Ok(None); // Caller should fall back to Claude API
            }

            // Override model_id with the router's decision
            let mut routed_request = request.clone();
            if self.post_hoc_enabled() {
                routed_request.params.capture_logprobs = true;
            }
            if let Some(ref id) = model_id {
                routed_request.model_id = Some(id.clone());
            }
            let response = self.generate(&routed_request).await?;

            let Some(assessment) = self.assess_response(&response) else {
                // Post-hoc disabled or no logprobs from the server — accept
                // the answer exactly as before (fail-safe).
                return Ok(Some(response));
            };
            if assessment.accepted {
                info!(
                    tier = %tier,
                    p_bar = format!("{:.3}", assessment.p_bar),
                    g = format!("{:.3}", assessment.g),
                    "Post-hoc confidence accepted local answer"
                );
                return Ok(Some(response));
            }

            // Low confidence — escalate to the next tier.
            let router = self.router.as_ref().expect("assess_response implies router");
            let next = router.next_tier(tier).unwrap_or(RoutingTier::CloudApi);
            info!(
                from = %tier,
                to = %next,
                p_bar = format!("{:.3}", assessment.p_bar),
                g = format!("{:.3}", assessment.g),
                threshold = router.config().post_hoc_accept_threshold,
                "Post-hoc confidence below threshold, escalating"
            );
            tier = next;
            model_id = match next {
                RoutingTier::LocalStrong => router.config().strong_model.clone(),
                _ => None,
            };
        }
    }

    /// Post-hoc (cascade) confidence of a generated response: `(p̄, g, accepted)`
    /// via [`crate::router::PostHocAssessment::as_tuple`]. Returns `None` when
    /// no router is configured, post-hoc is disabled, or the backend returned
    /// no logprobs — callers should then treat the answer as accepted.
    ///
    /// Exposed so the gateway can log calibration inputs alongside outcomes.
    pub fn assess_response(
        &self,
        response: &InferenceResponse,
    ) -> Option<crate::router::PostHocAssessment> {
        self.router.as_ref()?.evaluate_post_hoc(response.mean_logprob)
    }

    /// Whether post-hoc (cascade) confidence checking is active.
    pub fn post_hoc_enabled(&self) -> bool {
        self.router
            .as_ref()
            .is_some_and(|r| r.config().post_hoc_enabled)
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
                post_hoc: None,
            },
        }
    }

    /// Load a model by id or path.
    ///
    /// For local backends (llama.cpp, mistral.rs) the id is resolved against
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

        // JitRL Tier B injection (arXiv:2601.18510, see `crate::jitrl`):
        // when enabled and a stored experience is similar enough, clone the
        // request and attach the clamped logit-bias map. Disabled (`jitrl` is
        // `None`) skips this block entirely — the request passes through
        // untouched and un-cloned.
        if let Some(ref jitrl) = self.jitrl {
            let model_id = self.jitrl_model_key(request.model_id.as_deref()).await;
            if let Some(model_id) = model_id
                && let Some(bias) = jitrl.prepare_bias(&request.user_prompt, &model_id) {
                    let mut biased = request.clone();
                    biased.params.logit_bias = Some(bias);
                    return backend.generate(&biased).await;
                }
        }

        backend.generate(request).await
    }

    /// Canonical JitRL model key — ONE resolution chain shared by the
    /// retrieval side ([`Self::generate`]) and the record side
    /// ([`Self::jitrl_record_feedback`]): explicit request model →
    /// `ModelManager`'s loaded id → backend-reported loaded model.
    ///
    /// HIGH-D (2026-07): record used to key by `endpoint.model` (the compat
    /// server's *configured* model name) while retrieval keyed by
    /// `request.model_id` / the loaded id — when those strings differed,
    /// every recorded reward was unretrievable (vocabulary isolation filters
    /// on exact `model_id` equality).
    async fn jitrl_model_key(&self, request_model: Option<&str>) -> Option<String> {
        if let Some(id) = request_model {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
        if let Some(id) = self.model_manager.loaded_model_id().await {
            return Some(id);
        }
        if let Ok(backend) = self.get_backend().await {
            if let Some(m) = backend.loaded_model().await {
                return Some(m.id);
            }
        }
        None
    }

    /// Record explicit JitRL feedback for a `(prompt, response)` pair
    /// (reward in `[-1, 1]`, positive = reinforce, negative = suppress).
    ///
    /// v1 tokenizes the response through the active OpenAI-compatible
    /// server's `/tokenize` endpoint so the stored token ids belong to the
    /// serving model's vocabulary. Errors honestly when JitRL is disabled or
    /// no compat endpoint is active — feedback is never fabricated.
    pub async fn jitrl_record_feedback(
        &self,
        prompt: &str,
        response: &str,
        reward: f32,
    ) -> Result<usize> {
        // Disabled check FIRST: "jitrl is disabled" must win over "no
        // tokenizer endpoint" for an honest error message.
        if self.jitrl.is_none() {
            return Err(InferenceError::Config(
                "jitrl is disabled — set [jitrl] enabled = true in inference.toml".to_string(),
            ));
        }
        let endpoint =
            self.compat_endpoint()
                .await
                .ok_or_else(|| InferenceError::BackendUnavailable {
                    backend: "jitrl-tokenizer".to_string(),
                    reason: "no OpenAI-compatible endpoint active — JitRL v1 needs the \
                             server's /tokenize to map the response onto the model's \
                             token ids"
                        .to_string(),
                })?;
        let tokenizer = crate::jitrl::HttpTokenizer::new(
            &endpoint.base_url,
            &endpoint.model,
            endpoint.api_key.clone(),
        );
        // HIGH-D key unification: store under the SAME key retrieval will use
        // (loaded model id first); the endpoint's configured model name is
        // only the last resort when nothing is loaded.
        let model_key = self
            .jitrl_model_key(None)
            .await
            .unwrap_or_else(|| endpoint.model.clone());
        self.jitrl_record_feedback_with(&tokenizer, prompt, response, reward, &model_key)
            .await
    }

    /// Tokenizer-injected core of [`Self::jitrl_record_feedback`] — split out
    /// so tests can prove the record→retrieve key roundtrip without an HTTP
    /// `/tokenize` endpoint.
    async fn jitrl_record_feedback_with(
        &self,
        tokenizer: &dyn crate::jitrl::JitrlTokenizer,
        prompt: &str,
        response: &str,
        reward: f32,
        model_key: &str,
    ) -> Result<usize> {
        let Some(ref jitrl) = self.jitrl else {
            return Err(InferenceError::Config(
                "jitrl is disabled — set [jitrl] enabled = true in inference.toml".to_string(),
            ));
        };
        jitrl
            .record_feedback(tokenizer, prompt, response, reward, model_key)
            .await
    }

    /// Whether JitRL is enabled and active.
    pub fn jitrl_enabled(&self) -> bool {
        self.jitrl.is_some()
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
            crate::adapter::CompatSource::Config => self
                .config
                .openai_compat
                .as_ref()
                .and_then(|c| c.resolved_api_key(&self.home_dir)),
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

    /// Get the current inference mode (Exo / llamafile / direct / cloud).
    pub async fn current_mode(&self) -> InferenceMode {
        self.manager.current_mode().await
    }

    /// Check if MLX bridge is available for evolution.
    pub async fn mlx_available(&self) -> bool {
        match &self.mlx {
            Some(m) => m.is_available().await,
            None => false,
        }
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
        calls: std::sync::Mutex<Vec<String>>,
        capture_flags: std::sync::Mutex<Vec<bool>>,
        loaded: RwLock<Option<ModelInfo>>,
    }

    impl CascadeStub {
        fn new(logprobs: &[(&str, Option<f32>)]) -> Self {
            Self {
                logprobs: logprobs
                    .iter()
                    .map(|(k, v)| (k.to_string(), *v))
                    .collect(),
                calls: std::sync::Mutex::new(Vec::new()),
                capture_flags: std::sync::Mutex::new(Vec::new()),
                loaded: RwLock::new(None),
            }
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
            self.calls.lock().unwrap().push(model.clone());
            self.capture_flags
                .lock()
                .unwrap()
                .push(request.params.capture_logprobs);
            let mean_logprob = self.logprobs.get(&model).copied().flatten();
            Ok(InferenceResponse {
                text: format!("answer from {model}"),
                tokens_generated: 2,
                tokens_prompt: 2,
                generation_time_ms: 1,
                tokens_per_second: 0.0,
                backend: BackendType::OpenAiCompat,
                model_id: model,
                mean_logprob,
            })
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    /// Build an engine with a [router] section written to inference.toml.
    async fn cascade_engine(tmp: &TempDir, post_hoc_enabled: bool) -> InferenceEngine {
        let toml = format!(
            r#"
enabled = true

[router]
enabled = true
fast_threshold = 0.7
strong_threshold = 0.35
fast_model = "fast-model"
strong_model = "strong-model"
post_hoc_enabled = {post_hoc_enabled}
"#
        );
        tokio::fs::write(tmp.path().join("inference.toml"), toml)
            .await
            .expect("write inference.toml");
        InferenceEngine::new(tmp.path()).await
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

    #[tokio::test]
    async fn cascade_low_confidence_escalates_fast_to_strong() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp, true).await;
        // fast tier answers with very low p̄ → rejected; strong tier is confident.
        let backend = Arc::new(CascadeStub::new(&[
            ("fast-model", Some(-4.0)),
            ("strong-model", Some(-0.05)),
        ]));
        engine.set_backend_for_test(backend.clone()).await;

        let response = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("answered locally");

        assert_eq!(response.model_id, "strong-model");
        assert_eq!(
            *backend.calls.lock().unwrap(),
            vec!["fast-model".to_string(), "strong-model".to_string()]
        );
        // Post-hoc mode must request logprobs from the backend.
        assert!(backend.capture_flags.lock().unwrap().iter().all(|&f| f));
        // Calibration inputs are exposed for logging.
        let (p_bar, g, accepted) = engine
            .assess_response(&response)
            .expect("assessment")
            .as_tuple();
        assert!(accepted);
        assert!(p_bar > 0.9);
        assert!(g >= 0.5);
    }

    #[tokio::test]
    async fn cascade_strong_low_confidence_signals_cloud_escalation() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp, true).await;
        // Both local tiers answer with low confidence → Ok(None) cloud signal.
        let backend = Arc::new(CascadeStub::new(&[
            ("fast-model", Some(-4.0)),
            ("strong-model", Some(-4.0)),
        ]));
        engine.set_backend_for_test(backend.clone()).await;

        let out = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok");

        assert!(out.is_none(), "low-confidence strong answer must escalate to cloud");
        assert_eq!(
            *backend.calls.lock().unwrap(),
            vec!["fast-model".to_string(), "strong-model".to_string()]
        );
    }

    #[tokio::test]
    async fn cascade_high_confidence_accepts_first_answer() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp, true).await;
        let backend = Arc::new(CascadeStub::new(&[("fast-model", Some(-0.05))]));
        engine.set_backend_for_test(backend.clone()).await;

        let response = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("answered locally");

        assert_eq!(response.model_id, "fast-model");
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cascade_disabled_behaves_like_legacy_router() {
        // Regression guard: post_hoc_enabled = false → the (low-confidence)
        // first answer is returned exactly as before, no logprobs requested.
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp, false).await;
        let backend = Arc::new(CascadeStub::new(&[("fast-model", Some(-4.0))]));
        engine.set_backend_for_test(backend.clone()).await;

        let response = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("answered locally");

        assert_eq!(response.model_id, "fast-model");
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
        assert!(
            backend.capture_flags.lock().unwrap().iter().all(|&f| !f),
            "legacy path must not request logprobs"
        );
        assert!(engine.assess_response(&response).is_none());
    }

    #[tokio::test]
    async fn cascade_without_logprobs_is_fail_safe() {
        // Post-hoc enabled but the server returns no logprobs → accept the
        // answer as today (no escalation, no error).
        let tmp = TempDir::new().expect("tempdir");
        let engine = cascade_engine(&tmp, true).await;
        let backend = Arc::new(CascadeStub::new(&[("fast-model", None)]));
        engine.set_backend_for_test(backend.clone()).await;

        let response = engine
            .route_and_generate(&fast_request())
            .await
            .expect("generate ok")
            .expect("answered locally");

        assert_eq!(response.model_id, "fast-model");
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
        assert!(engine.assess_response(&response).is_none());
    }

    // ── JitRL (arXiv:2601.18510) engine-wiring tests ────────────────────

    /// Stub backend that records the `logit_bias` carried by every request.
    /// It ignores the bias when generating — which also proves that a backend
    /// without a bias surface passes through unaffected.
    struct BiasCaptureStub {
        biases: std::sync::Mutex<Vec<Option<std::collections::HashMap<u32, f32>>>>,
        loaded: RwLock<Option<ModelInfo>>,
    }

    impl BiasCaptureStub {
        fn new() -> Self {
            Self {
                biases: std::sync::Mutex::new(Vec::new()),
                loaded: RwLock::new(None),
            }
        }
    }

    #[async_trait]
    impl InferenceBackend for BiasCaptureStub {
        fn name(&self) -> &str {
            "bias-capture-stub"
        }

        fn requires_local_file(&self) -> bool {
            false
        }

        async fn load_model(
            &self,
            model_path: &str,
            _params: &GenerationParams,
        ) -> Result<ModelInfo> {
            let info = CascadeStub::stub_info(model_path);
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
            self.biases
                .lock()
                .unwrap()
                .push(request.params.logit_bias.clone());
            Ok(InferenceResponse {
                text: "ok".to_string(),
                tokens_generated: 1,
                tokens_prompt: 1,
                generation_time_ms: 1,
                tokens_per_second: 0.0,
                backend: BackendType::OpenAiCompat,
                model_id: request.model_id.clone().unwrap_or_default(),
                mean_logprob: None,
            })
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    fn jitrl_request(prompt: &str) -> InferenceRequest {
        InferenceRequest {
            system_prompt: String::new(),
            user_prompt: prompt.to_string(),
            params: GenerationParams::default(),
            model_id: Some("jitrl-model".to_string()),
        }
    }

    /// Seed one positive experience for `jitrl-model` into the store file
    /// that `JitrlEngine` will read from `home`.
    fn seed_experience(home: &std::path::Path, prompt: &str) {
        let store =
            crate::jitrl::ExperienceStore::new(home.join("jitrl_experience.jsonl"), 100);
        store
            .append(&crate::jitrl::ExperienceRecord {
                id: "seed".to_string(),
                model_id: "jitrl-model".to_string(),
                sketch: crate::jitrl::fingerprint::shingle_sketch(prompt),
                token_weights: [(42u32, 1.0f32)].into_iter().collect(),
                reward: 1.0,
                created_at: chrono::Utc::now().timestamp(),
            })
            .expect("seed experience");
    }

    #[tokio::test]
    async fn jitrl_disabled_leaves_request_untouched() {
        // No [jitrl] section at all — even with a seeded store file present,
        // the request must pass through with no logit_bias (byte-identical).
        let tmp = TempDir::new().expect("tempdir");
        seed_experience(tmp.path(), "please summarize this quarterly report");
        let engine = InferenceEngine::new(tmp.path()).await;
        assert!(!engine.jitrl_enabled());
        let backend = Arc::new(BiasCaptureStub::new());
        engine.set_backend_for_test(backend.clone()).await;

        engine
            .generate(&jitrl_request("please summarize this quarterly report"))
            .await
            .expect("generate ok");

        let biases = backend.biases.lock().unwrap();
        assert_eq!(biases.len(), 1);
        assert!(biases[0].is_none(), "disabled JitRL must not touch the request");
    }

    async fn jitrl_engine(tmp: &TempDir) -> InferenceEngine {
        tokio::fs::write(
            tmp.path().join("inference.toml"),
            "enabled = true\n\n[jitrl]\nenabled = true\n",
        )
        .await
        .expect("write inference.toml");
        InferenceEngine::new(tmp.path()).await
    }

    #[tokio::test]
    async fn jitrl_enabled_injects_bias_for_similar_prompt() {
        let tmp = TempDir::new().expect("tempdir");
        seed_experience(tmp.path(), "please summarize this quarterly report");
        let engine = jitrl_engine(&tmp).await;
        assert!(engine.jitrl_enabled());
        let backend = Arc::new(BiasCaptureStub::new());
        engine.set_backend_for_test(backend.clone()).await;

        // The backend ignores the bias — response still succeeds, proving a
        // bias-less backend is transparently unaffected.
        let resp = engine
            .generate(&jitrl_request("please summarize this quarterly report for me"))
            .await
            .expect("generate ok");
        assert_eq!(resp.text, "ok");

        let biases = backend.biases.lock().unwrap();
        assert_eq!(biases.len(), 1);
        let bias = biases[0].as_ref().expect("similar prompt must carry bias");
        let b = bias.get(&42).copied().expect("seeded token biased");
        assert!(b > 0.0 && b <= 2.0, "clamped positive bias, got {b}");
    }

    #[tokio::test]
    async fn jitrl_enabled_skips_bias_for_dissimilar_prompt() {
        let tmp = TempDir::new().expect("tempdir");
        seed_experience(tmp.path(), "please summarize this quarterly report");
        let engine = jitrl_engine(&tmp).await;
        let backend = Arc::new(BiasCaptureStub::new());
        engine.set_backend_for_test(backend.clone()).await;

        engine
            .generate(&jitrl_request("write a haiku about mountains in winter"))
            .await
            .expect("generate ok");

        let biases = backend.biases.lock().unwrap();
        assert!(biases[0].is_none(), "no similar experience → untouched request");
    }

    /// Deterministic mock tokenizer: one token per whitespace-separated word,
    /// id = word char count (vocabulary-free — test only).
    struct WordLenTokenizer;

    #[async_trait]
    impl crate::jitrl::JitrlTokenizer for WordLenTokenizer {
        async fn encode(&self, text: &str) -> Result<Vec<u32>> {
            Ok(text
                .split_whitespace()
                .map(|w| w.chars().count() as u32)
                .collect())
        }
    }

    #[tokio::test]
    async fn jitrl_record_and_retrieve_share_one_model_key() {
        // HIGH-D regression: record keyed by `endpoint.model` while retrieval
        // keyed by `request.model_id`/loaded id — a recorded reward was
        // unretrievable whenever the strings differed. Both sides now resolve
        // through `jitrl_model_key`; this proves the roundtrip end-to-end.
        let tmp = TempDir::new().expect("tempdir");
        let engine = jitrl_engine(&tmp).await;
        assert!(engine.jitrl_enabled());
        let backend = Arc::new(BiasCaptureStub::new());
        engine.set_backend_for_test(backend.clone()).await;

        // Load "jitrl-model" so the canonical key resolves from the manager
        // (the same source `generate` consults).
        engine
            .load_model("jitrl-model")
            .await
            .expect("stub load ok");

        // Record through the same resolution the public entry point uses
        // (jitrl_model_key(None) — no request model), tokenizer injected.
        let key = engine
            .jitrl_model_key(None)
            .await
            .expect("a loaded model must yield a key");
        assert_eq!(key, "jitrl-model");
        let n = engine
            .jitrl_record_feedback_with(
                &WordLenTokenizer,
                "please summarize this quarterly report",
                "revenue grew twelve percent",
                1.0,
                &key,
            )
            .await
            .expect("record ok");
        assert!(n > 0);

        // Retrieval on a similar prompt for the SAME model must see the bias.
        engine
            .generate(&jitrl_request("please summarize this quarterly report for me"))
            .await
            .expect("generate ok");
        let biases = backend.biases.lock().unwrap();
        let bias = biases
            .last()
            .and_then(|b| b.as_ref())
            .expect("recorded reward must be retrievable under the unified key");
        assert!(bias.values().all(|v| *v > 0.0), "positive reinforcement expected");
    }

    #[tokio::test]
    async fn jitrl_feedback_errors_honestly_without_tokenizer_endpoint() {
        // JitRL enabled but no OpenAI-compat endpoint → record_feedback must
        // fail loudly (token ids cannot be fabricated), not degrade silently.
        let tmp = TempDir::new().expect("tempdir");
        let engine = jitrl_engine(&tmp).await;
        let err = engine
            .jitrl_record_feedback("prompt", "response", 1.0)
            .await
            .expect_err("no tokenizer endpoint must be an error");
        assert!(matches!(err, InferenceError::BackendUnavailable { .. }));

        // And when JitRL itself is disabled, the error says so.
        let tmp2 = TempDir::new().expect("tempdir");
        let engine2 = InferenceEngine::new(tmp2.path()).await;
        let err2 = engine2
            .jitrl_record_feedback("prompt", "response", 1.0)
            .await
            .expect_err("disabled jitrl must error");
        assert!(matches!(err2, InferenceError::Config(_)));
    }
}
