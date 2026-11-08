//! Ed25519 verification for license payloads.
//!
//! **Signing** lives in the closed-source `commercial/duduclaw-license` crate;
//! only the verifier is open-source. A binary that ships with DuDuClaw embeds
//! one or more issuer public keys via [`PublicKeyRegistry`], and uses
//! [`verify_license`] to authenticate licenses it loads from disk or the
//! network.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::error::{LicenseError, Result};
use crate::license::License;

/// Verify a license's signature against an Ed25519 public key.
///
/// This is the low-level primitive. Most callers should use
/// [`PublicKeyRegistry::verify`] which dispatches by `license.public_key_id`.
///
/// # Errors
/// - `LicenseError::ParseError` if the public key bytes are not 32 bytes
///   or do not decode to a valid Ed25519 point.
/// - `LicenseError::InvalidSignature` if the signature does not match.
pub fn verify_license(license: &License, public_key: &[u8]) -> Result<()> {
    let key_array: [u8; 32] = public_key
        .try_into()
        .map_err(|_| LicenseError::ParseError("public key must be 32 bytes".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&key_array)
        .map_err(|e| LicenseError::ParseError(format!("invalid public key: {e}")))?;

    let sig_array: [u8; 64] = license
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| LicenseError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_array);

    let payload = license.canonical_payload()?;
    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| LicenseError::InvalidSignature)?;

    Ok(())
}

/// Registry of issuer public keys keyed by ID.
///
/// Binaries embed one or more (key_id, public_key) pairs at compile time;
/// `verify_license_by_id` dispatches to the right key based on the license
/// claim. This enables key rotation without invalidating existing licenses
/// (old licenses keep verifying against the old key while new ones use the
/// new key).
#[derive(Debug, Clone, Default)]
pub struct PublicKeyRegistry {
    keys: Vec<(String, Vec<u8>)>,
}

impl PublicKeyRegistry {
    /// Construct an empty registry. Use [`Self::with_key`] to add keys.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a public key under the given ID.
    pub fn with_key(mut self, id: impl Into<String>, public_key: impl Into<Vec<u8>>) -> Self {
        self.keys.push((id.into(), public_key.into()));
        self
    }

    /// Look up a key by ID.
    pub fn get(&self, id: &str) -> Option<&[u8]> {
        self.keys
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v.as_slice())
    }

    /// Verify a license using whichever key is named in `license.public_key_id`.
    ///
    /// # Errors
    /// Returns `LicenseError::UnknownPublicKeyId` if the registry does not
    /// contain a key matching the license claim.
    pub fn verify(&self, license: &License) -> Result<()> {
        let key = self
            .get(&license.public_key_id)
            .ok_or_else(|| LicenseError::UnknownPublicKeyId(license.public_key_id.clone()))?;
        verify_license(license, key)
    }

    /// True if the registry contains at least one key. Used by callers to
    /// decide whether license enforcement is even possible — a build with
    /// no embedded keys can only operate in OpenSource mode.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::CURRENT_SCHEMA_VERSION;
    use crate::tier::LicenseTier;
    use chrono::{Duration, Utc};
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn make_test_license() -> License {
        License::new(
            "sub_test_001",
            "cus_test_001",
            LicenseTier::SelfHostPro,
            "abc123",
            Duration::days(365),
            "v1",
        )
    }

    /// Helper that mirrors the closed-source signing path so that tests
    /// in this crate can exercise verification without depending on the
    /// `commercial/` signer.
    fn sign_for_test(license: &License, signing_key: &SigningKey) -> Vec<u8> {
        let payload = license.canonical_payload().unwrap();
        signing_key.sign(&payload).to_bytes().to_vec()
    }

    #[test]
    fn verify_succeeds_for_genuine_signature() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();

        license.signature = sign_for_test(&license, &signing_key);

        verify_license(&license, &public_key.to_bytes()).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_customer() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();
        license.signature = sign_for_test(&license, &signing_key);

        license.customer_id = "cus_pirate".into();

        let err = verify_license(&license, &public_key.to_bytes()).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    #[test]
    fn verify_rejects_tampered_last_phone_home() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();
        license.signature = sign_for_test(&license, &signing_key);

        license.last_phone_home = Utc::now() + Duration::days(1000);

        assert!(matches!(
            verify_license(&license, &public_key.to_bytes()).unwrap_err(),
            LicenseError::InvalidSignature
        ));
    }

    #[test]
    fn verify_accepts_signed_max_agents_quota() {
        // A P-License quota that was signed must verify (round-trip).
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();
        license.max_agents = Some(3);
        license.signature = sign_for_test(&license, &signing_key);
        verify_license(&license, &public_key.to_bytes()).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_max_agents() {
        // THE tamper test: sign a "sell 3 agents" license, then locally raise the
        // quota to 8 by editing license.json. The recomputed canonical no longer
        // matches the signature → rejected → downgrade to OpenSource (fail-closed).
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();
        license.max_agents = Some(3);
        license.signature = sign_for_test(&license, &signing_key);

        // Attacker edits the plaintext quota upward.
        license.max_agents = Some(8);

        let err = verify_license(&license, &public_key.to_bytes()).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    #[test]
    fn verify_rejects_stripped_max_agents() {
        // Removing the signed quota entirely (None) is also tampering — it must
        // not silently fall back to the tier default.
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key();
        let mut license = make_test_license();
        license.max_agents = Some(2);
        license.signature = sign_for_test(&license, &signing_key);

        license.max_agents = None;

        let err = verify_license(&license, &public_key.to_bytes()).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    fn shared_vector() -> (serde_json::Value, License, Vec<u8>) {
        let fixture: serde_json::Value = serde_json::from_str(
            include_str!("../../../tests/fixtures/license_response_v2.json")).unwrap();
        let license: License = serde_json::from_value(fixture["license"].clone()).unwrap();
        let public = hex::decode(fixture["public_key_hex"].as_str().unwrap()).unwrap();
        (fixture, license, public)
    }

    #[test]
    fn issued_vector_without_installation_pubkey_keeps_bytes_and_signature() {
        let (fixture, license, public) = shared_vector();
        assert!(license.installation_pubkey.is_none());
        assert_eq!(String::from_utf8(license.canonical_payload().unwrap()).unwrap(), concat!(
            r#"{"version":2,"subscription_id":"sub_rebind_vector","customer_id":"cus_vector","#,
            r#""tier":"self_host_pro","machine_fingerprint":"macos-uuid-v2:00112233445566778899aabbccddeeff","#,
            r#""issued_at":"2026-11-05T00:00:00Z","expires_at":"2026-12-05T00:00:00Z","#,
            r#""last_phone_home":"2026-11-06T00:00:00Z","public_key_id":"test-vector","max_agents":7}"#));
        verify_license(&license, &public).unwrap();
        // The full serialized artifact (and its response digest) is unchanged too.
        assert_eq!(crate::refresh::license_digest(&license).unwrap(), fixture["license_digest"]);
    }

    #[test]
    fn installation_pubkey_tamper_and_ignoring_the_field_both_fail_verification() {
        let (fixture, mut license, public) = shared_vector();
        license.installation_pubkey = Some("ab".repeat(32));
        assert!(matches!(verify_license(&license, &public), Err(LicenseError::InvalidSignature)));

        // Sign with the vector's test-only seed, including a key.
        let seed: [u8; 32] = hex::decode(fixture["test_only_seed_hex"].as_str().unwrap()).unwrap()
            .try_into().unwrap();
        let issuer = SigningKey::from_bytes(&seed);
        assert_eq!(issuer.verifying_key().to_bytes().to_vec(), public);
        license.signature = sign_for_test(&license, &issuer);
        verify_license(&license, &public).unwrap();
        for replacement in [Some("cd".repeat(32)), None] {
            // `None` models an older binary that deserializes the field away: its
            // canonical payload lacks the key, so the issuer must only add the key
            // for requests that themselves carried installation_pubkey.
            let mut altered = license.clone();
            altered.installation_pubkey = replacement;
            assert!(matches!(verify_license(&altered, &public), Err(LicenseError::InvalidSignature)));
        }
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let other_key = SigningKey::generate(&mut OsRng);
        let mut license = make_test_license();
        license.signature = sign_for_test(&license, &signing_key);

        let err =
            verify_license(&license, &other_key.verifying_key().to_bytes()).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidSignature));
    }

    #[test]
    fn verify_rejects_short_public_key() {
        let license = make_test_license();
        let err = verify_license(&license, &[0u8; 8]).unwrap_err();
        assert!(matches!(err, LicenseError::ParseError(_)));
    }

    #[test]
    fn registry_lookup_by_id() {
        let signing_key_v1 = SigningKey::generate(&mut OsRng);
        let signing_key_v2 = SigningKey::generate(&mut OsRng);

        let registry = PublicKeyRegistry::new()
            .with_key("v1", signing_key_v1.verifying_key().to_bytes().to_vec())
            .with_key("v2", signing_key_v2.verifying_key().to_bytes().to_vec());

        let mut license_v1 = make_test_license();
        assert_eq!(license_v1.public_key_id, "v1");
        license_v1.signature = sign_for_test(&license_v1, &signing_key_v1);
        assert!(registry.verify(&license_v1).is_ok());

        let mut license_v2 = make_test_license();
        license_v2.public_key_id = "v2".into();
        license_v2.signature = sign_for_test(&license_v2, &signing_key_v2);
        assert!(registry.verify(&license_v2).is_ok());
    }

    #[test]
    fn registry_rejects_unknown_key_id() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let registry = PublicKeyRegistry::new()
            .with_key("v1", signing_key.verifying_key().to_bytes().to_vec());

        let mut license = make_test_license();
        license.public_key_id = "v99".into();
        license.signature = sign_for_test(&license, &signing_key);

        let err = registry.verify(&license).unwrap_err();
        assert!(matches!(err, LicenseError::UnknownPublicKeyId(id) if id == "v99"));
    }

    #[test]
    fn registry_default_is_empty() {
        let registry = PublicKeyRegistry::new();
        assert!(registry.is_empty());
        let _ = CURRENT_SCHEMA_VERSION; // referenced to validate the import
    }
}
