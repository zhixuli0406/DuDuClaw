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
    pub api_key_enc: String,
    pub password_enc: String,
    pub poll_enabled: bool,
    pub poll_interval_seconds: u64,
    pub poll_models: Vec<String>,
    pub webhook_enabled: bool,
    pub webhook_secret: String,
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
