//! WP-E2 — box-side Ed25519 device identity for the relay client
//! (`relay_client.rs`).
//!
//! Mirrors `a2a_signing.rs`'s load-or-generate convention (same crate,
//! `ed25519-dalek`; same atomic owner-only-permissions write — see that
//! module's doc for the create-time-mode rationale), but the derived public
//! value here is a relay-shaped `device_id`
//! (`duduclaw_core::relay_protocol::validate_device_id`), not a JWKS `kid`.
//!
//! Key material lives at `<home>/relay_device_key` (32 raw private-key
//! bytes, `chmod 600`). Missing key ⇒ generated on first use, once per
//! `home_dir` (so `device_id` — and hence relay registration — is stable
//! across gateway restarts). The relay never sees this file: only the
//! derived `device_id` and the public key (`pubkey_b64`) ever cross the
//! network, at `POST /v1/device/register` and every `/v1/device/ws`
//! challenge-response.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// A box's relay device identity: an Ed25519 keypair plus its derived,
/// relay-format `device_id`.
pub struct RelayDeviceIdentity {
    signing_key: SigningKey,
    device_id: String,
}

impl RelayDeviceIdentity {
    /// Build an identity from raw 32-byte private-key material. `pub` for
    /// tests and for the round-trip proof against
    /// `duduclaw_core::relay_protocol::validate_device_id`; production
    /// callers go through [`Self::load_or_generate`].
    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret);
        let device_id = derive_device_id(&signing_key.verifying_key());
        Self { signing_key, device_id }
    }

    /// Load the identity from `path`, generating (and persisting) it on
    /// first run. Returns `(identity, generated)` — `generated` is `true`
    /// only on the very first call for a given path.
    pub fn load_or_generate(path: &Path) -> Result<(Self, bool), String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let secret: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                    format!(
                        "relay 裝置私鑰長度不符（預期 32 bytes，實際 {}）",
                        bytes.len()
                    )
                })?;
                Ok((Self::from_secret_bytes(secret), false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let identity = generate_and_persist(path)?;
                Ok((identity, true))
            }
            Err(e) => Err(format!("讀取 relay 裝置私鑰失敗（{}）：{e}", path.display())),
        }
    }

    /// The relay-format `device_id` derived from this identity's public
    /// key. Stable for the lifetime of the key file.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Base64 (standard) of the 32 raw public-key bytes — the exact shape
    /// `POST /v1/device/register`'s `pubkey_b64` field and
    /// `duduclaw-relay::crypto::decode_pubkey` expect.
    pub fn pubkey_b64(&self) -> String {
        BASE64.encode(self.signing_key.verifying_key().to_bytes())
    }

    /// Sign `message` (the relay's raw challenge nonce bytes, already
    /// base64-decoded by the caller) and return the signature, base64
    /// (standard) — the exact shape the `auth` client frame's
    /// `signature_b64` and `duduclaw-relay::crypto::verify_signature`
    /// expect.
    pub fn sign(&self, message: &[u8]) -> String {
        let sig: Signature = self.signing_key.sign(message);
        BASE64.encode(sig.to_bytes())
    }

    /// Canonical key path: `<home>/relay_device_key`.
    pub fn default_key_path(home_dir: &Path) -> PathBuf {
        home_dir.join("relay_device_key")
    }
}

/// Generate a fresh key, persist it with `0600` perms, return the identity.
fn generate_and_persist(path: &Path) -> Result<RelayDeviceIdentity, String> {
    use rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut OsRng);
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("建立 relay 裝置金鑰目錄失敗（{}）：{e}", dir.display()))?;
        }
    }
    write_key_owner_only(path, &signing_key.to_bytes())?;
    let device_id = derive_device_id(&signing_key.verifying_key());
    Ok(RelayDeviceIdentity { signing_key, device_id })
}

/// Persist the raw private key so it is **never** briefly world/group-readable.
///
/// On Unix the file is created atomically with mode `0600` (the mode is
/// applied at `open` time, closing the window a later `chmod` would leave
/// open); `set_owner_only_permissions` re-asserts `0600` afterwards in case
/// the file pre-existed with looser permissions.
fn write_key_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("開啟 relay 裝置私鑰檔失敗（{}）：{e}", path.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("寫入 relay 裝置私鑰失敗（{}）：{e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
            .map_err(|e| format!("寫入 relay 裝置私鑰失敗（{}）：{e}", path.display()))?;
    }
    set_owner_only_permissions(path)
}

fn set_owner_only_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("設定 relay 裝置私鑰權限失敗（{}）：{e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Derive a relay-shaped `device_id` from a public key: `box-<32 lowercase
/// hex chars>` — a SHA-256 fingerprint truncated to 16 bytes, prefixed so it
/// never starts with a digit-only run that could be mistaken for something
/// else in a log line. 4 + 32 = 36 bytes, well inside the relay's `4..=64`
/// byte window (`duduclaw_core::relay_protocol::validate_device_id`) and
/// deterministic for a given key — a restart never orphans a previous
/// registration.
pub fn derive_device_id(vk: &VerifyingKey) -> String {
    let digest = Sha256::digest(vk.to_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("box-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> RelayDeviceIdentity {
        // Deterministic key from fixed bytes — reproducible tests.
        RelayDeviceIdentity::from_secret_bytes([7u8; 32])
    }

    #[test]
    fn device_id_is_deterministic_for_a_fixed_key() {
        assert_eq!(test_identity().device_id(), test_identity().device_id());
    }

    #[test]
    fn device_id_differs_across_keys() {
        let a = RelayDeviceIdentity::from_secret_bytes([1u8; 32]);
        let b = RelayDeviceIdentity::from_secret_bytes([2u8; 32]);
        assert_ne!(a.device_id(), b.device_id());
    }

    #[test]
    fn derived_device_id_satisfies_the_relays_own_validator() {
        // WP-E2 protocol-compat proof: whatever this module derives, the
        // shared core validator (also used by duduclaw-relay's
        // registration/hook/ws handlers) must accept.
        let identity = test_identity();
        assert!(duduclaw_core::relay_protocol::validate_device_id(identity.device_id()).is_ok());
    }

    #[test]
    fn pubkey_b64_decodes_to_exactly_32_bytes() {
        let identity = test_identity();
        let decoded = BASE64.decode(identity.pubkey_b64()).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn sign_then_verify_roundtrip_against_the_relays_own_crypto() {
        // Proves the client-side signature this module produces is exactly
        // what the relay's `crypto::verify_signature` accepts — the other
        // half of the challenge-response handshake.
        let identity = test_identity();
        let pubkey = BASE64.decode(identity.pubkey_b64()).unwrap();
        let pubkey: [u8; 32] = pubkey.try_into().unwrap();
        let nonce = b"some-32-byte-ish-challenge-nonce";
        let sig_b64 = identity.sign(nonce);

        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pubkey);
        let sig_bytes = BASE64.decode(&sig_b64).unwrap();
        assert!(key.verify(nonce, &sig_bytes).is_ok());
    }

    #[test]
    fn sign_over_wrong_message_fails_verification() {
        let identity = test_identity();
        let pubkey = BASE64.decode(identity.pubkey_b64()).unwrap();
        let pubkey: [u8; 32] = pubkey.try_into().unwrap();
        let sig_b64 = identity.sign(b"message-a");

        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pubkey);
        let sig_bytes = BASE64.decode(&sig_b64).unwrap();
        assert!(key.verify(b"message-b", &sig_bytes).is_err());
    }

    #[test]
    fn load_or_generate_creates_then_reuses_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = RelayDeviceIdentity::default_key_path(dir.path());
        let (identity1, generated) = RelayDeviceIdentity::load_or_generate(&path).unwrap();
        assert!(generated, "first call generates");
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "key must be owner-only");
        }
        let (identity2, generated2) = RelayDeviceIdentity::load_or_generate(&path).unwrap();
        assert!(!generated2, "second call reuses");
        assert_eq!(identity1.device_id(), identity2.device_id());
        assert_eq!(identity1.pubkey_b64(), identity2.pubkey_b64());
    }

    #[test]
    fn default_key_path_matches_the_documented_filename() {
        let home = Path::new("/tmp/example-home");
        assert_eq!(
            RelayDeviceIdentity::default_key_path(home),
            home.join("relay_device_key")
        );
    }

    #[test]
    fn load_rejects_wrong_length_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_key");
        std::fs::write(&path, b"too short").unwrap();
        assert!(RelayDeviceIdentity::load_or_generate(&path).is_err());
    }
}
