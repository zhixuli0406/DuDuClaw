//! Ed25519 challenge-response primitives for device authentication.
//!
//! Mirrors the challenge-response convention already used elsewhere in the
//! workspace for a single shared key (issue a random nonce, verify a
//! signature over it, expire unused challenges), adapted for a relay that
//! authenticates many independent devices — one Ed25519 public key per
//! device, looked up from storage at connection time rather than held by a
//! single `AuthManager`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

/// Maximum age of an issued challenge before it must be rejected.
pub const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A per-connection challenge nonce.
#[derive(Clone)]
pub struct Challenge {
    pub issued_at: std::time::Instant,
    pub nonce: [u8; 32],
}

impl Challenge {
    pub fn is_expired(&self) -> bool {
        self.issued_at.elapsed() > CHALLENGE_TTL
    }

    pub fn nonce_b64(&self) -> String {
        BASE64.encode(self.nonce)
    }
}

/// Issue a fresh random 32-byte challenge.
pub fn issue_challenge() -> Challenge {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut nonce = [0u8; 32];
    rng.fill(&mut nonce).expect("system RNG must not fail");
    Challenge {
        issued_at: std::time::Instant::now(),
        nonce,
    }
}

/// Decode and validate a base64 Ed25519 public key. Must decode to exactly
/// 32 raw bytes.
pub fn decode_pubkey(pubkey_b64: &str) -> Result<[u8; 32], String> {
    let bytes = BASE64
        .decode(pubkey_b64)
        .map_err(|e| format!("invalid public key base64: {e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("public key must be 32 bytes, got {}", v.len()))
}

/// Verify a base64-encoded Ed25519 signature over `challenge.nonce` using
/// the raw 32-byte `pubkey`. Fails closed: an expired challenge, malformed
/// signature, or bad signature are all treated identically as auth failure.
pub fn verify_signature(pubkey: &[u8], challenge: &Challenge, signature_b64: &str) -> Result<(), String> {
    if challenge.is_expired() {
        return Err("challenge expired".to_string());
    }
    let sig = BASE64
        .decode(signature_b64)
        .map_err(|e| format!("invalid signature base64: {e}"))?;
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pubkey);
    key.verify(&challenge.nonce, &sig)
        .map_err(|_| "signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::time::Duration;

    fn gen_keypair() -> Ed25519KeyPair {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    }

    fn sign_challenge(key: &Ed25519KeyPair, challenge: &Challenge) -> String {
        let sig = key.sign(&challenge.nonce);
        BASE64.encode(sig.as_ref())
    }

    #[test]
    fn roundtrip_sign_verify_succeeds() {
        let key = gen_keypair();
        let pubkey: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let challenge = issue_challenge();
        let sig_b64 = sign_challenge(&key, &challenge);
        assert!(verify_signature(&pubkey, &challenge, &sig_b64).is_ok());
    }

    #[test]
    fn wrong_key_is_rejected() {
        let signer = gen_keypair();
        let other = gen_keypair();
        let other_pubkey: [u8; 32] = other.public_key().as_ref().try_into().unwrap();
        let challenge = issue_challenge();
        let sig_b64 = sign_challenge(&signer, &challenge);
        assert!(verify_signature(&other_pubkey, &challenge, &sig_b64).is_err());
    }

    #[test]
    fn signature_over_different_nonce_is_rejected() {
        let key = gen_keypair();
        let pubkey: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let challenge_a = issue_challenge();
        let challenge_b = issue_challenge();
        let sig_b64 = sign_challenge(&key, &challenge_a);
        assert!(verify_signature(&pubkey, &challenge_b, &sig_b64).is_err());
    }

    #[test]
    fn expired_challenge_is_rejected() {
        let key = gen_keypair();
        let pubkey: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let mut challenge = issue_challenge();
        challenge.issued_at = std::time::Instant::now()
            .checked_sub(CHALLENGE_TTL + Duration::from_secs(1))
            .expect("test host must have been up for >31s");
        let sig_b64 = sign_challenge(&key, &challenge);
        assert!(verify_signature(&pubkey, &challenge, &sig_b64).is_err());
    }

    #[test]
    fn garbage_signature_is_rejected() {
        let pubkey = [0u8; 32];
        let challenge = issue_challenge();
        assert!(verify_signature(&pubkey, &challenge, "not-base64!!").is_err());
    }

    #[test]
    fn decode_pubkey_rejects_wrong_length() {
        let short = BASE64.encode([0u8; 16]);
        assert!(decode_pubkey(&short).is_err());
    }

    #[test]
    fn decode_pubkey_accepts_32_bytes() {
        let key = gen_keypair();
        let b64 = BASE64.encode(key.public_key().as_ref());
        assert!(decode_pubkey(&b64).is_ok());
    }
}
