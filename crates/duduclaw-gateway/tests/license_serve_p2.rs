//! P2 owner control-plane (refresh + CRL) integration tests.
//!
//! These link against the *clean* (non-test) `duduclaw-gateway` lib build and
//! exercise the public P2 surface end-to-end:
//!   - store: `get_license_by_subscription_id`, `touch_refresh`
//!   - re-sign: `resign_license_for_refresh` preserves the validity window and
//!     only advances `last_phone_home`; a mismatched issuer/verify pair fails
//!   - CRL: `sign_crl`'s bytes verify via the *client* `SignedCrl::verify`
//!   - refresh gates: `refresh_decision` revoked / fingerprint / expired / ok
//!   - per-IP rate limiting
//!
//! The equivalent inline `#[cfg(test)]` unit tests exist too, but the crate's
//! lib-test target is currently blocked by unrelated uncommitted WIP; this
//! integration target compiles cleanly and gives the live green signal.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

use duduclaw_gateway::distributor_store::{
    issue_signed_oem_license, resign_license_for_refresh, DistributorInput, DistributorStore,
    IssuedLicense,
};
use duduclaw_gateway::license_serve::{
    refresh_decision, sign_crl, within_rate_limit, RefreshDecision,
};
use duduclaw_license::{generate_fingerprint, License, PublicKeyRegistry, SignedCrl};

/// A throwaway Ed25519 issuer keypair, trusted under key id "v2".
fn issuer_pair() -> ([u8; 32], PublicKeyRegistry) {
    let signing = SigningKey::generate(&mut OsRng);
    let seed = signing.to_bytes();
    let registry =
        PublicKeyRegistry::new().with_key("v2", signing.verifying_key().to_bytes().to_vec());
    (seed, registry)
}

fn make_store() -> (tempfile::TempDir, DistributorStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("distributor.db");
    (dir, DistributorStore::new(&path))
}

#[test]
fn store_get_by_subscription_id_and_touch_refresh() {
    let (_dir, store) = make_store();
    let did = store
        .add_distributor(&DistributorInput {
            name: "Acme".into(),
            contact: None,
            note: None,
        })
        .unwrap();
    store
        .add_license(&IssuedLicense {
            id: "lic-1".into(),
            distributor_id: did,
            subscription_id: "dist-acme-abc123".into(),
            customer_id: "dist-acme".into(),
            tier: "oem".into(),
            machine_fingerprint: "fp".into(),
            issued_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + Duration::days(365)).to_rfc3339(),
            status: "active".into(),
            revoked_at: None,
            license_blob: "blob".into(),
            last_refresh_at: None,
        })
        .unwrap();

    let found = store
        .get_license_by_subscription_id("dist-acme-abc123")
        .expect("lookup by subscription id");
    assert_eq!(found.id, "lic-1");
    assert!(found.last_refresh_at.is_none());
    assert!(store.get_license_by_subscription_id("missing").is_none());

    store.touch_refresh("lic-1").unwrap();
    assert!(store
        .get_license("lic-1")
        .unwrap()
        .last_refresh_at
        .is_some());
}

#[test]
fn resign_preserves_validity_and_reverifies() {
    let (seed, registry) = issuer_pair();
    let fp = generate_fingerprint();
    let (original, blob) =
        issue_signed_oem_license(&seed, &registry, "v2", "dist-r-1", "dist-r", &fp, 365, None, None)
            .unwrap();

    let rec = IssuedLicense {
        id: "lic-r".into(),
        distributor_id: "d".into(),
        subscription_id: "dist-r-1".into(),
        customer_id: "dist-r".into(),
        tier: "oem".into(),
        machine_fingerprint: fp.clone(),
        issued_at: original.issued_at.to_rfc3339(),
        expires_at: original.expires_at.to_rfc3339(),
        status: "active".into(),
        revoked_at: None,
        license_blob: blob,
        last_refresh_at: None,
    };

    let (resigned, new_blob) = resign_license_for_refresh(&seed, &registry, &rec).unwrap();
    // No extension: window + identity preserved to the second.
    assert_eq!(resigned.issued_at, original.issued_at);
    assert_eq!(resigned.expires_at, original.expires_at);
    assert_eq!(resigned.subscription_id, "dist-r-1");
    assert_eq!(resigned.machine_fingerprint, fp);
    assert_eq!(resigned.public_key_id, "v2");
    assert!(resigned.last_phone_home >= original.last_phone_home);

    // Re-signed blob round-trips and re-verifies against the trusting registry.
    let decoded = BASE64.decode(new_blob.trim()).unwrap();
    let parsed: License = serde_json::from_slice(&decoded).unwrap();
    assert!(registry.verify(&parsed).is_ok());
}

#[test]
fn resign_fails_for_mismatched_issuer_key() {
    let (seed, _registry) = issuer_pair();
    // Trust a DIFFERENT key under "v2".
    let (_other_seed, other_registry) = issuer_pair();
    let rec = IssuedLicense {
        id: "lic-m".into(),
        distributor_id: "d".into(),
        subscription_id: "dist-m-1".into(),
        customer_id: "dist-m".into(),
        tier: "oem".into(),
        machine_fingerprint: "fp".into(),
        issued_at: Utc::now().to_rfc3339(),
        expires_at: (Utc::now() + Duration::days(365)).to_rfc3339(),
        status: "active".into(),
        revoked_at: None,
        license_blob: "blob".into(),
        last_refresh_at: None,
    };
    assert!(resign_license_for_refresh(&seed, &other_registry, &rec).is_err());
}

#[test]
fn crl_signature_verifies_via_client() {
    let (seed, registry) = issuer_pair();
    let generated_at = Utc::now();
    let revoked = vec!["dist-a-1".to_string(), "dist-b-2".to_string()];
    let ttl = 7 * 24 * 60 * 60;
    let sig = sign_crl(generated_at, &revoked, ttl, "v2", &seed).unwrap();

    let crl = SignedCrl {
        generated_at,
        revoked: revoked.clone(),
        ttl_seconds: ttl,
        public_key_id: "v2".to_string(),
        signature: BASE64.encode(sig),
    };
    // Byte-level payload alignment proven by the client verifier accepting it.
    assert!(crl.verify(&registry).is_ok());
    assert!(crl.is_revoked("dist-a-1"));
    assert!(!crl.is_revoked("dist-z-9"));

    // Tampering with the list breaks the signature.
    let mut tampered = crl;
    tampered.revoked.push("dist-injected".into());
    assert!(tampered.verify(&registry).is_err());
}

#[test]
fn refresh_decision_gates() {
    let future = (Utc::now() + Duration::days(30)).to_rfc3339();
    let past = (Utc::now() - Duration::days(1)).to_rfc3339();

    let base = |status: &str, fp: &str, expires: &str| IssuedLicense {
        id: "lic".into(),
        distributor_id: "d".into(),
        subscription_id: "dist-x-1".into(),
        customer_id: "dist-x".into(),
        tier: "oem".into(),
        machine_fingerprint: fp.into(),
        issued_at: Utc::now().to_rfc3339(),
        expires_at: expires.into(),
        status: status.into(),
        revoked_at: if status == "revoked" {
            Some("2026-07-01T00:00:00+00:00".into())
        } else {
            None
        },
        license_blob: "blob".into(),
        last_refresh_at: None,
    };

    // Revoked wins even with a wrong fingerprint.
    let revoked = base("revoked", "fp-a", &future);
    assert_eq!(
        refresh_decision(&revoked, "fp-WRONG", Utc::now()),
        RefreshDecision::Revoked {
            effective_from: "2026-07-01T00:00:00+00:00".into()
        }
    );

    // Fingerprint mismatch → forbidden.
    let active = base("active", "fp-a", &future);
    assert_eq!(
        refresh_decision(&active, "fp-b", Utc::now()),
        RefreshDecision::Forbidden
    );

    // Expired → forbidden.
    let expired = base("active", "fp-a", &past);
    assert_eq!(
        refresh_decision(&expired, "fp-a", Utc::now()),
        RefreshDecision::Forbidden
    );

    // Valid → proceed.
    assert_eq!(
        refresh_decision(&active, "fp-a", Utc::now()),
        RefreshDecision::Proceed
    );
}

#[test]
fn rate_limiter_trips_after_max() {
    let limiter: Mutex<HashMap<IpAddr, (Instant, u32)>> = Mutex::new(HashMap::new());
    let ip: IpAddr = "203.0.113.5".parse().unwrap();
    for _ in 0..3 {
        assert!(within_rate_limit(&limiter, ip, 3));
    }
    assert!(!within_rate_limit(&limiter, ip, 3));
    // A different IP is unaffected.
    let other: IpAddr = "203.0.113.6".parse().unwrap();
    assert!(within_rate_limit(&limiter, other, 3));
}
