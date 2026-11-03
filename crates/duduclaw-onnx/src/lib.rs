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

/// Refuse employee-writable native models while the gateway holds the root key.
/// Call before opening model files or entering any ORT API.
pub fn refuse_in_process_models() -> Result<(), String> {
    if duduclaw_core::root_key::is_held() {
        return Err("獨立金鑰模式下不在主程式內載入 ONNX 模型（隱私偵測、本機知識庫向量、Kokoro 語音暫停使用）".into());
    }
    Ok(())
}
