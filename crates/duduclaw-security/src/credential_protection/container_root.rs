//! Account-bound wrapping under the root launcher's isolated key.
//! Disk holds only authenticated ciphertext; it is never a key fallback.

use std::path::{Path, PathBuf};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::hmac;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;
use super::{BackendKind, KeyStore, ProtectionLevel, StoreError};
use crate::crypto::CryptoEngine;

#[cfg(unix)]
#[path = "container_root_file.rs"]
mod file;

const MAX_BLOB: u64 = 4096;
const FILE_NAME: &str = "container-wrapped-key.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wrapped {
    version: u32,
    account: String,
    ciphertext: String,
}

pub struct ContainerRootStore {
    directory: PathBuf,
    account: String,
    wrapping_key: Zeroizing<[u8; 32]>,
}

impl ContainerRootStore {
    /// Derive an account-specific key immediately; do not retain a root copy.
    pub fn new(home: &Path, root: &[u8; 32]) -> Self {
        let account = super::marker::account_for_home(home);
        let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, root));
        context.update(b"duduclaw/protected-key-wrap/v1\0");
        context.update(account.as_bytes());
        let tag = context.sign();
        let mut wrapping_key = Zeroizing::new([0; 32]);
        wrapping_key.copy_from_slice(tag.as_ref());
        Self { directory: home.join("protected"), account, wrapping_key }
    }

    fn wrap(&self, account: &str, key: &[u8]) -> Result<Vec<u8>, StoreError> {
        if account != self.account || key.len() != 32 { return Err(StoreError::Tampered); }
        let cipher = CryptoEngine::new(&self.wrapping_key)
            .and_then(|engine| engine.encrypt(key))
            .map_err(|_| unavailable())?;
        serde_json::to_vec(&Wrapped { version: 1, account: account.into(), ciphertext: BASE64.encode(cipher) })
            .map_err(|_| unavailable())
    }

    fn unwrap(&self, account: &str, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        if bytes.len() > MAX_BLOB as usize || account != self.account { return Err(StoreError::Tampered); }
        let blob: Wrapped = serde_json::from_slice(bytes).map_err(|_| StoreError::Tampered)?;
        if blob.version != 1 || blob.account != account { return Err(StoreError::Tampered); }
        let cipher = BASE64.decode(blob.ciphertext).map_err(|_| StoreError::Tampered)?;
        let key = Zeroizing::new(CryptoEngine::new(&self.wrapping_key)
            .and_then(|engine| engine.decrypt(&cipher)).map_err(|_| StoreError::Tampered)?);
        if key.len() != 32 { return Err(StoreError::Tampered); }
        Ok(key)
    }
}

fn unavailable() -> StoreError {
    StoreError::Unavailable("無法存取加密後的保護金鑰，請檢查資料夾權限與可用空間。".into())
}

impl KeyStore for ContainerRootStore {
    fn backend(&self) -> BackendKind { BackendKind::ContainerRootKey }
    fn level(&self) -> ProtectionLevel { ProtectionLevel::ProcessIsolated }
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        #[cfg(unix)]
        return self.unwrap(account, &file::read(&self.directory)?);
        #[cfg(not(unix))]
        { let _ = account; Err(unavailable()) }
    }
    fn write_new(&self, account: &str, key: &[u8]) -> Result<(), StoreError> {
        let blob = self.wrap(account, key)?;
        #[cfg(unix)]
        return file::write(&self.directory, &blob);
        #[cfg(not(unix))]
        { let _ = blob; Err(unavailable()) }
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        if account != self.account { return Err(StoreError::Tampered); }
        #[cfg(unix)]
        return file::delete(&self.directory);
        #[cfg(not(unix))]
        Err(unavailable())
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
