//! Error type for the pet pack + background-removal pipeline.

use thiserror::Error;

/// Errors surfaced by pet pack generation, loading, and background removal.
#[derive(Debug, Error)]
pub enum PetError {
    /// I/O failure touching the pets directory or a pack file.
    #[error("io error: {0}")]
    Io(String),

    /// `pet.json` failed to (de)serialize.
    #[error("manifest (de)serialization error: {0}")]
    Manifest(String),

    /// Image decode/encode/resize failure.
    #[error("image error: {0}")]
    Image(String),

    /// A requested pet slug does not exist on disk.
    #[error("pet not found: {0}")]
    NotFound(String),

    /// The supplied display name sanitized to an empty slug.
    #[error("invalid pet name: {0}")]
    InvalidName(String),

    /// Background-removal model missing / unavailable.
    #[error("model unavailable: {0}")]
    ModelUnavailable(String),

    /// Model download or checksum verification failure.
    #[error("model download error: {0}")]
    ModelDownload(String),

    /// ONNX inference failure.
    #[error("inference error: {0}")]
    Inference(String),
}

impl From<std::io::Error> for PetError {
    fn from(e: std::io::Error) -> Self {
        PetError::Io(e.to_string())
    }
}

/// Result alias for the crate.
pub type Result<T> = std::result::Result<T, PetError>;
