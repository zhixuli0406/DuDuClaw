//! Installation policy; product identity and commercial entitlements are separate.
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductProfile {
    #[default]
    Standard,
    FutureOffice,
}

impl ProductProfile {
    /// Explicit unknown or empty values must never become an unrestricted profile.
    pub fn resolve(config: Option<&str>, env: Option<&str>) -> Result<Self, String> {
        for value in [config, env].into_iter().flatten() {
            if !matches!(value, "standard" | "futureoffice") {
                return Err("Invalid product profile; expected standard or futureoffice".into());
            }
        }
        match env.or(config) {
            None | Some("standard") => Ok(Self::Standard),
            Some("futureoffice") => Ok(Self::FutureOffice),
            _ => unreachable!(),
        }
    }
    pub fn blocked_channels(self) -> &'static [&'static str] {
        if self == Self::FutureOffice {
            &["wecom", "dingtalk", "feishu"]
        } else {
            &[]
        }
    }
    pub fn blocked_runtimes(self) -> &'static [&'static str] {
        if self == Self::FutureOffice {
            &["qwen", "kimi"]
        } else {
            &[]
        }
    }
    pub fn blocked_providers(self) -> &'static [&'static str] {
        if self == Self::FutureOffice {
            &["minimax", "deepseek"]
        } else {
            &[]
        }
    }
    pub fn require_channel(self, id: &str) -> Result<(), String> {
        self.require("channel", id, self.blocked_channels())
    }
    pub fn require_runtime(self, id: &str) -> Result<(), String> {
        let canonical = crate::runtime_catalog::spec_for(id)
            .map(|spec| spec.id)
            .unwrap_or(id);
        self.require("runtime", canonical, self.blocked_runtimes())
    }
    pub fn require_provider(self, id: &str) -> Result<(), String> {
        self.require("provider", id, self.blocked_providers())
    }
    fn require(self, kind: &str, id: &str, blocked: &[&str]) -> Result<(), String> {
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

/// Read each time rather than caching a permissive result after a configuration error.
/// The environment is read only; tests supply it explicitly to the pure resolver.
pub fn load(home: &Path) -> Result<ProductProfile, String> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("Unable to read product profile configuration".into()),
    };
    let table = text
        .parse::<toml::Table>()
        .map_err(|_| "Invalid config.toml; product policy cannot be determined")?;
    let env_value = std::env::var_os("DUDUCLAW_PRODUCT_PROFILE");
    let env = env_value
        .as_ref()
        .map(|v| {
            v.to_str()
                .ok_or("Invalid product profile environment encoding")
        })
        .transpose()?;
    ProductProfile::from_table(&table, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unspecified_installation_preserves_all_options() {
        let p = ProductProfile::resolve(None, None).unwrap();
        for id in ["wecom", "dingtalk", "feishu"] {
            assert!(p.require_channel(id).is_ok());
        }
        for id in ["qwen", "kimi"] {
            assert!(p.require_runtime(id).is_ok());
        }
        for id in ["minimax", "deepseek"] {
            assert!(p.require_provider(id).is_ok());
        }
    }
    #[test]
    fn exclusions_are_typed_and_exact() {
        let p = ProductProfile::FutureOffice;
        assert!(p.require_runtime("qwen").is_err());
        assert!(p.require_runtime("qwen-code").is_err());
        assert!(p.require_runtime(" KIMI ").is_err());
        assert!(p.require_provider("qwen").is_ok());
        assert!(p.require_provider("deepseek").is_err());
        assert!(p.require_provider("custom-deepseek-proxy").is_ok());
        assert!(p.require_runtime("qwen-test").is_ok());
        assert!(p.require_channel("feishu").is_err());
        assert!(p.require_channel("line").is_ok());
    }
    #[test]
    fn catalog_view_only_excludes_requested_cli_ids() {
        let standard: Vec<_> =
            crate::runtime_catalog::cli_specs_for_profile(ProductProfile::Standard)
                .map(|s| s.id)
                .collect();
        let dealer: Vec<_> =
            crate::runtime_catalog::cli_specs_for_profile(ProductProfile::FutureOffice)
                .map(|s| s.id)
                .collect();
        assert!(standard.contains(&"qwen") && standard.contains(&"kimi"));
        assert!(!dealer.contains(&"qwen") && !dealer.contains(&"kimi"));
        assert_eq!(dealer.len() + 2, standard.len());
        assert!(dealer.contains(&"claude") && dealer.contains(&"codex"));
    }
    #[test]
    fn invalid_explicit_values_never_disable_restrictions() {
        for s in ["", "FutureOffice", "futureoffice ", "unknown"] {
            assert!(ProductProfile::resolve(Some(s), Some("standard")).is_err());
            assert!(ProductProfile::resolve(Some("futureoffice"), Some(s)).is_err());
        }
        assert!(
            ProductProfile::from_table(&"[product]\nprofile=2".parse().unwrap(), None).is_err()
        );
        assert_eq!(
            ProductProfile::resolve(Some("standard"), Some("futureoffice")).unwrap(),
            ProductProfile::FutureOffice
        );
    }
}
