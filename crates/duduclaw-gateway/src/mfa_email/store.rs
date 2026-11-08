//! Private relay credentials and durable enrollment budget, stored in the
//! shared descriptor-pinned `state/` directory (`crate::installation_identity`).
use std::{io, path::Path};
use serde::{Deserialize, Serialize};
use crate::installation_identity::{StateDir, MAX_STATE_BYTES};

// Intentionally no Debug; this is a bearer secret, never a diagnostic value.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credential { pub credential_id: String, pub secret: String }
impl Credential {
    pub(super) fn key(&self) -> Result<Vec<u8>, &'static str> {
        if self.credential_id.is_empty() || self.credential_id.len() > 128
            || !self.credential_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || self.secret.len() != 64 { return Err(super::DELIVERY_FAILED); }
        hex::decode(&self.secret).map_err(|_| super::DELIVERY_FAILED)
    }
}
impl Drop for Credential {
    fn drop(&mut self) { use zeroize::Zeroize; self.secret.zeroize(); }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnrollmentState {
    /// Pre-challenge builds stored a random id here. The server now derives the
    /// installation id from the installation public key; accept and drop it.
    #[serde(default, rename = "installation_id", skip_serializing)]
    _legacy_installation_id: Option<serde::de::IgnoredAny>,
    pub last_enrollment_attempt: Option<i64>,
}
impl EnrollmentState {
    pub(super) fn new() -> Self {
        Self { _legacy_installation_id: None, last_enrollment_attempt: None }
    }
    pub(super) fn reserve(&mut self, now: i64) -> Result<(), &'static str> {
        // Backward wall-clock movement also fails closed. Persist BEFORE HTTP,
        // including failed enrollments, so restarts cannot reset the budget.
        if self.last_enrollment_attempt.is_some_and(|last| now.saturating_sub(last) < 3600) {
            return Err(super::DELIVERY_FAILED);
        }
        self.last_enrollment_attempt = Some(now);
        Ok(())
    }
}

pub(super) struct Store {
    directory: StateDir,
    _lock: std::fs::File,
}

const CREDENTIAL_FILE: &str = "mfa-relay.json";
const STATE_FILE: &str = "mfa-relay-state.json";
const LOCK_FILE: &str = "mfa-relay.lock";

impl Store {
    pub(super) fn open(home: &Path) -> io::Result<Self> {
        let directory = StateDir::open(home)?;
        let lock = directory.lock(LOCK_FILE)?;
        crate::installation_identity::try_lock(&lock, true)?;
        Ok(Self { directory, _lock: lock })
    }
    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
        self.directory.read(name)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other)).transpose()
    }
    fn write(&self, name: &str, value: &impl Serialize) -> io::Result<()> {
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(io::Error::other)?);
        if bytes.len() > MAX_STATE_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        self.directory.write_atomic(name, &bytes)
    }
    pub(super) fn credential(&self) -> io::Result<Option<Credential>> { self.read(CREDENTIAL_FILE) }
    pub(super) fn save_credential(&self, credential: &Credential) -> io::Result<()> { self.write(CREDENTIAL_FILE, credential) }
    pub(super) fn state(&self) -> io::Result<EnrollmentState> {
        Ok(self.read(STATE_FILE)?.unwrap_or_else(EnrollmentState::new))
    }
    pub(super) fn save_state(&self, state: &EnrollmentState) -> io::Result<()> { self.write(STATE_FILE, state) }
}

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_relay_credential_budget_and_lock_survive_reopen() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        assert!(Store::open(home.path()).is_err());
        let credential = Credential { credential_id: "fixture-credential".into(), secret: "ab".repeat(32) };
        store.save_credential(&credential).unwrap();
        let mut state = store.state().unwrap();
        state.reserve(10_000).unwrap();
        store.save_state(&state).unwrap();
        drop(store);
        let store = Store::open(home.path()).unwrap();
        let loaded = store.credential().unwrap().unwrap();
        assert_eq!(loaded.credential_id, "fixture-credential");
        assert_eq!(loaded.key().unwrap(), vec![0xab; 32]);
        let mut state = store.state().unwrap();
        assert!(state.reserve(9_999).is_err());
        assert!(state.reserve(13_599).is_err());
        state.reserve(13_600).unwrap();
    }
}

/// Called only on a blocking worker. Parallel logins wait within the overall
/// delivery deadline rather than immediately invalidating a freshly issued OTP.
pub(super) fn open_wait(home: &Path, timeout: std::time::Duration) -> io::Result<Store> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match Store::open(home) {
            Ok(store) => return Ok(store),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(20)));
            }
            Err(error) => return Err(error),
        }
    }
}
