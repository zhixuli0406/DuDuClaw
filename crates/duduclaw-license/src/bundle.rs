//! Canonical white-label **branding bundle** format + signing/verification.
//!
//! This is the shared, byte-stable source of truth for the distributor
//! branding bundle. It lives in the OSS `duduclaw-license` crate — rather than
//! in either the gateway or the cloud control-plane — so that **every** signer
//! (gateway owner-offline signing, cloud control-plane signing) and the sole
//! verifier (the customer instance, via the gateway) compute *byte-identical*
//! canonical bytes over the same struct definitions. If the format lived in two
//! places it could silently drift and a cloud-signed bundle would fail to
//! verify on a downstream instance.
//!
//! What lives here (format + crypto only, no policy):
//! - [`BrandingConfig`] — the serializable branding data (field order + serde
//!   attributes are the wire format; do not reorder).
//! - [`BrandingBundle`] — a signed envelope wrapping a `BrandingConfig`.
//! - [`sign_bundle`] / [`verify_bundle`] — Ed25519 over the canonical payload.
//!
//! What does **not** live here (stays with each caller): input validation /
//! HTML sanitization (ammonia), persistence, the immutable vendor block, and
//! the resolution order (local file > bundle > default). Those are policy and
//! are duplicated deliberately at each edge — the *format*, however, is single.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};

use crate::key::PublicKeyRegistry;

/// The persisted / returned branding record. Every field is optional; `None`
/// means "fall back to the DuDuClaw default". There is intentionally **no**
/// vendor field here — the vendor block is a separate const-assembled response
/// section owned by each front-end.
///
/// The field order and serde attributes are the on-wire format and are covered
/// by [`sign_bundle`] — **do not reorder or change `serde` attributes**, or a
/// bundle signed by one build will fail to verify on another.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrandingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_data_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Rich HTML "about the distributor" block. Always stored **already
    /// sanitized**: each front-end runs it through `ammonia` before signing /
    /// on read. The format layer never sanitizes — it only carries bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_html: Option<String>,
    /// Brand accent colour as `#rrggbb`. The dashboard derives the
    /// primary/accent CSS scale from this single hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Current bundle schema version. A bundle with a different `schema` is
/// rejected (fail-closed) so a future incompatible format is never applied.
pub const BUNDLE_SCHEMA: u32 = 1;

/// The issuer key id bundles are signed under (pinned to the baked v2 key).
pub const BUNDLE_KEY_ID: &str = "v2";

/// A distributor-signed branding bundle. Dropped into `~/.duduclaw/` it applies
/// automatically once its signature verifies against the baked issuer key — no
/// license required (the signature *is* the authorization to *display* a brand;
/// *editing* still needs the white_label license).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrandingBundle {
    pub schema: u32,
    pub distributor_id: String,
    pub subscription_id: String,
    pub branding: BrandingConfig,
    pub issued_at: String,
    pub public_key_id: String,
    /// base64 Ed25519 signature over [`BundlePayload`].
    pub signature: String,
}

/// Canonical signing payload — field ORDER is the wire format (mirrors the
/// `CrlPayload` convention). Everything except `signature`. Do not reorder.
#[derive(Serialize)]
struct BundlePayload<'a> {
    schema: u32,
    distributor_id: &'a str,
    subscription_id: &'a str,
    branding: &'a BrandingConfig,
    issued_at: &'a str,
    public_key_id: &'a str,
}

fn bundle_canonical_bytes(
    schema: u32,
    distributor_id: &str,
    subscription_id: &str,
    branding: &BrandingConfig,
    issued_at: &str,
    public_key_id: &str,
) -> Result<Vec<u8>, String> {
    let payload = BundlePayload {
        schema,
        distributor_id,
        subscription_id,
        branding,
        issued_at,
        public_key_id,
    };
    serde_json::to_vec(&payload).map_err(|e| format!("serialize bundle payload: {e}"))
}

/// Sign a branding bundle with the issuer seed. Pure + side-effect-free so the
/// round-trip is unit-testable with a throwaway keypair. `branding` is signed
/// AS GIVEN — the caller is responsible for sanitizing it first (the gateway
/// owner-side and the cloud control-plane both do, using an identical ammonia
/// allowlist).
pub fn sign_bundle(
    signing_seed: &[u8; 32],
    distributor_id: &str,
    subscription_id: &str,
    branding: &BrandingConfig,
    issued_at: &str,
    public_key_id: &str,
) -> Result<BrandingBundle, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let canonical = bundle_canonical_bytes(
        BUNDLE_SCHEMA,
        distributor_id,
        subscription_id,
        branding,
        issued_at,
        public_key_id,
    )?;
    let key = SigningKey::from_bytes(signing_seed);
    let sig = key.sign(&canonical).to_bytes().to_vec();
    Ok(BrandingBundle {
        schema: BUNDLE_SCHEMA,
        distributor_id: distributor_id.to_string(),
        subscription_id: subscription_id.to_string(),
        branding: branding.clone(),
        issued_at: issued_at.to_string(),
        public_key_id: public_key_id.to_string(),
        signature: BASE64.encode(sig),
    })
}

/// Verify a bundle against a public-key registry. Fail-closed: wrong schema,
/// unknown key id, malformed signature, or a signature mismatch all → `Err`.
pub fn verify_bundle(bundle: &BrandingBundle, registry: &PublicKeyRegistry) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    if bundle.schema != BUNDLE_SCHEMA {
        return Err(format!(
            "unsupported bundle schema {} (expected {BUNDLE_SCHEMA})",
            bundle.schema
        ));
    }
    let key_bytes = registry
        .get(&bundle.public_key_id)
        .ok_or_else(|| format!("unknown bundle public_key_id '{}'", bundle.public_key_id))?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| "bundle public key must be 32 bytes".to_string())?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_array).map_err(|e| format!("invalid public key: {e}"))?;

    let sig_bytes = BASE64
        .decode(bundle.signature.trim())
        .map_err(|_| "bundle signature is not valid base64".to_string())?;
    let sig_array: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "bundle signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_array);

    let canonical = bundle_canonical_bytes(
        bundle.schema,
        &bundle.distributor_id,
        &bundle.subscription_id,
        &bundle.branding,
        &bundle.issued_at,
        &bundle.public_key_id,
    )?;
    verifying_key
        .verify(&canonical, &signature)
        .map_err(|_| "bundle signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn throwaway_registry() -> (PublicKeyRegistry, [u8; 32]) {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;
        let signing = SigningKey::generate(&mut OsRng);
        let seed: [u8; 32] = signing.to_bytes();
        let registry =
            PublicKeyRegistry::new().with_key("v2", signing.verifying_key().to_bytes().to_vec());
        (registry, seed)
    }

    fn sample_branding() -> BrandingConfig {
        BrandingConfig {
            product_name: Some("Acme 智慧助理".to_string()),
            accent_color: Some("#123456".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn bundle_sign_verify_roundtrip() {
        let (registry, seed) = throwaway_registry();
        let bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-12T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        assert!(verify_bundle(&bundle, &registry).is_ok());
    }

    #[test]
    fn bundle_tamper_is_rejected() {
        let (registry, seed) = throwaway_registry();
        let mut bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-12T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        bundle.branding.product_name = Some("Evil Corp".to_string());
        assert!(verify_bundle(&bundle, &registry).is_err());
    }

    #[test]
    fn bundle_wrong_key_and_schema_rejected() {
        let (_registry, seed) = throwaway_registry();
        let (other_registry, _other_seed) = throwaway_registry();
        let bundle = sign_bundle(
            &seed,
            "d",
            "d-sub",
            &sample_branding(),
            "2026-07-12T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        assert!(verify_bundle(&bundle, &other_registry).is_err());
    }
}
