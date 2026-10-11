//! Generic immutable installation policy; private packaging supplies restrictions.
use serde::Serialize;
use std::path::Path;

mod compiled {
    include!(concat!(env!("OUT_DIR"), "/installation_policy.rs"));
}

#[cfg(any(
    test,
    all(feature = "installation-policy-test-hooks", debug_assertions)
))]
mod synthetic {
    pub const CHANNELS: &[&str] = &["slack"];
    pub const RUNTIMES: &[&str] = &["claude"];
    pub const PROVIDERS: &[&str] = &["openai"];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductProfile {
    #[default]
    Standard,
    Restricted,
}

fn policy_available() -> bool {
    if compiled::AVAILABLE {
        return true;
    }
    // Dependent-crate fixtures are debug-test tooling. Release libraries/binaries
    // must not acquire a synthetic policy through --all-features/--all-targets.
    cfg!(any(
        test,
        all(feature = "installation-policy-test-hooks", debug_assertions)
    ))
}

fn resolve_with_installation(
    config: Option<&str>,
    env: Option<&str>,
    available: bool,
    required: bool,
) -> Result<ProductProfile, String> {
    for value in [config, env].into_iter().flatten() {
        if !matches!(value, "standard" | "restricted") {
            return Err("Invalid Product profile; expected standard or restricted".into());
        }
    }
    if required {
        if !available {
            return Err("Product profile requires an unavailable installation policy".into());
        }
        if [config, env].contains(&Some("standard")) {
            return Err("Product profile is installation-owned and cannot be relaxed".into());
        }
        return Ok(ProductProfile::Restricted);
    }
    match env.or(config) {
        None | Some("standard") => Ok(ProductProfile::Standard),
        Some("restricted") if available => Ok(ProductProfile::Restricted),
        Some("restricted") => Err("Product profile requires a compiled installation policy".into()),
        _ => unreachable!(),
    }
}

impl ProductProfile {
    pub fn resolve(config: Option<&str>, env: Option<&str>) -> Result<Self, String> {
        resolve_with_installation(config, env, policy_available(), compiled::REQUIRED)
    }

    fn restricted(self) -> bool {
        compiled::REQUIRED || self == Self::Restricted
    }

    pub fn check_available(self) -> Result<(), String> {
        if self.restricted() && !policy_available() {
            Err("Product profile requires a compiled installation policy".into())
        } else {
            Ok(())
        }
    }

    pub fn blocked_channels(self) -> &'static [&'static str] {
        if !self.restricted() {
            return &[];
        }
        if compiled::AVAILABLE {
            return compiled::CHANNELS;
        }
        #[cfg(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        ))]
        return synthetic::CHANNELS;
        #[cfg(not(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        )))]
        &[]
    }

    pub fn blocked_runtimes(self) -> &'static [&'static str] {
        if !self.restricted() {
            return &[];
        }
        if compiled::AVAILABLE {
            return compiled::RUNTIMES;
        }
        #[cfg(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        ))]
        return synthetic::RUNTIMES;
        #[cfg(not(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        )))]
        &[]
    }

    pub fn blocked_providers(self) -> &'static [&'static str] {
        if !self.restricted() {
            return &[];
        }
        if compiled::AVAILABLE {
            return compiled::PROVIDERS;
        }
        #[cfg(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        ))]
        return synthetic::PROVIDERS;
        #[cfg(not(any(
            test,
            all(feature = "installation-policy-test-hooks", debug_assertions)
        )))]
        &[]
    }

    pub fn require_channel(self, id: &str) -> Result<(), String> {
        self.require("channel", id, self.blocked_channels())
    }

    pub fn require_runtime(self, id: &str) -> Result<(), String> {
        let canonical = crate::runtime_catalog::spec_for(id)
            .map(|spec| spec.id)
            .unwrap_or(id);
        self.check_available()?;
        // An unknown blocked runtime must not quietly turn into a permissive policy.
        if self
            .blocked_runtimes()
            .iter()
            .any(|id| crate::runtime_catalog::spec_for(id).is_none_or(|spec| spec.id != *id))
        {
            return Err("Product profile contains a noncanonical runtime selector".into());
        }
        self.require("runtime", canonical, self.blocked_runtimes())
    }

    pub fn require_provider(self, id: &str) -> Result<(), String> {
        self.require("provider", id, self.blocked_providers())
    }

    fn require(self, kind: &str, id: &str, blocked: &[&str]) -> Result<(), String> {
        self.check_available()?;
        if blocked.contains(&id) {
            Err(format!("Product profile does not allow {kind} '{id}'"))
        } else {
            Ok(())
        }
    }

    pub fn from_table(table: &toml::Table, env: Option<&str>) -> Result<Self, String> {
        let config = match table.get("product") {
            None => None,
            Some(v) => {
                let product = v.as_table().ok_or("[product] must be a table")?;
                match product.get("profile") {
                    None => None,
                    Some(v) => Some(v.as_str().ok_or("product.profile must be a string")?),
                }
            }
        };
        Self::resolve(config, env)
    }
}

/// Invalid configuration never reuses a previously permissive policy.
pub fn load(home: &Path) -> Result<ProductProfile, String> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("Unable to read Product profile configuration".into()),
    };
    let table = text
        .parse::<toml::Table>()
        .map_err(|_| "Invalid config.toml; Product profile cannot be determined")?;
    let env_value = std::env::var_os("DUDUCLAW_PRODUCT_PROFILE");
    let env = env_value
        .as_ref()
        .map(|v| {
            v.to_str()
                .ok_or("Invalid Product profile environment encoding")
        })
        .transpose()?;
    ProductProfile::from_table(&table, env)
}

#[cfg(test)]
#[path = "installation_policy_build.rs"]
mod build_input_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_installation_without_a_policy_preserves_all_options() {
        assert_eq!(
            resolve_with_installation(None, None, false, false).unwrap(),
            ProductProfile::Standard
        );
        assert!(resolve_with_installation(Some("restricted"), None, false, false).is_err());
        assert!(resolve_with_installation(None, Some("restricted"), false, false).is_err());
    }

    #[test]
    fn compiled_private_policy_is_mandatory_even_without_config_or_environment() {
        assert_eq!(
            resolve_with_installation(None, None, true, true).unwrap(),
            ProductProfile::Restricted
        );
        for (config, env) in [
            (Some("standard"), None),
            (None, Some("standard")),
            (Some("standard"), Some("restricted")),
        ] {
            assert!(resolve_with_installation(config, env, true, true).is_err());
        }
        assert!(resolve_with_installation(None, None, false, true).is_err());
    }

    #[test]
    fn synthetic_policy_has_exact_typed_selectors() {
        if compiled::AVAILABLE {
            return; // Private packaging has its own matrix acceptance suite.
        }
        let policy = ProductProfile::Restricted;
        assert!(policy.require_runtime(" CLAUDE ").is_err());
        assert!(policy.require_provider("claude").is_ok());
        assert!(policy.require_provider("openai").is_err());
        assert!(policy.require_channel("slack").is_err());
        assert!(policy.require_channel("line").is_ok());
        assert!(policy.require_provider("custom-openai-proxy").is_ok());
        let available: Vec<_> = crate::runtime_catalog::cli_specs_for_profile(policy)
            .map(|spec| spec.id)
            .collect();
        assert!(!available.contains(&"claude"));
        assert!(available.contains(&"codex"));
    }

    #[test]
    fn compiled_policy_cannot_be_bypassed_by_constructing_a_standard_variant() {
        if !compiled::REQUIRED {
            return;
        }
        assert_eq!(
            ProductProfile::resolve(None, None).unwrap(),
            ProductProfile::Restricted
        );
        assert!(ProductProfile::resolve(Some("standard"), None).is_err());
        assert!(ProductProfile::resolve(None, Some("standard")).is_err());
        for id in compiled::CHANNELS {
            assert!(ProductProfile::Standard.require_channel(id).is_err());
        }
        for id in compiled::RUNTIMES {
            assert!(ProductProfile::Standard.require_runtime(id).is_err());
        }
        for id in compiled::PROVIDERS {
            assert!(ProductProfile::Standard.require_provider(id).is_err());
        }
    }

    #[test]
    fn invalid_explicit_values_never_disable_restrictions() {
        for value in ["", "Restricted", "restricted ", "unknown"] {
            assert!(resolve_with_installation(Some(value), Some("standard"), true, false).is_err());
            assert!(
                resolve_with_installation(Some("restricted"), Some(value), true, false).is_err()
            );
        }
        assert!(
            ProductProfile::from_table(&"[product]\nprofile=2".parse().unwrap(), None).is_err()
        );
    }
}
