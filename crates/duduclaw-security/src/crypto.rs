use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use duduclaw_core::error::{DuDuClawError, Result};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// AES-256-GCM encryption engine.
pub struct CryptoEngine {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl CryptoEngine {
    /// Create a new engine from a 32-byte key.
    pub fn new(key_bytes: &[u8; KEY_LEN]) -> Result<Self> {
        let unbound = UnboundKey::new(&AES_256_GCM, key_bytes)
            .map_err(|e| DuDuClawError::Security(format!("failed to create key: {e}")))?;
        Ok(Self {
            key: LessSafeKey::new(unbound),
            rng: SystemRandom::new(),
        })
    }

    /// Generate a random 32-byte key.
    pub fn generate_key() -> Result<[u8; KEY_LEN]> {
        let rng = SystemRandom::new();
        let mut key = [0u8; KEY_LEN];
        rng.fill(&mut key)
            .map_err(|e| DuDuClawError::Security(format!("system RNG failure: {e}")))?;
        Ok(key)
    }

    /// Encrypt plaintext. The returned `Vec` has the 12-byte nonce prepended.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce_bytes)
            .map_err(|e| DuDuClawError::Security(format!("RNG failure: {e}")))?;

        let nonce = Nonce::assume_unique_for_key(nonce_bytes);

        // ring encrypts in-place and appends a tag, so we need room for the tag.
        let mut in_out = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
            .map_err(|e| DuDuClawError::Security(format!("encryption failed: {e}")))?;

        // Prepend the nonce.
        let mut result = Vec::with_capacity(NONCE_LEN + in_out.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&in_out);
        Ok(result)
    }

    /// Decrypt ciphertext that was produced by [`Self::encrypt`].
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() < NONCE_LEN + aead::AES_256_GCM.tag_len() {
            return Err(DuDuClawError::Security(
                "ciphertext too short".to_string(),
            ));
        }

        let (nonce_bytes, encrypted) = ciphertext.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce_bytes)
            .map_err(|e| DuDuClawError::Security(format!("invalid nonce: {e}")))?;

        let mut in_out = encrypted.to_vec();
        let plaintext = self
            .key
            .open_in_place(nonce, Aad::empty(), &mut in_out)
            .map_err(|e| DuDuClawError::Security(format!("decryption failed: {e}")))?;

        Ok(plaintext.to_vec())
    }

    /// Encrypt a string and return a base64-encoded result.
    pub fn encrypt_string(&self, text: &str) -> Result<String> {
        let encrypted = self.encrypt(text.as_bytes())?;
        Ok(BASE64.encode(&encrypted))
    }

    /// Decrypt a base64-encoded string that was produced by [`Self::encrypt_string`].
    pub fn decrypt_string(&self, encoded: &str) -> Result<String> {
        let decoded = BASE64
            .decode(encoded)
            .map_err(|e| DuDuClawError::Security(format!("base64 decode failed: {e}")))?;
        let plaintext = self.decrypt(&decoded)?;
        String::from_utf8(plaintext)
            .map_err(|e| DuDuClawError::Security(format!("invalid UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bytes() {
        let key = CryptoEngine::generate_key().unwrap();
        let engine = CryptoEngine::new(&key).unwrap();
        let original = b"hello world";
        let encrypted = engine.encrypt(original).unwrap();
        let decrypted = engine.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn round_trip_string() {
        let key = CryptoEngine::generate_key().unwrap();
        let engine = CryptoEngine::new(&key).unwrap();
        let original = "secret message";
        let encrypted = engine.encrypt_string(original).unwrap();
        let decrypted = engine.decrypt_string(&encrypted).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn decrypt_short_ciphertext_fails() {
        let key = CryptoEngine::generate_key().unwrap();
        let engine = CryptoEngine::new(&key).unwrap();
        assert!(engine.decrypt(&[0u8; 5]).is_err());
    }
}
