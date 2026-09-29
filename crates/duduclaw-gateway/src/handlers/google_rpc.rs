//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Google credential paths (admin only) ────────────────────────────────
    //
    // Three ways to authorize the same nineteen Google tools; see
    // `docs/guides/google-no-oauth-client.md`. The OAuth path has its own
    // connect flow (`mcp.oauth.*`); these three RPCs cover the two that are
    // pure configuration — service-account delegation and the Apps Script
    // bridge — so operators do not have to hand-edit `config.toml`.
    //
    // The stored secret is NEVER returned. `get` reports only whether one is
    // set; a UI that needs to change it collects a new one.

    /// `google.integration.set` — flip the `[integrations] google_workspace`
    /// master gate. Params: `{ enabled: bool }`. Response:
    /// `{ enabled, changed }`.
    ///
    /// G6 (2026-09 feature audit): before this the gate could only be opened
    /// by hand-editing `config.toml` or as a side effect of completing an
    /// OAuth / credential save, so the Google tab was visible to everyone
    /// while the tools behind it refused every call on a stock install. The
    /// page now reads the flag and offers this.
    pub(crate) async fn handle_google_integration_set(&self, params: Value) -> WsFrame {
        let Some(enabled) = params.get("enabled").and_then(|v| v.as_bool()) else {
            return WsFrame::error_response("", "Missing boolean 'enabled' parameter");
        };
        match crate::google_workspace::set_integration_enabled(&self.home_dir, enabled) {
            Ok(changed) => {
                if changed {
                    info!(enabled, "google.integration.set updated [integrations] google_workspace");
                }
                WsFrame::ok_response("", json!({ "enabled": enabled, "changed": changed }))
            }
            Err(e) => WsFrame::error_response(
                "",
                &format!("無法寫入 config.toml [integrations] google_workspace：{e}"),
            ),
        }
    }

    /// Report which Google credential sections are configured.
    pub(crate) async fn handle_google_credentials_get(&self) -> WsFrame {
        let raw = tokio::fs::read_to_string(self.home_dir.join("config.toml"))
            .await
            .unwrap_or_default();

        let sa = crate::google_service_account::parse_config(&raw, &self.home_dir);
        let (sa_configured, sa_key_file, sa_subject, sa_error) = match sa {
            Ok(Some(c)) => (
                true,
                c.key_file.display().to_string(),
                c.subject,
                String::new(),
            ),
            Ok(None) => (false, String::new(), String::new(), String::new()),
            // A present-but-broken section must be visible in the UI, not
            // rendered as "not configured".
            Err(e) => (false, String::new(), String::new(), e.to_string()),
        };

        let bridge = crate::google_apps_script::config_for_home(&self.home_dir).await;
        let (gas_configured, gas_url, gas_error) = match bridge {
            Ok(Some(c)) => (true, c.url, String::new()),
            Ok(None) => (false, String::new(), String::new()),
            Err(e) => (false, String::new(), e.to_string()),
        };

        // The effective path, resolved the same way a real tool call would.
        let effective = match crate::google_workspace::resolve_backend(&self.home_dir).await {
            Ok(crate::google_workspace::GoogleBackend::Direct(_)) => "direct",
            Ok(crate::google_workspace::GoogleBackend::AppsScript(_)) => "apps_script",
            Err(_) => "none",
        };

        WsFrame::ok_response(
            "",
            json!({
                "integration_enabled":
                    crate::google_workspace::integration_enabled(&self.home_dir),
                "effective": effective,
                "service_account": {
                    "configured": sa_configured,
                    "key_file": sa_key_file,
                    "subject": sa_subject,
                    "error": sa_error,
                },
                "apps_script": {
                    "configured": gas_configured,
                    "url": gas_url,
                    "error": gas_error,
                },
                "required_scopes": crate::google_workspace::REQUIRED_SCOPES,
            }),
        )
    }

    /// Write (or clear) one Google credential section.
    ///
    /// `mode` selects exactly one path and REMOVES the other's section, so the
    /// configured path and the effective path can never disagree — a silent
    /// leftover section is the kind of thing an operator only discovers when a
    /// tool call authorizes as the wrong identity. `mode: "none"` clears both.
    pub(crate) async fn handle_google_credentials_set(&self, params: Value) -> WsFrame {
        let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(mode, "service_account" | "apps_script" | "none") {
            return WsFrame::error_response(
                "",
                "mode must be 'service_account', 'apps_script' or 'none'",
            );
        }

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut integrations = table
            .get("integrations")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();

        // Both alternatives are mutually exclusive with each other; start from
        // a clean slate and re-add only the selected one.
        integrations.remove("google_service_account");
        integrations.remove("google_apps_script");

        match mode {
            "service_account" => {
                let key_file = params
                    .get("key_file")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("");
                let subject = params
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("");
                // Validate the values themselves — same helper the config-file
                // path uses, so the dashboard accepts and rejects exactly what a
                // hand-edited config.toml would. Reading the key file here means
                // a wrong path or an OAuth-client JSON is caught at save time
                // rather than at the first tool call hours later.
                let cfg = match crate::google_service_account::build_config(
                    key_file,
                    subject,
                    &self.home_dir,
                ) {
                    Ok(c) => c,
                    Err(e) => return WsFrame::error_response("", &e.to_string()),
                };
                if let Err(e) = crate::google_service_account::load_key(&cfg.key_file) {
                    return WsFrame::error_response("", &e.to_string());
                }

                let mut sa = toml::map::Map::new();
                sa.insert("key_file".into(), toml::Value::String(key_file.into()));
                sa.insert("subject".into(), toml::Value::String(subject.into()));
                integrations.insert("google_service_account".into(), toml::Value::Table(sa));
            }
            "apps_script" => {
                let url = params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("");
                if let Err(e) = crate::google_apps_script::validate_url(url) {
                    return WsFrame::error_response("", &e.to_string());
                }
                let mut gas = toml::map::Map::new();
                gas.insert("url".into(), toml::Value::String(url.into()));

                match params
                    .get("secret")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(secret) => {
                        match crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                            Some(enc) => {
                                gas.insert("secret".into(), toml::Value::String(enc));
                            }
                            None => {
                                return WsFrame::error_response(
                                    "",
                                    "Could not encrypt the bridge secret — keyfile write failed (disk full or permission denied). See gateway log.",
                                );
                            }
                        }
                    }
                    // No new secret supplied: keep the stored one so "edit the
                    // URL" does not silently wipe the credential.
                    None => {
                        let existing = table
                            .get("integrations")
                            .and_then(|v| v.as_table())
                            .and_then(|t| t.get("google_apps_script"))
                            .and_then(|v| v.as_table())
                            .and_then(|t| t.get("secret"))
                            .and_then(|v| v.as_str());
                        match existing {
                            Some(s) => {
                                gas.insert("secret".into(), toml::Value::String(s.into()));
                            }
                            None => {
                                return WsFrame::error_response("", "secret is required");
                            }
                        }
                    }
                }
                integrations.insert("google_apps_script".into(), toml::Value::Table(gas));
            }
            _ => {}
        }

        // Saving a working credential IS the opt-in — flip the master gate in
        // the same write so the tools this credential unlocks actually reach
        // agents, instead of requiring a manual `google_workspace = true`
        // config.toml edit that nothing pointed to except a warning banner.
        // Clearing credentials (mode "none") leaves the gate untouched: the
        // OAuth-vault path may still be in use.
        if mode != "none" {
            integrations.insert("google_workspace".into(), toml::Value::Boolean(true));
        }

        table.insert("integrations".into(), toml::Value::Table(integrations));
        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config.toml: {e}"));
        }
        WsFrame::ok_response("", json!({ "ok": true, "mode": mode }))
    }

    /// Live-test the configured credential path: mint a token (service account)
    /// or call the bridge's `status` action. Reports what the credential can
    /// actually do, not merely that the fields parse.
    pub(crate) async fn handle_google_credentials_test(&self) -> WsFrame {
        match crate::google_workspace::resolve_backend(&self.home_dir).await {
            Ok(crate::google_workspace::GoogleBackend::Direct(_)) => WsFrame::ok_response(
                "",
                json!({
                    "ok": true,
                    "path": "direct",
                    "detail": "取得存取權杖成功，19 個工具皆可用。",
                }),
            ),
            Ok(crate::google_workspace::GoogleBackend::AppsScript(cfg)) => {
                match crate::google_apps_script::call(
                    &cfg,
                    crate::google_apps_script::BridgeAction::Status,
                    json!({}),
                )
                .await
                {
                    Ok(v) => WsFrame::ok_response(
                        "",
                        json!({
                            "ok": true,
                            "path": "apps_script",
                            "account": v.get("account").and_then(|a| a.as_str()).unwrap_or(""),
                            "detail": "橋接連線成功，支援 Gmail／行事曆／Sheets。",
                        }),
                    ),
                    Err(e) => WsFrame::error_response("", &e.to_string()),
                }
            }
            Err(e) => WsFrame::error_response("", &e.to_string()),
        }
    }

    /// Check if a provider's OAuth flow has completed (token exists).
    pub(crate) async fn handle_mcp_oauth_status(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        // Connection state, not access-token freshness: a Google access token
        // expires every hour, and reporting that as "not connected" is what made
        // a working integration look like it had lost its credentials. A token
        // with a refresh token attached is connected; the refresh happens on the
        // next call. Only an expired token with nothing to refresh from needs
        // the user to authorize again.
        let token = mcp_oauth::get_stored_token(&self.home_dir, provider_id);
        match token {
            Some(t) => {
                let expired = mcp_oauth::token_expired(&t);
                let can_refresh = t.refresh_token.is_some();
                WsFrame::ok_response(
                    "",
                    json!({
                        "authenticated": !expired || can_refresh,
                        "access_token_valid": !expired,
                        "can_refresh": can_refresh,
                        "expires_at": t.expires_at,
                        "scopes": t.scopes,
                    }),
                )
            }
            None => WsFrame::ok_response(
                "",
                json!({
                    "authenticated": false,
                    "access_token_valid": false,
                    "can_refresh": false,
                }),
            ),
        }
    }

    /// Display name for a built-in OAuth provider (proper nouns, untranslated).
    pub(crate) fn oauth_provider_name(provider_id: &str) -> String {
        match provider_id {
            "google" => "Google".to_string(),
            "github" => "GitHub".to_string(),
            "notion" => "Notion".to_string(),
            "slack" => "Slack".to_string(),
            other => other.to_string(),
        }
    }

    /// Revoke (remove) a stored OAuth token for a provider.
    pub(crate) async fn handle_mcp_oauth_revoke(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        match mcp_oauth::remove_token(&self.home_dir, provider_id) {
            Ok(()) => {
                info!(provider = %provider_id, "MCP OAuth token revoked");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
