//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── W2-2 (E1/E2, D-C1): channel behavior + access control ────────────
    //
    // Reads/writes go through the exact same `ChannelSettingsManager` the
    // `channel_config`/`channel_config_list` MCP tools use
    // (`duduclaw-cli::mcp::handle_channel_config`) — a fresh instance is
    // opened per call (as the MCP handlers already do; the underlying SQLite
    // file, not the in-process cache, is the single state authority) so a
    // channel-side `/pair`-adjacent change and a dashboard edit are never two
    // different stores. Validation (`CONFIG_KEYS`/`DASHBOARD_ACCESS_KEYS`/
    // `validate_scope_id`/`validate_setting_value`) is centralized in
    // `crate::channel_settings` for the same reason.

    /// Open a fresh `ChannelSettingsManager` against the shared session DB.
    pub(crate) fn open_channel_settings(
        &self,
    ) -> Result<crate::channel_settings::ChannelSettingsManager, String> {
        crate::channel_settings::ChannelSettingsManager::from_session_db(
            &self.home_dir.join("sessions.db"),
        )
    }

    /// Validate `{ channel, scope_id? }` params shared by all six methods.
    /// `force_global`: access-control keys only ever take effect at the
    /// `"global"` scope (`channel_reply::check_user_access_gate`/
    /// `is_channel_admin` both hardcode `"global"`) — accepting a different
    /// scope_id for `access_get`/`access_set` would silently write settings
    /// nothing ever reads, so those two force it instead of trusting input.
    pub(crate) fn parse_channel_scope(
        &self,
        params: &Value,
        force_global: bool,
    ) -> std::result::Result<(String, String), WsFrame> {
        let channel = match params.get("channel").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => {
                return Err(WsFrame::error_response(
                    "",
                    "Missing required parameter: channel",
                ));
            }
        };
        if !crate::channel_settings::VALID_CHANNEL_TYPES.contains(&channel.as_str()) {
            return Err(WsFrame::error_response(
                "",
                &format!("Invalid channel type: {channel}"),
            ));
        }
        let scope_id = if force_global {
            "global".to_string()
        } else {
            params
                .get("scope_id")
                .and_then(|v| v.as_str())
                .unwrap_or("global")
                .to_string()
        };
        if let Err(e) = crate::channel_settings::validate_scope_id(&scope_id) {
            return Err(WsFrame::error_response(
                "",
                &format!("Invalid scope_id: {e}"),
            ));
        }
        Ok((channel, scope_id))
    }

    /// `channels.config_get` — read a channel's behavior settings (E1).
    /// Params: `{ channel, scope_id? }` (scope_id defaults to "global").
    /// Response: `{ success, channel, scope_id, settings{}, scopes[] }` —
    /// `scopes` lists other known scopes (e.g. Discord guild ids) for that
    /// channel type so the caller can offer a per-scope override, without
    /// this task committing to a full per-scope editor UI.
    pub(crate) async fn handle_channels_config_get(&self, params: Value) -> WsFrame {
        let (channel, scope_id) = match self.parse_channel_scope(&params, false) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let mgr = match self.open_channel_settings() {
            Ok(m) => m,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open settings store: {e}"));
            }
        };
        let all = mgr.get_all(&channel, &scope_id).await;
        let scopes = mgr.list_scopes(&channel).await;
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "channel": channel,
                "scope_id": scope_id,
                "settings": config_settings_to_json(&all),
                "scopes": scopes,
            }),
        )
    }

    /// `channels.config_set` — write a channel's behavior settings (E1).
    /// Params: `{ channel, scope_id?, settings: { <CONFIG_KEYS field>: value, ... } }`.
    /// Partial update: only the fields present in `settings` change; a field
    /// set to JSON `null` clears the override for that scope (falls back to
    /// global / the hardcoded default). Fail-closed: any key outside
    /// `CONFIG_KEYS` rejects the whole call before writing anything.
    /// Response: `{ success, channel, scope_id, changes[] }`.
    pub(crate) async fn handle_channels_config_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let (channel, scope_id) = match self.parse_channel_scope(&params, false) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let settings_obj = match params.get("settings").and_then(|v| v.as_object()) {
            Some(o) if !o.is_empty() => o,
            Some(_) => return WsFrame::error_response("", "settings object is empty"),
            None => {
                return WsFrame::error_response(
                    "",
                    "Missing required parameter: settings (object)",
                );
            }
        };
        for key in settings_obj.keys() {
            if !crate::channel_settings::CONFIG_KEYS.contains(&key.as_str()) {
                return WsFrame::error_response("", &format!("Unknown setting key: {key}"));
            }
        }
        let mgr = match self.open_channel_settings() {
            Ok(m) => m,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open settings store: {e}"));
            }
        };
        let changes = match self
            .apply_channel_settings(&mgr, &channel, &scope_id, settings_obj)
            .await
        {
            Ok(c) => c,
            Err(frame) => return frame,
        };

        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "channel_config_set",
                &channel,
                duduclaw_security::audit::Severity::Info,
                json!({
                    "scope_id": scope_id,
                    "changes": changes,
                    "actor": ctx.user_id,
                    "source": "dashboard",
                }),
            ),
        );
        crate::dashboard_feedback::emit(
            &self.home_dir,
            crate::dashboard_feedback::EV_CHANNEL_CONFIG_CHANGED,
            json!({ "action": "config_set", "channel": channel, "scope_id": scope_id }),
        )
        .await;

        info!(
            channel = channel.as_str(),
            scope_id = scope_id.as_str(),
            "channels.config_set completed"
        );
        WsFrame::ok_response(
            "",
            json!({ "success": true, "channel": channel, "scope_id": scope_id, "changes": changes }),
        )
    }

    /// `channels.access_get` — read a channel's access-control settings (E2):
    /// `require_pairing` / `allowed_users` / `blocked_users` / `admin_users`.
    /// Always the `"global"` scope (see `parse_channel_scope`). Params:
    /// `{ channel }`. Response: `{ success, channel, settings{} }`.
    pub(crate) async fn handle_channels_access_get(&self, params: Value) -> WsFrame {
        let (channel, scope_id) = match self.parse_channel_scope(&params, true) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let mgr = match self.open_channel_settings() {
            Ok(m) => m,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open settings store: {e}"));
            }
        };
        let all = mgr.get_all(&channel, &scope_id).await;
        WsFrame::ok_response(
            "",
            json!({ "success": true, "channel": channel, "settings": access_settings_to_json(&all) }),
        )
    }

    /// `channels.access_set` — write a channel's access-control settings (E2).
    /// Params: `{ channel, settings: { <DASHBOARD_ACCESS_KEYS field>: value, ... } }`.
    /// Dashboard-only write path for `admin_users` (who may press `!STOP`
    /// in-channel) — the `channel_config` MCP tool refuses that key by design
    /// (`MCP_ACCESS_KEYS` excludes it), so an agent can never self-grant admin.
    /// Fail-closed on unknown keys, same partial-update / `null`-clears
    /// semantics as `config_set`. Response: `{ success, channel, changes[] }`.
    pub(crate) async fn handle_channels_access_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let (channel, scope_id) = match self.parse_channel_scope(&params, true) {
            Ok(v) => v,
            Err(frame) => return frame,
        };
        let settings_obj = match params.get("settings").and_then(|v| v.as_object()) {
            Some(o) if !o.is_empty() => o,
            Some(_) => return WsFrame::error_response("", "settings object is empty"),
            None => {
                return WsFrame::error_response(
                    "",
                    "Missing required parameter: settings (object)",
                );
            }
        };
        for key in settings_obj.keys() {
            if !crate::channel_settings::DASHBOARD_ACCESS_KEYS.contains(&key.as_str()) {
                return WsFrame::error_response("", &format!("Unknown setting key: {key}"));
            }
        }
        let mgr = match self.open_channel_settings() {
            Ok(m) => m,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open settings store: {e}"));
            }
        };
        let changes = match self
            .apply_channel_settings(&mgr, &channel, &scope_id, settings_obj)
            .await
        {
            Ok(c) => c,
            Err(frame) => return frame,
        };

        // `admin_users` controls `!STOP` — always Warning severity regardless
        // of which other keys were touched in the same call.
        let severity = if settings_obj.contains_key(crate::channel_settings::keys::ADMIN_USERS) {
            duduclaw_security::audit::Severity::Warning
        } else {
            duduclaw_security::audit::Severity::Info
        };
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "channel_access_set",
                &channel,
                severity,
                json!({
                    "scope_id": scope_id,
                    "changes": changes,
                    "actor": ctx.user_id,
                    "source": "dashboard",
                }),
            ),
        );
        crate::dashboard_feedback::emit(
            &self.home_dir,
            crate::dashboard_feedback::EV_CHANNEL_CONFIG_CHANGED,
            json!({ "action": "access_set", "channel": channel, "scope_id": scope_id }),
        )
        .await;

        info!(channel = channel.as_str(), "channels.access_set completed");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "channel": channel, "scope_id": scope_id, "changes": changes }),
        )
    }

    /// Shared partial-update loop for `config_set`/`access_set`: validates
    /// every value before writing anything (so a bad field in a multi-field
    /// call can't leave a half-applied setting), converts to the store's
    /// string encoding, and returns the applied changes for the audit event.
    pub(crate) async fn apply_channel_settings(
        &self,
        mgr: &crate::channel_settings::ChannelSettingsManager,
        channel: &str,
        scope_id: &str,
        settings_obj: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Vec<Value>, WsFrame> {
        // Validate-then-write: reject the whole call on the first bad value
        // instead of applying a valid prefix and erroring on the rest.
        let mut encoded: Vec<(&str, Option<String>)> = Vec::with_capacity(settings_obj.len());
        for (key, value) in settings_obj {
            if value.is_null() {
                encoded.push((key.as_str(), None));
                continue;
            }
            let value_str = match json_value_to_setting_string(key, value) {
                Ok(s) => s,
                Err(e) => return Err(WsFrame::error_response("", &e)),
            };
            if let Err(e) = crate::channel_settings::validate_setting_value(key, &value_str) {
                return Err(WsFrame::error_response(
                    "",
                    &format!("Invalid value for {key}: {e}"),
                ));
            }
            encoded.push((key.as_str(), Some(value_str)));
        }

        let mut changes = Vec::with_capacity(encoded.len());
        for (key, value_str) in encoded {
            match &value_str {
                None => {
                    if let Err(e) = mgr.delete(channel, scope_id, key).await {
                        return Err(WsFrame::error_response(
                            "",
                            &format!("Failed to clear {key}: {e}"),
                        ));
                    }
                }
                Some(v) => {
                    if let Err(e) = mgr.set(channel, scope_id, key, v).await {
                        return Err(WsFrame::error_response(
                            "",
                            &format!("Failed to set {key}: {e}"),
                        ));
                    }
                }
            }
            changes.push(json!({ "key": key, "value": value_str }));
        }
        Ok(changes)
    }
}
