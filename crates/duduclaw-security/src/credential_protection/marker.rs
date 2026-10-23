//! The non-secret marker file `<home>/credential_protection.json`, the
//! per-home account name, the key id and the canary.
//!
//! The marker only answers "was protection enabled here, and with which key
//! id". It is not a security boundary: a same-user process can edit it. What
//! it buys is detection — a key that does not match the marker's `key_id` and
//! `canary` is reported as changed instead of being used (design §6).

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

use crate::crypto::CryptoEngine;

use super::BackendKind;

/// File name of the marker under the DuDuClaw home.
pub const MARKER_FILE: &str = "credential_protection.json";
/// Keychain service name shared by every home on the machine.
pub const SERVICE: &str = "duduclaw-protected-key";
/// Fixed plaintext encrypted into the canary.
const CANARY_PLAINTEXT: &[u8] = b"duduclaw-protected-key-canary-v1";
/// Domain separator for the key id hash.
const KEY_ID_DOMAIN: &[u8] = b"duduclaw-protected-key-id:";
/// Hex characters kept from each SHA-256 (64 bits: an identifier, not a MAC).
const ID_HEX_LEN: usize = 16;
/// Marker format version.
const MARKER_VERSION: u32 = 1;

/// The parsed marker file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marker {
    pub version: u32,
    pub backend: BackendKind,
    pub account: String,
    pub key_id: String,
    pub canary: String,
    pub enabled_at: String,
}

/// What reading the marker found.
#[derive(Debug)]
pub enum MarkerRead {
    Absent,
    Present(Marker),
    /// The file exists but cannot be used; carries a short reason.
    Invalid(String),
}

pub fn marker_path(home: &Path) -> PathBuf {
    home.join(MARKER_FILE)
}

/// The keychain account for this home: `home-` + the first 16 hex of the
/// SHA-256 of the canonical home path. Recomputed on every call, never read
/// from a file, so nothing on disk can point the gateway at another item.
pub fn account_for_home(home: &Path) -> String {
    let canonical = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let hash = digest(&SHA256, canonical.to_string_lossy().as_bytes());
    format!("home-{}", short_hex(hash.as_ref()))
}

/// Identifier of a key (never the key itself).
pub fn key_id(key: &[u8]) -> String {
    let mut input = Vec::with_capacity(KEY_ID_DOMAIN.len() + key.len());
    input.extend_from_slice(KEY_ID_DOMAIN);
    input.extend_from_slice(key);
    let hash = digest(&SHA256, &input);
    zeroize::Zeroize::zeroize(&mut input);
    short_hex(hash.as_ref())
}

fn short_hex(bytes: &[u8]) -> String {
    let full: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    full.chars().take(ID_HEX_LEN).collect()
}

/// Encrypt the fixed canary text with `engine`.
pub fn make_canary(engine: &CryptoEngine) -> Result<String, String> {
    engine
        .encrypt(CANARY_PLAINTEXT)
        .map(|ct| BASE64.encode(ct))
        .map_err(|e| format!("canary encryption failed: {e}"))
}

/// Whether `canary` decrypts to the fixed text under `engine`.
pub fn canary_matches(engine: &CryptoEngine, canary: &str) -> bool {
    let Ok(bytes) = BASE64.decode(canary) else {
        return false;
    };
    matches!(engine.decrypt(&bytes), Ok(plain) if plain == CANARY_PLAINTEXT)
}

pub fn new_marker(backend: BackendKind, account: String, key_id: String, canary: String) -> Marker {
    Marker {
        version: MARKER_VERSION,
        backend,
        account,
        key_id,
        canary,
        enabled_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

pub fn read_marker(home: &Path) -> MarkerRead {
    let path = marker_path(home);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return MarkerRead::Absent,
        Err(e) => return MarkerRead::Invalid(format!("cannot read marker: {e}")),
    };
    match serde_json::from_slice::<Marker>(&bytes) {
        Ok(m) if m.version == MARKER_VERSION => MarkerRead::Present(m),
        Ok(m) => MarkerRead::Invalid(format!("unsupported marker version {}", m.version)),
        Err(e) => MarkerRead::Invalid(format!("marker is not valid JSON: {e}")),
    }
}

/// Write the marker atomically with owner-only permissions.
pub fn write_marker(home: &Path, marker: &Marker) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("cannot create home: {e}"))?;
    let body = serde_json::to_vec_pretty(marker).map_err(|e| format!("marker encode: {e}"))?;
    let path = marker_path(home);
    // Random suffix + create_new: a pre-planted file or symlink at the
    // temporary name makes the write fail instead of being followed.
    let tmp = home.join(format!(".{MARKER_FILE}.tmp-{}", random_suffix()?));
    let result = write_owner_only(&tmp, &body).and_then(|()| {
        std::fs::rename(&tmp, &path).map_err(|e| format!("marker rename: {e}"))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn remove_marker(home: &Path) -> Result<(), String> {
    match std::fs::remove_file(marker_path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove marker: {e}")),
    }
}

fn random_suffix() -> Result<String, String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "system RNG failure".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_owner_only(path: &Path, body: &[u8]) -> Result<(), String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| format!("marker open: {e}"))?;
    file.write_all(body).map_err(|e| format!("marker write: {e}"))?;
    file.sync_all().map_err(|e| format!("marker sync: {e}"))?;
    duduclaw_core::platform::set_owner_only(path).map_err(|e| format!("marker chmod: {e}"))
}
