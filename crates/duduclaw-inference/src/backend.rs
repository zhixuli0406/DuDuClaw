//! Unified inference backend trait.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{GenerationParams, InferenceRequest, InferenceResponse, ModelInfo};

/// Unified interface for all inference backends.
///
/// Implementations: LlamaCppBackend, OpenAiCompatBackend, (future) MistralRsBackend
#[async_trait]
pub trait InferenceBackend: Send + Sync {
    /// Human-readable backend name (e.g., "llama.cpp (Metal)")
    fn name(&self) -> &str;

    /// Load a model into memory.
    async fn load_model(&self, model_path: &str, params: &GenerationParams) -> Result<ModelInfo>;

    /// Unload the currently loaded model, freeing memory.
    async fn unload_model(&self) -> Result<()>;

    /// Get info about the currently loaded model, if any.
    async fn loaded_model(&self) -> Option<ModelInfo>;

    /// Generate text from a prompt (blocking until complete).
    async fn generate(&self, request: &InferenceRequest) -> Result<InferenceResponse>;

    /// Check if the backend is available on this system.
    async fn is_available(&self) -> bool;

    /// Whether `load_model` expects `model_path` to be an existing file under
    /// `models_dir`. Local backends (llama.cpp, mistral.rs) return `true`;
    /// remote backends (OpenAI-compatible HTTP) return `false` because the
    /// model lives on a server and `model_path` is treated as the model id.
    ///
    /// `InferenceEngine::load_model` consults this to skip `ModelManager::resolve_path`
    /// for remote backends — otherwise the engine errors with `ModelNotFound`
    /// before the request ever reaches the backend.
    fn requires_local_file(&self) -> bool {
        true
    }

    /// Estimate memory required for a model file (in MB).
    fn estimate_memory_mb(&self, file_size_bytes: u64) -> u64 {
        // Rule of thumb: GGUF file size ≈ memory needed + ~10% overhead
        (file_size_bytes / (1024 * 1024)) * 11 / 10
    }
}
