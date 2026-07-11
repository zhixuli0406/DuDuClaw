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
//! backend = "vault"          # "local" | "vault" | "env" | "onepassword" | "infisical"
//! vault_addr  = "http://127.0.0.1:8200"
//! vault_token = ""           # plaintext Vault token (currently the only field used)
//! vault_token_enc = ""       # base64(AES-256-GCM encrypted token) — RESERVED, see note
//! vault_mount = "secret"     # KV v2 mount point
//!
//! # 1Password Connect (self-hosted):
//! onepassword_host  = "https://op-connect.internal:8080"
//! onepassword_token = ""     # or onepassword_token_enc (keyfile-encrypted)
//! onepassword_vault = "<vault-id>"
//!
//! # Infisical:
//! infisical_addr        = "https://app.infisical.com"
//! infisical_token       = ""  # or infisical_token_enc (keyfile-encrypted)
//! infisical_project_id  = "<workspace-id>"
//! infisical_environment = "prod"
//! ```
//!
//! ## Note on `vault_token_enc`
//!
//! `vault_token_enc` is decrypted read-only at runtime via the per-machine
//! keyfile (`~/.duduclaw/.keyfile`). Callers that have a `home_dir` should use
//! [`SecretManagerConfig::resolved_vault_token`], which prefers the decrypted
//! `vault_token_enc` and falls back to the plaintext `vault_token`. The
//! plaintext-only [`SecretManagerConfig::effective_vault_token`] is retained
//! for callers without a `home_dir`.

mod env;
mod infisical;
mod local;
mod onepassword;
mod vault;

pub use env::EnvSecretAdapter;
pub use infisical::InfisicalAdapter;
pub use local::LocalSecretAdapter;
pub use onepassword::OnePasswordConnectAdapter;
pub use vault::VaultHttpAdapter;

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

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
    /// 1Password Connect (self-hosted).
    #[serde(rename = "onepassword")]
    OnePassword,
    /// Infisical.
    Infisical,
}

impl fmt::Display for SecretBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretBackend::Local => write!(f, "local"),
            SecretBackend::Vault => write!(f, "vault"),
            SecretBackend::Env => write!(f, "env"),
            SecretBackend::OnePassword => write!(f, "onepassword"),
            SecretBackend::Infisical => write!(f, "infisical"),
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
            "onepassword" | "1password" | "op" => SecretBackend::OnePassword,
            "infisical" => SecretBackend::Infisical,
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
    ///
    /// Decrypted read-side via [`SecretManagerConfig::resolved_vault_token`]
    /// (preferred over the plaintext `vault_token` when set).
    pub vault_token_enc: Option<String>,

    /// KV v2 mount point (defaults to `"secret"`).
    pub vault_mount: Option<String>,

    // ── 1Password Connect ────────────────────────────────────────────────
    /// Connect server host, e.g. `https://op-connect.internal:8080`.
    pub onepassword_host: Option<String>,
    /// Connect API token (plaintext).
    pub onepassword_token: Option<String>,
    /// Encrypted Connect token (base64 AES-256-GCM), decrypted via the keyfile.
    pub onepassword_token_enc: Option<String>,
    /// Vault id (or name) items are read from.
    pub onepassword_vault: Option<String>,

    // ── Infisical ────────────────────────────────────────────────────────
    /// Infisical base address (defaults to `https://app.infisical.com`).
    pub infisical_addr: Option<String>,
    /// Infisical service token (plaintext).
    pub infisical_token: Option<String>,
    /// Encrypted Infisical token (base64 AES-256-GCM), decrypted via the keyfile.
    pub infisical_token_enc: Option<String>,
    /// Project / workspace id.
    pub infisical_project_id: Option<String>,
    /// Environment slug (defaults to `prod`).
    pub infisical_environment: Option<String>,
}

fn default_backend() -> String {
    "local".to_string()
}

/// Resolve an encrypted-or-plaintext token pair: prefer the keyfile-decrypted
/// `enc` value, fall back to `plain`, else `None`. The decrypted value is never
/// logged (a decrypt failure warns without the ciphertext/plaintext).
fn resolve_enc_or_plain(
    enc: Option<&str>,
    plain: Option<&str>,
    home_dir: &std::path::Path,
) -> Option<String> {
    if let Some(enc) = enc.filter(|s| !s.is_empty()) {
        if let Some(decrypted) = crate::keyfile::decrypt_keyfile_value(enc, home_dir) {
            return Some(decrypted);
        }
        tracing::warn!(
            "[secret_manager] *_token_enc set but could not be decrypted; \
             falling back to plaintext token if present"
        );
    }
    plain.filter(|s| !s.is_empty()).map(str::to_string)
}

impl Default for SecretManagerConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            vault_addr: None,
            vault_token: None,
            vault_token_enc: None,
            vault_mount: None,
            onepassword_host: None,
            onepassword_token: None,
            onepassword_token_enc: None,
            onepassword_vault: None,
            infisical_addr: None,
            infisical_token: None,
            infisical_token_enc: None,
            infisical_project_id: None,
            infisical_environment: None,
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

    /// Plaintext-only effective Vault token (no decryption).
    ///
    /// Prefer [`Self::resolved_vault_token`], which additionally decrypts
    /// `vault_token_enc`. This accessor is retained for callers that do not
    /// have a `home_dir` and only ever set the plaintext `vault_token`.
    pub fn effective_vault_token(&self) -> Option<&str> {
        self.vault_token.as_deref().filter(|s| !s.is_empty())
    }

    /// Resolve the effective Vault token to authenticate with.
    ///
    /// Resolution order:
    /// 1. If `vault_token_enc` is set + non-empty, decrypt it read-only via the
    ///    per-machine keyfile ([`crate::keyfile::decrypt_keyfile_value`]). On
    ///    any decrypt failure (missing/short keyfile, bad ciphertext) this
    ///    falls through to step 2 rather than failing hard.
    /// 2. Otherwise fall back to the plaintext `vault_token`.
    ///
    /// Returns `None` when neither yields a non-empty token. The decrypted
    /// token is never logged.
    pub fn resolved_vault_token(&self, home_dir: &std::path::Path) -> Option<String> {
        if let Some(enc) = self.vault_token_enc.as_deref() {
            if !enc.is_empty() {
                if let Some(plain) = crate::keyfile::decrypt_keyfile_value(enc, home_dir) {
                    return Some(plain);
                }
                tracing::warn!(
                    "[secret_manager] vault_token_enc set but could not be decrypted; \
                     falling back to plaintext vault_token if present"
                );
            }
        }
        self.vault_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// Resolve the effective 1Password Connect token (decrypts
    /// `onepassword_token_enc` via the keyfile, else plaintext). Never logged.
    pub fn resolved_onepassword_token(&self, home_dir: &std::path::Path) -> Option<String> {
        resolve_enc_or_plain(
            self.onepassword_token_enc.as_deref(),
            self.onepassword_token.as_deref(),
            home_dir,
        )
    }

    /// Resolve the effective Infisical token (decrypts `infisical_token_enc` via
    /// the keyfile, else plaintext). Never logged.
    pub fn resolved_infisical_token(&self, home_dir: &std::path::Path) -> Option<String> {
        resolve_enc_or_plain(
            self.infisical_token_enc.as_deref(),
            self.infisical_token.as_deref(),
            home_dir,
        )
    }

    /// Effective Infisical address (falls back to the SaaS default).
    pub fn effective_infisical_addr(&self) -> &str {
        self.infisical_addr
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("https://app.infisical.com")
    }

    /// Effective Infisical environment slug (falls back to `prod`).
    pub fn effective_infisical_environment(&self) -> &str {
        self.infisical_environment
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("prod")
    }

    /// Build a concrete [`SecretManager`] backend from this config.
    ///
    /// This is the wiring entry point that turns `[secret_manager]` config into
    /// a live adapter:
    /// - `"local"` → in-process AES adapter ([`LocalSecretAdapter`]).
    /// - `"env"`   → process environment variables ([`EnvSecretAdapter`]).
    /// - `"vault"` → HashiCorp Vault KV v2 ([`VaultHttpAdapter`]), authenticated
    ///   with the token from [`Self::resolved_vault_token`] (which decrypts
    ///   `vault_token_enc` via the per-machine keyfile when present).
    ///
    /// `home_dir` is the DuDuClaw home (`~/.duduclaw`) used to locate the
    /// keyfile for `vault_token_enc` decryption. Returns an error when the
    /// `vault` backend is selected but no token can be resolved, or when
    /// `backend` is unrecognised.
    pub fn build_manager(
        &self,
        home_dir: &std::path::Path,
    ) -> Result<Box<dyn SecretManager>> {
        let backend = match self.backend.as_str() {
            "local" => SecretBackend::Local,
            "vault" => SecretBackend::Vault,
            "env" => SecretBackend::Env,
            "onepassword" | "1password" | "op" => SecretBackend::OnePassword,
            "infisical" => SecretBackend::Infisical,
            other => {
                return Err(DuDuClawError::Security(format!(
                    "unknown secret_manager backend '{other}' \
                     (expected local|vault|env|onepassword|infisical)"
                )))
            }
        };
        self.build_manager_for(backend, home_dir)
    }

    /// Build a [`SecretManager`] for an explicitly-chosen backend, using this
    /// config's connection params (addr/mount/token).
    ///
    /// Used by the `secret://<backend>/<name>` indirection path so that a
    /// reference's own backend (e.g. `secret://vault/...`) is honored even when
    /// the default `[secret_manager].backend` differs.
    pub fn build_manager_for(
        &self,
        backend: SecretBackend,
        home_dir: &std::path::Path,
    ) -> Result<Box<dyn SecretManager>> {
        match backend {
            SecretBackend::Local => Ok(Box::new(LocalSecretAdapter::new_ephemeral())),
            SecretBackend::Env => Ok(Box::new(EnvSecretAdapter::new())),
            SecretBackend::Vault => {
                let token = self.resolved_vault_token(home_dir).ok_or_else(|| {
                    DuDuClawError::Security(
                        "secret_manager vault backend selected but no token could be \
                         resolved (set vault_token, or vault_token_enc with a valid \
                         ~/.duduclaw/.keyfile)"
                            .to_string(),
                    )
                })?;
                Ok(Box::new(VaultHttpAdapter::new(
                    self.effective_vault_addr(),
                    token,
                    self.effective_vault_mount(),
                )))
            }
            SecretBackend::OnePassword => {
                let token = self.resolved_onepassword_token(home_dir).ok_or_else(|| {
                    DuDuClawError::Security(
                        "secret_manager onepassword backend selected but no token could be \
                         resolved (set onepassword_token or onepassword_token_enc)"
                            .to_string(),
                    )
                })?;
                let host = self
                    .onepassword_host
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        DuDuClawError::Security(
                            "secret_manager onepassword backend requires onepassword_host"
                                .to_string(),
                        )
                    })?;
                let vault = self
                    .onepassword_vault
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        DuDuClawError::Security(
                            "secret_manager onepassword backend requires onepassword_vault"
                                .to_string(),
                        )
                    })?;
                Ok(Box::new(OnePasswordConnectAdapter::new(host, token, vault)))
            }
            SecretBackend::Infisical => {
                let token = self.resolved_infisical_token(home_dir).ok_or_else(|| {
                    DuDuClawError::Security(
                        "secret_manager infisical backend selected but no token could be \
                         resolved (set infisical_token or infisical_token_enc)"
                            .to_string(),
                    )
                })?;
                let project_id = self
                    .infisical_project_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        DuDuClawError::Security(
                            "secret_manager infisical backend requires infisical_project_id"
                                .to_string(),
                        )
                    })?;
                Ok(Box::new(InfisicalAdapter::new(
                    self.effective_infisical_addr(),
                    token,
                    project_id,
                    self.effective_infisical_environment(),
                )))
            }
        }
    }
}

// ─── secret:// reference resolver ──────────────────────────────────────────

/// Resolve a config value that may be a `secret://<backend>/<name>` reference.
///
/// - **Non-reference** (anything not starting with `secret://`) → returns the
///   value unchanged (wrapped in `Some`). Existing inline / `_enc` secrets are
///   passed through untouched.
/// - **Reference** → parses the URI, builds the backend via
///   [`SecretManagerConfig::build_manager_for`] (the reference's own backend is
///   honored, e.g. `secret://vault/...` always uses Vault), and fetches
///   `uri.name`.
///
/// Fail-soft: any parse / build / fetch error logs a `warn` and yields `None`,
/// so the caller behaves as if the secret were unset rather than panicking.
/// The resolved secret value is never logged.
///
/// `home_dir` is the DuDuClaw home (`~/.duduclaw`), used to locate the keyfile
/// for `vault_token_enc` decryption when building a Vault backend.
pub async fn resolve_secret_reference(
    value: &str,
    sm_cfg: &SecretManagerConfig,
    home_dir: &Path,
) -> Option<String> {
    if !value.starts_with("secret://") {
        return Some(value.to_string());
    }

    let uri = match SecretUri::parse(value) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(reference = %value, "invalid secret:// reference: {e}");
            return None;
        }
    };

    let manager = match sm_cfg.build_manager_for(uri.backend, home_dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(reference = %value, "cannot build secret backend: {e}");
            return None;
        }
    };

    match manager.get(&uri.name).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(reference = %value, "secret resolution failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod resolve_reference_tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[tokio::test]
    async fn non_reference_passthrough() {
        let cfg = SecretManagerConfig::default();
        // A plain token / inline value is returned unchanged.
        assert_eq!(
            resolve_secret_reference("plain-token", &cfg, &tmp_home()).await,
            Some("plain-token".to_string())
        );
        // Empty string is also a non-reference passthrough.
        assert_eq!(
            resolve_secret_reference("", &cfg, &tmp_home()).await,
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn invalid_uri_returns_none() {
        let cfg = SecretManagerConfig::default();
        // Looks like a reference but the backend is unknown → fail-soft None.
        assert!(
            resolve_secret_reference("secret://bogus/whatever", &cfg, &tmp_home())
                .await
                .is_none()
        );
        // Missing backend/name separator.
        assert!(
            resolve_secret_reference("secret://oops", &cfg, &tmp_home())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn env_backend_round_trip() {
        // Use a unique env var name so parallel tests don't collide.
        let var = format!("DUDUCLAW_TEST_SECRET_{}", std::process::id());
        // SAFETY: single-threaded test setup/teardown around this env var; the
        // name is process-unique so concurrent tests don't race on it.
        unsafe { std::env::set_var(&var, "resolved-via-env") };
        let cfg = SecretManagerConfig::default();
        let reference = format!("secret://env/{var}");
        let resolved = resolve_secret_reference(&reference, &cfg, &tmp_home()).await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(resolved.as_deref(), Some("resolved-via-env"));
    }

    #[tokio::test]
    async fn env_backend_missing_var_returns_none() {
        let cfg = SecretManagerConfig::default();
        assert!(
            resolve_secret_reference(
                "secret://env/DUDUCLAW_DEFINITELY_UNSET_VAR_XYZ",
                &cfg,
                &tmp_home()
            )
            .await
            .is_none()
        );
    }
}

#[cfg(test)]
mod token_resolution_tests {
    use super::*;
    use crate::crypto::CryptoEngine;

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-secretmgr-test-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn with_keyfile(&self) -> CryptoEngine {
            let key = CryptoEngine::generate_key().unwrap();
            std::fs::write(self.0.join(".keyfile"), key).unwrap();
            CryptoEngine::new(&key).unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn enc_token_wins_over_plaintext() {
        let home = TempHome::new();
        let engine = home.with_keyfile();
        let enc = engine.encrypt_string("encrypted-token").unwrap();
        let cfg = SecretManagerConfig {
            vault_token: Some("plaintext-token".into()),
            vault_token_enc: Some(enc),
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_vault_token(home.path()).as_deref(),
            Some("encrypted-token")
        );
    }

    #[test]
    fn plaintext_only_returns_plaintext() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig {
            vault_token: Some("plaintext-token".into()),
            vault_token_enc: None,
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_vault_token(home.path()).as_deref(),
            Some("plaintext-token")
        );
    }

    #[test]
    fn neither_returns_none() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig::default();
        assert!(cfg.resolved_vault_token(home.path()).is_none());
    }

    #[test]
    fn enc_decrypt_failure_falls_back_to_plaintext() {
        // No keyfile present → enc cannot be decrypted → fall back.
        let home = TempHome::new();
        let cfg = SecretManagerConfig {
            vault_token: Some("plaintext-token".into()),
            vault_token_enc: Some("garbage-ciphertext".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_vault_token(home.path()).as_deref(),
            Some("plaintext-token")
        );
    }

    // ─── build_manager factory (Vault adapter wiring) ───────────

    #[test]
    fn build_manager_local_and_env_ok() {
        let home = TempHome::new();
        let local = SecretManagerConfig { backend: "local".into(), ..Default::default() };
        assert!(local.build_manager(home.path()).is_ok());
        let env = SecretManagerConfig { backend: "env".into(), ..Default::default() };
        assert!(env.build_manager(home.path()).is_ok());
    }

    #[test]
    fn build_manager_unknown_backend_errors() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig { backend: "bogus".into(), ..Default::default() };
        assert!(cfg.build_manager(home.path()).is_err());
    }

    #[test]
    fn build_manager_vault_without_token_errors() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig {
            backend: "vault".into(),
            vault_addr: Some("http://127.0.0.1:8200".into()),
            ..Default::default()
        };
        assert!(
            cfg.build_manager(home.path()).is_err(),
            "vault backend without a token must fail to build"
        );
    }

    #[test]
    fn build_manager_vault_with_plaintext_token_ok() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig {
            backend: "vault".into(),
            vault_token: Some("s.dev-token".into()),
            ..Default::default()
        };
        assert!(cfg.build_manager(home.path()).is_ok());
    }

    #[test]
    fn build_manager_for_honors_reference_backend_over_default() {
        // Default backend is "local", but a `secret://vault/...` reference must
        // still build a Vault adapter (per-reference backend wins).
        let home = TempHome::new();
        let cfg = SecretManagerConfig {
            backend: "local".into(),
            vault_token: Some("s.dev-token".into()),
            ..Default::default()
        };
        assert!(cfg.build_manager_for(SecretBackend::Vault, home.path()).is_ok());
        // And Vault-without-token still errors even via the explicit path.
        let cfg2 = SecretManagerConfig { backend: "local".into(), ..Default::default() };
        assert!(cfg2.build_manager_for(SecretBackend::Vault, home.path()).is_err());
    }

    #[test]
    fn build_manager_vault_with_encrypted_token_ok() {
        // vault_token_enc + a valid keyfile → resolved_vault_token decrypts it,
        // so the Vault adapter builds successfully without any plaintext token.
        let home = TempHome::new();
        let engine = home.with_keyfile();
        let enc = engine.encrypt_string("s.encrypted-token").unwrap();
        let cfg = SecretManagerConfig {
            backend: "vault".into(),
            vault_token_enc: Some(enc),
            ..Default::default()
        };
        assert!(
            cfg.build_manager(home.path()).is_ok(),
            "vault backend with decryptable vault_token_enc must build"
        );
    }
}
