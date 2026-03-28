//! llama.cpp backend — Metal / CUDA / Vulkan / CPU inference via `llama-cpp-2`.

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::info;

use crate::backend::InferenceBackend;
use crate::error::{InferenceError, Result};
use crate::types::*;

/// llama.cpp inference backend.
pub struct LlamaCppBackend {
    loaded_model: RwLock<Option<ModelInfo>>,
}

impl LlamaCppBackend {
    pub fn new() -> Self {
        Self {
            loaded_model: RwLock::new(None),
        }
    }
}

#[async_trait]
impl InferenceBackend for LlamaCppBackend {
    fn name(&self) -> &str {
        #[cfg(feature = "metal")]
        { "llama.cpp (Metal)" }
        #[cfg(feature = "cuda")]
        { "llama.cpp (CUDA)" }
        #[cfg(not(any(feature = "metal", feature = "cuda")))]
        { "llama.cpp (Vulkan/CPU)" }
    }

    async fn load_model(&self, model_path: &str, params: &GenerationParams) -> Result<ModelInfo> {
        info!(path = model_path, "Loading GGUF model via llama.cpp");

        let file_size = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
        let model_id = std::path::Path::new(model_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let info = ModelInfo {
            id: model_id,
            path: model_path.to_string(),
            architecture: "gguf".to_string(),
            parameter_count: "auto".to_string(),
            quantization: "auto".to_string(),
            file_size_bytes: file_size,
            estimated_memory_mb: file_size / (1024 * 1024) * 11 / 10,
            is_loaded: true,
            context_length: params.context_size,
        };

        // TODO: actual llama-cpp-2 model loading
        // let model = llama_cpp_2::LlamaModel::load_from_file(model_path, ...)?;
        *self.loaded_model.write().await = Some(info.clone());
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        *self.loaded_model.write().await = None;
        Ok(())
    }

    async fn loaded_model(&self) -> Option<ModelInfo> {
        self.loaded_model.read().await.clone()
    }

    async fn generate(&self, _request: &InferenceRequest) -> Result<InferenceResponse> {
        let model_id = {
            self.loaded_model.read().await
                .as_ref()
                .ok_or(InferenceError::NoModelLoaded)?
                .id.clone()
        };

        Err(InferenceError::GenerationFailed(
            format!("llama.cpp backend for model '{model_id}' is not yet fully implemented. Use 'openai_compat' or 'mistral_rs' backend instead.")
        ))
    }

    async fn is_available(&self) -> bool {
        true
    }
}
