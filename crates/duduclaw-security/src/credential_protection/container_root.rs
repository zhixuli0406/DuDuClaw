//! Account-bound wrapping under the root launcher's isolated key.
//! Disk holds only authenticated ciphertext; it is never a key fallback.

#[cfg(unix)]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
#[cfg(unix)]
use ring::hmac;
#[cfg(unix)]
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;
use super::{BackendKind, KeyStore, ProtectionLevel, StoreError};
#[cfg(unix)]
use crate::crypto::CryptoEngine;
#[cfg(unix)]
use duduclaw_core::trusted_home::TrustedHome;

#[cfg(unix)]
#[path = "container_root_file.rs"]
mod file;

#[cfg(unix)]
const MAX_BLOB: u64 = 4096;
#[cfg(unix)]
const FILE_NAME: &str = "container-wrapped-key.json";

#[cfg(unix)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wrapped {
    version: u32,
    account: String,
    ciphertext: String,
}

#[cfg(unix)]
pub struct ContainerRootStore {
    home: super::SharedHomePin,
    #[cfg(test)]
    root: Option<Zeroizing<[u8; 32]>>,
}

#[cfg(unix)]
impl ContainerRootStore {
    /// Production stores retain no root or derived wrapping key. Each operation
    /// borrows the current root under its revocation lock. The shared pin is
    /// initialized by CredentialProtection, including after missing-home bootstrap.
    pub(super) fn attested(home: super::SharedHomePin) -> Self {
        Self { home, #[cfg(test)] root: None }
    }

    #[cfg(test)]
    pub(super) fn new(home: super::SharedHomePin, root: &[u8; 32]) -> Self {
        Self { home, root: Some(Zeroizing::new(*root)) }
    }

    #[cfg(test)]
    pub(super) fn after_directory_open(f: impl FnOnce() + 'static) {
        file::after_directory_open(f);
    }

    fn trusted_home(&self) -> Result<&TrustedHome, StoreError> {
        let home = self.home.get().ok_or_else(unavailable)?
            .as_ref().map_err(|_| StoreError::Tampered)?;
        home.verify().map_err(|_| StoreError::Tampered)?;
        Ok(home)
    }

    fn check_account(&self, account: &str) -> Result<&TrustedHome, StoreError> {
        let home = self.trusted_home()?;
        if account != super::marker::account_for_resolved_home(home.path()) {
            return Err(StoreError::Tampered);
        }
        Ok(home)
    }

    fn derive(account: &str, root: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, root));
        context.update(b"duduclaw/protected-key-wrap/v1\0");
        context.update(account.as_bytes());
        let tag = context.sign();
        let mut wrapping_key = Zeroizing::new([0; 32]);
        wrapping_key.copy_from_slice(tag.as_ref());
        wrapping_key
    }

    fn with_wrapping<T>(&self, account: &str, f: impl FnOnce(&[u8; 32]) -> Result<T, StoreError>) -> Result<T, StoreError> {
        #[cfg(test)]
        if let Some(root) = &self.root { return f(&Self::derive(account, root)); }
        duduclaw_core::root_key::with_key(|root| f(&Self::derive(account, root)))
            .unwrap_or_else(|| Err(StoreError::Unavailable(
                duduclaw_core::root_key::refused_reason().unwrap_or("inactive").into())))
    }

    fn wrap(&self, account: &str, key: &[u8]) -> Result<Vec<u8>, StoreError> {
        self.check_account(account)?;
        if key.len() != 32 { return Err(StoreError::Tampered); }
        let cipher = self.with_wrapping(account, |wrapping_key| CryptoEngine::new(wrapping_key)
            .and_then(|engine| engine.encrypt(key)).map_err(|_| unavailable()))?;
        serde_json::to_vec(&Wrapped { version: 1, account: account.into(), ciphertext: BASE64.encode(cipher) })
            .map_err(|_| unavailable())
    }

    fn unwrap(&self, account: &str, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        self.check_account(account)?;
        if bytes.len() > MAX_BLOB as usize { return Err(StoreError::Tampered); }
        let blob: Wrapped = serde_json::from_slice(bytes).map_err(|_| StoreError::Tampered)?;
        if blob.version != 1 || blob.account != account { return Err(StoreError::Tampered); }
        let cipher = BASE64.decode(blob.ciphertext).map_err(|_| StoreError::Tampered)?;
        let key = Zeroizing::new(self.with_wrapping(account, |wrapping_key| CryptoEngine::new(wrapping_key)
            .and_then(|engine| engine.decrypt(&cipher)).map_err(|_| StoreError::Tampered))?);
        if key.len() != 32 { return Err(StoreError::Tampered); }
        Ok(key)
    }
}

#[cfg(unix)]
fn unavailable() -> StoreError {
    StoreError::Unavailable("無法存取加密後的保護金鑰，請檢查資料夾權限與可用空間。".into())
}

#[cfg(unix)]
impl KeyStore for ContainerRootStore {
    fn backend(&self) -> BackendKind { BackendKind::ContainerRootKey }
    fn level(&self) -> ProtectionLevel { ProtectionLevel::ProcessIsolated }
    fn refusal_reason(&self) -> Option<&'static str> {
        #[cfg(test)]
        if self.root.is_some() { return None; }
        duduclaw_core::root_key::refused_reason()
    }
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        if let Some(reason) = self.refusal_reason() { return Err(StoreError::Unavailable(reason.into())); }
        let home = self.check_account(account)?;
        self.unwrap(account, &file::read(home)?)
    }
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        let blob = self.wrap(account, key)?;
        file::write(self.check_account(account)?, &blob)
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        file::delete(self.check_account(account)?)
    }
}

/// An offered but refused root key is unavailable, including on a fresh home.
pub struct RefusedRootStore(pub &'static str);

impl KeyStore for RefusedRootStore {
    fn backend(&self) -> BackendKind { BackendKind::ContainerRootKey }
    fn level(&self) -> ProtectionLevel { ProtectionLevel::ProcessIsolated }
    fn refusal_reason(&self) -> Option<&'static str> { Some(self.0) }
    fn read(&self, _: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        Err(StoreError::Unavailable(self.0.into()))
    }
    fn write_new(&self, _: &str, _: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::Unavailable(self.0.into()))
    }
    fn delete(&self, _: &str) -> Result<(), StoreError> { Ok(()) }
}
