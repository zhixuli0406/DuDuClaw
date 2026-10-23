//! Where the protected key physically lives: the [`KeyStore`] seam.
//!
//! One implementation per OS credential store (`macos.rs`, `windows.rs`), an
//! [`UnsupportedStore`] for platforms with none (Linux, Docker), and an
//! in-memory [`MemoryKeyStore`] for tests — including the gateway's tests, which
//! is why it is public rather than `cfg(test)`. Nothing on the default path
//! ever constructs a `MemoryKeyStore`; [`super::platform_store`] picks the
//! platform store by `cfg`, never by configuration or environment, so an
//! employee cannot redirect the gateway to a store it prepared.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use zeroize::Zeroizing;

use super::{BackendKind, ProtectionLevel};

/// Why a store operation did not return a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store answered: there is no item for this account.
    NotFound,
    /// The store needs a person: the keychain is locked, or (macOS) the item
    /// no longer trusts this executable — typically after an upgrade changed
    /// the binary's code hash. Reads never prompt, so this is what a prompt
    /// would have been.
    InteractionRequired,
    /// The store cannot be used on this machine (no credential store, or the
    /// backend failed outright). Carries a short diagnostic, never a secret.
    Unavailable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound => f.write_str("no protected key item in the credential store"),
            StoreError::InteractionRequired => {
                f.write_str("the credential store needs a person to unlock or re-authorize it")
            }
            StoreError::Unavailable(msg) => write!(f, "credential store unavailable: {msg}"),
        }
    }
}

/// A credential store that can hold the protected key for one account name.
///
/// Implementations must never prompt in [`KeyStore::read`]: the gateway is a
/// daemon and a dialog nobody sees would hang it. [`KeyStore::read_interactive`]
/// is the one call allowed to involve a person (`duduclaw credentials
/// authorize`, run at the machine).
pub trait KeyStore: Send + Sync {
    /// Which backend this is (recorded in the marker file, shown in the UI).
    fn backend(&self) -> BackendKind;
    /// The protection level this backend gives against a same-user process.
    fn level(&self) -> ProtectionLevel;
    /// Read the key bytes for `account` without any user interaction.
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError>;
    /// Read allowing the OS to ask the person at the machine. Defaults to
    /// [`KeyStore::read`] for stores that never ask.
    fn read_interactive(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        self.read(account)
    }
    /// Replace whatever is stored for `account` with `key`. Implementations
    /// delete any existing item first, so an item planted before enabling is
    /// never adopted and (macOS) the new item trusts only this executable.
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError>;
    /// Remove the item for `account`. A missing item is success.
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

/// Share one store between a protection and the code that inspects or
/// tampers with it (tests, the gateway's per-home test store).
impl<T: KeyStore + ?Sized> KeyStore for std::sync::Arc<T> {
    fn backend(&self) -> BackendKind {
        (**self).backend()
    }
    fn level(&self) -> ProtectionLevel {
        (**self).level()
    }
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        (**self).read(account)
    }
    fn read_interactive(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        (**self).read_interactive(account)
    }
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        (**self).write_new(account, key)
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        (**self).delete(account)
    }
}

/// The store for platforms without a supported credential store.
pub struct UnsupportedStore;

impl KeyStore for UnsupportedStore {
    fn backend(&self) -> BackendKind {
        BackendKind::None
    }
    fn level(&self) -> ProtectionLevel {
        ProtectionLevel::File
    }
    fn read(&self, _account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        Err(StoreError::Unavailable(
            "this platform has no supported OS credential store".into(),
        ))
    }
    fn write_new(&self, _account: &str, _key: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::Unavailable(
            "this platform has no supported OS credential store".into(),
        ))
    }
    fn delete(&self, _account: &str) -> Result<(), StoreError> {
        Ok(())
    }
}

/// In-memory store for tests. Behaves like a credential store whose items
/// another same-user process can overwrite (exactly what the macOS keychain
/// allows, design §2.3 E7), so substitution detection can be tested without a
/// real keychain.
pub struct MemoryKeyStore {
    backend: BackendKind,
    level: ProtectionLevel,
    items: Mutex<HashMap<String, Vec<u8>>>,
    forced_error: Mutex<Option<StoreError>>,
}

impl MemoryKeyStore {
    /// A store that reports the given backend and level.
    pub fn new(backend: BackendKind, level: ProtectionLevel) -> Self {
        Self {
            backend,
            level,
            items: Mutex::new(HashMap::new()),
            forced_error: Mutex::new(None),
        }
    }

    /// A store that reports itself as an app-bound macOS keychain.
    pub fn app_bound() -> Self {
        Self::new(BackendKind::MacosKeychain, ProtectionLevel::AppBound)
    }

    /// Simulate another process overwriting the stored value.
    pub fn tamper(&self, account: &str, value: &[u8]) {
        self.lock_items().insert(account.to_string(), value.to_vec());
    }

    /// Simulate the item disappearing behind the gateway's back.
    pub fn remove_behind_back(&self, account: &str) {
        self.lock_items().remove(account);
    }

    /// Make every read fail with `err` until cleared with `None`.
    pub fn force_read_error(&self, err: Option<StoreError>) {
        *self
            .forced_error
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = err;
    }

    fn lock_items(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>> {
        self.items.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl KeyStore for MemoryKeyStore {
    fn backend(&self) -> BackendKind {
        self.backend
    }
    fn level(&self) -> ProtectionLevel {
        self.level
    }
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        if let Some(err) = self
            .forced_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Err(err);
        }
        self.lock_items()
            .get(account)
            .map(|v| Zeroizing::new(v.clone()))
            .ok_or(StoreError::NotFound)
    }
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        self.lock_items().insert(account.to_string(), key.to_vec());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.lock_items().remove(account);
        Ok(())
    }
}
