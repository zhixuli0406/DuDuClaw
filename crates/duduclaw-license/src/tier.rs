//! License tier definitions with ordering and capability classification.
//!
//! Tier hierarchy follows the v2.0 subscription model:
//!
//! ```text
//! OpenSource < Hobby < Solo < Studio < Business
//!                                        \
//!                                         SelfHostPro < Oem
//! ```
//!
//! Cloud tiers (Hobby/Solo/Studio/Business) and self-host tiers
//! (SelfHostPro/Oem) form two parallel chains that both inherit from
//! OpenSource. `LicenseTier::Ord` reflects the linear ordering used for
//! sorting and "at least this tier" comparisons; for feature-inheritance
//! purposes use `FeatureGate::inheritance_chain` instead.

use serde::{Deserialize, Serialize};
use std::fmt;

/// License tiers in ascending order of capabilities.
///
/// Total ordering:
/// `OpenSource < Hobby < Solo < Studio < Business < PersonalProSelfHost < SelfHostPro < Oem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseTier {
    /// Default when no license file exists. Apache 2.0 core, zero commercial modules.
    OpenSource,

    /// Cloud trial tier (14 days).
    Hobby,

    /// Cloud paid tier — NT$990/mo.
    Solo,

    /// Cloud paid tier — NT$2,990/mo (premium templates included).
    Studio,

    /// Cloud paid tier — NT$8,900/mo (Odoo + private support).
    Business,

    /// Self-host **Personal** subscription — NT$490/mo or NT$4,900/yr.
    /// The personal-form-factor self-host tier: unlocks premium templates +
    /// priority patches for individual developers who self-host. Sits below
    /// [`SelfHostPro`](Self::SelfHostPro) (the enterprise self-host tier).
    PersonalProSelfHost,

    /// Self-host subscription tier — NT$1,490/mo or NT$14,900/yr.
    SelfHostPro,

    /// OEM white-label / redistribution license (custom pricing, Year 2+).
    Oem,
}

impl LicenseTier {
    /// Returns the canonical TOML section name for this tier.
    ///
    /// Used by `FeatureGate` to look up feature flags in `features.toml`.
    pub fn as_toml_key(&self) -> &'static str {
        match self {
            Self::OpenSource => "opensource",
            Self::Hobby => "hobby",
            Self::Solo => "solo",
            Self::Studio => "studio",
            Self::Business => "business",
            Self::PersonalProSelfHost => "personal_pro_self_host",
            Self::SelfHostPro => "self_host_pro",
            Self::Oem => "oem",
        }
    }

    /// Returns `true` if this tier is only valid when delivered via DuDuClaw Cloud.
    ///
    /// Cloud-only tiers cannot be issued as self-host licenses.
    pub fn is_cloud_only(&self) -> bool {
        matches!(self, Self::Hobby | Self::Solo | Self::Studio | Self::Business)
    }

    /// Returns `true` if this tier is only valid for self-hosted deployments.
    pub fn is_self_host_only(&self) -> bool {
        matches!(self, Self::PersonalProSelfHost | Self::SelfHostPro | Self::Oem)
    }

    /// Returns `true` if this tier represents a paid commercial subscription.
    pub fn is_paid(&self) -> bool {
        !matches!(self, Self::OpenSource | Self::Hobby)
    }
}

impl Default for LicenseTier {
    fn default() -> Self {
        Self::OpenSource
    }
}

impl fmt::Display for LicenseTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_toml_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_ordering() {
        assert!(LicenseTier::OpenSource < LicenseTier::Hobby);
        assert!(LicenseTier::Hobby < LicenseTier::Solo);
        assert!(LicenseTier::Solo < LicenseTier::Studio);
        assert!(LicenseTier::Studio < LicenseTier::Business);
        assert!(LicenseTier::Business < LicenseTier::SelfHostPro);
        assert!(LicenseTier::SelfHostPro < LicenseTier::Oem);
        assert!(LicenseTier::OpenSource < LicenseTier::Oem);
    }

    #[test]
    fn tier_equality() {
        assert_eq!(LicenseTier::Solo, LicenseTier::Solo);
        assert_ne!(LicenseTier::Solo, LicenseTier::Studio);
    }

    #[test]
    fn tier_display_matches_toml_key() {
        assert_eq!(LicenseTier::OpenSource.to_string(), "opensource");
        assert_eq!(LicenseTier::Hobby.to_string(), "hobby");
        assert_eq!(LicenseTier::Solo.to_string(), "solo");
        assert_eq!(LicenseTier::Studio.to_string(), "studio");
        assert_eq!(LicenseTier::Business.to_string(), "business");
        assert_eq!(LicenseTier::SelfHostPro.to_string(), "self_host_pro");
        assert_eq!(LicenseTier::Oem.to_string(), "oem");
    }

    #[test]
    fn tier_serde_roundtrip() {
        let tiers = [
            LicenseTier::OpenSource,
            LicenseTier::Hobby,
            LicenseTier::Solo,
            LicenseTier::Studio,
            LicenseTier::Business,
            LicenseTier::SelfHostPro,
            LicenseTier::Oem,
        ];
        for tier in tiers {
            let json = serde_json::to_string(&tier).unwrap();
            let parsed: LicenseTier = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, tier);
        }
    }

    #[test]
    fn tier_sort() {
        let mut tiers = vec![
            LicenseTier::Oem,
            LicenseTier::OpenSource,
            LicenseTier::Studio,
            LicenseTier::Solo,
            LicenseTier::SelfHostPro,
            LicenseTier::Hobby,
            LicenseTier::Business,
        ];
        tiers.sort();
        assert_eq!(
            tiers,
            vec![
                LicenseTier::OpenSource,
                LicenseTier::Hobby,
                LicenseTier::Solo,
                LicenseTier::Studio,
                LicenseTier::Business,
                LicenseTier::SelfHostPro,
                LicenseTier::Oem,
            ]
        );
    }

    #[test]
    fn cloud_only_classification() {
        assert!(LicenseTier::Hobby.is_cloud_only());
        assert!(LicenseTier::Solo.is_cloud_only());
        assert!(LicenseTier::Studio.is_cloud_only());
        assert!(LicenseTier::Business.is_cloud_only());
        assert!(!LicenseTier::OpenSource.is_cloud_only());
        assert!(!LicenseTier::SelfHostPro.is_cloud_only());
        assert!(!LicenseTier::Oem.is_cloud_only());
    }

    #[test]
    fn self_host_only_classification() {
        assert!(LicenseTier::SelfHostPro.is_self_host_only());
        assert!(LicenseTier::Oem.is_self_host_only());
        assert!(!LicenseTier::OpenSource.is_self_host_only());
        assert!(!LicenseTier::Hobby.is_self_host_only());
        assert!(!LicenseTier::Studio.is_self_host_only());
    }

    #[test]
    fn paid_classification() {
        assert!(!LicenseTier::OpenSource.is_paid());
        assert!(!LicenseTier::Hobby.is_paid());
        assert!(LicenseTier::Solo.is_paid());
        assert!(LicenseTier::Studio.is_paid());
        assert!(LicenseTier::Business.is_paid());
        assert!(LicenseTier::SelfHostPro.is_paid());
        assert!(LicenseTier::Oem.is_paid());
    }

    #[test]
    fn default_is_opensource() {
        assert_eq!(LicenseTier::default(), LicenseTier::OpenSource);
    }

    #[test]
    fn personal_pro_self_host_classification() {
        let t = LicenseTier::PersonalProSelfHost;
        assert_eq!(t.to_string(), "personal_pro_self_host");
        assert_eq!(t.as_toml_key(), "personal_pro_self_host");
        assert!(t.is_self_host_only());
        assert!(!t.is_cloud_only());
        assert!(t.is_paid());
        // serde round-trip
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"personal_pro_self_host\"");
        assert_eq!(serde_json::from_str::<LicenseTier>(&json).unwrap(), t);
        // ordering: a lighter self-host tier than SelfHostPro, above Business
        assert!(LicenseTier::Business < t);
        assert!(t < LicenseTier::SelfHostPro);
    }
}
