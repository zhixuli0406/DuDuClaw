//! Odoo connection configuration.
//!
//! [O-1c] Parses the `[odoo]` section from config.toml with encrypted credentials.

use serde::{Deserialize, Serialize};

/// Odoo connection configuration from `config.toml [odoo]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OdooConfig {
    pub url: String,
    pub db: String,
    pub protocol: String,
    pub auth_method: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub api_key_enc: String,
    #[serde(skip_serializing)]
    pub password_enc: String,
    pub poll_enabled: bool,
    pub poll_interval_seconds: u64,
    pub poll_models: Vec<String>,
    pub webhook_enabled: bool,
    #[serde(skip_serializing)]
    pub webhook_secret: String,
    /// Encrypted webhook secret (takes precedence over `webhook_secret`).
    #[serde(default, skip_serializing)]
    pub webhook_secret_enc: String,
    pub features_crm: bool,
    pub features_sale: bool,
    pub features_inventory: bool,
    pub features_accounting: bool,
    pub features_project: bool,
    pub features_hr: bool,
    /// Global opt-out list for the built-in security block list. Applies to
    /// every agent that has no per-agent `[odoo].unblock_models` of its own.
    /// Empty ⇒ the default block list is fully in force.
    #[serde(default)]
    pub unblock_models: Vec<String>,
}

impl Default for OdooConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            db: String::new(),
            protocol: "jsonrpc".to_string(),
            auth_method: "api_key".to_string(),
            username: String::new(),
            api_key_enc: String::new(),
            password_enc: String::new(),
            // G4 (2026-09 feature audit): default OFF. This used to be `true`,
            // which was harmless only because nothing consumed it — the
            // polling task did not exist. Now that it does, an unset key must
            // not start a background loop against the operator's ERP; "未設定
            // 不得預設開". `odoo.configure` already defaults an absent
            // `poll_enabled` param to `false`, so the two now agree.
            poll_enabled: false,
            poll_interval_seconds: 60,
            poll_models: vec![
                "crm.lead".to_string(),
                "sale.order".to_string(),
            ],
            webhook_enabled: false,
            webhook_secret: String::new(),
            webhook_secret_enc: String::new(),
            features_crm: true,
            features_sale: true,
            features_inventory: true,
            features_accounting: true,
            features_project: false,
            features_hr: false,
            unblock_models: Vec::new(),
        }
    }
}

/// The `features_*` module a model belongs to, by Odoo's model-name prefix.
/// `None` ⇒ the model is outside the six switchable modules (e.g.
/// `res.partner`) and no module switch applies.
pub fn feature_module_for_model(model: &str) -> Option<&'static str> {
    let model = model.trim();
    let module = model.split('.').next().unwrap_or("");
    match module {
        "crm" => Some("crm"),
        "sale" => Some("sale"),
        "stock" | "product" => Some("inventory"),
        "account" => Some("accounting"),
        "project" => Some("project"),
        "hr" => Some("hr"),
        _ => None,
    }
}

impl OdooConfig {
    /// Whether the dashboard's 功能模組 switches allow a call on `model`.
    /// Models outside the six modules are always allowed here (the security
    /// block list and per-agent `allowed_models` still apply).
    pub fn feature_allows_model(&self, model: &str) -> Result<(), &'static str> {
        let Some(module) = feature_module_for_model(model) else {
            return Ok(());
        };
        let on = match module {
            "crm" => self.features_crm,
            "sale" => self.features_sale,
            "inventory" => self.features_inventory,
            "accounting" => self.features_accounting,
            "project" => self.features_project,
            "hr" => self.features_hr,
            _ => true,
        };
        if on { Ok(()) } else { Err(module) }
    }

    /// Check if Odoo integration is configured (URL and DB are set).
    pub fn is_configured(&self) -> bool {
        !self.url.is_empty() && !self.db.is_empty()
    }

    /// Load from a TOML table's `[odoo]` section.
    pub fn from_toml(table: &toml::Table) -> Self {
        table
            .get("odoo")
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_not_configured() {
        let config = OdooConfig::default();
        assert!(!config.is_configured());
        assert!(config.url.is_empty());
        assert!(config.db.is_empty());
    }

    #[test]
    fn configured_when_url_and_db_set() {
        let config = OdooConfig {
            url: "https://odoo.example.com".to_string(),
            db: "mydb".to_string(),
            ..Default::default()
        };
        assert!(config.is_configured());
    }

    #[test]
    fn from_toml_parses_section() {
        let toml_str = r#"
[odoo]
url = "https://odoo.example.com"
db = "production"
username = "admin"
api_key_enc = "encrypted_key_here"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let config = OdooConfig::from_toml(&table);
        assert_eq!(config.url, "https://odoo.example.com");
        assert_eq!(config.db, "production");
        assert_eq!(config.username, "admin");
        assert!(config.is_configured());
    }

    #[test]
    fn from_toml_missing_section_returns_default() {
        let table = toml::Table::new();
        let config = OdooConfig::from_toml(&table);
        assert!(!config.is_configured());
    }

    #[test]
    fn polling_and_webhook_default_off() {
        // Regression (G4): an unset `[odoo] poll_enabled` must not start the
        // background poller. Before the poller existed this default was `true`
        // and nothing noticed.
        let config = OdooConfig::default();
        assert!(!config.poll_enabled);
        assert!(!config.webhook_enabled);
    }

    #[test]
    fn feature_switches_gate_models_by_prefix() {
        let mut c = OdooConfig::default();
        assert_eq!(c.feature_allows_model("crm.lead"), Ok(()));
        assert_eq!(c.feature_allows_model("project.task"), Err("project"));
        assert_eq!(c.feature_allows_model("res.partner"), Ok(()));
        c.features_crm = false;
        assert_eq!(c.feature_allows_model("crm.lead"), Err("crm"));
        c.features_inventory = false;
        assert_eq!(c.feature_allows_model("stock.quant"), Err("inventory"));
        assert_eq!(c.feature_allows_model("product.product"), Err("inventory"));
        // Exact module token, not a substring: "crmx.foo" is not CRM.
        assert_eq!(feature_module_for_model("crmx.foo"), None);
        c.features_project = true;
        assert_eq!(c.feature_allows_model("project.task"), Ok(()));
    }

    #[test]
    fn default_features_enabled() {
        let config = OdooConfig::default();
        assert!(config.features_crm);
        assert!(config.features_sale);
        assert!(config.features_inventory);
        assert!(config.features_accounting);
        assert!(!config.features_project);
        assert!(!config.features_hr);
    }
}
