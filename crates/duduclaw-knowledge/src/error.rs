//! Error type shared by the store, ACL and retrieval code.
//!
//! Messages meant for people are zh-TW. [`KbError::PermissionDenied`] never
//! names a collection or a source: a caller that may not read something must
//! not learn that it exists.

/// Result alias for this crate.
pub type KbResult<T> = Result<T, KbError>;

#[derive(Debug, thiserror::Error)]
pub enum KbError {
    #[error("knowledge database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("knowledge storage error: {0}")]
    Io(#[from] std::io::Error),
    /// The caller may not see or change the target. Deliberately carries no
    /// detail.
    #[error("permission denied")]
    PermissionDenied,
    /// Bad input; the message is shown to the person who sent it.
    #[error("{0}")]
    Invalid(String),
    /// A concurrent change won (stale revision / timestamp).
    #[error("{0}")]
    Conflict(String),
    /// A closed-set ingest failure (`too_large`, `quota_sources`, …).
    #[error("{message}")]
    Rejected { code: &'static str, message: String },
}

impl KbError {
    /// Short machine code for logs and the access log `outcome`.
    pub fn code(&self) -> &'static str {
        match self {
            KbError::Db(_) | KbError::Io(_) => "error",
            KbError::PermissionDenied => "permission_denied",
            KbError::Invalid(_) => "invalid",
            KbError::Conflict(_) => "conflict",
            KbError::Rejected { code, .. } => code,
        }
    }

    /// Text safe to show a dashboard user or an AI employee. Storage errors
    /// are summarised (their detail goes to the gateway log, not the caller).
    pub fn public_message(&self) -> String {
        match self {
            KbError::Db(_) | KbError::Io(_) => "知識庫暫時無法使用，請稍後再試。".to_string(),
            KbError::PermissionDenied => "permission denied".to_string(),
            KbError::Invalid(m) | KbError::Conflict(m) => m.clone(),
            KbError::Rejected { message, .. } => message.clone(),
        }
    }
}
