use duduclaw_core::error::{DuDuClawError, Result};

/// Authentication manager supporting two modes:
///
/// 1. **Token** — a pre-shared secret string (legacy, simple).
/// 2. **Ed25519 challenge-response** — the client signs a server-issued
///    random challenge with its Ed25519 private key; the server verifies
///    with a stored public key.
///
/// If an Ed25519 public key is configured, challenge-response is used in
/// preference to the token. Both can be configured for flexibility.
pub struct AuthManager {
    token: Option<String>,
    /// Raw 32-byte Ed25519 public key (if configured).
    ed25519_pubkey: Option<Vec<u8>>,
    /// Active challenge with creation timestamp (set by `issue_challenge`, consumed by `verify_ed25519`).
    challenge: std::sync::Mutex<Option<(std::time::Instant, [u8; 32])>>,
}

/// Maximum age of a challenge before it expires (30 seconds).
const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

impl AuthManager {
    /// Create a new [`AuthManager`] with optional token auth.
    pub fn new(token: Option<String>) -> Self {
        Self {
            token,
            ed25519_pubkey: None,
            challenge: std::sync::Mutex::new(None),
        }
    }

    /// Create a new [`AuthManager`] with Ed25519 public key auth.
    ///
    /// `pubkey_bytes` must be 32 raw bytes (uncompressed Ed25519 public key).
    pub fn with_ed25519(pubkey_bytes: Vec<u8>) -> Self {
        Self {
            token: None,
            ed25519_pubkey: Some(pubkey_bytes),
            challenge: std::sync::Mutex::new(None),
        }
    }

    /// Returns `true` when any form of authentication is required.
    pub fn is_auth_required(&self) -> bool {
        self.token.is_some() || self.ed25519_pubkey.is_some()
    }

    /// Returns `true` when Ed25519 challenge-response is configured.
    pub fn is_ed25519(&self) -> bool {
        self.ed25519_pubkey.is_some()
    }

    /// Generate a random 32-byte challenge, store it, and return it
    /// base64-encoded for transmission to the client.
    pub fn issue_challenge(&self) -> String {
        use ring::rand::SecureRandom;
        let rng = ring::rand::SystemRandom::new();
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes).expect("RNG should not fail");

        if let Ok(mut guard) = self.challenge.lock() {
            *guard = Some((std::time::Instant::now(), bytes));
        }

        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    }

    /// Verify an Ed25519 signature against the stored challenge.
    ///
    /// `signature_b64` — base64-encoded 64-byte Ed25519 signature.
    ///
    /// Returns `Ok(())` on success; an error on any failure (invalid key,
    /// bad signature, or no active challenge).
    pub fn verify_ed25519(&self, signature_b64: &str) -> Result<()> {
        let pubkey_bytes = self.ed25519_pubkey.as_ref().ok_or_else(|| {
            DuDuClawError::Security("Ed25519 not configured".to_owned())
        })?;

        let (created_at, challenge) = self
            .challenge
            .lock()
            .map_err(|_| DuDuClawError::Security("challenge lock poisoned".to_owned()))?
            .take()
            .ok_or_else(|| DuDuClawError::Security("no active challenge".to_owned()))?;

        // Reject expired challenges
        if created_at.elapsed() > CHALLENGE_TTL {
            return Err(DuDuClawError::Security("challenge expired".to_owned()));
        }

        let sig_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            signature_b64,
        )
        .map_err(|e| DuDuClawError::Security(format!("bad signature base64: {e}")))?;

        let pubkey = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ED25519,
            pubkey_bytes,
        );

        pubkey
            .verify(&challenge, &sig_bytes)
            .map_err(|_| DuDuClawError::Security("Ed25519 signature verification failed".to_owned()))
    }

    /// Validate a provided bearer token against the configured token.
    ///
    /// Uses constant-time comparison to prevent timing attacks.
    pub fn validate(&self, provided_token: &str) -> Result<()> {
        match &self.token {
            Some(expected) => {
                if constant_time_eq(expected.as_bytes(), provided_token.as_bytes()) {
                    Ok(())
                } else {
                    Err(DuDuClawError::Security(
                        "invalid authentication token".to_owned(),
                    ))
                }
            }
            None => Ok(()), // No token required
        }
    }
}

/// Constant-time byte-slice equality check.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_auth_required() {
        let mgr = AuthManager::new(None);
        assert!(!mgr.is_auth_required());
        assert!(mgr.validate("anything").is_ok());
    }

    #[test]
    fn test_valid_token() {
        let mgr = AuthManager::new(Some("secret".to_owned()));
        assert!(mgr.is_auth_required());
        assert!(mgr.validate("secret").is_ok());
    }

    #[test]
    fn test_invalid_token() {
        let mgr = AuthManager::new(Some("secret".to_owned()));
        assert!(mgr.validate("wrong").is_err());
    }

    #[test]
    fn test_challenge_issued() {
        let mgr = AuthManager::new(None);
        let challenge = mgr.issue_challenge();
        assert!(!challenge.is_empty());
        // base64 of 32 bytes = 44 chars
        assert_eq!(challenge.len(), 44);
    }

    #[test]
    fn test_verify_ed25519_no_active_challenge() {
        let pk = vec![0u8; 32]; // dummy key
        let mgr = AuthManager::with_ed25519(pk);
        assert!(mgr.verify_ed25519("dGVzdA==").is_err());
    }
}
