//! Ordered, domain-separated wire contract, matching the notice feed envelope.
use chrono::{DateTime, Utc};
use duduclaw_license::PublicKeyRegistry;
use serde::{Deserialize, Serialize};

pub const BRAND_KIND: &str = "duduclaw.distributor_brand.v1";
pub const MAX_LOGO_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedDistributorBrand {
    pub kind: String,
    pub subscription_id: String,
    pub distributor_id: String,
    pub version: i64,
    pub product_name: String,
    pub logo_light: String,
    pub logo_dark: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub nonce: String,
    pub public_key_id: String,
    pub signature: String,
}

#[derive(Serialize)]
struct BrandPayload<'a> {
    kind: &'a str,
    subscription_id: &'a str,
    distributor_id: &'a str,
    version: i64,
    product_name: &'a str,
    logo_light: &'a str,
    logo_dark: Option<&'a str>,
    issued_at: DateTime<Utc>,
    nonce: &'a str,
    public_key_id: &'a str,
}

pub fn validate_name(name: &str) -> Result<(), String> {
    let count = name.chars().count();
    if !(1..=40).contains(&count) || name.trim().is_empty()
        || name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '\u{2028}' | '\u{2029}'))
        || name.split_whitespace().any(|part| url::Url::parse(part).is_ok())
        || name.contains("://") || name.to_ascii_lowercase().contains("www.")
    {
        return Err("invalid distributor product name".into());
    }
    Ok(())
}

impl SignedDistributorBrand {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&BrandPayload {
            kind: &self.kind, subscription_id: &self.subscription_id,
            distributor_id: &self.distributor_id, version: self.version,
            product_name: &self.product_name, logo_light: &self.logo_light,
            logo_dark: self.logo_dark.as_deref(), issued_at: self.issued_at,
            nonce: &self.nonce, public_key_id: &self.public_key_id,
        }).map_err(|e| e.to_string())
    }

    pub fn verify_for(&self, registry: &PublicKeyRegistry, subscription_id: &str) -> Result<(), String> {
        use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        if self.kind != BRAND_KIND || self.subscription_id != subscription_id || self.version < 0 {
            return Err("brand kind, subscription or version refused".into());
        }
        validate_name(&self.product_name)?;
        crate::image_data::validate_image_data_uri(&self.logo_light, MAX_LOGO_BYTES, "logo_light")?;
        if let Some(dark) = &self.logo_dark {
            crate::image_data::validate_image_data_uri(dark, MAX_LOGO_BYTES, "logo_dark")?;
        }
        let public = registry.get(&self.public_key_id).ok_or("unknown brand public key")?;
        let public: [u8; 32] = public.try_into().map_err(|_| "invalid brand public key")?;
        let key = VerifyingKey::from_bytes(&public).map_err(|e| e.to_string())?;
        let signature = BASE64.decode(&self.signature).map_err(|e| e.to_string())?;
        let signature: [u8; 64] = signature.as_slice().try_into().map_err(|_| "invalid brand signature")?;
        key.verify(&self.canonical_bytes()?, &Signature::from_bytes(&signature)).map_err(|e| e.to_string())
    }
}
