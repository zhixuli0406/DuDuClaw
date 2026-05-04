//! Identity-resolution error type.

use thiserror::Error;

/// Errors returned by an [`crate::IdentityProvider`].
///
/// `resolve_by_channel` returns `Ok(None)` for "person not found" — that's
/// the normal "stranger sends a message" case and not an error. Errors here
/// describe genuine provider failures (network, schema drift, IO).
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Upstream is unreachable. The chained provider may degrade to cache.
    #[error("identity provider '{provider}' unreachable: {reason}")]
    Unreachable { provider: String, reason: String },

    /// Upstream responded with a malformed / unexpected schema.
    #[error("identity provider '{provider}' returned malformed data: {reason}")]
    Malformed { provider: String, reason: String },

    /// The provider was asked for a record it cannot fulfil at all (e.g.
    /// `lookup_project_members` against a provider that does not model
    /// projects).
    #[error("identity provider '{provider}' does not support: {operation}")]
    Unsupported { provider: String, operation: String },

    /// IO error reading the cache or local config.
    #[error("identity IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Catch-all for provider-internal failures with no clean classification.
    #[error("identity provider '{provider}' error: {reason}")]
    Internal { provider: String, reason: String },
}

impl IdentityError {
    pub fn unreachable(provider: impl Into<String>, reason: impl Into<String>) -> Self {
        IdentityError::Unreachable { provider: provider.into(), reason: reason.into() }
    }

    pub fn malformed(provider: impl Into<String>, reason: impl Into<String>) -> Self {
        IdentityError::Malformed { provider: provider.into(), reason: reason.into() }
    }
}
