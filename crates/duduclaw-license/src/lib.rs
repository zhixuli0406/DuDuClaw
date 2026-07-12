//! DuDuClaw license client (Apache 2.0).
//!
//! This crate is the **open-source** half of the license module:
//!
//! - Parses license files (schema v2, JSON)
//! - Verifies Ed25519 signatures against embedded issuer public keys
//! - Computes machine fingerprints (`SHA-256(hostname::MAC)[..16]`)
//! - Loads `features.toml` and gates commercial feature flags by tier
//! - Persists license files atomically to `~/.duduclaw/license.json`
//!
//! **Signing** licenses lives in the closed-source `commercial/duduclaw-license`
//! crate, which carries the issuer private key and re-uses the types defined
//! here. The two halves communicate only through the on-wire JSON schema.
//!
//! ## Why the split
//!
//! A binary that runs on a customer machine never needs to sign a license —
//! it only needs to verify one. Putting the verifier in OSS:
//!
//! - lets Apache 2.0 forks self-issue their own licenses for internal use
//! - keeps the signing key + control-plane code commercially protected
//! - makes the customer-facing CLI (`duduclaw license activate / status / refresh / …`)
//!   work without any closed-source dependency
//!
//! See `commercial/docs/spec-license-module.md` for the full design rationale.

pub mod bundle;
pub mod crl;
pub mod error;
pub mod fingerprint;
pub mod gate;
pub mod key;
pub mod license;
pub mod storage;
pub mod tier;

// Re-export primary types for ergonomic use.
pub use bundle::{
    sign_bundle, verify_bundle, BrandingBundle, BrandingConfig, BUNDLE_KEY_ID, BUNDLE_SCHEMA,
};
pub use crl::SignedCrl;
pub use error::{LicenseError, Result};
pub use fingerprint::generate_fingerprint;
pub use gate::FeatureGate;
pub use key::{verify_license, PublicKeyRegistry};
pub use license::{License, CURRENT_SCHEMA_VERSION};
pub use storage::{
    default_license_path, delete_default, license_dir, load_default, load_from, save_default,
    save_to,
};
pub use tier::LicenseTier;

/// The shipped `features.toml` contents, embedded at compile time.
///
/// Callers that don't want to read from disk can use this together with
/// [`FeatureGate::from_str`] to get a gate built from the canonical manifest:
///
/// ```ignore
/// let gate = duduclaw_license::FeatureGate::from_str(
///     duduclaw_license::EMBEDDED_FEATURES_TOML
/// ).unwrap();
/// ```
pub const EMBEDDED_FEATURES_TOML: &str = include_str!("../features.toml");

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn embedded_features_toml_is_loadable() {
        let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();

        // Sanity checks against the v2 manifest
        assert!(!gate.check(LicenseTier::OpenSource, "premium_templates"));
        assert!(gate.check(LicenseTier::Studio, "premium_templates"));
        assert!(gate.check(LicenseTier::SelfHostPro, "dashboard_enterprise"));
        assert!(gate.check(LicenseTier::Business, "odoo_integration_supported"));
        assert!(gate.check(LicenseTier::Oem, "white_label"));
    }
}
