//! CE/EE edition detection and feature gating.
//!
//! [O-1b] Auto-detects installed modules to determine Community vs Enterprise.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

use crate::connector::OdooConnector;

/// Odoo edition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Edition {
    Community,
    Enterprise,
    Unknown,
}

/// Enterprise-only module names used for detection.
const EE_MODULES: &[&str] = &[
    "web_studio",
    "approvals",
    "sign",
    "marketing_automation",
    "hr_payroll",
    "mrp_workorder",
    "quality_control",
    "documents",
];

/// Feature gate based on edition and installed modules.
pub struct EditionGate {
    pub edition: Edition,
    pub installed_modules: HashSet<String>,
    pub odoo_version: String,
}

impl EditionGate {
    pub fn unknown() -> Self {
        Self {
            edition: Edition::Unknown,
            installed_modules: HashSet::new(),
            odoo_version: String::new(),
        }
    }

    /// Detect edition by checking which EE modules are installed.
    pub async fn detect(conn: &OdooConnector) -> Result<Self, String> {
        // Get version
        let version_info = conn.version().await?;
        let odoo_version = version_info
            .get("server_version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Check installed EE modules
        let domain = vec![
            json!(["name", "in", EE_MODULES]),
            json!(["state", "=", "installed"]),
        ];
        let installed = conn
            .search_read("ir.module.module", domain, &["name"], 20)
            .await?;

        let installed_names: HashSet<String> = installed
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect();

        let edition = if installed_names.is_empty() {
            Edition::Community
        } else {
            Edition::Enterprise
        };

        info!(
            edition = ?edition,
            version = %odoo_version,
            ee_modules = installed_names.len(),
            "Odoo edition detected"
        );

        Ok(Self {
            edition,
            installed_modules: installed_names,
            odoo_version,
        })
    }

    /// Check if a specific EE feature is available.
    pub fn can_use(&self, feature: &str) -> bool {
        match feature {
            "approval" => self.installed_modules.contains("approvals"),
            "sign" => self.installed_modules.contains("sign"),
            "studio" => self.installed_modules.contains("web_studio"),
            "marketing" => self.installed_modules.contains("marketing_automation"),
            "payroll" => self.installed_modules.contains("hr_payroll"),
            "quality" => self.installed_modules.contains("quality_control"),
            _ => true, // CE features are always available
        }
    }

    /// Get a fallback message for unavailable EE features.
    pub fn fallback_message(&self, feature: &str) -> &'static str {
        match feature {
            "approval" => "Approval workflows require Odoo Enterprise. Please handle approvals manually in Odoo.",
            "sign" => "Document signing requires Odoo Enterprise Sign module. Please send signing links manually.",
            "studio" => "Custom fields via Studio require Odoo Enterprise.",
            "marketing" => "Marketing automation requires Odoo Enterprise.",
            _ => "This feature is not available in your Odoo edition.",
        }
    }
}
