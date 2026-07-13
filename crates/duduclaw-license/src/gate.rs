//! Feature gating based on license tier and `features.toml` configuration.
//!
//! Inheritance chains (v2.0 subscription model):
//!
//! ```text
//! OpenSource  → (none)
//! Hobby       → opensource
//! Solo        → hobby → opensource
//! Studio      → solo → hobby → opensource
//! Business    → studio → solo → hobby → opensource
//! SelfHostPro → opensource          (parallel chain — does NOT inherit cloud tiers)
//! Oem         → self_host_pro → opensource
//! ```
//!
//! Self-host tiers intentionally do not inherit Cloud-tier flags so that
//! a feature like `cloud_only = true` in `[hobby]` does not accidentally
//! propagate to a `SelfHostPro` license.

use std::collections::HashMap;
use std::path::Path;

use crate::error::{LicenseError, Result};
use crate::license::License;
use crate::tier::LicenseTier;

/// Runtime feature gate that checks whether a feature is available
/// at a given license tier.
#[derive(Debug, Clone)]
pub struct FeatureGate {
    /// Parsed tier configurations keyed by their TOML section name
    /// (e.g. `"opensource"`, `"self_host_pro"`).
    tiers: HashMap<String, toml::Value>,
}

impl FeatureGate {
    /// Load feature definitions from a TOML file.
    ///
    /// # Errors
    /// Returns `LicenseError::FileNotFound` or `LicenseError::ParseError`.
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LicenseError::FileNotFound(path.display().to_string())
            } else {
                LicenseError::ParseError(format!("failed to read features.toml: {e}"))
            }
        })?;
        Self::from_str(&content)
    }

    /// Load feature definitions from a TOML string.
    ///
    /// # Errors
    /// Returns `LicenseError::ParseError` if the TOML is invalid.
    pub fn from_str(content: &str) -> Result<Self> {
        let table: HashMap<String, toml::Value> =
            toml::from_str(content).map_err(|e| LicenseError::ParseError(e.to_string()))?;
        Ok(Self { tiers: table })
    }

    /// Check if a boolean feature is available at the given tier.
    ///
    /// Feature lookup walks the inheritance chain from the requested tier
    /// down to its base, returning the first explicit definition found.
    /// Undefined features return `false`.
    pub fn check(&self, tier: LicenseTier, feature: &str) -> bool {
        for tier_key in Self::inheritance_chain(tier) {
            if let Some(section) = self.tiers.get(*tier_key) {
                if let Some(value) = section.get(feature) {
                    return value.as_bool().unwrap_or(false);
                }
            }
        }
        false
    }

    /// Return the maximum number of agents for a given tier.
    /// `0` means unlimited.
    pub fn max_agents(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "max_agents")
    }

    /// Effective agent-count limit for a license: the signed per-license
    /// `max_agents` override when present, else the tier default. `0` (either
    /// source) means unlimited. This is the single resolution point for the
    /// P-License "sell N agents" feature — enforcement callers must use it rather
    /// than `max_agents(tier)` so a per-license override actually bites.
    pub fn effective_max_agents(&self, lic: &License) -> usize {
        lic.max_agents
            .map(|v| v as usize)
            .unwrap_or_else(|| self.max_agents(lic.tier))
    }

    /// Return the maximum number of channels for a given tier.
    /// `0` means unlimited.
    pub fn max_channels(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "max_channels")
    }

    /// Return the maximum number of local models for a given tier.
    /// `0` means unlimited.
    pub fn max_local_models(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "max_local_models")
    }

    /// Return the monthly message cap. `0` means unlimited.
    pub fn max_messages_per_month(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "max_messages_per_month")
    }

    /// Return the memory storage quota in gigabytes. `0` means unlimited.
    pub fn memory_quota_gb(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "memory_quota_gb")
    }

    /// Effective memory storage quota in gigabytes for a tier — the single
    /// resolution point the M1 moat-gate enforcement (`duduclaw-memory` write
    /// path, wired through `license_runtime`) must consult. `0` means unlimited.
    ///
    /// Non-breaking contract: opensource / self-host tiers carry `0` in
    /// `features.toml` (unlimited), so enforcement is a no-op for them. Only the
    /// Cloud paid tiers with an explicit nonzero cap (Studio = 1, Business = 10)
    /// bite.
    ///
    /// This period the value is purely tier-based. A per-license override
    /// (mirroring [`effective_max_agents`](Self::effective_max_agents), e.g.
    /// selling a custom quota on a signed seat) is a deliberate future
    /// extension — kept as a separate named entry so callers depend on the
    /// resolution point, not the raw tier lookup.
    pub fn effective_memory_quota_gb(&self, tier: LicenseTier) -> usize {
        self.memory_quota_gb(tier)
    }

    /// Return the phone-home refresh interval in days for this tier.
    /// `0` means phone-home is disabled (OpenSource / OEM perpetual).
    pub fn phone_home_interval_days(&self, tier: LicenseTier) -> i64 {
        self.get_integer(tier, "license_phone_home_interval_days") as i64
    }

    /// Return the offline grace period in days for this tier.
    /// `0` disables the grace-period check (license never expires for offline reasons).
    pub fn grace_period_days(&self, tier: LicenseTier) -> i64 {
        self.get_integer(tier, "license_grace_period_days") as i64
    }

    /// Return the included office-hour allocation per month.
    pub fn office_hour_hours_per_month(&self, tier: LicenseTier) -> usize {
        self.get_integer(tier, "office_hour_hours_per_month")
    }

    /// Helper to read an integer field from a tier section,
    /// following the same inheritance chain as `check()`.
    fn get_integer(&self, tier: LicenseTier, key: &str) -> usize {
        for tier_key in Self::inheritance_chain(tier) {
            if let Some(section) = self.tiers.get(*tier_key) {
                if let Some(value) = section.get(key) {
                    return value
                        .as_integer()
                        .map(|v| if v < 0 { 0 } else { v as usize })
                        .unwrap_or(0);
                }
            }
        }
        0
    }

    /// Return the tier inheritance chain from highest precedence to lowest.
    ///
    /// Cloud and self-host tiers form parallel chains; they share only the
    /// `OpenSource` base. Each tier inherits all features from the tiers
    /// below it in its chain.
    pub fn inheritance_chain(tier: LicenseTier) -> &'static [&'static str] {
        match tier {
            LicenseTier::OpenSource => &["opensource"],
            LicenseTier::Hobby => &["hobby", "opensource"],
            LicenseTier::Solo => &["solo", "hobby", "opensource"],
            LicenseTier::Studio => &["studio", "solo", "hobby", "opensource"],
            LicenseTier::Business => &["business", "studio", "solo", "hobby", "opensource"],
            // Partner (NFR) is an independent self-host grant — its own values
            // over the opensource base, never the cloud chain.
            LicenseTier::Partner => &["partner", "opensource"],
            LicenseTier::PersonalProSelfHost => &["personal_pro_self_host", "opensource"],
            LicenseTier::SelfHostPro => &["self_host_pro", "opensource"],
            LicenseTier::Oem => &["oem", "self_host_pro", "opensource"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOML: &str = r#"
[opensource]
max_channels = 0
max_agents = 0
max_local_models = 0
memory_quota_gb = 0
premium_templates = false
industry_evolution_params = false
dashboard_enterprise = false
priority_security_patch = false
license_phone_home_interval_days = 0
license_grace_period_days = 0

[hobby]
cloud_only = true
max_agents = 1
max_channels = 1
max_messages_per_month = 100

[solo]
cloud_only = true
max_agents = 1
max_channels = 2

[studio]
cloud_only = true
max_agents = 3
max_channels = 5
memory_quota_gb = 1
premium_templates = true

[business]
cloud_only = true
max_agents = 0
max_channels = 0
memory_quota_gb = 10
premium_templates = true
industry_evolution_params = true
dashboard_enterprise = true
odoo_integration_supported = true

[self_host_pro]
self_host_only = true
premium_templates = true
industry_evolution_params = true
dashboard_enterprise = true
priority_security_patch = true
license_phone_home_interval_days = 7
license_grace_period_days = 30

[oem]
self_host_only = true
white_label = true
redistribution = true
license_phone_home_interval_days = 7
license_grace_period_days = 60
"#;

    fn gate() -> FeatureGate {
        FeatureGate::from_str(TEST_TOML).unwrap()
    }

    // --- Limits ---

    #[test]
    fn opensource_has_no_limits() {
        let g = gate();
        assert_eq!(g.max_agents(LicenseTier::OpenSource), 0);
        assert_eq!(g.max_channels(LicenseTier::OpenSource), 0);
        assert_eq!(g.memory_quota_gb(LicenseTier::OpenSource), 0);
    }

    #[test]
    fn hobby_limits() {
        let g = gate();
        assert_eq!(g.max_agents(LicenseTier::Hobby), 1);
        assert_eq!(g.max_channels(LicenseTier::Hobby), 1);
        assert_eq!(g.max_messages_per_month(LicenseTier::Hobby), 100);
    }

    #[test]
    fn solo_inherits_hobby_when_unspecified() {
        let g = gate();
        // Solo doesn't define max_messages_per_month — inherits from Hobby
        assert_eq!(g.max_messages_per_month(LicenseTier::Solo), 100);
        // Solo defines its own max_agents
        assert_eq!(g.max_agents(LicenseTier::Solo), 1);
        assert_eq!(g.max_channels(LicenseTier::Solo), 2);
    }

    #[test]
    fn studio_inherits_solo_chain() {
        let g = gate();
        assert_eq!(g.max_agents(LicenseTier::Studio), 3);
        assert_eq!(g.memory_quota_gb(LicenseTier::Studio), 1);
        // Inherited from Hobby through Solo
        assert_eq!(g.max_messages_per_month(LicenseTier::Studio), 100);
    }

    #[test]
    fn effective_max_agents_override_beats_tier() {
        use crate::license::License;
        let g = gate();
        let mut lic = License::new(
            "sub",
            "cus",
            LicenseTier::Studio,
            "fp",
            chrono::Duration::days(30),
            "v1",
        );
        // No override → tier default (Studio = 3).
        assert_eq!(g.effective_max_agents(&lic), 3);
        // Override raises it.
        lic.max_agents = Some(10);
        assert_eq!(g.effective_max_agents(&lic), 10);
        // Override lowers it (a distributor "sell 2" on a Studio seat).
        lic.max_agents = Some(2);
        assert_eq!(g.effective_max_agents(&lic), 2);
    }

    #[test]
    fn effective_max_agents_zero_override_is_unlimited() {
        use crate::license::License;
        let g = gate();
        let mut lic = License::new(
            "sub",
            "cus",
            LicenseTier::Solo,
            "fp",
            chrono::Duration::days(30),
            "v1",
        );
        // Solo tier default is 1; Some(0) override = unlimited (the sentinel).
        lic.max_agents = Some(0);
        assert_eq!(g.effective_max_agents(&lic), 0);
    }

    #[test]
    fn business_inherits_full_cloud_chain() {
        let g = gate();
        assert_eq!(g.max_agents(LicenseTier::Business), 0);
        assert_eq!(g.memory_quota_gb(LicenseTier::Business), 10);
    }

    #[test]
    fn effective_memory_quota_gb_tier_based() {
        let g = gate();
        // Free / self-host base tiers → 0 = unlimited (enforcement no-op).
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::OpenSource), 0);
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::Hobby), 0);
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::SelfHostPro), 0);
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::Oem), 0);
        // Cloud paid tiers with an explicit cap actually bite.
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::Studio), 1);
        assert_eq!(g.effective_memory_quota_gb(LicenseTier::Business), 10);
    }

    #[test]
    fn self_host_pro_does_not_inherit_cloud() {
        let g = gate();
        // cloud_only is true in Hobby but should NOT propagate to SelfHostPro
        assert!(!g.check(LicenseTier::SelfHostPro, "cloud_only"));
        // self_host_only IS set in SelfHostPro
        assert!(g.check(LicenseTier::SelfHostPro, "self_host_only"));
    }

    #[test]
    fn oem_inherits_self_host_pro() {
        let g = gate();
        assert!(g.check(LicenseTier::Oem, "premium_templates"));
        assert!(g.check(LicenseTier::Oem, "industry_evolution_params"));
        assert!(g.check(LicenseTier::Oem, "dashboard_enterprise"));
        assert!(g.check(LicenseTier::Oem, "self_host_only"));
        // OEM-specific
        assert!(g.check(LicenseTier::Oem, "white_label"));
        assert!(g.check(LicenseTier::Oem, "redistribution"));
    }

    // --- Feature gating ---

    #[test]
    fn opensource_has_no_commercial_features() {
        let g = gate();
        assert!(!g.check(LicenseTier::OpenSource, "premium_templates"));
        assert!(!g.check(LicenseTier::OpenSource, "industry_evolution_params"));
        assert!(!g.check(LicenseTier::OpenSource, "dashboard_enterprise"));
    }

    #[test]
    fn studio_unlocks_premium_templates_but_not_dashboard_enterprise() {
        let g = gate();
        assert!(g.check(LicenseTier::Studio, "premium_templates"));
        assert!(!g.check(LicenseTier::Studio, "industry_evolution_params"));
        assert!(!g.check(LicenseTier::Studio, "dashboard_enterprise"));
    }

    #[test]
    fn business_unlocks_all_cloud_value_adds() {
        let g = gate();
        assert!(g.check(LicenseTier::Business, "premium_templates"));
        assert!(g.check(LicenseTier::Business, "industry_evolution_params"));
        assert!(g.check(LicenseTier::Business, "dashboard_enterprise"));
        assert!(g.check(LicenseTier::Business, "odoo_integration_supported"));
    }

    #[test]
    fn self_host_pro_unlocks_all_value_adds() {
        let g = gate();
        assert!(g.check(LicenseTier::SelfHostPro, "premium_templates"));
        assert!(g.check(LicenseTier::SelfHostPro, "industry_evolution_params"));
        assert!(g.check(LicenseTier::SelfHostPro, "dashboard_enterprise"));
        assert!(g.check(LicenseTier::SelfHostPro, "priority_security_patch"));
    }

    #[test]
    fn unknown_feature_returns_false_at_every_tier() {
        let g = gate();
        for tier in [
            LicenseTier::OpenSource,
            LicenseTier::Hobby,
            LicenseTier::Solo,
            LicenseTier::Studio,
            LicenseTier::Business,
            LicenseTier::SelfHostPro,
            LicenseTier::Oem,
        ] {
            assert!(!g.check(tier, "completely_made_up_feature"));
        }
    }

    // --- Subscription metadata ---

    #[test]
    fn phone_home_interval_for_self_host_pro() {
        let g = gate();
        assert_eq!(g.phone_home_interval_days(LicenseTier::SelfHostPro), 7);
        assert_eq!(g.grace_period_days(LicenseTier::SelfHostPro), 30);
    }

    #[test]
    fn phone_home_interval_for_oem_uses_oem_override() {
        let g = gate();
        assert_eq!(g.phone_home_interval_days(LicenseTier::Oem), 7);
        // OEM overrides grace period
        assert_eq!(g.grace_period_days(LicenseTier::Oem), 60);
    }

    #[test]
    fn opensource_disables_phone_home() {
        let g = gate();
        assert_eq!(g.phone_home_interval_days(LicenseTier::OpenSource), 0);
        assert_eq!(g.grace_period_days(LicenseTier::OpenSource), 0);
    }

    // --- Inheritance chain ---

    #[test]
    fn inheritance_chain_cloud_path() {
        assert_eq!(
            FeatureGate::inheritance_chain(LicenseTier::Business),
            &["business", "studio", "solo", "hobby", "opensource"]
        );
    }

    #[test]
    fn inheritance_chain_self_host_path() {
        assert_eq!(
            FeatureGate::inheritance_chain(LicenseTier::Oem),
            &["oem", "self_host_pro", "opensource"]
        );
    }

    #[test]
    fn inheritance_chain_self_host_does_not_include_cloud_tiers() {
        let chain = FeatureGate::inheritance_chain(LicenseTier::SelfHostPro);
        assert!(!chain.contains(&"hobby"));
        assert!(!chain.contains(&"solo"));
        assert!(!chain.contains(&"studio"));
        assert!(!chain.contains(&"business"));
    }

    // --- Error paths ---

    #[test]
    fn invalid_toml_returns_parse_error() {
        let result = FeatureGate::from_str("{{invalid toml");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LicenseError::ParseError(_)));
    }

    #[test]
    fn missing_file_returns_file_not_found() {
        let result = FeatureGate::from_file(Path::new("/nonexistent/features.toml"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LicenseError::FileNotFound(_)));
    }

    // --- features.toml authoritative parse ---

    #[test]
    fn embedded_features_toml_parses_and_self_consistent() {
        // The shipped features.toml at repo root must be loadable and
        // produce sensible values. This guards against drift between the
        // tests above and the actual v2 manifest.
        let toml_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("features.toml");
        let gate = FeatureGate::from_file(&toml_path)
            .expect("features.toml must exist next to Cargo.toml");

        // Sanity checks
        assert!(!gate.check(LicenseTier::OpenSource, "premium_templates"));
        assert!(gate.check(LicenseTier::Studio, "premium_templates"));
        assert!(gate.check(LicenseTier::SelfHostPro, "dashboard_enterprise"));
        assert!(gate.check(LicenseTier::Oem, "white_label"));

        // Phone-home defaults
        assert_eq!(gate.phone_home_interval_days(LicenseTier::SelfHostPro), 7);
        assert!(gate.grace_period_days(LicenseTier::SelfHostPro) >= 30);
    }
}
