//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Save Odoo configuration to config.toml `[odoo]` section.
    ///
    /// Encrypts api_key/password/webhook_secret before storing.
    /// Refuses to store credentials if encryption is unavailable.
    /// Uses atomic write (temp + rename).
    pub(crate) async fn handle_odoo_configure(&self, params: Value) -> WsFrame {
        // Validate URL
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) if Self::is_safe_odoo_url(u) => u,
            Some(_) => {
                return WsFrame::error_response(
                    "",
                    "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1)",
                );
            }
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        // Validate database name
        let db = match params.get("db").and_then(|v| v.as_str()).map(str::trim) {
            Some(d)
                if !d.is_empty()
                    && d.len() < 64
                    && d.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
            {
                d
            }
            Some(_) => {
                return WsFrame::error_response(
                    "",
                    "Invalid database name (alphanumeric, _, - only, max 63 chars)",
                );
            }
            _ => return WsFrame::error_response("", "Missing 'db' parameter"),
        };

        // Validate protocol (whitelist)
        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("xmlrpc") => "xmlrpc",
            Some("jsonrpc") | None => "jsonrpc",
            _ => {
                return WsFrame::error_response(
                    "",
                    "Invalid protocol: must be 'jsonrpc' or 'xmlrpc'",
                );
            }
        };

        // Validate auth_method (whitelist)
        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => {
                return WsFrame::error_response(
                    "",
                    "Invalid auth_method: must be 'api_key' or 'password'",
                );
            }
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Build the [odoo] section
        let mut odoo = toml::map::Map::new();
        odoo.insert("url".into(), toml::Value::String(url.into()));
        odoo.insert("db".into(), toml::Value::String(db.into()));
        odoo.insert("protocol".into(), toml::Value::String(protocol.into()));
        odoo.insert(
            "auth_method".into(),
            toml::Value::String(auth_method.into()),
        );
        let username = params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if username.len() > 256 {
            return WsFrame::error_response("", "Username too long (max 256 chars)");
        }
        odoo.insert("username".into(), toml::Value::String(username.into()));

        // Encrypt credentials — refuse to store if encryption is unavailable (CRIT-1)
        if let Some(api_key) = params
            .get("api_key")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            match crate::config_crypto::encrypt_value(api_key, &self.home_dir) {
                Some(enc) => {
                    odoo.insert("api_key_enc".into(), toml::Value::String(enc));
                }
                None => {
                    return WsFrame::error_response(
                        "",
                        "Could not encrypt API key — keyfile write failed (disk full or permission denied). See gateway log.",
                    );
                }
            }
        } else {
            // Preserve existing encrypted key if not provided
            if let Some(existing) = table
                .get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("api_key_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("api_key_enc".into(), toml::Value::String(existing.into()));
            }
        }

        if let Some(password) = params
            .get("password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            match crate::config_crypto::encrypt_value(password, &self.home_dir) {
                Some(enc) => {
                    odoo.insert("password_enc".into(), toml::Value::String(enc));
                }
                None => {
                    return WsFrame::error_response(
                        "",
                        "Could not encrypt password — keyfile write failed (disk full or permission denied). See gateway log.",
                    );
                }
            }
        } else {
            // Preserve existing encrypted password if not provided
            if let Some(existing) = table
                .get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("password_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("password_enc".into(), toml::Value::String(existing.into()));
            }
        }

        // Polling config
        odoo.insert(
            "poll_enabled".into(),
            toml::Value::Boolean(
                params
                    .get("poll_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            ),
        );
        odoo.insert(
            "poll_interval_seconds".into(),
            toml::Value::Integer(
                params
                    .get("poll_interval_seconds")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(300)
                    .clamp(60, 86400),
            ),
        );
        if let Some(models) = params.get("poll_models").and_then(|v| v.as_array()) {
            let arr: Vec<toml::Value> = models
                .iter()
                .take(50) // cap at 50 models to prevent oversized config
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| Self::is_valid_odoo_model(s))
                        .map(|s| toml::Value::String(s.into()))
                })
                .collect();
            odoo.insert("poll_models".into(), toml::Value::Array(arr));
        }

        // Global unblock list: models the operator explicitly releases from the
        // built-in security block list (per-agent [odoo].unblock_models still
        // overrides). Absent param preserves the stored list so older dashboard
        // builds can't silently wipe it.
        if let Some(models) = params.get("unblock_models").and_then(|v| v.as_array()) {
            let arr: Vec<toml::Value> = models
                .iter()
                .take(50)
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| Self::is_valid_odoo_model(s))
                        .map(|s| toml::Value::String(s.into()))
                })
                .collect();
            odoo.insert("unblock_models".into(), toml::Value::Array(arr));
        } else if let Some(existing) = table
            .get("odoo")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("unblock_models"))
        {
            odoo.insert("unblock_models".into(), existing.clone());
        }

        // Webhook config
        odoo.insert(
            "webhook_enabled".into(),
            toml::Value::Boolean(
                params
                    .get("webhook_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            ),
        );
        if let Some(secret) = params
            .get("webhook_secret")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            match crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                Some(enc) => {
                    odoo.insert("webhook_secret_enc".into(), toml::Value::String(enc));
                }
                None => {
                    return WsFrame::error_response(
                        "",
                        "Could not encrypt webhook secret — keyfile write failed (disk full or permission denied). See gateway log.",
                    );
                }
            }
        } else {
            // Preserve existing webhook secret
            if let Some(existing) = table
                .get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("webhook_secret_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert(
                    "webhook_secret_enc".into(),
                    toml::Value::String(existing.into()),
                );
            }
        }

        // Feature toggles
        for feature in &[
            "features_crm",
            "features_sale",
            "features_inventory",
            "features_accounting",
            "features_project",
            "features_hr",
        ] {
            if let Some(v) = params.get(*feature).and_then(|v| v.as_bool()) {
                odoo.insert((*feature).into(), toml::Value::Boolean(v));
            }
        }

        table.insert("odoo".into(), toml::Value::Table(odoo));

        // Atomic write: temp + rename
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!("odoo.configure completed");
        WsFrame::ok_response("", json!({ "success": true }))
    }
}
