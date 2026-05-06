//! Environment-variable backed secret adapter.
//!
//! Reads secrets from the process environment. Useful for CI pipelines and
//! Docker-based deployments that inject secrets via env vars.
//!
//! The `put` and `delete` operations are not supported for this backend —
//! they return an error indicating that env vars are read-only.

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};

use super::SecretManager;

/// A read-only [`SecretManager`] backed by process environment variables.
pub struct EnvSecretAdapter;

impl EnvSecretAdapter {
    /// Create a new adapter. No configuration is required.
    pub fn new() -> Self {
        Self
    }
}

impl Default for EnvSecretAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretManager for EnvSecretAdapter {
    /// Read a secret from `std::env::var(name)`.
    async fn get(&self, name: &str) -> Result<String> {
        std::env::var(name).map_err(|_| {
            DuDuClawError::Security(format!(
                "environment variable '{name}' is not set"
            ))
        })
    }

    /// Not supported — environment variables are read-only at runtime.
    async fn put(&self, name: &str, _value: &str) -> Result<()> {
        Err(DuDuClawError::Security(format!(
            "env backend is read-only: cannot write secret '{name}'"
        )))
    }

    /// Not supported — environment variables are read-only at runtime.
    async fn delete(&self, name: &str) -> Result<()> {
        Err(DuDuClawError::Security(format!(
            "env backend is read-only: cannot delete secret '{name}'"
        )))
    }

    /// Returns `true` if the environment variable is set.
    async fn exists(&self, name: &str) -> Result<bool> {
        Ok(std::env::var(name).is_ok())
    }
}
