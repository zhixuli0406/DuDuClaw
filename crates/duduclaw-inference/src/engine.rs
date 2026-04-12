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

        Self {
            config,
            backend: RwLock::new(None),
            model_manager,
            hardware: RwLock::new(None),
            router,
            manager,
            mlx,
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
                    model,
                };
                *self.backend.write().await = Some(Arc::new(OpenAiCompatBackend::new(compat)));
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
            return Ok(Box::new(OpenAiCompatBackend::new(compat.clone())));
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
    pub async fn route_and_generate(
        &self,
        request: &InferenceRequest,
    ) -> Result<Option<InferenceResponse>> {
        let decision = self.route(&request.system_prompt, &request.user_prompt);

        match decision.tier {
            RoutingTier::CloudApi => {
                info!(reason = %decision.reason, "Escalating to Cloud API");
                Ok(None) // Caller should fall back to Claude API
            }
            RoutingTier::LocalFast | RoutingTier::LocalStrong => {
                // Override model_id with the router's decision
                let mut routed_request = request.clone();
                if let Some(ref model_id) = decision.model_id {
                    routed_request.model_id = Some(model_id.clone());
                }
                let response = self.generate(&routed_request).await?;
                Ok(Some(response))
            }
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
    pub async fn load_model(&self, model_id: &str) -> Result<ModelInfo> {
        let backend = self.get_backend().await?;
        let model_path = self.model_manager.resolve_path(model_id).await?;
        let path_str = model_path.to_string_lossy().to_string();

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

    /// Run an evolution reflection using local MLX model (Apple Silicon only).
    ///
    /// Returns `Ok(Some(response))` if MLX handled it locally,
    /// `Ok(None)` if MLX is not available and caller should use Claude API.
    pub async fn mlx_evolution_reflection(
        &self,
        reflection_type: &str,
        agent_soul: &str,
        context: &str,
    ) -> Result<Option<String>> {
        let mlx = match &self.mlx {
            Some(m) if m.is_available().await => m,
            _ => return Ok(None),
        };

        let result = mlx.run_evolution_reflection(reflection_type, agent_soul, context).await?;
        Ok(Some(result))
    }

    /// Check if MLX bridge is available for evolution.
    pub async fn mlx_available(&self) -> bool {
        match &self.mlx {
            Some(m) => m.is_available().await,
            None => false,
        }
    }
}
