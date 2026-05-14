//! Redaction-pipeline error type.

use thiserror::Error;

/// Errors emitted by the redaction pipeline.
///
/// The pipeline is *fail-closed*: any of these errors at redact-time MUST
/// halt the outgoing LLM request — the caller is responsible for not
/// leaking raw text when redaction fails.
#[derive(Debug, Error)]
pub enum RedactionError {
    /// Vault read / write failure (IO, lock, schema).
    #[error("redaction vault error: {0}")]
    Vault(String),

    /// A rule could not be compiled (bad regex, malformed spec).
    #[error("rule '{rule_id}' compilation failed: {reason}")]
    RuleCompile { rule_id: String, reason: String },

    /// Crypto failure (encrypt / decrypt / key load).
    #[error("redaction crypto error: {0}")]
    Crypto(String),

    /// Per-agent key is missing and could not be generated.
    #[error("missing redaction key for agent '{agent_id}'")]
    MissingKey { agent_id: String },

    /// A string that looked like a token could not be parsed.
    #[error("invalid token: {0}")]
    InvalidToken(String),

    /// Configuration is malformed.
    #[error("redaction config error: {0}")]
    Config(String),

    /// Underlying IO error.
    #[error("redaction io error: {0}")]
    Io(#[from] std::io::Error),

    /// Underlying SQLite error.
    #[error("redaction sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Underlying identity provider error.
    #[error("redaction identity error: {0}")]
    Identity(#[from] duduclaw_identity::IdentityError),

    /// TOML deserialisation error (profile / config files).
    #[error("redaction toml error: {0}")]
    Toml(#[from] toml::de::Error),

    /// JSON error (audit / egress arg traversal).
    #[error("redaction json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl RedactionError {
    pub fn vault(reason: impl Into<String>) -> Self {
        RedactionError::Vault(reason.into())
    }

    pub fn crypto(reason: impl Into<String>) -> Self {
        RedactionError::Crypto(reason.into())
    }

    pub fn config(reason: impl Into<String>) -> Self {
        RedactionError::Config(reason.into())
    }

    pub fn rule_compile(rule_id: impl Into<String>, reason: impl Into<String>) -> Self {
        RedactionError::RuleCompile {
            rule_id: rule_id.into(),
            reason: reason.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, RedactionError>;
