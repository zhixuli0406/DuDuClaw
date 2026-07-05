//! Optional OS keychain storage for the master encryption secret.
//!
//! DuDuClaw encrypts API keys / tokens at rest with AES-256-GCM, but the master
//! key itself has historically lived on the filesystem (per-machine keyfile).
//! This module adds an *optional* path to store that master secret in the
//! OS-native credential store instead:
//!
//! - **macOS** — Keychain
//! - **Windows** — Credential Manager
//! - **Linux** — Secret Service (GNOME Keyring / KWallet)
//!
//! It is gated behind the non-default `keychain` cargo feature so the default
//! build has zero extra native dependencies. When the feature is **off**, every
//! function degrades gracefully:
//!
//! - [`get_secret`] returns `Ok(None)` (no entry — caller uses the filesystem key)
//! - [`store_secret`] returns `Err(KeychainError::NotBuilt)`
//! - [`delete_secret`] returns `Ok(())` (nothing to delete)
//! - [`resolve_master_key`] returns [`MasterKeySource::Filesystem`]
//!
//! ## Migration (non-breaking, additive)
//!
//! The existing filesystem-key encryption code is intentionally left untouched.
//! To migrate a deployment onto the keychain, build with `--features keychain`
//! and store the existing master key under service `"duduclaw"`, account
//! `"master-key"`:
//!
//! ```ignore
//! duduclaw_core::keychain::store_secret(
//!     duduclaw_core::keychain::MASTER_KEY_SERVICE,
//!     duduclaw_core::keychain::MASTER_KEY_ACCOUNT,
//!     &existing_master_key_material,
//! )?;
//! ```
//!
//! Thereafter [`resolve_master_key`] prefers the keychain entry; if it is
//! absent (or the feature is not built, or the backend errors), it falls back to
//! the existing filesystem key path — so the change can be rolled out and rolled
//! back safely.

use std::fmt;
use std::path::Path;

/// Keychain service name for DuDuClaw secrets.
pub const MASTER_KEY_SERVICE: &str = "duduclaw";
/// Keychain account name for the master encryption key.
pub const MASTER_KEY_ACCOUNT: &str = "master-key";

/// Errors from keychain operations.
#[derive(Debug)]
pub enum KeychainError {
    /// The `keychain` cargo feature was not built, so storing is unavailable.
    NotBuilt,
    /// The underlying OS keychain backend returned an error.
    Backend(String),
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeychainError::NotBuilt => write!(
                f,
                "keychain feature not built — rebuild with `--features keychain`"
            ),
            KeychainError::Backend(msg) => write!(f, "keychain backend error: {msg}"),
        }
    }
}

impl std::error::Error for KeychainError {}

/// Convenience result type for keychain operations.
pub type Result<T> = std::result::Result<T, KeychainError>;

/// Where the master key should be read from.
///
/// The secret material carried by [`MasterKeySource::Keychain`] is deliberately
/// redacted in the `Debug` impl so it cannot leak into logs.
pub enum MasterKeySource {
    /// Master key was found in the OS keychain; carries the secret material.
    Keychain(String),
    /// No keychain entry (or feature disabled) — the caller must fall back to
    /// the existing filesystem keyfile, exactly as before.
    Filesystem,
}

impl fmt::Debug for MasterKeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Never print the secret material.
            MasterKeySource::Keychain(_) => f.write_str("Keychain(<redacted>)"),
            MasterKeySource::Filesystem => f.write_str("Filesystem"),
        }
    }
}

// ── Feature ON: real keyring-backed implementation ──────────────────────────

#[cfg(feature = "keychain")]
fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(service, account).map_err(|e| KeychainError::Backend(e.to_string()))
}

/// Store `secret` under `(service, account)` in the OS keychain.
#[cfg(feature = "keychain")]
pub fn store_secret(service: &str, account: &str, secret: &str) -> Result<()> {
    entry(service, account)?
        .set_password(secret)
        .map_err(|e| KeychainError::Backend(e.to_string()))
}

/// Fetch the secret for `(service, account)`. Returns `Ok(None)` when no entry
/// exists (fail-open to the filesystem path), `Err` only on a real backend fault.
#[cfg(feature = "keychain")]
pub fn get_secret(service: &str, account: &str) -> Result<Option<String>> {
    match entry(service, account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeychainError::Backend(e.to_string())),
    }
}

/// Delete the secret for `(service, account)`. A missing entry is treated as
/// success (idempotent).
#[cfg(feature = "keychain")]
pub fn delete_secret(service: &str, account: &str) -> Result<()> {
    match entry(service, account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError::Backend(e.to_string())),
    }
}

// ── Feature OFF: graceful stubs ─────────────────────────────────────────────

/// Stub: storing is unavailable without the `keychain` feature.
#[cfg(not(feature = "keychain"))]
pub fn store_secret(_service: &str, _account: &str, _secret: &str) -> Result<()> {
    Err(KeychainError::NotBuilt)
}

/// Stub: no keychain backend, so there is never an entry — callers fall back to
/// the filesystem key.
#[cfg(not(feature = "keychain"))]
pub fn get_secret(_service: &str, _account: &str) -> Result<Option<String>> {
    Ok(None)
}

/// Stub: nothing to delete without the `keychain` feature.
#[cfg(not(feature = "keychain"))]
pub fn delete_secret(_service: &str, _account: &str) -> Result<()> {
    Ok(())
}

// ── Resolver ────────────────────────────────────────────────────────────────

/// Decide where the master key should come from.
///
/// Preference order:
/// 1. OS keychain entry at `(MASTER_KEY_SERVICE, MASTER_KEY_ACCOUNT)` — only when
///    the `keychain` feature is built and an entry actually exists.
/// 2. The existing filesystem keyfile (unchanged behavior).
///
/// A keychain *miss* (`Ok(None)`) or a *backend error* both fall back to the
/// filesystem — fail-safe: an unavailable keychain must never make secrets
/// unreadable when a working filesystem key exists.
///
/// `home_dir` is accepted for symmetry with the filesystem-keyfile lookup (which
/// is rooted at the DuDuClaw home). The filesystem key itself is still owned by
/// the existing encryption code; this resolver only signals which source to use.
pub fn resolve_master_key(home_dir: &Path) -> MasterKeySource {
    let _ = home_dir;
    match get_secret(MASTER_KEY_SERVICE, MASTER_KEY_ACCOUNT) {
        Ok(Some(secret)) => MasterKeySource::Keychain(secret),
        // Miss or backend error → fall back to the filesystem key (fail-safe).
        Ok(None) | Err(_) => MasterKeySource::Filesystem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Stub-path behavior (feature OFF, the default test build) ─────────────

    #[cfg(not(feature = "keychain"))]
    #[test]
    fn stub_get_secret_returns_none() {
        assert_eq!(get_secret(MASTER_KEY_SERVICE, "anything").unwrap(), None);
    }

    #[cfg(not(feature = "keychain"))]
    #[test]
    fn stub_store_secret_errors_not_built() {
        let err = store_secret(MASTER_KEY_SERVICE, "anything", "secret").unwrap_err();
        assert!(matches!(err, KeychainError::NotBuilt));
        // Error message is actionable.
        assert!(err.to_string().contains("--features keychain"));
    }

    #[cfg(not(feature = "keychain"))]
    #[test]
    fn stub_delete_secret_is_ok() {
        assert!(delete_secret(MASTER_KEY_SERVICE, "anything").is_ok());
    }

    #[cfg(not(feature = "keychain"))]
    #[test]
    fn resolver_falls_back_to_filesystem_when_feature_off() {
        let src = resolve_master_key(Path::new("/tmp/duduclaw-home"));
        assert!(matches!(src, MasterKeySource::Filesystem));
        // Debug output must never contain secret material.
        assert_eq!(format!("{src:?}"), "Filesystem");
    }

    // Debug redaction holds for the keychain variant too.
    #[test]
    fn keychain_variant_debug_is_redacted() {
        let src = MasterKeySource::Keychain("super-secret".to_string());
        let dbg = format!("{src:?}");
        assert!(!dbg.contains("super-secret"), "secret leaked in Debug: {dbg}");
        assert_eq!(dbg, "Keychain(<redacted>)");
    }

    // ── Live keychain round-trip (feature ON, real OS keychain) ──────────────
    // Cannot run in headless CI (no Secret Service / no unlocked keychain), so
    // it is ignored by default and only exercised on demand:
    //   cargo test -p duduclaw-core --features keychain -- --ignored
    #[cfg(feature = "keychain")]
    #[test]
    #[ignore = "requires a real OS keychain (macOS Keychain / Windows Cred Mgr / Secret Service)"]
    fn live_store_get_delete_roundtrip() {
        let (svc, acct) = ("duduclaw-test", "roundtrip");
        store_secret(svc, acct, "s3cr3t").unwrap();
        assert_eq!(get_secret(svc, acct).unwrap().as_deref(), Some("s3cr3t"));
        delete_secret(svc, acct).unwrap();
        assert_eq!(get_secret(svc, acct).unwrap(), None);
    }
}
