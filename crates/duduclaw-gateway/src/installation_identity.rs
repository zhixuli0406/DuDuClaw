//! Per-installation identity shared by the license and MFA relay flows.
//!
//! * [`StateDir`] — the descriptor-pinned private `<home>/state` directory
//!   (Unix: `O_NOFOLLOW` descriptors, 0600, single hard link, owner uid, no
//!   macOS ACL; Windows: `duduclaw_core::windows_private_state`).
//! * [`InstallationKey`] — the Ed25519 `state/installation.key` (raw 32-byte
//!   seed). It is generated only when the file is absent (`NotFound`) and with
//!   `O_CREAT|O_EXCL`; every other read failure fails closed and never
//!   overwrites. This is deliberately not `identity.key` (an HMAC key that
//!   same-uid AI employees can already read).
//!
//! The license carries the owner-approved public key (`installation_pubkey`).
//! The relay is usable only when that value equals this key's public half.
use std::{io, path::Path, time::{Duration, Instant}};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const STATE_DIR: &str = "state";
pub const INSTALLATION_KEY_FILE: &str = "installation.key";
const INSTALLATION_LOCK_FILE: &str = "installation.lock";
/// Upper bound for any single private state file.
pub(crate) const MAX_STATE_BYTES: usize = 4096;
const LOCK_WAIT: Duration = Duration::from_secs(2);

#[cfg(unix)]
#[path = "installation_identity/state_dir_unix.rs"]
mod state_dir;
#[cfg(windows)]
#[path = "installation_identity/state_dir_windows.rs"]
mod state_dir;

#[cfg(any(unix, windows))]
pub use state_dir::StateDir;

/// Never downgrade private state to path-based or public writes.
#[cfg(not(any(unix, windows)))]
pub struct StateDir;
#[cfg(not(any(unix, windows)))]
impl StateDir {
    pub fn open(_: &Path) -> io::Result<Self> { Err(io::ErrorKind::Unsupported.into()) }
    pub fn lock(&self, _: &str) -> io::Result<std::fs::File> { Err(io::ErrorKind::Unsupported.into()) }
    pub fn read(&self, _: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> { Err(io::ErrorKind::Unsupported.into()) }
    pub fn write_atomic(&self, _: &str, _: &[u8]) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
    pub fn create_new(&self, _: &str, _: &[u8]) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
}

fn contended(error: io::Error) -> io::Error {
    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
        io::ErrorKind::WouldBlock.into()
    } else { error }
}

/// Non-blocking lock attempt; contention is reported as `WouldBlock`.
pub(crate) fn try_lock(file: &std::fs::File, exclusive: bool) -> io::Result<()> {
    let result = if exclusive { fs2::FileExt::try_lock_exclusive(file) } else { fs2::FileExt::try_lock_shared(file) };
    result.map_err(contended)
}

/// Bounded wait for a lock; callers run on blocking threads.
fn lock_wait(file: &std::fs::File, exclusive: bool, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_lock(file, exclusive) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            other => return other,
        }
    }
}

/// The installation's Ed25519 key. No `Debug`/`Clone`: the seed is secret.
/// `ed25519-dalek` is built with `zeroize`, so dropping clears the seed.
pub struct InstallationKey {
    signing: SigningKey,
}

impl InstallationKey {
    fn from_seed(seed: &Zeroizing<[u8; 32]>) -> Self {
        Self { signing: SigningKey::from_bytes(seed) }
    }

    fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != 32 { return Err(io::ErrorKind::InvalidData.into()); }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(bytes);
        Ok(Self::from_seed(&seed))
    }

    pub fn public_key_bytes(&self) -> [u8; 32] { self.signing.verifying_key().to_bytes() }

    /// 64 lowercase hex characters, the `installation_pubkey` wire format.
    pub fn public_key_hex(&self) -> String { hex::encode(self.public_key_bytes()) }

    /// Short authentication string shown to the owner; see [`sas_fingerprint`].
    pub fn fingerprint(&self) -> String { sas_fingerprint(&self.public_key_bytes()) }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] { self.signing.sign(message).to_bytes() }
}

/// SAS fingerprint: the first 16 lowercase hex characters of
/// SHA-256(raw 32-byte public key), grouped 4-4-4-4 with `-`
/// (e.g. `1a2b-3c4d-5e6f-7a8b`). The license console uses the same definition.
pub fn sas_fingerprint(public_key: &[u8; 32]) -> String {
    let prefix: Vec<char> = hex::encode(Sha256::digest(public_key)).chars().take(16).collect();
    prefix.chunks(4).map(|group| group.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

/// SAS for a 64-hex public key string (e.g. the license's bound key).
pub fn sas_for_hex(public_key_hex: &str) -> Option<String> {
    if !duduclaw_license::is_valid_installation_pubkey(public_key_hex) { return None; }
    let bytes: [u8; 32] = hex::decode(public_key_hex).ok()?.try_into().ok()?;
    Some(sas_fingerprint(&bytes))
}

fn read_key(dir: &StateDir) -> io::Result<Option<InstallationKey>> {
    match dir.read(INSTALLATION_KEY_FILE)? {
        Some(bytes) => InstallationKey::parse(&bytes).map(Some),
        None => Ok(None),
    }
}

/// Read the key without creating it. `Ok(None)` only when the file is absent.
pub fn load(home: &Path) -> io::Result<Option<InstallationKey>> {
    let dir = StateDir::open(home)?;
    let lock = dir.lock(INSTALLATION_LOCK_FILE)?;
    lock_wait(&lock, false, LOCK_WAIT)?;
    read_key(&dir)
}

/// Ensure the key exists. Returns `(key, created)`. Generation happens only
/// when the key file is `NotFound`; malformed, unsafe (symlink, hard link,
/// permissions, ACL) or unreadable files are errors and are never replaced.
pub fn load_or_create(home: &Path) -> io::Result<(InstallationKey, bool)> {
    let dir = StateDir::open(home)?;
    let lock = dir.lock(INSTALLATION_LOCK_FILE)?;
    lock_wait(&lock, true, LOCK_WAIT)?;
    if let Some(key) = read_key(&dir)? { return Ok((key, false)); }
    let mut seed = Zeroizing::new([0u8; 32]);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed[..]);
    dir.create_new(INSTALLATION_KEY_FILE, &seed[..])?;
    Ok((InstallationKey::from_seed(&seed), true))
}

/// Public key hex for refresh/rebind requests, generating the key if absent.
/// `None` on any failure: callers then omit the field rather than abort.
pub fn ensure_public_key_hex(home: &Path) -> Option<String> {
    match load_or_create(home) {
        Ok((key, created)) => {
            if created { tracing::info!(fingerprint = %key.fingerprint(), "generated MFA relay installation key"); }
            Some(key.public_key_hex())
        }
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "installation key unavailable; refusing to regenerate");
            None
        }
    }
}

/// SAS of the existing local key, without creating one.
pub fn fingerprint(home: &Path) -> Option<String> {
    load(home).ok().flatten().map(|key| key.fingerprint())
}

/// Relay key gate (design §2.1): the license must carry a bound key and the
/// local key must exist and match. Payment tier is never affected by this.
pub(crate) fn relay_key_status(bound: Option<&str>, local: io::Result<Option<InstallationKey>>)
    -> Result<InstallationKey, &'static str> {
    let Some(bound) = bound else { return Err("relay_key_unbound") };
    match local {
        Ok(Some(key)) if key.public_key_hex() == bound => Ok(key),
        _ => Err("relay_key_mismatch"),
    }
}

#[cfg(test)]
pub(crate) fn key_from_seed_for_test(seed: [u8; 32]) -> InstallationKey {
    InstallationKey::from_seed(&Zeroizing::new(seed))
}

#[cfg(test)]
#[path = "installation_identity/tests.rs"]
mod tests;
