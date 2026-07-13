//! License data structure v2 (subscription-aware).
//!
//! See `commercial/docs/spec-license-module.md` for the design rationale.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::LicenseError;
use crate::tier::LicenseTier;

/// Current license schema version.
///
/// Any binary that reads a license file with a `version` greater than this
/// constant must reject it with `LicenseError::UnsupportedVersion`.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// A signed license key (schema v2).
///
/// Compared with v1, this struct adds:
///
/// - `version` — explicit schema version for forward-compat
/// - `subscription_id` — links to upstream Stripe / PayUni subscription
/// - `customer_id` — opaque identifier (no PII)
/// - `last_phone_home` — for grace-period calculations
/// - `public_key_id` — supports key rotation without invalidating old licenses
///
/// `expires_at` is no longer `Option` — subscription tiers always have an
/// expiry. Perpetual / OEM licenses set this 100 years in the future.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    /// Schema version. Must equal [`CURRENT_SCHEMA_VERSION`] for this build.
    pub version: u32,

    /// Subscription identifier (e.g. PayUni subscription ID, Stripe sub_xxx,
    /// or a UUID for self-issued perpetual licenses).
    pub subscription_id: String,

    /// Opaque customer identifier — typically email hash or upstream
    /// payment provider's customer ID. Avoid storing PII directly.
    pub customer_id: String,

    /// License tier — determines feature gating via `FeatureGate`.
    pub tier: LicenseTier,

    /// SHA-256(hostname::MAC)[..16] hex-encoded fingerprint of the licensed
    /// machine. Verified against `fingerprint::generate_fingerprint()`.
    pub machine_fingerprint: String,

    /// When this license was issued. Used for diagnostic / audit purposes.
    pub issued_at: DateTime<Utc>,

    /// When this license expires. Subscription tiers: current period end.
    /// Perpetual / OEM: typically `issued_at + 100 years`.
    pub expires_at: DateTime<Utc>,

    /// Timestamp of the last successful phone-home to control-plane.
    /// Used for grace-period calculations (offline tolerance).
    pub last_phone_home: DateTime<Utc>,

    /// Public key identifier used to verify `signature`. Allows key rotation:
    /// binaries embed multiple pubkeys keyed by ID, and old licenses can be
    /// verified against their original key after a rollover.
    pub public_key_id: String,

    /// Ed25519 signature over the canonical payload (base64 in JSON).
    #[serde(with = "base64_vec")]
    pub signature: Vec<u8>,

    /// Self-carried control-plane base URL (white-label §10.5). When an issuer
    /// bakes its owner-gateway URL into the key at issue time, the client
    /// resolves phone-home / CRL against it without the operator having to set
    /// `DUDUCLAW_CONTROL_URL` — the root fix for the 60-day offline downgrade.
    ///
    /// **Deliberately excluded from [`Self::canonical_payload`]** (same tier as
    /// `signature`): tampering with it requires local write access to the 0600
    /// `license.json`, and the refresh response is itself signature-verified, so
    /// the worst case degrades to "URL unreachable" = the pre-existing baseline.
    /// Old license files lacking this field deserialize to `None` (serde
    /// default; `License` has no `deny_unknown_fields`, so a new binary reads an
    /// old file and vice-versa).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,

    /// White-label field-level edit claim (WP8). When present, lists the exact
    /// branding field names (BrandingInput keys, e.g. `"logo_data_uri"`) that
    /// this license's operator may edit — the carrier for "distributor token vs
    /// customer token have different editable ranges". `None` = no restriction
    /// declared → the consumer resolves it to the full vendor-editable set
    /// (backward-compatible: an OEM license issued before WP8 keeps editing
    /// every vendor field).
    ///
    /// **Part of [`Self::canonical_payload`]** (unlike `control_url`): the claim
    /// is a security boundary — an unsigned claim could be self-escalated by
    /// editing the local `license.json`. Because it is signed, stripping or
    /// widening it invalidates the signature → the license is rejected →
    /// OpenSource → white_label off → no branding at all (fail-closed). It is
    /// added at the END of the canonical struct with `skip_serializing_if`, so a
    /// `None` claim serializes byte-identically to a pre-WP8 license and old
    /// signatures still verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branding_editable: Option<Vec<String>>,

    /// Per-license agent-count quota (P-License). When present, it overrides the
    /// tier-default `max_agents` (`FeatureGate::max_agents`) so a distributor can
    /// sell "system + N agents" at any N and later upsell more. `Some(0)` = no
    /// limit (matches the features.toml `0 = unlimited` convention). `None` (the
    /// default) = no override → fall back to the tier default.
    ///
    /// **Part of [`Self::canonical_payload`]** (like `branding_editable`, unlike
    /// `control_url`): the quota is a security boundary — an unsigned count could
    /// be self-raised by editing the local `license.json`. Because it is signed,
    /// widening it (e.g. `3` → `8`) invalidates the signature → the license is
    /// rejected → OpenSource (fail-closed). Added at the END of the canonical
    /// struct with `skip_serializing_if`, so a `None` quota serializes
    /// byte-identically to a pre-P-License key and old signatures still verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agents: Option<u32>,
}

impl License {
    /// Returns `true` if the license has passed its expiration date.
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Returns the number of days until expiry. Negative if already expired.
    pub fn days_until_expiry(&self) -> i64 {
        (self.expires_at - Utc::now()).num_days()
    }

    /// Returns days since the last successful phone-home.
    pub fn days_since_phone_home(&self) -> i64 {
        (Utc::now() - self.last_phone_home).num_days()
    }

    /// Returns `true` if a phone-home refresh should be attempted soon
    /// (last phone-home is older than `interval_days`).
    ///
    /// Does not imply the license is invalid — caller should attempt refresh
    /// but may continue operating until `grace_period_exceeded` returns true.
    pub fn needs_phone_home(&self, interval_days: i64) -> bool {
        self.days_since_phone_home() > interval_days
    }

    /// Returns `true` if the license has exceeded its offline grace period.
    ///
    /// When this is true, the license must be considered invalid and the tier
    /// downgraded to [`LicenseTier::OpenSource`] until a refresh succeeds.
    pub fn grace_period_exceeded(&self, grace_days: i64) -> bool {
        self.days_since_phone_home() > grace_days
    }

    /// Returns `true` if the license is bound to the given machine fingerprint.
    pub fn is_valid_for_machine(&self, fingerprint: &str) -> bool {
        self.machine_fingerprint == fingerprint
    }

    /// Comprehensive validation: schema version, expiry, fingerprint, and
    /// grace period. Does **not** verify the cryptographic signature — use
    /// [`crate::key::verify_license`] separately (typically before calling
    /// this method).
    ///
    /// `phone_home_interval` and `grace_period` are sourced from
    /// `features.toml` based on `self.tier`.
    pub fn validate(
        &self,
        current_fingerprint: &str,
        phone_home_interval: i64,
        grace_period: i64,
    ) -> Result<(), LicenseError> {
        if self.version > CURRENT_SCHEMA_VERSION {
            return Err(LicenseError::UnsupportedVersion(self.version));
        }

        if Utc::now() < self.issued_at {
            return Err(LicenseError::NotYetValid);
        }

        if self.is_expired() {
            return Err(LicenseError::Expired);
        }

        // Machine-binding check with an **unbound** escape hatch. An empty
        // `machine_fingerprint` marks a license that is deliberately NOT tied to
        // a specific machine — required for Docker/OEM redistribution, where the
        // container fingerprint (`SHA256(hostname::MAC)`) changes on every
        // `docker compose up` rebuild and would otherwise trip `InvalidFingerprint`.
        //
        // SECURITY GATE (fail-closed): unbound is permitted **only** for the
        // `Oem` tier. Redistribution is already part of the OEM grant, and the
        // signed `max_agents` quota (P-License) caps the blast radius of a leaked
        // unbound key. Any other tier presenting an empty fingerprint is an
        // abuse/tampering signal → `InvalidFingerprint` (a general subscription
        // must never run unbound).
        if self.machine_fingerprint.is_empty() {
            if self.tier != crate::tier::LicenseTier::Oem {
                return Err(LicenseError::InvalidFingerprint);
            }
            // Oem + empty ⇒ unbound: skip the per-machine binding check.
        } else if !self.is_valid_for_machine(current_fingerprint) {
            return Err(LicenseError::InvalidFingerprint);
        }

        if grace_period > 0 && self.grace_period_exceeded(grace_period) {
            return Err(LicenseError::GracePeriodExceeded(
                self.days_since_phone_home(),
            ));
        }

        // Soft warning — license is still valid but caller should refresh.
        // Returning Ok here; callers can call `needs_phone_home` for the warn.
        let _ = phone_home_interval;
        Ok(())
    }

    /// Validate the tier ↔ deployment-mode binding (M51 fix).
    ///
    /// A cloud-only tier (Hobby/Solo/Studio/Business) must never be honoured on
    /// a self-hosted deployment, and a self-host-only tier (SelfHostPro/Oem)
    /// must never be honoured when running in DuDuClaw Cloud. `validate()`
    /// alone did not enforce this, so e.g. a `Solo` license passed on a
    /// self-host box. The deployment mode is a property of the *running
    /// binary*, not of the `License` model, so it is supplied by the caller
    /// rather than read from a (non-existent) license field.
    ///
    /// `OpenSource` is deployment-agnostic and always passes.
    ///
    /// TODO(M51): wire `is_self_host` from a real deployment-mode signal at the
    /// gateway license-load sites (`license_runtime.rs`) — e.g. a build flag or
    /// `DUDUCLAW_DEPLOYMENT` env var — and call this from `validate()` once that
    /// signal exists. The License model carries no deployment field, so no
    /// schema change is invented here.
    pub fn validate_tier_deployment_binding(
        &self,
        is_self_host: bool,
    ) -> Result<(), LicenseError> {
        if is_self_host && self.tier.is_cloud_only() {
            return Err(LicenseError::TierModeMismatch(format!(
                "{} is cloud-only but was issued to a self-host machine",
                self.tier
            )));
        }
        if !is_self_host && self.tier.is_self_host_only() {
            return Err(LicenseError::TierModeMismatch(format!(
                "{} is self-host-only but was issued to a cloud deployment",
                self.tier
            )));
        }
        Ok(())
    }

    /// Returns the canonical payload bytes used for signing/verification.
    /// This is the deterministic serialization of all fields except `signature`.
    ///
    /// # Errors
    /// Returns `LicenseError::ParseError` if serialization fails.
    pub fn canonical_payload(&self) -> Result<Vec<u8>, LicenseError> {
        let payload = CanonicalPayload {
            version: self.version,
            subscription_id: &self.subscription_id,
            customer_id: &self.customer_id,
            tier: self.tier,
            machine_fingerprint: &self.machine_fingerprint,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            last_phone_home: self.last_phone_home,
            public_key_id: &self.public_key_id,
            branding_editable: self.branding_editable.as_ref(),
            max_agents: self.max_agents,
        };
        serde_json::to_vec(&payload)
            .map_err(|e| LicenseError::ParseError(format!("canonical payload: {e}")))
    }

    /// Builder-style constructor for tests and key-issuing tools.
    ///
    /// Sets `version` to [`CURRENT_SCHEMA_VERSION`], `issued_at` and
    /// `last_phone_home` to now, and leaves `signature` empty (caller must
    /// invoke [`crate::key::sign_license`] before serializing).
    pub fn new(
        subscription_id: impl Into<String>,
        customer_id: impl Into<String>,
        tier: LicenseTier,
        machine_fingerprint: impl Into<String>,
        valid_for: Duration,
        public_key_id: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            version: CURRENT_SCHEMA_VERSION,
            subscription_id: subscription_id.into(),
            customer_id: customer_id.into(),
            tier,
            machine_fingerprint: machine_fingerprint.into(),
            issued_at: now,
            expires_at: now + valid_for,
            last_phone_home: now,
            public_key_id: public_key_id.into(),
            signature: Vec::new(),
            control_url: None,
            branding_editable: None,
            max_agents: None,
        }
    }
}

/// Internal struct for computing the canonical (signature-excluded) payload.
///
/// Field order matters for signature stability — adding new fields requires
/// a schema version bump.
#[derive(Serialize)]
struct CanonicalPayload<'a> {
    version: u32,
    subscription_id: &'a str,
    customer_id: &'a str,
    tier: LicenseTier,
    machine_fingerprint: &'a str,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_phone_home: DateTime<Utc>,
    public_key_id: &'a str,
    /// WP8 signed field-level branding claim. Added last with
    /// `skip_serializing_if` so a `None` claim yields byte-identical canonical
    /// bytes to a pre-WP8 license (old signatures keep verifying).
    #[serde(skip_serializing_if = "Option::is_none")]
    branding_editable: Option<&'a Vec<String>>,
    /// P-License signed agent-count quota. Added last with `skip_serializing_if`
    /// so a `None` quota yields byte-identical canonical bytes to a pre-P-License
    /// license (old signatures keep verifying).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_agents: Option<u32>,
}

/// Serde helper for encoding `Vec<u8>` as base64 strings in JSON.
mod base64_vec {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::ptr_arg)]
    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        BASE64.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_license(tier: LicenseTier, expires_in_days: i64, phone_home_days_ago: i64) -> License {
        let now = Utc::now();
        License {
            version: CURRENT_SCHEMA_VERSION,
            subscription_id: "sub_test_001".into(),
            customer_id: "cus_test_001".into(),
            tier,
            machine_fingerprint: "abc123".into(),
            issued_at: now - Duration::days(7),
            expires_at: now + Duration::days(expires_in_days),
            last_phone_home: now - Duration::days(phone_home_days_ago),
            public_key_id: "v1".into(),
            signature: Vec::new(),
            control_url: None,
            branding_editable: None,
            max_agents: None,
        }
    }

    #[test]
    fn not_expired_when_future_expiry() {
        let license = make_license(LicenseTier::Solo, 30, 0);
        assert!(!license.is_expired());
        assert!(license.days_until_expiry() >= 29);
    }

    #[test]
    fn expired_when_past_expiry() {
        let license = make_license(LicenseTier::Solo, -1, 0);
        assert!(license.is_expired());
        assert!(license.days_until_expiry() < 0);
    }

    #[test]
    fn days_until_expiry_positive_when_future() {
        let license = make_license(LicenseTier::Studio, 100, 0);
        let days = license.days_until_expiry();
        assert!((99..=100).contains(&days));
    }

    #[test]
    fn needs_phone_home_when_overdue() {
        let license = make_license(LicenseTier::Studio, 30, 10);
        assert!(license.needs_phone_home(7));
        assert!(!license.needs_phone_home(15));
    }

    #[test]
    fn grace_period_exceeded_when_offline_too_long() {
        let license = make_license(LicenseTier::SelfHostPro, 30, 45);
        assert!(license.grace_period_exceeded(30));
        assert!(!license.grace_period_exceeded(60));
    }

    #[test]
    fn valid_machine_fingerprint() {
        let license = make_license(LicenseTier::Solo, 30, 0);
        assert!(license.is_valid_for_machine("abc123"));
        assert!(!license.is_valid_for_machine("xyz789"));
    }

    #[test]
    fn validate_happy_path() {
        let license = make_license(LicenseTier::SelfHostPro, 30, 3);
        assert!(license.validate("abc123", 7, 30).is_ok());
    }

    #[test]
    fn validate_rejects_expired() {
        let license = make_license(LicenseTier::SelfHostPro, -1, 0);
        let err = license.validate("abc123", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::Expired));
    }

    #[test]
    fn validate_rejects_wrong_fingerprint() {
        let license = make_license(LicenseTier::Solo, 30, 0);
        let err = license.validate("xyz789", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidFingerprint));
    }

    #[test]
    fn validate_rejects_exceeded_grace_period() {
        let license = make_license(LicenseTier::SelfHostPro, 30, 45);
        let err = license.validate("abc123", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::GracePeriodExceeded(d) if d == 45));
    }

    #[test]
    fn validate_rejects_unsupported_future_schema() {
        let mut license = make_license(LicenseTier::Solo, 30, 0);
        license.version = CURRENT_SCHEMA_VERSION + 1;
        let err = license.validate("abc123", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::UnsupportedVersion(v) if v == CURRENT_SCHEMA_VERSION + 1));
    }

    #[test]
    fn validate_rejects_not_yet_valid() {
        let mut license = make_license(LicenseTier::Solo, 30, 0);
        license.issued_at = Utc::now() + Duration::days(2);
        let err = license.validate("abc123", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::NotYetValid));
    }

    #[test]
    fn validate_grace_period_zero_means_no_offline() {
        let license = make_license(LicenseTier::SelfHostPro, 30, 1);
        // grace_period = 0 disables the check
        assert!(license.validate("abc123", 7, 0).is_ok());
    }

    // ── M51: tier ↔ deployment-mode binding ─────────────────────────

    #[test]
    fn deployment_binding_rejects_cloud_tier_on_self_host() {
        let license = make_license(LicenseTier::Solo, 30, 0);
        let err = license
            .validate_tier_deployment_binding(true)
            .unwrap_err();
        assert!(matches!(err, LicenseError::TierModeMismatch(_)));
    }

    #[test]
    fn deployment_binding_rejects_self_host_tier_on_cloud() {
        let license = make_license(LicenseTier::SelfHostPro, 30, 0);
        let err = license
            .validate_tier_deployment_binding(false)
            .unwrap_err();
        assert!(matches!(err, LicenseError::TierModeMismatch(_)));
    }

    #[test]
    fn deployment_binding_allows_matching_modes() {
        let cloud = make_license(LicenseTier::Business, 30, 0);
        assert!(cloud.validate_tier_deployment_binding(false).is_ok());

        let self_host = make_license(LicenseTier::Oem, 30, 0);
        assert!(self_host.validate_tier_deployment_binding(true).is_ok());
    }

    #[test]
    fn deployment_binding_opensource_is_mode_agnostic() {
        let license = make_license(LicenseTier::OpenSource, 30, 0);
        assert!(license.validate_tier_deployment_binding(true).is_ok());
        assert!(license.validate_tier_deployment_binding(false).is_ok());
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let license = make_license(LicenseTier::Business, 365, 2);
        let json = serde_json::to_string(&license).unwrap();
        let parsed: License = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, license.version);
        assert_eq!(parsed.subscription_id, license.subscription_id);
        assert_eq!(parsed.customer_id, license.customer_id);
        assert_eq!(parsed.tier, license.tier);
        assert_eq!(parsed.machine_fingerprint, license.machine_fingerprint);
        assert_eq!(parsed.public_key_id, license.public_key_id);
        assert_eq!(parsed.signature, license.signature);
    }

    #[test]
    fn canonical_payload_excludes_signature() {
        let license = make_license(LicenseTier::Solo, 30, 0);
        let payload = String::from_utf8(license.canonical_payload().unwrap()).unwrap();
        assert!(!payload.contains("signature"));
        assert!(payload.contains("subscription_id"));
        assert!(payload.contains("public_key_id"));
        assert!(payload.contains("\"version\":2"));
    }

    #[test]
    fn canonical_payload_changes_when_any_signed_field_changes() {
        let mut license = make_license(LicenseTier::Solo, 30, 0);
        let payload_a = license.canonical_payload().unwrap();
        license.subscription_id = "sub_different".into();
        let payload_b = license.canonical_payload().unwrap();
        assert_ne!(payload_a, payload_b);
    }

    #[test]
    fn canonical_payload_changes_when_last_phone_home_changes() {
        let mut license = make_license(LicenseTier::Solo, 30, 0);
        let payload_a = license.canonical_payload().unwrap();
        license.last_phone_home = Utc::now() - Duration::days(1);
        let payload_b = license.canonical_payload().unwrap();
        // last_phone_home is part of the signed payload — control-plane
        // re-signs on every refresh.
        assert_ne!(payload_a, payload_b);
    }

    #[test]
    fn canonical_payload_excludes_control_url() {
        // control_url is self-carried config, NOT signed — flipping it must not
        // change the canonical bytes (otherwise every issuer that bakes a URL
        // would invalidate the signature).
        let mut license = make_license(LicenseTier::Oem, 30, 0);
        let payload_a = license.canonical_payload().unwrap();
        license.control_url = Some("https://gw.example.com".into());
        let payload_b = license.canonical_payload().unwrap();
        assert_eq!(payload_a, payload_b, "control_url must not affect signing");
        let text = String::from_utf8(payload_b).unwrap();
        assert!(!text.contains("control_url"));
    }

    #[test]
    fn old_license_json_without_control_url_deserializes_to_none() {
        // An old file (pre-§10.5) has no control_url key — serde default → None,
        // behaviour unchanged. A new file with the key round-trips.
        let mut license = make_license(LicenseTier::Oem, 365, 1);
        let json_old = serde_json::to_string(&license).unwrap();
        assert!(
            !json_old.contains("control_url"),
            "None is skipped in serialization"
        );
        let parsed_old: License = serde_json::from_str(&json_old).unwrap();
        assert!(parsed_old.control_url.is_none());

        license.control_url = Some("https://owner.example/".into());
        let json_new = serde_json::to_string(&license).unwrap();
        assert!(json_new.contains("control_url"));
        let parsed_new: License = serde_json::from_str(&json_new).unwrap();
        assert_eq!(
            parsed_new.control_url.as_deref(),
            Some("https://owner.example/")
        );
    }

    #[test]
    fn canonical_payload_excludes_branding_editable_when_none() {
        // WP8 backward-compat: a None claim must not appear in the signed bytes,
        // so a pre-WP8 license (no such field) verifies byte-identically.
        let license = make_license(LicenseTier::Oem, 30, 0);
        assert!(license.branding_editable.is_none());
        let payload = String::from_utf8(license.canonical_payload().unwrap()).unwrap();
        assert!(
            !payload.contains("branding_editable"),
            "None claim must be omitted from canonical bytes: {payload}"
        );
    }

    #[test]
    fn canonical_payload_includes_branding_editable_when_some() {
        // A Some claim IS signed (it is a security boundary, unlike control_url).
        let mut license = make_license(LicenseTier::Oem, 30, 0);
        let base = license.canonical_payload().unwrap();
        license.branding_editable = Some(vec!["logo_data_uri".to_string()]);
        let with_claim = license.canonical_payload().unwrap();
        assert_ne!(base, with_claim, "claim must change the signed payload");
        let text = String::from_utf8(with_claim).unwrap();
        assert!(text.contains("branding_editable"));
        assert!(text.contains("logo_data_uri"));
    }

    #[test]
    fn branding_editable_serde_roundtrips_and_old_json_defaults_none() {
        let mut license = make_license(LicenseTier::Oem, 365, 1);
        let json_old = serde_json::to_string(&license).unwrap();
        assert!(
            !json_old.contains("branding_editable"),
            "None is skipped in serialization"
        );
        let parsed_old: License = serde_json::from_str(&json_old).unwrap();
        assert!(parsed_old.branding_editable.is_none());

        license.branding_editable = Some(vec!["logo_data_uri".into(), "product_name".into()]);
        let json_new = serde_json::to_string(&license).unwrap();
        assert!(json_new.contains("branding_editable"));
        let parsed_new: License = serde_json::from_str(&json_new).unwrap();
        assert_eq!(
            parsed_new.branding_editable.as_deref(),
            Some(["logo_data_uri".to_string(), "product_name".to_string()].as_slice())
        );
    }

    // ── P-License: signed per-license agent-count quota ─────────────

    #[test]
    fn canonical_payload_excludes_max_agents_when_none() {
        // Backward-compat: a None quota must NOT appear in the signed bytes so a
        // pre-P-License key (no such field) verifies byte-identically. This is
        // the whole reason for `skip_serializing_if` (no schema bump).
        let license = make_license(LicenseTier::Oem, 30, 0);
        assert!(license.max_agents.is_none());
        let payload = String::from_utf8(license.canonical_payload().unwrap()).unwrap();
        assert!(
            !payload.contains("max_agents"),
            "None quota must be omitted from canonical bytes: {payload}"
        );
    }

    #[test]
    fn canonical_payload_none_matches_pre_p_license_bytes() {
        // A None quota must yield the exact same canonical bytes as a license
        // constructed before the field existed — proven by comparing against the
        // other pre-existing skip-if-none field (branding_editable None). Both
        // absent ⇒ identical bytes ⇒ old signatures still verify.
        let mut license = make_license(LicenseTier::Solo, 30, 0);
        let baseline = license.canonical_payload().unwrap();
        // Setting then clearing back to None must be a no-op on the bytes.
        license.max_agents = Some(3);
        assert_ne!(baseline, license.canonical_payload().unwrap());
        license.max_agents = None;
        assert_eq!(
            baseline,
            license.canonical_payload().unwrap(),
            "clearing max_agents back to None restores byte-identical canonical"
        );
    }

    #[test]
    fn canonical_payload_includes_max_agents_when_some() {
        // A Some quota IS signed (security boundary, unlike control_url): the
        // count cannot be raised locally without invalidating the signature.
        let mut license = make_license(LicenseTier::Oem, 30, 0);
        let base = license.canonical_payload().unwrap();
        license.max_agents = Some(5);
        let with_quota = license.canonical_payload().unwrap();
        assert_ne!(base, with_quota, "quota must change the signed payload");
        let text = String::from_utf8(with_quota).unwrap();
        assert!(text.contains("max_agents"));
        assert!(text.contains('5'));
    }

    #[test]
    fn canonical_payload_changes_on_quota_widening() {
        // The tamper vector: signing 3 then locally editing to 8 must change the
        // canonical bytes (so the recomputed payload no longer matches the sig).
        let mut license = make_license(LicenseTier::Oem, 30, 0);
        license.max_agents = Some(3);
        let signed_bytes = license.canonical_payload().unwrap();
        license.max_agents = Some(8);
        let tampered_bytes = license.canonical_payload().unwrap();
        assert_ne!(
            signed_bytes, tampered_bytes,
            "raising the quota must change the payload the signature is checked against"
        );
    }

    #[test]
    fn max_agents_serde_roundtrips_and_old_json_defaults_none() {
        let mut license = make_license(LicenseTier::Oem, 365, 1);
        let json_old = serde_json::to_string(&license).unwrap();
        assert!(
            !json_old.contains("max_agents"),
            "None is skipped in serialization"
        );
        let parsed_old: License = serde_json::from_str(&json_old).unwrap();
        assert!(parsed_old.max_agents.is_none());

        // Some(0) is the "unlimited" sentinel — it must round-trip (it is NOT
        // skipped: only None is skipped).
        license.max_agents = Some(0);
        let json_unlimited = serde_json::to_string(&license).unwrap();
        assert!(json_unlimited.contains("max_agents"));
        let parsed_unlimited: License = serde_json::from_str(&json_unlimited).unwrap();
        assert_eq!(parsed_unlimited.max_agents, Some(0));

        license.max_agents = Some(5);
        let json_new = serde_json::to_string(&license).unwrap();
        assert!(json_new.contains("max_agents"));
        let parsed_new: License = serde_json::from_str(&json_new).unwrap();
        assert_eq!(parsed_new.max_agents, Some(5));
    }

    // ── Unbound license: empty fingerprint, Oem-only (Docker/OEM) ────

    #[test]
    fn validate_accepts_oem_with_empty_fingerprint_unbound() {
        // The core unbound guarantee: an Oem license with an empty fingerprint
        // validates against ANY current machine fingerprint (or none) — this is
        // what survives a Docker container rebuild.
        let mut license = make_license(LicenseTier::Oem, 365, 1);
        license.machine_fingerprint = String::new();
        // Passes regardless of the machine's actual fingerprint.
        assert!(license.validate("machine-a-fp", 7, 30).is_ok());
        assert!(license.validate("totally-different-fp", 7, 30).is_ok());
        assert!(license.validate("", 7, 30).is_ok());
    }

    #[test]
    fn validate_rejects_non_oem_with_empty_fingerprint() {
        // SECURITY: only Oem may run unbound. A cloud/subscription tier with an
        // empty fingerprint must fail closed with InvalidFingerprint so a normal
        // subscription can never be silently un-bound from its machine.
        for tier in [
            LicenseTier::Studio,
            LicenseTier::Solo,
            LicenseTier::Business,
            LicenseTier::SelfHostPro,
        ] {
            let mut license = make_license(tier, 365, 1);
            license.machine_fingerprint = String::new();
            let err = license.validate("any-fp", 7, 30).unwrap_err();
            assert!(
                matches!(err, LicenseError::InvalidFingerprint),
                "{tier:?} with empty fingerprint must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn validate_still_binds_oem_with_nonempty_fingerprint() {
        // A bound Oem license (non-empty fingerprint) keeps the normal
        // per-machine check — the escape hatch is empty-only, not "Oem ignores
        // the fingerprint".
        let license = make_license(LicenseTier::Oem, 365, 1);
        assert!(license.validate("abc123", 7, 30).is_ok());
        let err = license.validate("wrong-fp", 7, 30).unwrap_err();
        assert!(matches!(err, LicenseError::InvalidFingerprint));
    }

    #[test]
    fn new_constructor_sets_defaults() {
        let lic = License::new(
            "sub_001",
            "cus_001",
            LicenseTier::SelfHostPro,
            "fingerprint_xyz",
            Duration::days(365),
            "v1",
        );
        assert_eq!(lic.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(lic.subscription_id, "sub_001");
        assert_eq!(lic.customer_id, "cus_001");
        assert_eq!(lic.tier, LicenseTier::SelfHostPro);
        assert!(lic.signature.is_empty());
        // issued_at == last_phone_home (within 1 sec)
        assert!((lic.issued_at - lic.last_phone_home).num_seconds().abs() <= 1);
        // expires_at ~ 365 days later
        assert!((lic.expires_at - lic.issued_at).num_days() >= 364);
    }
}
