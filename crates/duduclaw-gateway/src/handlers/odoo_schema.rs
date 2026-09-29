//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Test the Odoo connection.
    ///
    /// Two modes:
    /// - **Inline (recommended for the dashboard "Test connection" button):** when
    ///   `params.url` is non-empty, build a transient config from `params`
    ///   (url / db / protocol / auth_method / username + api_key|password).
    ///   The config is **never written to disk** — this lets the user verify
    ///   credentials before persisting them.
    /// - **Stored:** when no `url` in params, fall back to `config.toml`
    ///   (original behaviour).
    ///
    /// Hybrid: in inline mode without an explicit credential, the saved
    /// credential is used so you can re-test after a small URL tweak without
    /// retyping the API key.
    pub(crate) async fn handle_odoo_test(&self, params: Value) -> WsFrame {
        let inline_url = params
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // Always read config.toml once — needed for stored-credential fallback in inline mode.
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        let (odoo_cfg, credential) = if inline_url.is_some() {
            match Self::build_test_config_from_params(&params) {
                Ok((cfg, Some(cred))) => (cfg, cred),
                Ok((cfg, None)) => {
                    // No credential in params — fall back to saved one so the user
                    // can re-test a tweaked URL without retyping the API key.
                    match self.resolve_odoo_credential(&table) {
                        Some(c) if !c.is_empty() => (cfg, c),
                        _ => {
                            return WsFrame::ok_response(
                                "",
                                json!({
                                    "success": false,
                                    "message": "No credential — enter API key/password, or save once first",
                                }),
                            );
                        }
                    }
                }
                Err(msg) => {
                    return WsFrame::ok_response(
                        "",
                        json!({
                            "success": false,
                            "message": msg,
                        }),
                    );
                }
            }
        } else {
            let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
            if !cfg.is_configured() {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": "Odoo not configured — fill URL and database, or save first",
                    }),
                );
            }
            let credential = match self.resolve_odoo_credential(&table) {
                Some(c) if !c.is_empty() => c,
                _ => {
                    return WsFrame::ok_response(
                        "",
                        json!({
                            "success": false,
                            "message": "No API key or password configured",
                        }),
                    );
                }
            };
            (cfg, credential)
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "message": format!("Connected — {} {}", st.edition, st.version),
                    }),
                )
            }
            Err(e) => {
                warn!("Odoo test connection failed: {e}");
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Connection failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                )
            }
        }
    }

    /// `odoo.discover_schema` — connect with the stored config, introspect the
    /// model/field schema, write a summary to the shared wiki, and return a
    /// lightweight model list to the dashboard. Metadata only — no business
    /// rows are read or returned. Honours the `.scope.toml` policy for the
    /// `odoo` namespace: a locked namespace skips the wiki write (fail-safe)
    /// but still returns the discovered models.
    pub(crate) async fn handle_odoo_discover_schema(&self, params: Value) -> WsFrame {
        let max_models = params
            .get("max_models")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(10, 1000))
            .unwrap_or(300);

        // Build connector from the stored global config (same path as status).
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response(
                "",
                json!({
                    "success": false,
                    "message": "Odoo not configured — fill URL and database, then save first",
                }),
            );
        }
        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": "No API key or password configured",
                    }),
                );
            }
        };

        let conn = match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Odoo discover_schema connect failed: {e}");
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Connection failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                );
            }
        };

        let report = match conn.introspect_schema(max_models).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Odoo introspect_schema failed: {e}");
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Schema scan failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                );
            }
        };

        // Lightweight model list for the UI (no field payload — that lives in
        // the wiki / odoo_schema_fields).
        let models: Vec<Value> = report
            .models
            .iter()
            .map(|m| {
                json!({
                    "model": m.model,
                    "name": m.name,
                    "custom": m.custom,
                    "field_count": m.field_count,
                })
            })
            .collect();

        // Write schema to the shared wiki, honouring the .scope.toml policy.
        let (wiki_written, wiki_note) = self.write_odoo_schema_wiki(&report).await;

        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "models": models,
                "total_models": report.total_models,
                "truncated": report.truncated,
                "wiki_written": wiki_written,
                "wiki_note": wiki_note,
            }),
        )
    }

    /// Persist an introspected schema to the shared wiki as two pages:
    /// `odoo/schema` (context layer — compact summary, auto-injectable) and
    /// `odoo/schema-fields` (deep layer — full field tables for search).
    /// Returns `(written, note)`. Skips the write (fail-safe) when the `odoo`
    /// namespace is locked by `.scope.toml`.
    pub(crate) async fn write_odoo_schema_wiki(&self, report: &duduclaw_odoo::SchemaReport) -> (bool, String) {
        // .scope.toml policy for the `odoo` namespace.
        let scope_path = self
            .home_dir
            .join("shared")
            .join("wiki")
            .join(".scope.toml");
        let scope_table = self.read_config_table(&scope_path).await;
        if let Some(mode) = scp_namespace_mode(&scope_table, "odoo") {
            if mode != "agent_writable" {
                return (
                    false,
                    format!("wiki 'odoo' namespace is '{mode}' — schema not written"),
                );
            }
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        if let Err(e) = store.ensure_scaffold() {
            return (false, format!("wiki scaffold failed: {e}"));
        }

        let summary = render_odoo_schema_summary(report);
        let details = render_odoo_schema_details(report);

        if let Err(e) = store.write_page("odoo/schema.md", &summary) {
            return (false, format!("wiki write failed: {e}"));
        }
        if let Err(e) = store.write_page("odoo/schema-fields.md", &details) {
            // Summary landed; report the partial state honestly.
            return (true, format!("summary written; field detail failed: {e}"));
        }
        (
            true,
            "schema written to wiki (odoo/schema, odoo/schema-fields)".into(),
        )
    }

    /// Build a transient `OdooConfig` + credential from RPC params for the
    /// "test before save" flow. Applies the same validation rules as
    /// `handle_odoo_configure` so the test path can't be used to bypass them.
    ///
    /// Returns `(config, Some(credential))` when params include an api_key /
    /// password, or `(config, None)` when the caller wants the handler to fall
    /// back to the stored credential.
    pub(crate) fn build_test_config_from_params(
        params: &Value,
    ) -> Result<(duduclaw_odoo::OdooConfig, Option<String>), String> {
        // URL — reuse the same SSRF-safe validator as `configure`.
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) if Self::is_safe_odoo_url(u) => u,
            Some(_) => {
                return Err(
                    "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1)".into(),
                );
            }
            _ => return Err("Missing 'url' parameter".into()),
        };

        // Database — alphanumeric + `_` + `-`, max 63 chars.
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
                return Err("Invalid database name (alphanumeric, _, - only, max 63 chars)".into());
            }
            _ => return Err("Missing 'db' parameter".into()),
        };

        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("xmlrpc") => "xmlrpc",
            Some("jsonrpc") | None => "jsonrpc",
            _ => return Err("Invalid protocol: must be 'jsonrpc' or 'xmlrpc'".into()),
        };

        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => return Err("Invalid auth_method: must be 'api_key' or 'password'".into()),
        };

        let username = params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if username.len() > 256 {
            return Err("Username too long (max 256 chars)".into());
        }

        let credential_field = if auth_method == "password" {
            "password"
        } else {
            "api_key"
        };
        let credential = params
            .get(credential_field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        let cfg = duduclaw_odoo::OdooConfig {
            url: url.into(),
            db: db.into(),
            protocol: protocol.into(),
            auth_method: auth_method.into(),
            username: username.into(),
            ..Default::default()
        };
        Ok((cfg, credential))
    }

    /// Strip identifiers that an attacker could weaponize from a connector
    /// error before forwarding it to the dashboard.
    ///
    /// The reqwest error includes the full URL (with any query string), and may
    /// echo embedded userinfo (`user:pass@host`) or `?api_key=…` query params on
    /// failure. We redact those first, then cap the length. M19: truncation
    /// alone leaked the URL/token on short errors, so scrubbing must happen
    /// regardless of length. We keep the high-level reason so the user can act.
    pub(crate) fn scrub_odoo_error(raw: &str) -> String {
        let scrubbed = scrub_secrets_from_text(raw);
        // Cap to avoid pushing megabyte HTML error pages back to the client.
        const MAX_LEN: usize = 240;
        let mut out = String::with_capacity(scrubbed.len().min(MAX_LEN));
        for ch in scrubbed.chars().take(MAX_LEN) {
            out.push(ch);
        }
        if scrubbed.chars().count() > MAX_LEN {
            out.push_str("…");
        }
        out
    }

    /// Resolve the Odoo credential from config.toml (encrypted or plaintext).
    ///
    /// Returns `None` if decryption fails — never returns raw ciphertext (CRIT-2).
    pub(crate) fn resolve_odoo_credential(&self, table: &toml::Table) -> Option<String> {
        let odoo_section = table.get("odoo")?.as_table()?;
        let auth_method = odoo_section
            .get("auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("api_key");

        let (enc_field, plain_field) = if auth_method == "password" {
            ("password_enc", "password")
        } else {
            ("api_key_enc", "api_key")
        };

        // Try encrypted first
        if let Some(enc_val) = odoo_section
            .get(enc_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Some(key) = crate::config_crypto::load_keyfile_public(&self.home_dir) {
                if let Ok(engine) = duduclaw_security::crypto::CryptoEngine::new(&key) {
                    if let Ok(decrypted) = engine.decrypt_string(enc_val) {
                        return Some(decrypted);
                    }
                }
            }
            // Decryption failed — do NOT return raw ciphertext as credential
            warn!("Failed to decrypt Odoo credential — keyfile may have changed");
            return None;
        }

        // Fallback to plaintext field (legacy / dev environments)
        let plain = odoo_section
            .get(plain_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        if plain.is_some() {
            warn!(
                field = plain_field,
                "Odoo credential stored in plaintext — re-save config to encrypt"
            );
        }
        plain
    }
}
