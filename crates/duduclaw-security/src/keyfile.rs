//! Read-only keyfile-backed decryption helper.
//!
//! Mirrors the gateway's `config_crypto::decrypt_value` semantics so that
//! consuming crates (secret_manager, inference) can decrypt `*_enc` config
//! fields that the gateway wrote AES-256-GCM-encrypted.
//!
//! The AES key lives in a 32-byte file at `~/.duduclaw/.keyfile`. This module
//! NEVER creates or modifies that file — a missing/short keyfile or bad
//! ciphertext yields `None` (fail-soft), never a panic and never a fresh key.

use std::path::Path;

use crate::crypto::CryptoEngine;

const KEY_LEN: usize = 32;

/// Load `<home_dir>/.keyfile` (must be exactly 32 bytes) read-only.
///
/// Returns `None` if the file is missing or not exactly 32 bytes long.
/// Never creates or modifies the keyfile.
fn load_keyfile(home_dir: &Path) -> Option<[u8; KEY_LEN]> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() == KEY_LEN {
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        Some(key)
    } else {
        tracing::warn!(
            path = %keyfile.display(),
            actual_len = bytes.len(),
            "Keyfile has incorrect length (expected 32 bytes) — decryption disabled"
        );
        None
    }
}

/// Load `~/.duduclaw/.keyfile` (32 bytes, read-only) and decrypt a base64
/// `CryptoEngine` ciphertext.
///
/// Returns `None` on missing/short keyfile or any decrypt failure (including
/// an empty plaintext). Never creates or modifies the keyfile.
pub fn decrypt_keyfile_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    if encrypted.is_empty() {
        return None;
    }
    let key = load_keyfile(home_dir)?;
    let engine = CryptoEngine::new(&key).ok()?;
    match engine.decrypt_string(encrypted) {
        Ok(plain) if !plain.is_empty() => Some(plain),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("keyfile decryption failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-keyfile-test-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        /// Write a fresh 32-byte keyfile and return the matching CryptoEngine.
        fn with_keyfile(&self) -> CryptoEngine {
            let key = CryptoEngine::generate_key().unwrap();
            std::fs::write(self.0.join(".keyfile"), key).unwrap();
            CryptoEngine::new(&key).unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn round_trips_encrypted_value() {
        let home = TempHome::new();
        let engine = home.with_keyfile();
        let cipher = engine.encrypt_string("super-secret-token").unwrap();
        let plain = decrypt_keyfile_value(&cipher, home.path());
        assert_eq!(plain.as_deref(), Some("super-secret-token"));
    }

    #[test]
    fn missing_keyfile_returns_none() {
        let home = TempHome::new();
        // No keyfile written.
        assert!(decrypt_keyfile_value("Zm9v", home.path()).is_none());
        // And we must not have created one.
        assert!(!home.path().join(".keyfile").exists());
    }

    #[test]
    fn short_keyfile_returns_none() {
        let home = TempHome::new();
        std::fs::write(home.path().join(".keyfile"), b"too-short").unwrap();
        assert!(decrypt_keyfile_value("Zm9v", home.path()).is_none());
    }

    #[test]
    fn empty_ciphertext_returns_none() {
        let home = TempHome::new();
        home.with_keyfile();
        assert!(decrypt_keyfile_value("", home.path()).is_none());
    }

    #[test]
    fn garbage_ciphertext_returns_none() {
        let home = TempHome::new();
        home.with_keyfile();
        // Valid keyfile but invalid ciphertext → fail-soft None.
        assert!(decrypt_keyfile_value("not-real-ciphertext!!!", home.path()).is_none());
    }
}
