//! Inference error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("Model not found: {path}")]
    ModelNotFound { path: String },

    #[error("Backend unavailable: {backend} — {reason}")]
    BackendUnavailable { backend: String, reason: String },

    #[error("Out of memory: need {required_mb}MB, available {available_mb}MB")]
    OutOfMemory { required_mb: u64, available_mb: u64 },

    #[error("Generation failed: {0}")]
    GenerationFailed(String),

    #[error("Model already loaded: {0}")]
    ModelAlreadyLoaded(String),

    #[error("No model loaded")]
    NoModelLoaded,

    #[error("Invalid model format: {0}")]
    InvalidFormat(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("HTTP backend error: {0}")]
    Http(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, InferenceError>;
