//! Premium (licensed) industry template resolution — CLI side.
//!
//! Free starter templates live in `templates/` and are copied by the wizard
//! unconditionally (they ship in the public, Apache-2.0 repo). The *premium*
//! industry templates — battle-tested SOUL.md / CONTRACT.toml / wiki knowledge
//! tuned per vertical — live in the gitignored `commercial/templates-premium/`
//! tree and are only unlocked for tiers whose license grants the
//! `premium_templates` feature (Studio / Business / SelfHostPro /
//! PersonalProSelfHost / OEM — see `crates/duduclaw-license/features.toml`).
//!
//! The filesystem side (directory resolution, discovery, labels, team
//! manifests) lives in `duduclaw_gateway::premium_templates` so the dashboard
//! gateway can drive the team staging flow with the exact same logic; this
//! module keeps the CLI-context license gate and re-exports the shared
//! symbols so `wizard.rs` callers are unchanged.
//!
//! **Fail-closed** per the project security convention: any error reading the
//! license, a missing/expired license, a missing directory, or an
//! unrecognised slug all resolve to *locked / unavailable*, never to an
//! accidental unlock.

pub use duduclaw_gateway::premium_templates::{
    discover_in, find_premium_templates_dir, PremiumIndustry,
};

use duduclaw_license::{
    load_default, FeatureGate, LicenseError, LicenseTier, EMBEDDED_FEATURES_TOML,
};

/// The feature flag (in `features.toml`) that unlocks premium templates.
const PREMIUM_FEATURE: &str = "premium_templates";

/// Pure gate check over a tier — does this tier's license grant
/// `premium_templates`? Fail-closed: a broken embedded features.toml denies.
///
/// Separated from filesystem/license-IO so it can be unit-tested directly.
pub fn premium_gate_open(tier: LicenseTier) -> bool {
    match FeatureGate::from_str(EMBEDDED_FEATURES_TOML) {
        Ok(gate) => gate.check(tier, PREMIUM_FEATURE),
        // If the embedded table can't parse we deny rather than guess.
        Err(_) => false,
    }
}

/// Is the premium-templates feature unlocked on *this* machine right now?
///
/// Fail-closed: no license file → OpenSource → locked; expired license →
/// locked; any read error → locked.
pub fn premium_unlocked() -> bool {
    match load_default() {
        Ok(license) => {
            if license.is_expired() {
                return false;
            }
            premium_gate_open(license.tier)
        }
        // FileNotFound == OpenSource == locked. Any other error also denies.
        Err(LicenseError::FileNotFound(_)) => false,
        Err(_) => false,
    }
}

/// The premium industries available to the user *right now*: present on disk
/// **and** unlocked by the active license. Empty when locked or absent.
pub fn available_premium_industries() -> Vec<PremiumIndustry> {
    if !premium_unlocked() {
        return Vec::new();
    }
    match find_premium_templates_dir() {
        Some(dir) => discover_in(&dir),
        None => Vec::new(),
    }
}

/// Premium templates exist on disk but are NOT unlocked — used to render a
/// gentle upsell hint in the wizard. Returns `false` when no premium tree is
/// installed at all (nothing to upsell).
pub fn premium_present_but_locked() -> bool {
    if premium_unlocked() {
        return false;
    }
    find_premium_templates_dir()
        .map(|d| !discover_in(&d).is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opensource_tier_is_gated() {
        assert!(!premium_gate_open(LicenseTier::OpenSource));
    }

    #[test]
    fn paid_tiers_unlock_premium_templates() {
        // These tiers grant premium_templates per features.toml.
        assert!(premium_gate_open(LicenseTier::Studio));
        assert!(premium_gate_open(LicenseTier::Business));
        assert!(premium_gate_open(LicenseTier::SelfHostPro));
        assert!(premium_gate_open(LicenseTier::PersonalProSelfHost));
        assert!(premium_gate_open(LicenseTier::Oem));
    }
}
