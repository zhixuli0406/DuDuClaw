use super::*;
use super::document::{BRAND_KIND, SignedDistributorBrand, MAX_LOGO_BYTES};
use super::runtime::{accept, interpret, FeedOutcome};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use duduclaw_license::{LicenseTier, PublicKeyRegistry};
use ed25519_dalek::{Signer, SigningKey};

pub(super) fn setup() -> (SigningKey, PublicKeyRegistry) {
    let key = SigningKey::from_bytes(&[7; 32]);
    let registry = PublicKeyRegistry::new().with_key("v1", key.verifying_key().to_bytes().to_vec());
    (key, registry)
}

fn sign(key: &SigningKey, mut doc: SignedDistributorBrand) -> SignedDistributorBrand {
    doc.signature = BASE64.encode(key.sign(&doc.canonical_bytes().unwrap()).to_bytes());
    doc
}

pub(super) fn doc(key: &SigningKey, version: i64) -> SignedDistributorBrand {
    sign(key, SignedDistributorBrand {
        kind: BRAND_KIND.into(), subscription_id: "sub_1".into(), distributor_id: "dist_1".into(),
        version, product_name: "範例 ** Brand".into(),
        logo_light: "data:image/png;base64,iVBORw0KGgo=".into(), logo_dark: None,
        issued_at: chrono::DateTime::parse_from_rfc3339("2026-10-31T00:00:00Z").unwrap().with_timezone(&chrono::Utc),
        nonce: "n1".into(), public_key_id: "v1".into(), signature: String::new(),
    })
}

#[test]
fn only_oem_and_bound_subscription_can_use_a_verified_brand() {
    let (key, registry) = setup();
    let accepted = accept(&registry, "sub_1", &doc(&key, 2), None).unwrap();
    assert!(effective_for(LicenseTier::Oem, Some("sub_1"), &accepted).is_some());
    for tier in [LicenseTier::OpenSource, LicenseTier::Studio, LicenseTier::Business] {
        assert_eq!(effective_for(tier, Some("sub_1"), &accepted), None);
    }
    assert_eq!(effective_for(LicenseTier::Oem, Some("sub_2"), &accepted), None);
    assert_eq!(effective_for(LicenseTier::Oem, None, &accepted), None);
}

#[test]
fn bad_signature_name_svg_magic_and_oversize_refuse_the_whole_document() {
    let (key, registry) = setup();
    let good = doc(&key, 1);
    let mut tampered = good.clone(); tampered.product_name = "Forged".into();
    assert!(accept(&registry, "sub_1", &tampered, None).is_err());
    for name in ["", " ", "a\nb", "<b>", "https://example.com", "www.example.com", "a\u{2028}b", "mailto:a@b.com"] {
        let mut bad = good.clone(); bad.product_name = name.into();
        assert!(accept(&registry, "sub_1", &sign(&key, bad), None).is_err(), "{name:?}");
    }
    let mut long = good.clone(); long.product_name = "字".repeat(41);
    assert!(accept(&registry, "sub_1", &sign(&key, long), None).is_err());
    for uri in ["data:image/svg+xml;base64,PHN2Zz4=", "data:image/png;base64,PHN2Zz4="] {
        let mut bad = good.clone(); bad.logo_dark = Some(uri.into());
        assert!(accept(&registry, "sub_1", &sign(&key, bad), None).is_err());
    }
    let mut bad = good.clone(); bad.logo_light = format!("data:image/png;base64,{}", BASE64.encode(vec![0u8; MAX_LOGO_BYTES + 1]));
    assert!(accept(&registry, "sub_1", &sign(&key, bad), None).is_err());
    let mut forty = good; forty.product_name = "字".repeat(40);
    assert!(accept(&registry, "sub_1", &sign(&key, forty), None).is_ok());
}

#[test]
fn rollback_subscription_unknown_key_and_request_replay_are_refused() {
    let (key, registry) = setup();
    let current = accept(&registry, "sub_1", &doc(&key, 5), None).unwrap();
    assert!(accept(&registry, "sub_1", &doc(&key, 4), Some(&current)).is_err());
    assert!(accept(&registry, "sub_2", &doc(&key, 6), None).is_err());
    let mut unknown = doc(&key, 6); unknown.public_key_id = "untrusted".into();
    assert!(accept(&registry, "sub_1", &sign(&key, unknown), None).is_err());
    let body = serde_json::json!({"nonce":"n1", "distributor_brand": doc(&key, 6)});
    assert!(interpret(&registry, "sub_1", "other", &body, None).is_err());
    let replay = serde_json::json!({"nonce":"new", "distributor_brand": doc(&key, 6)});
    assert!(interpret(&registry, "sub_1", "new", &replay, None).is_err());
    assert!(matches!(interpret(&registry, "sub_1", "n1", &body, Some(&current)).unwrap().0, FeedOutcome::Accept(_)));
    assert_eq!(interpret(&registry, "sub_1", "n1", &serde_json::json!({"nonce":"n1", "distributor_brand":null}), None).unwrap().0, FeedOutcome::NoOpinion);
}

#[test]
fn absent_brand_is_null_not_a_document_with_nullable_required_strings() {
    let (key, registry) = setup();
    for field in ["product_name", "logo_light"] {
        let mut value = serde_json::to_value(doc(&key, 1)).unwrap();
        value[field] = serde_json::Value::Null;
        assert!(interpret(&registry, "sub_1", "n1", &serde_json::json!({
            "nonce": "n1", "distributor_brand": value,
        }), None).is_err());
    }
    assert_eq!(interpret(&registry, "sub_1", "n1", &serde_json::json!({
        "nonce": "n1", "distributor_brand": null,
    }), None).unwrap(), (FeedOutcome::NoOpinion, None));
}

#[test]
fn invalid_cached_document_never_becomes_a_brand() {
    let (key, registry) = setup();
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(runtime::CACHE_FILENAME);
    let good = doc(&key, 3);
    std::fs::write(&path, serde_json::to_vec(&good).unwrap()).unwrap();
    assert!(runtime::read_cached(home.path(), &registry, "sub_1").unwrap().is_some());
    let mut bad = good; bad.product_name = "Tampered".into();
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(runtime::read_cached(home.path(), &registry, "sub_1").is_err());
}

#[test]
fn channel_escaping_preserves_default_output() {
    assert_eq!(escape_channel_name("DuDuClaw"), "DuDuClaw");
    assert_eq!(escape_channel_name("A*_[]()!`~\\ &<>"), "A\\*\\_\\[\\]\\(\\)\\!\\`\\~\\\\ \\&\\<\\>");
    assert_eq!(escape_channel_name("@everyone"), "@\u{200b}everyone");
    assert_eq!(product_name(), "DuDuClaw");
}

#[test]
fn brand_revision_tracks_names_logos_and_original_fallback() {
    let (key, registry) = setup();
    let mut brand = accept(&registry, "sub_1", &doc(&key, 1), None).unwrap().brand;
    let original = revision(None);
    let first = revision(Some(&brand));
    assert_eq!(first, revision(Some(&brand.clone())));
    assert_ne!(first, original);
    brand.product_name = "Changed".into();
    assert_ne!(first, revision(Some(&brand)));
    let renamed = revision(Some(&brand));
    brand.logo_dark = Some(brand.logo_light.clone());
    assert_ne!(renamed, revision(Some(&brand)));
    assert_eq!(original, revision(None));
}

#[test]
fn shared_vectors_pin_canonical_bytes_signature_and_validation() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
    let (_, registry) = setup();
    for vector in vectors["signed_vectors"].as_array().unwrap() {
        let doc: SignedDistributorBrand = serde_json::from_value(vector["document"].clone()).unwrap();
        assert_eq!(String::from_utf8(doc.canonical_bytes().unwrap()).unwrap(), vector["canonical"].as_str().unwrap());
        assert!(accept(&registry, "sub_1", &doc, None).is_ok());
    }
    for vector in vectors["name_vectors"].as_array().unwrap() {
        assert_eq!(document::validate_name(vector["input"].as_str().unwrap()).is_ok(), vector["valid"].as_bool().unwrap());
    }
}
