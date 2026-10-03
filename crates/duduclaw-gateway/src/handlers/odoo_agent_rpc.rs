//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── RFC-21 §2: per-agent Odoo credential isolation ──────────────────────

    /// Read + parse an agent's `agent.toml` into a `toml::Table`.
    pub(crate) async fn read_agent_toml_table(&self, agent_id: &str) -> Result<toml::Table, String> {
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }
        let reg = self.registry.read().await;
        let agent = reg
            .get(agent_id)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
        let path = agent.dir.join("agent.toml");
        drop(reg);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read agent.toml: {e}"))?;
        content
            .parse::<toml::Table>()
            .map_err(|e| format!("Failed to parse agent.toml: {e}"))
    }

    /// `odoo.agent_config_get {agent_id}` — return the agent's `[odoo]` override
    /// block WITHOUT any secret. Credentials are reported as booleans + a masked
    /// placeholder; cleartext / ciphertext is never returned. Absent block →
    /// `{ configured: false }` (fail-safe, not an error).
    pub(crate) async fn handle_odoo_agent_config_get(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let table = match self.read_agent_toml_table(agent_id).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let section = match table.get("odoo").and_then(|v| v.as_table()) {
            Some(s) => s,
            None => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "agent_id": agent_id,
                        "configured": false,
                    }),
                );
            }
        };

        let get_str = |k: &str| {
            section
                .get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        let get_arr = |k: &str| -> Vec<String> {
            section
                .get(k)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let company_ids: Vec<i64> = section
            .get("company_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_integer()).collect())
            .unwrap_or_default();

        // Credential presence — never the value. `_enc` (encrypted or a
        // `secret://` ref) or legacy plaintext both count as "set".
        let api_key_set = section
            .get("api_key_enc")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
            || section
                .get("api_key")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());
        let password_set = section
            .get("password_enc")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
            || section
                .get("password")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());

        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "configured": true,
                "profile": get_str("profile"),
                "url": get_str("url"),
                "db": get_str("db"),
                "username": get_str("username"),
                "allowed_models": get_arr("allowed_models"),
                "unblock_models": get_arr("unblock_models"),
                "allowed_actions": get_arr("allowed_actions"),
                "company_ids": company_ids,
                "api_key_set": api_key_set,
                "api_key": if api_key_set { Some(SECRET_MASK_SET) } else { None },
                "password_set": password_set,
            }),
        )
    }

    /// `odoo.agent_config_set {agent_id, url, db, user/username, api_key,
    /// password, profile, allowed_models, allowed_actions, company_ids}` — write
    /// the agent's `[odoo]` override. api_key/password are AES-256-GCM encrypted
    /// into `*_enc` (cleartext never persisted); the masked placeholder
    /// (`***set***`) is refused as a real secret so a round-trip get→set doesn't
    /// overwrite the stored key. url/db go through the same SSRF/HTTPS/db-name
    /// validators as the global path (via `apply_odoo_to_table`).
    pub(crate) async fn handle_odoo_agent_config_set(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if agent_id.is_empty() || !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        // Build the nested `{ "odoo": {...} }` shape `apply_odoo_to_table`
        // consumes from the flat RPC params. `user` is accepted as an alias for
        // `username`.
        let mut odoo_obj = serde_json::Map::new();
        for key in [
            "url",
            "db",
            "username",
            "api_key",
            "password",
            "profile",
            "allowed_models",
            "unblock_models",
            "allowed_actions",
            "company_ids",
        ] {
            if let Some(v) = params.get(key) {
                odoo_obj.insert(key.to_string(), v.clone());
            }
        }
        if !odoo_obj.contains_key("username") {
            if let Some(v) = params.get("user") {
                odoo_obj.insert("username".to_string(), v.clone());
            }
        }
        if odoo_obj.is_empty() {
            return WsFrame::error_response("", "No Odoo fields to update");
        }
        let wrapped = json!({ "odoo": odoo_obj });
        let home = self.home_dir.clone();

        let mut applied_changes: Vec<String> = Vec::new();
        let result = self
            .update_agent_toml(&agent_id, |table| {
                let changes = apply_odoo_to_table(table, &wrapped, &home)?;
                applied_changes = changes;
                Ok(())
            })
            .await;

        match result {
            Ok(hot_reloaded) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "agent_id": agent_id,
                    "changes": applied_changes,
                    "hot_reloaded": hot_reloaded,
                }),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `odoo.agent_test {agent_id}` — test connectivity using the agent's
    /// effective Odoo config: the global `config.toml [odoo]` overlaid with the
    /// agent's `agent.toml [odoo]` (url/db/username), and the agent's credential
    /// if present, else the global one. Reuses the same SSRF-safe URL validator
    /// as `odoo.configure`. Never writes to disk.
    pub(crate) async fn handle_odoo_agent_test(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let agent_table = match self.read_agent_toml_table(agent_id).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Start from the global config, overlay agent overrides.
        let global_path = self.home_dir.join("config.toml");
        let global_table = self.read_config_table(&global_path).await;
        let mut cfg = duduclaw_odoo::OdooConfig::from_toml(&global_table);

        if let Some(a) = agent_table.get("odoo").and_then(|v| v.as_table()) {
            if let Some(u) = a
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cfg.url = u.to_string();
            }
            if let Some(d) = a
                .get("db")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cfg.db = d.to_string();
            }
            if let Some(un) = a
                .get("username")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cfg.username = un.to_string();
            }
            if let Some(pr) = a
                .get("protocol")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cfg.protocol = pr.to_string();
            }

            if let Some(am) = a
                .get("auth_method")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cfg.auth_method = am.to_string();
            }
        }
        // v1.68.0: JSON-RPC only, same rule as `odoo.configure` — whether
        // `xmlrpc` came from the agent override or the global config.
        if cfg.protocol == "xmlrpc" {
            return WsFrame::error_response(
                "",
                "XML-RPC is not supported: DuDuClaw connects to Odoo over JSON-RPC only",
            );
        }

        if !cfg.is_configured() {
            return WsFrame::ok_response(
                "",
                json!({
                    "success": false,
                    "message": "Odoo not configured for this agent (no url/db — set an override or configure the global Odoo first)",
                }),
            );
        }
        // Fail-closed SSRF/HTTPS re-check on the EFFECTIVE url actually dialed.
        if !Self::is_safe_odoo_url(&cfg.url) {
            return WsFrame::ok_response(
                "",
                json!({
                    "success": false,
                    "message": "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1) and must not target a private/reserved host",
                }),
            );
        }

        // Credential: prefer the agent's, else the global one.
        let credential = self
            .resolve_odoo_credential(&agent_table)
            .filter(|s| !s.is_empty())
            .or_else(|| self.resolve_odoo_credential(&global_table))
            .filter(|s| !s.is_empty());
        let credential = match credential {
            Some(c) => c,
            None => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": "No API key/password — set one on this agent or on the global Odoo config",
                    }),
                );
            }
        };

        match duduclaw_odoo::OdooConnector::connect(&cfg, &credential).await {
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
                warn!("Odoo agent test connection failed: {e}");
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

    /// Mask sensitive values (tokens, secrets, keys) in a TOML table.
    ///
    /// Recurses into nested tables, arrays, and arrays-of-tables (e.g.
    /// `[[accounts]]`) so a plaintext `oauth_token` living inside an
    /// array-of-tables can't slip through unmasked (WP-H1: previously only
    /// `toml::Value::Table` was recursed into — `toml::Value::Array` was a
    /// dead end, so `[[accounts]]` entries, the standard shape for
    /// multi-account config, were never masked and their plaintext
    /// `oauth_token` was readable verbatim via the `system.config` RPC).
    /// Mask secrets that live as **table key names** rather than as values.
    ///
    /// `[mcp_keys]` stores each API key as the section name
    /// (`[mcp_keys."ddc_prod_…"]`) with only metadata as the value, so
    /// [`Self::mask_sensitive_fields`] — which can only ever rewrite values —
    /// rendered every internal MCP key verbatim through `system.config`
    /// (credentials-doctrine design §1.4; that key is the gateway's own
    /// admin-scope credential). Key names are rewritten with the same
    /// `mask_mcp_key` dialect the dedicated `mcp_keys.list` RPC already uses,
    /// so the two admin surfaces agree instead of contradicting each other.
    ///
    /// Masked names can collide (two keys sharing a prefix); a numeric suffix
    /// keeps the rendered TOML valid and the entry count honest rather than
    /// silently dropping a row.
    pub(crate) fn mask_keyed_secret_tables(table: &mut toml::Table) {
        let Some(mcp_keys) = table.get_mut("mcp_keys").and_then(|v| v.as_table_mut()) else {
            return;
        };
        let original = std::mem::take(mcp_keys);
        for (key, value) in original {
            let base = mask_mcp_key(&key);
            let mut name = base.clone();
            let mut n = 2;
            while mcp_keys.contains_key(&name) {
                name = format!("{base}#{n}");
                n += 1;
            }
            mcp_keys.insert(name, value);
        }
    }

    pub(crate) fn mask_sensitive_fields(table: &mut toml::Table) {
        let sensitive_patterns = ["token", "secret", "key", "password"];
        for (key, value) in table.iter_mut() {
            let is_sensitive = sensitive_patterns
                .iter()
                .any(|p| key.to_lowercase().contains(p));
            Self::mask_toml_value(value, is_sensitive);
        }
    }

    /// Recursive helper for [`Self::mask_sensitive_fields`]. `is_sensitive`
    /// reflects whether the *containing key* matched a sensitive pattern —
    /// it gates whether a scalar (or array of scalars) directly under that
    /// key gets masked. Tables and arrays-of-tables are always recursed
    /// into regardless of `is_sensitive`: each nested table judges its own
    /// keys independently one level down, which is what lets `[[accounts]]`
    /// entries get their `oauth_token` masked even though the `accounts`
    /// key itself isn't sensitive.
    pub(crate) fn mask_toml_value(value: &mut toml::Value, is_sensitive: bool) {
        match value {
            toml::Value::String(s) if is_sensitive && !s.is_empty() => {
                // Fully mask sensitive values — do NOT leak any prefix chars (MCP-M7)
                *s = "********".to_string();
            }
            toml::Value::Table(t) => Self::mask_sensitive_fields(t),
            toml::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    Self::mask_toml_value(v, is_sensitive);
                }
            }
            _ => {}
        }
    }
}
