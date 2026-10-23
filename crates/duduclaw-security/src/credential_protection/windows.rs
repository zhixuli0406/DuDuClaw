//! Windows Credential Manager backend (`keyring` 3, `windows-native`).
//!
//! Credential Manager protects with DPAPI per Windows user. It has no
//! per-program access control: any program running as the same user can call
//! `CredRead`, so this is `UserStore` — it keeps the key out of files and
//! backups, not away from a same-user shell (design §2.2).
//!
//! Not run on a Windows machine by the author; compiled with the mingw
//! cross-check only.

use zeroize::Zeroizing;

use super::marker::SERVICE;
use super::store::{KeyStore, StoreError};
use super::{BackendKind, ProtectionLevel};

/// The Windows Credential Manager store.
pub struct WindowsCredentialStore;

fn entry(account: &str) -> Result<keyring::Entry, StoreError> {
    keyring::Entry::new(SERVICE, account).map_err(|e| map_err(&e))
}

fn map_err(e: &keyring::Error) -> StoreError {
    match e {
        keyring::Error::NoEntry => StoreError::NotFound,
        keyring::Error::NoStorageAccess(_) => StoreError::InteractionRequired,
        other => StoreError::Unavailable(format!("Windows Credential Manager error: {other}")),
    }
}

impl KeyStore for WindowsCredentialStore {
    fn backend(&self) -> BackendKind {
        BackendKind::WindowsCredentialManager
    }

    fn level(&self) -> ProtectionLevel {
        ProtectionLevel::UserStore
    }

    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        entry(account)?
            .get_secret()
            .map(Zeroizing::new)
            .map_err(|e| map_err(&e))
    }

    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        let entry = entry(account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(map_err(&e)),
        }
        entry.set_secret(key).map_err(|e| map_err(&e))
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(&e)),
        }
    }
}
