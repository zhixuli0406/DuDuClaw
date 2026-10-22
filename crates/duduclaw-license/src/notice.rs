//! Signed distributor notice (the About-page block a distributor manages in
//! the license console).
//!
//! The control-plane answers `POST /v1/license/distributor-notice` with an
//! Ed25519-signed document bound to one subscription. The gateway verifies it
//! with the same [`PublicKeyRegistry`] it trusts for licenses, so the cached
//! copy on disk cannot be edited by the end customer, and a copy made for one
//! customer does not verify for another.
//!
//! The request nonce is part of the signed bytes, so an answer recorded for
//! one request cannot be replayed to a later one; the cached copy is checked
//! without it.
//!
//! `html: None` is a signed "no notice": the distributor cleared it or never
//! saved one. It overrides a notice compiled into the build.
//!
//! The signed bytes are the JSON of [`NoticePayload`]; the first field `kind`
//! keeps them distinct from every other document this key signs (licenses,
//! CRL). Field order is part of the wire format. The signer is
//! `commercial/cloud-control-plane/src/handlers/notice_feed.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{LicenseError, Result};
use crate::key::PublicKeyRegistry;

/// Domain-separation tag, first field of the signed payload.
pub const NOTICE_KIND: &str = "duduclaw.distributor_notice.v1";

/// Largest `html` accepted before any further processing (bytes). Matches the
/// sanitizer limit on both sides.
pub const MAX_SIGNED_NOTICE_BYTES: usize = 64 * 1024;

/// A signed distributor notice as served by the control-plane.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SignedDistributorNotice {
    pub kind: String,
    pub subscription_id: String,
    pub distributor_id: String,
    /// Version number of the saved notice; `0` when none was ever saved.
    /// Monotonic per control-plane, so a client can refuse an older one.
    pub version: i64,
    /// Sanitized HTML, or `None` for "show no notice".
    pub html: Option<String>,
    /// Hex SHA-256 of `html`, `None` together with `html`.
    pub sha256: Option<String>,
    pub issued_at: DateTime<Utc>,
    /// The nonce of the request this answer was signed for.
    pub nonce: String,
    pub public_key_id: String,
    /// Base64-encoded Ed25519 signature over [`Self::canonical_bytes`].
    pub signature: String,
}

/// The signed part of [`SignedDistributorNotice`] (everything but the
/// signature, in wire order).
#[derive(Debug, Serialize)]
pub struct NoticePayload<'a> {
    pub kind: &'a str,
    pub subscription_id: &'a str,
    pub distributor_id: &'a str,
    pub version: i64,
    pub html: Option<&'a str>,
    pub sha256: Option<&'a str>,
    pub issued_at: DateTime<Utc>,
    pub nonce: &'a str,
    pub public_key_id: &'a str,
}

/// Hex SHA-256 of a notice body.
pub fn notice_sha256(html: &str) -> String {
    hex::encode(Sha256::digest(html.as_bytes()))
}

impl NoticePayload<'_> {
    /// The exact bytes that are signed and verified.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self)
            .map_err(|e| LicenseError::ParseError(format!("serialize notice payload: {e}")))
    }
}

impl SignedDistributorNotice {
    fn payload(&self) -> NoticePayload<'_> {
        NoticePayload {
            kind: &self.kind,
            subscription_id: &self.subscription_id,
            distributor_id: &self.distributor_id,
            version: self.version,
            html: self.html.as_deref(),
            sha256: self.sha256.as_deref(),
            issued_at: self.issued_at,
            nonce: &self.nonce,
            public_key_id: &self.public_key_id,
        }
    }

    /// Bytes covered by the signature.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.payload().canonical_bytes()
    }

    /// Verify everything a client must check before showing the notice:
    /// the kind tag, the binding to `subscription_id`, size, the body hash
    /// and the Ed25519 signature. Sanitizing the HTML stays with the caller.
    pub fn verify_for(&self, registry: &PublicKeyRegistry, subscription_id: &str) -> Result<()> {
        use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        if self.kind != NOTICE_KIND {
            return Err(LicenseError::ParseError(format!(
                "unexpected notice kind '{}'",
                self.kind
            )));
        }
        if self.subscription_id != subscription_id {
            return Err(LicenseError::ParseError(
                "notice was issued for another subscription".into(),
            ));
        }
        if self.version < 0 {
            return Err(LicenseError::ParseError("negative notice version".into()));
        }
        match (&self.html, &self.sha256) {
            (Some(html), Some(sha)) => {
                if html.len() > MAX_SIGNED_NOTICE_BYTES {
                    return Err(LicenseError::ParseError("notice is too large".into()));
                }
                if !notice_sha256(html).eq_ignore_ascii_case(sha) {
                    return Err(LicenseError::ParseError("notice hash mismatch".into()));
                }
            }
            (None, None) => {}
            _ => {
                return Err(LicenseError::ParseError(
                    "notice html and sha256 must be both present or both absent".into(),
                ));
            }
        }

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
            .map_err(|e| LicenseError::ParseError(format!("decode notice signature: {e}")))?;
        let sig_array: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| LicenseError::InvalidSignature)?;
        verifying_key
            .verify(&self.canonical_bytes()?, &Signature::from_bytes(&sig_array))
            .map_err(|_| LicenseError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(key: &SigningKey, html: Option<&str>) -> SignedDistributorNotice {
        let mut notice = SignedDistributorNotice {
            kind: NOTICE_KIND.into(),
            subscription_id: "sub_a".into(),
            distributor_id: "dist_1".into(),
            version: 7,
            html: html.map(str::to_string),
            sha256: html.map(notice_sha256),
            issued_at: Utc::now(),
            nonce: "n".into(),
            public_key_id: "v1".into(),
            signature: String::new(),
        };
        let sig = key.sign(&notice.canonical_bytes().unwrap());
        notice.signature = BASE64.encode(sig.to_bytes());
        notice
    }

    fn setup() -> (SigningKey, PublicKeyRegistry) {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        let registry =
            PublicKeyRegistry::new().with_key("v1", key.verifying_key().to_bytes().to_vec());
        (key, registry)
    }

    #[test]
    fn valid_notice_and_signed_clear_verify() {
        let (key, registry) = setup();
        assert!(
            signed(&key, Some("<p>總經銷</p>"))
                .verify_for(&registry, "sub_a")
                .is_ok()
        );
        assert!(signed(&key, None).verify_for(&registry, "sub_a").is_ok());
    }

    #[test]
    fn notice_for_another_subscription_is_refused() {
        let (key, registry) = setup();
        assert!(
            signed(&key, Some("<p>x</p>"))
                .verify_for(&registry, "sub_b")
                .is_err()
        );
    }

    #[test]
    fn edited_body_hash_or_fields_are_refused() {
        let (key, registry) = setup();
        let good = signed(&key, Some("<p>x</p>"));

        let mut body_only = good.clone();
        body_only.html = Some("<p>phish</p>".into());
        assert!(body_only.verify_for(&registry, "sub_a").is_err());

        let mut body_and_hash = good.clone();
        body_and_hash.html = Some("<p>phish</p>".into());
        body_and_hash.sha256 = Some(notice_sha256("<p>phish</p>"));
        assert!(matches!(
            body_and_hash.verify_for(&registry, "sub_a"),
            Err(LicenseError::InvalidSignature)
        ));

        let mut bumped = good.clone();
        bumped.version = 99;
        assert!(bumped.verify_for(&registry, "sub_a").is_err());

        let mut renonced = good.clone();
        renonced.nonce = "other".into();
        assert!(renonced.verify_for(&registry, "sub_a").is_err());

        let mut cleared = good;
        cleared.html = None;
        cleared.sha256 = None;
        assert!(cleared.verify_for(&registry, "sub_a").is_err());
    }

    #[test]
    fn wrong_kind_unknown_key_and_half_cleared_are_refused() {
        let (key, registry) = setup();
        let mut kind = signed(&key, None);
        kind.kind = "duduclaw.crl".into();
        assert!(kind.verify_for(&registry, "sub_a").is_err());

        let mut unknown = signed(&key, None);
        unknown.public_key_id = "v9".into();
        assert!(matches!(
            unknown.verify_for(&registry, "sub_a"),
            Err(LicenseError::UnknownPublicKeyId(_))
        ));

        let mut half = signed(&key, None);
        half.sha256 = Some(notice_sha256(""));
        assert!(half.verify_for(&registry, "sub_a").is_err());
    }

    #[test]
    fn oversized_body_is_refused_before_signature_work() {
        let (key, registry) = setup();
        let big = "a".repeat(MAX_SIGNED_NOTICE_BYTES + 1);
        assert!(
            signed(&key, Some(&big))
                .verify_for(&registry, "sub_a")
                .is_err()
        );
    }

    #[test]
    fn wire_field_order_is_stable() {
        let payload = NoticePayload {
            kind: NOTICE_KIND,
            subscription_id: "s",
            distributor_id: "d",
            version: 1,
            html: None,
            sha256: None,
            issued_at: DateTime::parse_from_rfc3339("2026-10-22T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            nonce: "n",
            public_key_id: "v1",
        };
        assert_eq!(
            String::from_utf8(payload.canonical_bytes().unwrap()).unwrap(),
            r#"{"kind":"duduclaw.distributor_notice.v1","subscription_id":"s","distributor_id":"d","version":1,"html":null,"sha256":null,"issued_at":"2026-10-22T00:00:00Z","nonce":"n","public_key_id":"v1"}"#
        );
    }
}
