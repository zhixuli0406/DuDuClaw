//! macOS keychain backend (`security-framework`, legacy file keychain API).
//!
//! An item added without an explicit access object gets the system default
//! ACL: decrypt is allowed only for the creating executable, identified by its
//! code hash (design §2.3 E8). That is the whole protection: another program
//! reading the item fails (or, with interaction allowed, makes macOS ask the
//! person at the machine). It is also why an upgraded `duduclaw` (new code
//! hash) needs `duduclaw credentials authorize` once.
//!
//! The ACL trusts the *executable*, so code injected into a `duduclaw`
//! process reads the key like the gateway does: measured on an ad-hoc signed
//! creator, and blocked once hardened runtime was on (design §2.3 E9/E10). The level
//! therefore depends on this process's own code-signing flags: `AppBound` only
//! when hardened runtime is on and `get-task-allow` is off, otherwise
//! `UserStore` (a same-user shell can get at it with some effort).
//!
//! Every non-interactive call switches keychain user interaction off for the
//! process (`SecKeychainSetUserInteractionAllowed`, a process-wide setting)
//! under one global lock and puts the previous value back afterwards, so the
//! gateway never blocks on a dialog nobody can see. Other keychain users in
//! the process (`secret://keychain`, TLS) do not take this lock and may see
//! "interaction not allowed" during that window instead of a prompt.
//!
//! The Security framework frees the password buffer it returned without
//! zeroing it; the copy we keep is zeroized, the framework's is not.

use std::path::PathBuf;
use std::sync::Mutex;

use core_foundation::base::TCFType;
use security_framework::os::macos::keychain::SecKeychain;
use security_framework::os::macos::keychain_item::SecKeychainItem;
use security_framework_sys::keychain::{
    SecKeychainFindGenericPassword, SecKeychainGetUserInteractionAllowed,
    SecKeychainSetUserInteractionAllowed,
};
use security_framework_sys::keychain_item::SecKeychainItemDelete;
use zeroize::Zeroizing;

use super::marker::SERVICE;
use super::store::{KeyStore, StoreError};
use super::{BackendKind, ProtectionLevel};

/// errSecItemNotFound
const ERR_ITEM_NOT_FOUND: i32 = -25300;
/// errSecAuthFailed — the item's ACL does not trust this program.
const ERR_AUTH_FAILED: i32 = -25293;
/// errSecInteractionNotAllowed — locked keychain or a prompt was needed.
const ERR_INTERACTION_NOT_ALLOWED: i32 = -25308;
/// errSecUserCanceled — the person pressed Deny / Cancel.
const ERR_USER_CANCELED: i32 = -128;

static KEYCHAIN_LOCK: Mutex<()> = Mutex::new(());

/// The macOS keychain store.
pub struct MacKeychainStore {
    /// `None` = the user's default (login) keychain. A path is only for tests
    /// with a throwaway keychain; nothing on the production path sets it.
    path: Option<PathBuf>,
}

impl MacKeychainStore {
    /// The user's default keychain — the production store.
    pub fn default_keychain() -> Self {
        Self { path: None }
    }

    /// A keychain file at `path`. Tests only: the caller creates and unlocks
    /// a throwaway keychain; this never touches the user's search list.
    #[doc(hidden)]
    pub fn at_path(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn open(&self) -> Result<SecKeychain, StoreError> {
        let opened = match &self.path {
            Some(p) => SecKeychain::open(p),
            None => SecKeychain::default(),
        };
        opened.map_err(|e| map_err(e.code()))
    }

    fn with_ui_disabled<T>(&self, f: impl FnOnce(&SecKeychain) -> Result<T, StoreError>) -> Result<T, StoreError> {
        let _serial = KEYCHAIN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _no_ui = NoInteraction::begin()?;
        let keychain = self.open()?;
        f(&keychain)
    }
}

/// Switches keychain user interaction off and restores the *previous* value
/// on drop (security-framework's own guard always turns it back on).
struct NoInteraction {
    previous: u8,
}

impl NoInteraction {
    fn begin() -> Result<Self, StoreError> {
        let mut previous: u8 = 1;
        // SAFETY: plain out-parameter call.
        let status = unsafe { SecKeychainGetUserInteractionAllowed(&mut previous) };
        if status != 0 {
            return Err(map_err(status));
        }
        // SAFETY: plain setter.
        let status = unsafe { SecKeychainSetUserInteractionAllowed(0) };
        if status != 0 {
            return Err(map_err(status));
        }
        Ok(Self { previous })
    }
}

impl Drop for NoInteraction {
    fn drop(&mut self) {
        // SAFETY: plain setter.
        unsafe {
            SecKeychainSetUserInteractionAllowed(self.previous);
        }
    }
}

/// Code-signing status flags of this process (`csops(CS_OPS_STATUS)`).
const CS_OPS_STATUS: u32 = 0;
const CS_GET_TASK_ALLOW: u32 = 0x0000_0004;
const CS_RUNTIME: u32 = 0x0001_0000;

unsafe extern "C" {
    fn csops(pid: i32, ops: u32, useraddr: *mut std::ffi::c_void, usersize: usize) -> i32;
}

/// Whether this executable runs with hardened runtime and without
/// `get-task-allow` — the condition under which the system refuses to load
/// foreign code into, or attach a debugger to, the trusted process.
/// Entitlements that re-open dyld variables or disable library validation
/// are a release-signing rule (design §8), not checked here.
pub fn hardened_runtime() -> bool {
    let mut flags: u32 = 0;
    // SAFETY: CS_OPS_STATUS writes one u32 into `flags`.
    let rc = unsafe {
        csops(
            std::process::id() as i32,
            CS_OPS_STATUS,
            (&mut flags as *mut u32).cast(),
            std::mem::size_of::<u32>(),
        )
    };
    rc == 0 && flags & CS_RUNTIME != 0 && flags & CS_GET_TASK_ALLOW == 0
}

/// Look an item up by attributes only: no decrypt, so no ACL check and no
/// prompt. Works for an item planted by another program.
fn find_item(keychain: &SecKeychain, account: &str) -> Result<Option<SecKeychainItem>, StoreError> {
    let mut item_ref = std::ptr::null_mut();
    // SAFETY: valid keychain ref; service/account pointers outlive the call;
    // null data out-params request attributes only.
    let status = unsafe {
        SecKeychainFindGenericPassword(
            keychain.as_CFTypeRef(),
            SERVICE.len() as u32,
            SERVICE.as_ptr().cast(),
            account.len() as u32,
            account.as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut item_ref,
        )
    };
    match status {
        ERR_ITEM_NOT_FOUND => Ok(None),
        0 if !item_ref.is_null() => {
            // SAFETY: returned under the create rule.
            Ok(Some(unsafe { SecKeychainItem::wrap_under_create_rule(item_ref) }))
        }
        other => Err(map_err(other)),
    }
}

/// Delete any item for `account` and confirm it is gone, so a planted item
/// is never adopted. Deleting needs no decrypt right (design §2.3 E6).
fn delete_existing(keychain: &SecKeychain, account: &str) -> Result<(), StoreError> {
    let Some(item) = find_item(keychain, account)? else {
        return Ok(());
    };
    // SAFETY: `item` is a live keychain item reference.
    let status = unsafe { SecKeychainItemDelete(item.as_concrete_TypeRef()) };
    if status != 0 {
        return Err(map_err(status));
    }
    match find_item(keychain, account)? {
        None => Ok(()),
        Some(_) => Err(StoreError::Unavailable("existing keychain item could not be removed".into())),
    }
}

fn map_err(code: i32) -> StoreError {
    match code {
        ERR_ITEM_NOT_FOUND => StoreError::NotFound,
        ERR_AUTH_FAILED | ERR_INTERACTION_NOT_ALLOWED | ERR_USER_CANCELED => StoreError::InteractionRequired,
        other => StoreError::Unavailable(format!("macOS keychain error {other}")),
    }
}

impl KeyStore for MacKeychainStore {
    fn backend(&self) -> BackendKind {
        BackendKind::MacosKeychain
    }

    fn level(&self) -> ProtectionLevel {
        if hardened_runtime() {
            ProtectionLevel::AppBound
        } else {
            ProtectionLevel::UserStore
        }
    }

    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        self.with_ui_disabled(|kc| {
            kc.find_generic_password(SERVICE, account)
                .map(|(password, _item)| Zeroizing::new(password.to_vec()))
                .map_err(|e| map_err(e.code()))
        })
    }

    fn read_interactive(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let _serial = KEYCHAIN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let keychain = self.open()?;
        keychain
            .find_generic_password(SERVICE, account)
            .map(|(password, _item)| Zeroizing::new(password.to_vec()))
            .map_err(|e| map_err(e.code()))
    }

    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        self.with_ui_disabled(|kc| {
            delete_existing(kc, account)?;
            kc.add_generic_password(SERVICE, account, key)
                .map_err(|e| map_err(e.code()))
        })
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.with_ui_disabled(|kc| delete_existing(kc, account))
    }
}
