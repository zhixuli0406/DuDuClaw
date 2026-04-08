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
            poll_enabled: true,
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
        }
    }
}

impl OdooConfig {
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
        let mut config = OdooConfig::default();
        config.url = "https://odoo.example.com".to_string();
        config.db = "mydb".to_string();
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
