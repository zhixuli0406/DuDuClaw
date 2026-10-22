//! Shared ONNX Runtime plumbing.
//!
//! | module | role |
//! |---|---|
//! | [`manifest`] | pinned ONNX Runtime release archives (URL / size / sha256) |
//! | [`install`] | verified, resumable downloads; ORT archive extraction (feature `install`) |
//! | [`runtime`] | `init_ort`, once per process (feature `runtime`) |
//!
//! Extracted from `duduclaw-redaction` so the redaction NER model and the
//! knowledge-base embedding models load the same library through the same
//! one-time initialisation.

pub mod manifest;

#[cfg(feature = "install")]
pub mod install;
#[cfg(feature = "runtime")]
pub mod runtime;

/// Errors from this crate. Messages are operator-facing zh-TW text.
#[derive(Debug, thiserror::Error)]
pub enum OnnxError {
    /// A failed download, verification, extraction or load. The text is
    /// safe to show (no query strings, no credentials).
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl OnnxError {
    pub fn msg(m: impl Into<String>) -> Self {
        OnnxError::Message(m.into())
    }
}

/// Result alias.
pub type OnnxResult<T> = Result<T, OnnxError>;
