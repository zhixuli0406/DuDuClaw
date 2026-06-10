//! Signed Certificate Revocation List (CRL) client.
//!
//! The control-plane returns a list of revoked subscription IDs with an
//! Ed25519 signature over a canonical payload. This module provides the
//! verifier so a client binary can:
//!
//! 1. fetch `GET /v1/license/crl` (the HTTP call lives in the caller),
//! 2. deserialize the JSON into [`SignedCrl`],
//! 3. verify the signature with [`PublicKeyRegistry`],
//! 4. consult [`SignedCrl::is_revoked`] before honouring its own license.
//!
//! Canonical payload schema must match `commercial/cloud-control-plane/src/handlers/crl.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{LicenseError, Result};
use crate::key::PublicKeyRegistry;

/// A signed CRL document as served by the control-plane.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignedCrl {
    pub generated_at: DateTime<Utc>,
    pub revoked: Vec<String>,
    pub ttl_seconds: u64,
    pub public_key_id: String,
    /// Base64-encoded Ed25519 signature.
    pub signature: String,
}

#[derive(Debug, Serialize)]
struct CrlPayload<'a> {
    generated_at: DateTime<Utc>,
    revoked: &'a [String],
    ttl_seconds: u64,
    public_key_id: &'a str,
}

impl SignedCrl {
    /// Verify the CRL signature using a [`PublicKeyRegistry`] containing
    /// the issuer keys this binary trusts.
    pub fn verify(&self, registry: &PublicKeyRegistry) -> Result<()> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let pubkey = registry
            .get(&self.public_key_id)
            .ok_or_else(|| LicenseError::UnknownPublicKeyId(self.public_key_id.clone()))?;

        let key_array: [u8; 32] = pubkey
            .try_into()
            .map_err(|_| LicenseError::ParseError("public key must be 32 bytes".into()))?;
        let verifying_key = VerifyingKey::from_bytes(&key_array)
            .map_err(|e| LicenseError::ParseError(format!("invalid public key: {e}")))?;

        let sig_bytes = BASE64
            .decode(&self.signature)
            .map_err(|e| LicenseError::ParseError(format!("decode CRL signature: {e}")))?;
        let sig_array: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| LicenseError::InvalidSignature)?;
        let signature = Signature::from_bytes(&sig_array);

        let canonical = serde_json::to_vec(&CrlPayload {
            generated_at: self.generated_at,
            revoked: &self.revoked,
            ttl_seconds: self.ttl_seconds,
            public_key_id: &self.public_key_id,
        })
        .map_err(|e| LicenseError::ParseError(format!("serialize CRL payload: {e}")))?;

        verifying_key
            .verify(&canonical, &signature)
            .map_err(|_| LicenseError::InvalidSignature)?;

        Ok(())
    }

    /// Returns `true` if the given subscription is in the revoked list.
    ///
    /// Callers should call [`Self::verify`] first; this method does not.
    pub fn is_revoked(&self, subscription_id: &str) -> bool {
        self.revoked.iter().any(|s| s == subscription_id)
    }

    /// Returns `true` if the CRL is older than its declared TTL and
    /// callers should refresh it.
    pub fn is_stale(&self) -> bool {
        let age = (Utc::now() - self.generated_at).num_seconds().max(0) as u64;
        age > self.ttl_seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use chrono::Duration;
    use ed25519_dalek::{Signer, SigningKey};

    fn sign_with(
        signing_key: &SigningKey,
        revoked: Vec<String>,
        public_key_id: &str,
    ) -> SignedCrl {
        let generated_at = Utc::now();
        let ttl = 60;
        let payload = CrlPayload {
            generated_at,
            revoked: &revoked,
            ttl_seconds: ttl,
            public_key_id,
        };
        let canonical = serde_json::to_vec(&payload).unwrap();
        let sig = signing_key.sign(&canonical);
        SignedCrl {
            generated_at,
            revoked,
            ttl_seconds: ttl,
            public_key_id: public_key_id.to_string(),
            signature: BASE64.encode(sig.to_bytes()),
        }
    }

    #[test]
    fn signed_crl_verifies_with_registered_key() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let pubkey = signing_key.verifying_key();
        let crl = sign_with(
            &signing_key,
            vec!["sub_one".into(), "sub_two".into()],
            "v1",
        );
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey.to_bytes().to_vec());

        assert!(crl.verify(&registry).is_ok());
        assert!(crl.is_revoked("sub_one"));
        assert!(crl.is_revoked("sub_two"));
        assert!(!crl.is_revoked("sub_other"));
    }

    #[test]
    fn signed_crl_rejects_unknown_public_key_id() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let pubkey = signing_key.verifying_key();
        let crl = sign_with(&signing_key, vec![], "v99");
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey.to_bytes().to_vec());

        let err = crl.verify(&registry).unwrap_err();
        assert!(matches!(err, LicenseError::UnknownPublicKeyId(id) if id == "v99"));
    }

    #[test]
    fn signed_crl_rejects_tampered_revoked_list() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let pubkey = signing_key.verifying_key();
        let mut crl = sign_with(&signing_key, vec!["sub_real".into()], "v1");
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey.to_bytes().to_vec());

        crl.revoked.push("sub_injected".into());

        let err = crl.verify(&registry).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    #[test]
    fn signed_crl_rejects_wrong_signing_key() {
        let real_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let attacker_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let crl = sign_with(&attacker_key, vec!["sub_x".into()], "v1");
        let registry =
            PublicKeyRegistry::new().with_key("v1", real_key.verifying_key().to_bytes().to_vec());

        let err = crl.verify(&registry).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    #[test]
    fn is_stale_after_ttl() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let mut crl = sign_with(&signing_key, vec![], "v1");
        crl.generated_at = Utc::now() - Duration::seconds(crl.ttl_seconds as i64 + 5);
        assert!(crl.is_stale());
    }

    #[test]
    fn is_not_stale_when_fresh() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let crl = sign_with(&signing_key, vec![], "v1");
        assert!(!crl.is_stale());
    }
}
