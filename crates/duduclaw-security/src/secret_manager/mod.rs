//! Secret Manager integration (ADR-004).
//!
//! Provides a unified abstraction over multiple secret storage backends:
//! - `local`  — in-process AES-256-GCM encrypted store (dev / testing)
//! - `vault`  — HashiCorp Vault KV v2 HTTP API (production)
//! - `env`    — reads from process environment variables (CI / override)
//!
//! ## URI Scheme
//!
//! Secrets are addressed with `secret://<backend>/<name>` URIs:
//!
//! ```text
//! secret://vault/brave_search
//! secret://local/figma_token
//! secret://env/MY_API_KEY
//! ```
//!
//! ## Config (`~/.duduclaw/config.toml`)
//!
//! ```toml
//! [secret_manager]
//! backend = "vault"          # "local" | "vault" | "env"
//! vault_addr  = "http://127.0.0.1:8200"
//! vault_token = ""           # set directly or use vault_token_enc
//! vault_token_enc = ""       # base64(AES-256-GCM encrypted token)
//! vault_mount = "secret"     # KV v2 mount point
//! ```

mod env;
mod local;
mod vault;

pub use env::EnvSecretAdapter;
pub use local::LocalSecretAdapter;
pub use vault::VaultHttpAdapter;

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

// ─── SecretBackend ─────────────────────────────────────────────────────────

/// The storage backend for a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretBackend {
    /// In-process AES-256-GCM encrypted store.
    Local,
    /// HashiCorp Vault KV v2.
    Vault,
    /// OS process environment variable.
    Env,
}

impl fmt::Display for SecretBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretBackend::Local => write!(f, "local"),
            SecretBackend::Vault => write!(f, "vault"),
            SecretBackend::Env => write!(f, "env"),
        }
    }
}

// ─── SecretUri ─────────────────────────────────────────────────────────────

/// A parsed `secret://<backend>/<name>` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretUri {
    pub backend: SecretBackend,
    /// Secret name (may contain `/` for path-based Vault secrets).
    pub name: String,
}

impl SecretUri {
    /// Parse a `secret://` URI string.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The scheme is not `secret://`
    /// - The backend is unrecognised
    /// - The secret name is empty
    pub fn parse(uri: &str) -> Result<Self> {
        let rest = uri
            .strip_prefix("secret://")
            .ok_or_else(|| DuDuClawError::Security(format!("invalid scheme in URI: {uri}")))?;

        // Split on the first `/` to separate backend from name.
        let slash = rest.find('/').ok_or_else(|| {
            DuDuClawError::Security(format!("URI missing backend/name separator: {uri}"))
        })?;

        let backend_str = &rest[..slash];
        let name = &rest[slash + 1..];

        if name.is_empty() {
            return Err(DuDuClawError::Security(format!(
                "secret name is empty in URI: {uri}"
            )));
        }

        let backend = match backend_str {
            "local" => SecretBackend::Local,
            "vault" => SecretBackend::Vault,
            "env" => SecretBackend::Env,
            other => {
                return Err(DuDuClawError::Security(format!(
                    "unknown secret backend '{other}' in URI: {uri}"
                )))
            }
        };

        Ok(Self {
            backend,
            name: name.to_string(),
        })
    }
}

impl fmt::Display for SecretUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret://{}/{}", self.backend, self.name)
    }
}

// ─── SecretManager trait ───────────────────────────────────────────────────

/// Unified async interface for reading and writing secrets.
#[async_trait]
pub trait SecretManager: Send + Sync {
    /// Retrieve a secret value by name.
    async fn get(&self, name: &str) -> Result<String>;

    /// Create or overwrite a secret.
    ///
    /// Both `name` and `value` must be non-empty.
    async fn put(&self, name: &str, value: &str) -> Result<()>;

    /// Delete a secret.
    ///
    /// Returns an error if the secret does not exist.
    async fn delete(&self, name: &str) -> Result<()>;

    /// Return `true` if a secret with the given name exists.
    async fn exists(&self, name: &str) -> Result<bool>;
}

// ─── SecretManagerConfig ──────────────────────────────────────────────────

/// Configuration section `[secret_manager]` in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretManagerConfig {
    /// Backend selection: `"local"`, `"vault"`, or `"env"`.
    #[serde(default = "default_backend")]
    pub backend: String,

    /// Vault server address (used when `backend = "vault"`).
    pub vault_addr: Option<String>,

    /// Plain-text Vault token (use `vault_token_enc` in production).
    pub vault_token: Option<String>,

    /// AES-256-GCM encrypted Vault token (base64 encoded).
    pub vault_token_enc: Option<String>,

    /// KV v2 mount point (defaults to `"secret"`).
    pub vault_mount: Option<String>,
}

fn default_backend() -> String {
    "local".to_string()
}

impl Default for SecretManagerConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            vault_addr: None,
            vault_token: None,
            vault_token_enc: None,
            vault_mount: None,
        }
    }
}

impl SecretManagerConfig {
    /// Parse from a full config.toml string. Reads the `[secret_manager]` table.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            secret_manager: SecretManagerConfig,
        }
        let wrapper: Wrapper = toml::from_str(s)
            .map_err(|e| DuDuClawError::Config(format!("failed to parse secret_manager config: {e}")))?;
        Ok(wrapper.secret_manager)
    }

    /// Effective Vault address (falls back to `http://127.0.0.1:8200`).
    pub fn effective_vault_addr(&self) -> &str {
        self.vault_addr
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("http://127.0.0.1:8200")
    }

    /// Effective KV v2 mount point (falls back to `"secret"`).
    pub fn effective_vault_mount(&self) -> &str {
        self.vault_mount
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("secret")
    }
}
