//! `secret://keychain/<service>/<account>` — OS credential store (WP-H1 P1).
//!
//! The natural default external source for a desktop-first product: macOS
//! Keychain, Windows Credential Manager, Linux Secret Service. Unlike Vault /
//! 1Password / Infisical it needs no server, no token, and no network, so a
//! single developer gets the doctrine's real prize — *rotating a credential
//! without touching `config.toml`* — with zero infrastructure.
//!
//! ## Where the dependency comes from
//!
//! No new crate. `duduclaw-core` has carried an optional `keyring` v3
//! dependency (apple-native / windows-native / sync-secret-service) behind its
//! non-default `keychain` feature since the master-key work; this adapter is a
//! thin wrapper over [`duduclaw_core::keychain`]. `duduclaw-security` re-exports
//! that feature as its own `keychain`, so a default build compiles no native
//! credential-store code at all.
//!
//! ## Degradation when the feature is off
//!
//! `duduclaw_core::keychain::get_secret` returns `Ok(None)` in a build without
//! the feature. That would make an unbuilt backend indistinguishable from an
//! empty keychain entry, which is exactly the "silent failure" this whole line
//! exists to eliminate — so this adapter checks [`is_available`] first and
//! returns a *loud* error instead. `describe()` still reports the reference as
//! configured (it is — it is right there in the file); the failure surfaces at
//! resolve time, in the warning and in `doctor`.
//!
//! ## Naming
//!
//! `secret://keychain/duduclaw/telegram` → service `duduclaw`, account
//! `telegram`. A name with no `/` (`secret://keychain/telegram`) uses the
//! default service [`duduclaw_core::keychain::MASTER_KEY_SERVICE`], so the
//! common single-app case stays short. Only the **first** `/` splits, so an
//! account may itself contain slashes.

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};

use super::SecretManager;

/// Split a reference name into `(service, account)`.
///
/// Both halves must be non-empty; `"duduclaw/"` is a configuration mistake, not
/// a request for an empty-named account.
pub fn split_name(name: &str) -> Result<(&str, &str)> {
    match name.split_once('/') {
        Some((service, account)) => {
            if service.is_empty() || account.is_empty() {
                Err(DuDuClawError::Security(format!(
                    "secret://keychain name '{name}' must be <service>/<account> with both parts \
                     non-empty"
                )))
            } else {
                Ok((service, account))
            }
        }
        None if name.is_empty() => Err(DuDuClawError::Security(
            "secret://keychain reference has an empty name".to_string(),
        )),
        None => Ok((duduclaw_core::keychain::MASTER_KEY_SERVICE, name)),
    }
}

/// Whether this build can actually talk to an OS credential store.
pub fn is_available() -> bool {
    duduclaw_core::keychain::is_available()
}

/// Reads (and writes) secrets in the OS-native credential store.
pub struct KeychainSecretAdapter;

impl KeychainSecretAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Blocking fetch shared by the async trait method and the synchronous
    /// resolver. The OS keychain call is a local IPC round-trip, so there is no
    /// async variant to wrap and no reason to maintain two code paths.
    pub fn get_blocking(&self, name: &str) -> Result<String> {
        if !is_available() {
            return Err(DuDuClawError::Security(
                "secret://keychain reference found, but this binary was built without the \
                 `keychain` feature — rebuild with `--features keychain` or move the credential \
                 to another source"
                    .to_string(),
            ));
        }
        let (service, account) = split_name(name)?;
        match duduclaw_core::keychain::get_secret(service, account) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(DuDuClawError::Security(format!(
                "no OS keychain entry for service '{service}' account '{account}'"
            ))),
            Err(e) => Err(DuDuClawError::Security(format!(
                "OS keychain lookup for service '{service}' failed: {e}"
            ))),
        }
    }
}

impl Default for KeychainSecretAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretManager for KeychainSecretAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        self.get_blocking(name)
    }

    async fn put(&self, name: &str, value: &str) -> Result<()> {
        if value.is_empty() {
            return Err(DuDuClawError::Security(
                "refusing to store an empty secret in the OS keychain".to_string(),
            ));
        }
        let (service, account) = split_name(name)?;
        duduclaw_core::keychain::store_secret(service, account, value)
            .map_err(|e| DuDuClawError::Security(format!("OS keychain write failed: {e}")))
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let (service, account) = split_name(name)?;
        duduclaw_core::keychain::delete_secret(service, account)
            .map_err(|e| DuDuClawError::Security(format!("OS keychain delete failed: {e}")))
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        if !is_available() {
            return Ok(false);
        }
        let (service, account) = split_name(name)?;
        duduclaw_core::keychain::get_secret(service, account)
            .map(|v| v.is_some())
            .map_err(|e| DuDuClawError::Security(format!("OS keychain lookup failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_splits_into_service_and_account() {
        assert_eq!(split_name("duduclaw/telegram").unwrap(), ("duduclaw", "telegram"));
        // Only the first slash splits — an account may contain more.
        assert_eq!(
            split_name("duduclaw/team/telegram").unwrap(),
            ("duduclaw", "team/telegram")
        );
    }

    #[test]
    fn bare_name_uses_the_default_service() {
        assert_eq!(
            split_name("telegram").unwrap(),
            (duduclaw_core::keychain::MASTER_KEY_SERVICE, "telegram")
        );
    }

    #[test]
    fn empty_halves_are_rejected_not_defaulted() {
        for bad in ["", "duduclaw/", "/telegram"] {
            assert!(split_name(bad).is_err(), "{bad:?} must not parse");
        }
    }

    /// Without the feature the adapter must fail **loudly**. The underlying
    /// `get_secret` returns `Ok(None)` in that build, which would otherwise be
    /// indistinguishable from a keychain that simply has no such entry.
    #[cfg(not(feature = "keychain"))]
    #[test]
    fn unbuilt_feature_is_an_error_not_a_silent_miss() {
        assert!(!is_available());
        let err = KeychainSecretAdapter::new()
            .get_blocking("duduclaw/telegram")
            .unwrap_err()
            .to_string();
        assert!(err.contains("keychain"), "{err}");
    }
}
