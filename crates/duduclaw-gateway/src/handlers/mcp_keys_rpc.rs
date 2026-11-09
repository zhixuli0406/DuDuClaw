//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_mcp_oauth_grants_list(&self) -> WsFrame {
        let home = self.home_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            duduclaw_core::mcp_oauth_store::load_store(&home)?.active_grants(&home)
        }).await;
        match result {
            Ok(Ok(grants)) => WsFrame::ok_response("", json!({ "grants": grants })),
            _ => WsFrame::error_response("", "OAuth grants or key configuration unavailable"),
        }
    }

    pub(crate) async fn audit_mcp_oauth_revoke(
        &self, ctx: &UserContext, id: Option<&str>, outcome: &str,
        grant: Option<&duduclaw_core::mcp_oauth_store::Grant>,
    ) -> bool {
        let event = duduclaw_security::audit::AuditEvent::new(
            "mcp_oauth_grant_revoke", "dashboard", duduclaw_security::audit::Severity::Info,
            json!({
                "actor": ctx.user_id, "family_id": id, "outcome": outcome,
                "client_name": grant.map(|g| g.client_name.as_str()),
                "scopes": grant.map(|g| g.scope.split_whitespace().collect::<Vec<_>>()),
            }),
        );
        let home = self.home_dir.clone();
        matches!(tokio::task::spawn_blocking(move || {
            duduclaw_security::audit::try_append_audit_event(&home, &event)
        }).await, Ok(Ok(())))
    }

    pub(crate) async fn handle_mcp_oauth_grants_revoke(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let id = params.get("id").and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok()).map(|id| id.to_string());
        let Some(id) = id else {
            if !self.audit_mcp_oauth_revoke(ctx, None, "invalid_request", None).await {
                return WsFrame::error_response("", "OAuth revocation refused; audit unavailable");
            }
            return WsFrame::error_response("", "Valid grant id required");
        };
        let home = self.home_dir.clone();
        let target = id.clone();
        let result = tokio::task::spawn_blocking(move || {
            duduclaw_core::mcp_oauth_store::update_store(&home, |store| {
                let grant = store.grants.get(&target).cloned();
                let revoked = store.revoke_family(&target);
                (revoked, grant)
            })
        }).await;
        match result {
            Ok(Ok((revoked, grant))) => {
                let outcome = if revoked { "revoked" } else { "not_found" };
                if !self.audit_mcp_oauth_revoke(ctx, Some(&id), outcome, grant.as_ref()).await {
                    return WsFrame::error_response("", "OAuth revocation processed; audit unavailable");
                }
                WsFrame::ok_response("", json!({ "success": true, "revoked": revoked }))
            }
            _ => {
                self.audit_mcp_oauth_revoke(ctx, Some(&id), "store_unavailable", None).await;
                WsFrame::error_response("", "OAuth grant store unavailable")
            }
        }
    }

    // ── MK: global [mcp_keys] in config.toml ──────────────────────────────────

    /// `mcp_keys.list` — list MCP API keys. NEVER returns cleartext keys; each
    /// entry carries a masked preview. Response:
    /// `{ keys: [{ masked, client_id, is_external, created_at, scopes[],
    /// rotate_recommended }] }`.
    pub(crate) async fn handle_mcp_keys_list(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let mut out: Vec<Value> = Vec::new();
        if let Some(keys) = table.get("mcp_keys").and_then(|v| v.as_table()) {
            for (key, val) in keys {
                let t = match val.as_table() {
                    Some(t) => t,
                    None => continue,
                };
                let client_id = t.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
                let is_external = t
                    .get("is_external")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let created_at = t.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                let scopes: Vec<String> = t
                    .get("scopes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                // 30-day rotation reminder (MK.4).
                let rotate_recommended = chrono::DateTime::parse_from_rfc3339(created_at)
                    .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_days() >= 30)
                    .unwrap_or(false);
                out.push(json!({
                    "masked": mask_mcp_key(key),
                    "client_id": client_id,
                    "is_external": is_external,
                    "created_at": created_at,
                    "scopes": scopes,
                    "rotate_recommended": rotate_recommended,
                }));
            }
        }
        WsFrame::ok_response("", json!({ "keys": out }))
    }

    /// `mcp_keys.create` — generate a new MCP API key. Returns the cleartext key
    /// ONCE (it is never recoverable afterwards). Params:
    /// `{ client_id, env?: prod|staging|dev, is_external?, scopes[] }`.
    /// Response: `{ success, key (cleartext, once), masked, client_id,
    /// is_external, created_at, scopes[] }`.
    pub(crate) async fn handle_mcp_keys_create(&self, params: Value) -> WsFrame {
        let client_id = match params
            .get("client_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            Some(c) if !c.is_empty() && c.len() <= 128 => c.to_string(),
            _ => {
                return WsFrame::error_response("", "Missing or invalid 'client_id' (1-128 chars)");
            }
        };
        let env = match params.get("env").and_then(|v| v.as_str()) {
            Some("prod") | None => "prod",
            Some("staging") => "staging",
            Some("dev") => "dev",
            Some(other) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid env '{other}'. Valid: prod, staging, dev"),
                );
            }
        };
        let is_external = params
            .get("is_external")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // v1.81 P1e: an external key's client id is never an ephemeral
        // sub-agent id (`eph-…`), which other gates resolve to its parent
        // employee (`personal_visibility::acting_employee`).
        if let Some(refusal) = external_client_id_refusal(is_external, &client_id) {
            return WsFrame::error_response("", refusal);
        }

        // Validate scopes against the known scope list (MK.4).
        let scopes_arr = match params.get("scopes").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => {
                return WsFrame::error_response("", "Missing 'scopes' (array of scope strings)");
            }
        };
        let mut scopes: Vec<String> = Vec::with_capacity(scopes_arr.len());
        for s in scopes_arr {
            let s = match s.as_str() {
                Some(s) => s.trim(),
                None => return WsFrame::error_response("", "scopes entries must be strings"),
            };
            if !KNOWN_MCP_SCOPES.contains(&s) {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "Unknown scope '{s}'. Valid: {}",
                        KNOWN_MCP_SCOPES.join(", ")
                    ),
                );
            }
            if !scopes.contains(&s.to_string()) {
                scopes.push(s.to_string());
            }
        }
        if scopes.is_empty() {
            return WsFrame::error_response("", "At least one scope is required");
        }

        let key = generate_mcp_key(env);
        // Defence-in-depth: ensure the generated key matches the canonical format.
        if !is_valid_mcp_key_format(&key) {
            return WsFrame::error_response(
                "",
                "Internal error: generated key failed format check",
            );
        }
        let created_at = Utc::now().to_rfc3339();

        let config_path = self.home_dir.join("config.toml");
        let (mut table, config_snapshot) = match self.read_toml_write_snapshot(&config_path).await {
            Ok(snapshot) => snapshot,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mcp_keys = table
            .entry("mcp_keys")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let mcp_keys = match mcp_keys.as_table_mut() {
            Some(t) => t,
            None => {
                return WsFrame::error_response("", "Invalid [mcp_keys] section in config.toml");
            }
        };
        let mut entry = toml::map::Map::new();
        entry.insert("client_id".into(), toml::Value::String(client_id.clone()));
        entry.insert("is_external".into(), toml::Value::Boolean(is_external));
        entry.insert("created_at".into(), toml::Value::String(created_at.clone()));
        entry.insert(
            "scopes".into(),
            toml::Value::Array(
                scopes
                    .iter()
                    .map(|s| toml::Value::String(s.clone()))
                    .collect(),
            ),
        );
        mcp_keys.insert(key.clone(), toml::Value::Table(entry));

        if let Err(e) = self.atomic_write_toml_snapshot(&config_path, &table, config_snapshot).await {
            return WsFrame::error_response("", &e);
        }
        info!(
            client_id = client_id.as_str(),
            env, "mcp_keys.create completed"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                // Returned exactly once — the gateway never stores or echoes it again.
                "key": key,
                "masked": mask_mcp_key(&key),
                "client_id": client_id,
                "is_external": is_external,
                "created_at": created_at,
                "scopes": scopes,
                "message": "Store this key now — it cannot be retrieved again.",
            }),
        )
    }

    /// `mcp_keys.revoke` — remove an `[mcp_keys.<key>]` entry. Params:
    /// `{ key }` (the full cleartext key). Response: `{ success, revoked }`.
    pub(crate) async fn handle_mcp_keys_revoke(&self, params: Value) -> WsFrame {
        let key = match params.get("key").and_then(|v| v.as_str()).map(str::trim) {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => return WsFrame::error_response("", "Missing 'key' parameter"),
        };
        let config_path = self.home_dir.join("config.toml");
        let (mut table, config_snapshot) = match self.read_toml_write_snapshot(&config_path).await {
            Ok(snapshot) => snapshot,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let removed = match table.get_mut("mcp_keys").and_then(|v| v.as_table_mut()) {
            Some(keys) => keys.remove(&key).is_some(),
            None => false,
        };
        if !removed {
            return WsFrame::error_response("", "Key not found");
        }
        if let Err(e) = self.atomic_write_toml_snapshot(&config_path, &table, config_snapshot).await {
            return WsFrame::error_response("", &e);
        }
        info!("mcp_keys.revoke completed");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "revoked": mask_mcp_key(&key) }),
        )
    }
}

/// Shown when an external key would be issued under an ephemeral sub-agent id.
pub(crate) const EXTERNAL_EPHEMERAL_ID: &str = "外部 MCP 金鑰的用戶端代號不能以 eph- 開頭（那是臨時子代理的代號）。 / An \
    external MCP key's client id must not be an ephemeral sub-agent id (eph-…).";

/// v1.81 P1e: an external key may not carry an ephemeral sub-agent id: any
/// client id starting `eph-` (case-insensitive, the prefix of
/// `crate::ephemeral::is_ephemeral_id`) is refused. Every mode.
pub(crate) fn external_client_id_refusal(is_external: bool, client_id: &str) -> Option<&'static str> {
    let lowered = client_id.trim().to_ascii_lowercase();
    (is_external && lowered.starts_with("eph-")).then_some(EXTERNAL_EPHEMERAL_ID)
}
