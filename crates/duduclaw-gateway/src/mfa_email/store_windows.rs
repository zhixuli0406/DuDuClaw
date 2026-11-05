//! Windows relay state uses pinned directories and owner-restricted DACLs.
use super::{Credential, EnrollmentState, CREDENTIAL_FILE, STATE_FILE};
use std::{io, path::Path};
use duduclaw_core::windows_private_state::SecureDirectory;

pub(in crate::mfa_email) struct Store {
    directory: SecureDirectory,
    _lock: std::fs::File,
}
impl Store {
    pub(in crate::mfa_email) fn open(home: &Path) -> io::Result<Self> {
        let directory = SecureDirectory::open(home, "state")?;
        let lock = directory.open_lock("mfa-relay.lock")?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                io::Error::from(io::ErrorKind::WouldBlock)
            } else { error }
        })?;
        Ok(Self { directory, _lock: lock })
    }
    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
        self.directory.read(name)?.map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other)).transpose()
    }
    fn write(&self, name: &str, value: &impl serde::Serialize) -> io::Result<()> {
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(io::Error::other)?);
        self.directory.write_atomic(name, &bytes)
    }
    pub(in crate::mfa_email) fn credential(&self) -> io::Result<Option<Credential>> { self.read(CREDENTIAL_FILE) }
    pub(in crate::mfa_email) fn save_credential(&self, credential: &Credential) -> io::Result<()> { self.write(CREDENTIAL_FILE, credential) }
    pub(in crate::mfa_email) fn state(&self) -> io::Result<EnrollmentState> {
        let state: EnrollmentState = self.read(STATE_FILE)?.unwrap_or_else(EnrollmentState::new);
        if uuid::Uuid::parse_str(&state.installation_id).is_err() { return Err(io::ErrorKind::InvalidData.into()); }
        Ok(state)
    }
    pub(in crate::mfa_email) fn save_state(&self, state: &EnrollmentState) -> io::Result<()> { self.write(STATE_FILE, state) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_relay_credential_budget_and_lock_survive_reopen() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        assert!(Store::open(home.path()).is_err());
        let credential = Credential { credential_id: "fixture-credential".into(), secret: "ab".repeat(32) };
        store.save_credential(&credential).unwrap();
        let mut state = store.state().unwrap();
        let installation = state.installation_id.clone();
        state.reserve(10_000).unwrap();
        store.save_state(&state).unwrap();
        drop(store);
        let store = Store::open(home.path()).unwrap();
        let loaded = store.credential().unwrap().unwrap();
        assert_eq!(loaded.credential_id, "fixture-credential");
        assert_eq!(loaded.key().unwrap(), vec![0xab; 32]);
        let mut state = store.state().unwrap();
        assert_eq!(state.installation_id, installation);
        assert!(state.reserve(9_999).is_err());
        assert!(state.reserve(13_599).is_err());
        state.reserve(13_600).unwrap();
    }
}
