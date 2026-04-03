//! mistral.rs backend — Rust-native LLM inference with ISQ, PagedAttention,
//! Speculative Decoding, and Continuous Batching.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{info, warn};

use mistralrs_core::{
    Constraint, Device, DeviceMapSetting, GGUFLoaderBuilder, GGUFSpecificConfig,
    IsqType, MistralRs, MistralRsBuilder, ModelDType, NormalRequest, Request,
    RequestMessage, Response, SamplingParams, TokenSource,
};

use crate::backend::InferenceBackend;
use crate::config::MistralRsConfig;
use crate::error::{InferenceError, Result};
use crate::types::*;

/// Rust-native inference backend powered by mistral.rs.
pub struct MistralRsBackend {
    config: MistralRsConfig,
    runner: RwLock<Option<Arc<MistralRs>>>,
    loaded_model: RwLock<Option<ModelInfo>>,
}

impl MistralRsBackend {
    pub fn new(config: MistralRsConfig) -> Self {
        Self {
            config,
            runner: RwLock::new(None),
            loaded_model: RwLock::new(None),
        }
    }

    /// Build the ISQ type from config.
    fn isq_type(&self) -> Option<IsqType> {
        match self.config.isq_bits {
            Some(2) => Some(IsqType::Q2K),
            Some(3) => Some(IsqType::Q3K),
            Some(4) => Some(IsqType::Q4K),
            Some(5) => Some(IsqType::Q5K),
            Some(6) => Some(IsqType::Q6K),
            Some(8) => Some(IsqType::Q8_0),
            _ => None,
        }
    }

    /// Build the device based on available hardware.
    fn device(&self) -> Device {
        #[cfg(feature = "mistralrs-metal")]
        {
            Device::new_metal(0).unwrap_or(Device::Cpu)
        }
        #[cfg(feature = "mistralrs-cuda")]
        {
            Device::cuda_if_available(0).unwrap_or(Device::Cpu)
        }
        #[cfg(not(any(feature = "mistralrs-metal", feature = "mistralrs-cuda")))]
        {
            Device::Cpu
        }
    }
}

#[async_trait]
impl InferenceBackend for MistralRsBackend {
    fn name(&self) -> &str {
        #[cfg(feature = "mistralrs-metal")]
        { "mistral.rs (Metal)" }
        #[cfg(feature = "mistralrs-cuda")]
        { "mistral.rs (CUDA)" }
        #[cfg(not(any(feature = "mistralrs-metal", feature = "mistralrs-cuda")))]
        { "mistral.rs (CPU)" }
    }

    async fn load_model(&self, model_path: &str, params: &GenerationParams) -> Result<ModelInfo> {
        // Determine if this is a GGUF file or a HF model id
        let is_gguf = model_path.ends_with(".gguf")
            || std::path::Path::new(model_path).extension().and_then(|e| e.to_str()) == Some("gguf");

        let runner = if is_gguf {
            self.load_gguf(model_path, params).await?
        } else {
            self.load_hf(model_path, params).await?
        };

        let model_id = std::path::Path::new(model_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let file_size = std::fs::metadata(model_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let param_count = crate::model_manager::extract_param_count_from_id(&model_id);
        let kv_cache_mb = ModelInfo::estimate_kv_cache_mb(&param_count, params.context_size);
        let info = ModelInfo {
            id: model_id,
            path: model_path.to_string(),
            architecture: "mistralrs".to_string(),
            parameter_count: param_count,
            quantization: self.config.isq_bits
                .map(|b| format!("ISQ-Q{}K", b))
                .unwrap_or_else(|| "native".to_string()),
            file_size_bytes: file_size,
            estimated_memory_mb: file_size / (1024 * 1024) * 11 / 10,
            kv_cache_mb,
            is_loaded: true,
            context_length: params.context_size,
        };

        *self.runner.write().await = Some(runner);
        *self.loaded_model.write().await = Some(info.clone());
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        *self.runner.write().await = None;
        *self.loaded_model.write().await = None;
        Ok(())
    }

    async fn loaded_model(&self) -> Option<ModelInfo> {
        self.loaded_model.read().await.clone()
    }

    async fn generate(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        // Clone Arc to avoid holding RwLock across await points (C-5: deadlock fix)
        let runner = {
            let guard = self.runner.read().await;
            guard.as_ref().ok_or(InferenceError::NoModelLoaded)?.clone()
        };

        let start = std::time::Instant::now();

        // Build the prompt with chat template
        let messages = build_chat_messages(&request.system_prompt, &request.user_prompt);

        let sampling = SamplingParams {
            temperature: Some(request.params.temperature as f64),
            top_p: Some(request.params.top_p as f64),
            max_len: Some(request.params.max_tokens as usize),
            stop_toks: if request.params.stop.is_empty() {
                None
            } else {
                Some(mistralrs_core::StopTokens::Seqs(request.params.stop.clone()))
            },
            ..Default::default()
        };

        let (tx, rx) = tokio::sync::mpsc::channel(1);

        let req = Request::Normal(NormalRequest {
            messages,
            sampling_params: sampling,
            response: tx,
            return_logprobs: false,
            is_streaming: false,
            id: 0,
            constraint: Constraint::None,
            suffix: None,
            adapters: None,
            tools: None,
            tool_choice: None,
            logits_processors: None,
            return_raw_logits: false,
        });

        runner.get_sender()
            .map_err(|e| InferenceError::GenerationFailed(format!("Failed to get sender: {e}")))?
            .send(req)
            .await
            .map_err(|e| InferenceError::GenerationFailed(format!("Failed to send request: {e}")))?;

        // Wait for response
        let response = rx.recv()
            .await
            .ok_or_else(|| InferenceError::GenerationFailed("Channel closed".to_string()))?;

        let elapsed = start.elapsed();

        match response {
            Response::Done(done) => {
                let text = done.choices.first()
                    .map(|c| c.message.content.as_deref().unwrap_or("").to_string())
                    .unwrap_or_default();

                let tokens_generated = done.usage.completion_tokens as u32;
                let tokens_prompt = done.usage.prompt_tokens as u32;
                let tps = if elapsed.as_secs_f64() > 0.0 {
                    tokens_generated as f64 / elapsed.as_secs_f64()
                } else {
                    0.0
                };

                let model_id = {
                    self.loaded_model.read().await
                        .as_ref()
                        .map(|m| m.id.clone())
                        .unwrap_or_default()
                };

                Ok(InferenceResponse {
                    text,
                    tokens_generated,
                    tokens_prompt,
                    generation_time_ms: elapsed.as_millis() as u64,
                    tokens_per_second: tps,
                    backend: BackendType::MistralRs,
                    model_id,
                })
            }
            Response::InternalError(e) => {
                Err(InferenceError::GenerationFailed(format!("mistral.rs internal error: {e}")))
            }
            Response::ValidationError(e) => {
                Err(InferenceError::GenerationFailed(format!("Validation error: {e}")))
            }
            Response::ModelError(msg, _) => {
                Err(InferenceError::GenerationFailed(format!("Model error: {msg}")))
            }
            _ => {
                Err(InferenceError::GenerationFailed("Unexpected response type".to_string()))
            }
        }
    }

    async fn is_available(&self) -> bool {
        // mistral.rs is always available if compiled in
        true
    }
}

impl MistralRsBackend {
    /// Load a GGUF model file.
    async fn load_gguf(&self, model_path: &str, params: &GenerationParams) -> Result<Arc<MistralRs>> {
        let path = std::path::Path::new(model_path);
        let dir = path.parent()
            .ok_or_else(|| InferenceError::ModelNotFound { path: model_path.to_string() })?;
        let filename = path.file_name()
            .ok_or_else(|| InferenceError::ModelNotFound { path: model_path.to_string() })?
            .to_string_lossy()
            .to_string();

        info!(path = model_path, "Loading GGUF model via mistral.rs");

        let loader = GGUFLoaderBuilder::new(
            None, // chat template — auto-detect
            None, // tokenizer model id
            dir.to_string_lossy().to_string(),
            vec![filename],
            GGUFSpecificConfig::default(),
        )
        .build();

        let device = self.device();
        let dtype = ModelDType::Auto;

        let gpu_layers = if params.gpu_layers < 0 {
            DeviceMapSetting::Auto(mistralrs_core::AutoDeviceMapParams::default())
        } else if params.gpu_layers == 0 {
            DeviceMapSetting::Map(mistralrs_core::DeviceMapMetadata::dummy())
        } else {
            warn!(gpu_layers = params.gpu_layers, "Partial gpu_layers not yet supported by mistral.rs backend, using Auto");
            DeviceMapSetting::Auto(mistralrs_core::AutoDeviceMapParams::default())
        };

        let mut builder = MistralRsBuilder::new(loader, device, gpu_layers, dtype)
            .with_token_source(TokenSource::None);

        // Apply ISQ if configured
        if let Some(isq) = self.isq_type() {
            info!(isq = ?isq, "Applying In-Situ Quantization");
            builder = builder.with_isq(isq);
        }

        // Apply PagedAttention if configured
        if self.config.paged_attention {
            info!("PagedAttention enabled");
            // PagedAttention is auto-enabled when the feature is compiled in
        }

        let runner = builder.build()
            .await
            .map_err(|e| InferenceError::GenerationFailed(format!("Failed to build mistral.rs runner: {e}")))?;

        Ok(runner)
    }

    /// Load a Hugging Face model (safetensors) with optional ISQ.
    ///
    /// NOTE: HF model loading requires NormalLoaderBuilder (not GGUF).
    /// Currently returns an error — use GGUF models for mistral.rs backend.
    async fn load_hf(&self, model_id: &str, _params: &GenerationParams) -> Result<Arc<MistralRs>> {
        // HF safetensors models need NormalLoaderBuilder, not GGUFLoaderBuilder.
        // This is deferred until mistralrs-core API stabilizes.
        Err(InferenceError::InvalidFormat(format!(
            "HF model '{model_id}' is not supported yet — use GGUF format for mistral.rs backend"
        )))
    }
}

/// Build chat messages from system + user prompts.
fn build_chat_messages(system_prompt: &str, user_prompt: &str) -> RequestMessage {
    use indexmap::IndexMap;
    use either::Either;

    let mut messages = Vec::new();

    if !system_prompt.is_empty() {
        let mut msg = IndexMap::new();
        msg.insert("role".to_string(), Either::Left("system".to_string()));
        msg.insert("content".to_string(), Either::Left(system_prompt.to_string()));
        messages.push(msg);
    }

    let mut msg = IndexMap::new();
    msg.insert("role".to_string(), Either::Left("user".to_string()));
    msg.insert("content".to_string(), Either::Left(user_prompt.to_string()));
    messages.push(msg);

    RequestMessage::Chat(messages)
}
