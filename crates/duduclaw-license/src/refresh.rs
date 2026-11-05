//! Request-bound v2 refresh/rebind envelopes. No unsigned or v1 fallback.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use crate::{License, LicenseError, PublicKeyRegistry, Result};

/// Digest the complete serialized artifact, including signature and control_url.
pub fn license_digest(license: &License) -> Result<String> {
    let encoded = serde_json::to_vec(license)
        .map_err(|e| LicenseError::ParseError(e.to_string()))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

/// Canonical UTF-8 compact JSON array shared with the issuer. Empty digest means
/// no license artifact (pending/revoked); status, reason and time are signed too.
#[allow(clippy::too_many_arguments)]
pub fn response_payload(status: &str, reason: &str, effective_from: &str,
    nonce: &str, subscription: &str, original_binding: &str,
    proposed_binding: &str, digest: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!([
        "duduclaw:license-response:v2", 2, status, reason, effective_from,
        nonce, subscription, original_binding, proposed_binding, digest,
    ])).map_err(|e| LicenseError::ParseError(e.to_string()))
}

fn field<'a>(body: &'a serde_json::Value, name: &str) -> Result<&'a str> {
    body.get(name).and_then(|v| v.as_str())
        .ok_or_else(|| LicenseError::ParseError(format!("response missing string {name}")))
}

#[allow(clippy::too_many_arguments)]
fn verify_envelope(registry: &PublicKeyRegistry, body: &serde_json::Value,
    status: &str, nonce: &str, subscription: &str, original: &str, proposed: &str,
    digest: &str) -> Result<()> {
    if body.get("response_signature_version").and_then(|v| v.as_u64()) != Some(2) {
        return Err(LicenseError::ParseError("response signature version must be 2".into()));
    }
    if nonce.is_empty() || field(body, "nonce")? != nonce
        || field(body, "status")? != status
        || field(body, "subscription_id")? != subscription
        || field(body, "original_fingerprint")? != original
        || field(body, "proposed_fingerprint")? != proposed
        || field(body, "license_digest")? != digest {
        return Err(LicenseError::ParseError("response request identity mismatch".into()));
    }
    let reason = field(body, "reason")?;
    let effective = field(body, "effective_from")?;
    let time = chrono::DateTime::parse_from_rfc3339(effective)
        .map_err(|_| LicenseError::ParseError("invalid response effective time".into()))?;
    if status == "revoked" && time > chrono::Utc::now() {
        return Err(LicenseError::ParseError("revocation is not effective yet".into()));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(field(body, "response_signature")?)
        .map_err(|_| LicenseError::InvalidSignature)?;
    let signature = Signature::from_slice(&bytes).map_err(|_| LicenseError::InvalidSignature)?;
    let key: [u8; 32] = registry.get(field(body, "public_key_id")?)
        .ok_or(LicenseError::InvalidSignature)?.try_into()
        .map_err(|_| LicenseError::InvalidSignature)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| LicenseError::InvalidSignature)?;
    key.verify(&response_payload(status, reason, effective, nonce, subscription,
        original, proposed, digest)?, &signature).map_err(|_| LicenseError::InvalidSignature)
}

/// Authenticate an active response before any file/state update.
#[allow(clippy::too_many_arguments)]
pub fn validate_response(registry: &PublicKeyRegistry, license: &License,
    body: &serde_json::Value, nonce: &str, subscription: &str,
    original_binding: &str, proposed_binding: &str) -> Result<()> {
    registry.verify(license)?;
    if license.subscription_id != subscription {
        return Err(LicenseError::ParseError("response subscription mismatch".into()));
    }
    if license.machine_fingerprint != original_binding && license.machine_fingerprint != proposed_binding {
        return Err(LicenseError::InvalidFingerprint);
    }
    if field(body, "public_key_id")? != license.public_key_id {
        return Err(LicenseError::InvalidSignature);
    }
    verify_envelope(registry, body, "active", nonce, subscription, original_binding,
        proposed_binding, &license_digest(license)?)
}

/// Authenticate a response without an artifact. Callers must still fence the
/// installed file against concurrent changes before applying a revocation.
#[allow(clippy::too_many_arguments)]
pub fn validate_status_response(registry: &PublicKeyRegistry, body: &serde_json::Value,
    status: &str, nonce: &str, subscription: &str, original_binding: &str,
    proposed_binding: &str) -> Result<()> {
    if !matches!(status, "pending" | "revoked") || body.get("license").is_some() {
        return Err(LicenseError::ParseError("invalid status-only response".into()));
    }
    verify_envelope(registry, body, status, nonce, subscription, original_binding, proposed_binding, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn fixture() -> (SigningKey, PublicKeyRegistry, License) {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        let registry = PublicKeyRegistry::new().with_key("test", key.verifying_key().to_bytes());
        let mut license = License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
            "old", chrono::Duration::days(30), "test");
        license.signature = key.sign(&license.canonical_payload().unwrap()).to_bytes().to_vec();
        (key, registry, license)
    }

    fn signed(key: &SigningKey, license: Option<&License>, status: &str) -> serde_json::Value {
        let digest = license.map(license_digest).transpose().unwrap().unwrap_or_default();
        let reason = if status == "revoked" { "cancelled" } else { "" };
        let effective = "2026-01-01T00:00:00Z";
        let signature = key.sign(&response_payload(status, reason, effective,
            "n", "sub", "old", "new", &digest).unwrap());
        serde_json::json!({"status":status, "reason":reason, "effective_from":effective,
            "nonce":"n", "subscription_id":"sub", "original_fingerprint":"old",
            "proposed_fingerprint":"new", "license_digest":digest, "public_key_id":"test",
            "response_signature_version":2,
            "response_signature":base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())})
    }

    #[test]
    fn shared_issuer_vector_verifies_active_revoked_and_pending() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/license_response_v2.json")).unwrap();
        let registry = PublicKeyRegistry::new().with_key("test-vector",
            hex::decode(fixture["public_key_hex"].as_str().unwrap()).unwrap());
        let license: License = serde_json::from_value(fixture["license"].clone()).unwrap();
        let nonce = fixture["nonce"].as_str().unwrap();
        let sub = fixture["subscription"].as_str().unwrap();
        let old = fixture["original_binding"].as_str().unwrap();
        let new = fixture["proposed_binding"].as_str().unwrap();
        validate_response(&registry, &license, &fixture, nonce, sub, old, new).unwrap();
        let payload = response_payload("active", fixture["reason"].as_str().unwrap(),
            fixture["effective_from"].as_str().unwrap(), nonce, sub, old, new,
            &license_digest(&license).unwrap()).unwrap();
        assert_eq!(String::from_utf8(payload).unwrap(), fixture["canonical_payload"]);
        for body in fixture["status_vectors"].as_array().unwrap() {
            validate_status_response(&registry, body, body["status"].as_str().unwrap(), nonce, sub, old, new).unwrap();
        }
    }

    #[test]
    fn future_signed_revocation_is_not_applied_early() {
        let (key, registry, _) = fixture();
        let mut body = signed(&key, None, "revoked");
        let effective = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        body["effective_from"] = serde_json::json!(effective);
        let signature = key.sign(&response_payload("revoked", "cancelled", &effective,
            "n", "sub", "old", "new", "").unwrap());
        body["response_signature"] = serde_json::json!(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()));
        assert!(validate_status_response(&registry, &body, "revoked", "n", "sub", "old", "new").is_err());
    }

    #[test]
    fn response_v2_binds_nonce_request_and_full_license() {
        let (key, registry, license) = fixture();
        let body = signed(&key, Some(&license), "active");
        let verify = |l: &License, b: &serde_json::Value| validate_response(&registry, l, b, "n", "sub", "old", "new");
        assert!(verify(&license, &body).is_ok());
        for name in ["status", "reason", "effective_from", "nonce", "subscription_id",
            "original_fingerprint", "proposed_fingerprint", "license_digest", "public_key_id"] {
            let mut altered = body.clone();
            altered[name] = serde_json::json!("altered");
            assert!(verify(&license, &altered).is_err(), "{name}");
        }
        let mut changed = license.clone();
        changed.control_url = Some("https://other.invalid".into());
        assert!(verify(&changed, &body).is_err());
        changed = license.clone();
        changed.max_agents = Some(999);
        changed.signature = key.sign(&changed.canonical_payload().unwrap()).to_bytes().to_vec();
        assert!(verify(&changed, &body).is_err());
        assert!(validate_response(&registry, &license, &body, "n", "sub", "old", "other").is_err());
        let mut replay = body.clone();
        replay["nonce"] = serde_json::json!("fresh");
        assert!(validate_response(&registry, &license, &replay, "fresh", "sub", "old", "new").is_err());
    }

    #[test]
    fn every_status_rejects_missing_v1_and_malformed_versions() {
        let (key, registry, license) = fixture();
        for status in ["active", "revoked", "pending"] {
            let body = signed(&key, (status == "active").then_some(&license), status);
            for version in [None, Some(serde_json::json!(1)), Some(serde_json::json!(3)),
                Some(serde_json::json!(null)), Some(serde_json::json!("2"))] {
                let mut altered = body.clone();
                if let Some(version) = version { altered["response_signature_version"] = version; }
                else { altered.as_object_mut().unwrap().remove("response_signature_version"); }
                let result = if status == "active" {
                    validate_response(&registry, &license, &altered, "n", "sub", "old", "new")
                } else {
                    validate_status_response(&registry, &altered, status, "n", "sub", "old", "new")
                };
                assert!(result.is_err(), "{status}");
            }
        }
    }

    #[test]
    fn revoked_and_pending_require_signed_identity_reason_and_effective_time() {
        let (key, registry, _) = fixture();
        for status in ["revoked", "pending"] {
            let body = signed(&key, None, status);
            let verify = |b: &serde_json::Value| validate_status_response(&registry, b, status, "n", "sub", "old", "new");
            assert!(verify(&body).is_ok());
            for name in ["reason", "effective_from", "status", "nonce", "subscription_id",
                "original_fingerprint", "proposed_fingerprint", "license_digest", "response_signature"] {
                let mut changed = body.clone();
                changed[name] = serde_json::json!("forged");
                assert!(verify(&changed).is_err(), "{status}/{name}");
                changed.as_object_mut().unwrap().remove(name);
                assert!(verify(&changed).is_err());
            }
            assert!(verify(&serde_json::json!({"status":status})).is_err());
        }
    }
}
