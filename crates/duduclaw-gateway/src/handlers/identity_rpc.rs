//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── IDR: [identity] in config.toml (RFC-21 §1 dashboard surface) ──────────

    /// `identity.config_get` — read config.toml `[identity]` (provider selector
    /// + Notion settings). The Notion api key is MASKED. Adds `wiki_cache.people_dir`
    /// for display. Response never contains a secret or a `null`.
    pub(crate) async fn handle_identity_config_get(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let mut resp = identity_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            let dir = self
                .home_dir
                .join("shared")
                .join("wiki")
                .join("identity")
                .join("people");
            obj.insert(
                "wiki_cache".into(),
                json!({ "people_dir": dir.to_string_lossy() }),
            );
        }
        WsFrame::ok_response("", resp)
    }

    /// `identity.config_set` — atomic write of config.toml `[identity]`.
    /// Params (all optional, partial): `{ provider, notion: { database_id,
    /// refresh_seconds, api_key (write-only, encrypted; '' clears), field_map } }`.
    /// The masked placeholder is never persisted as a real secret. Response:
    /// `{ success, changes[] }`.
    pub(crate) async fn handle_identity_config_set(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        let mut changes = Vec::new();
        if let Err(e) = apply_identity_to_table(&mut table, &params, &mut changes) {
            return WsFrame::error_response("", &e);
        }

        // Notion api key secret: encrypt → `api_key_enc`, never store cleartext.
        // An empty string clears it; the masked placeholder is refused so a
        // dashboard echo can't overwrite the real secret with "***set***".
        if let Some(api_key) = params
            .get("notion")
            .and_then(|v| v.as_object())
            .and_then(|n| n.get("api_key"))
            .and_then(|v| v.as_str())
        {
            if api_key != SECRET_MASK_SET {
                let idt = match table
                    .entry("identity")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                {
                    Some(t) => t,
                    None => return WsFrame::error_response("", "Invalid [identity] section"),
                };
                let nt = match idt
                    .entry("notion")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                {
                    Some(t) => t,
                    None => {
                        return WsFrame::error_response("", "Invalid [identity.notion] section");
                    }
                };
                nt.remove("api_key");
                if api_key.is_empty() {
                    nt.remove("api_key_enc");
                    changes.push("identity.notion.api_key cleared".to_string());
                } else if let Some(enc) =
                    crate::config_crypto::encrypt_value(api_key, &self.home_dir)
                {
                    nt.insert("api_key_enc".into(), toml::Value::String(enc));
                    changes.push("identity.notion.api_key = [ENCRYPTED]".to_string());
                } else {
                    return WsFrame::error_response(
                        "",
                        "Failed to encrypt identity.notion.api_key",
                    );
                }
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid identity fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "identity.config_set completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }

    /// Build the identity provider from config.toml `[identity]` — the same
    /// `duduclaw_identity` provider trait the `identity_resolve` MCP tool uses.
    /// Falls back to the wiki cache (fail-safe) when Notion is selected but not
    /// fully configured. Returns `(provider, provider_label)`.
    ///
    /// G5 (2026-09 feature audit): the selection logic moved to
    /// [`crate::identity_provider::build_identity_provider`] so the channel
    /// `<sender>` path and the MCP tool get the same answer this RPC does —
    /// both used to hard-code the wiki cache.
    pub(crate) async fn build_identity_provider(
        &self,
    ) -> (
        std::sync::Arc<dyn duduclaw_identity::IdentityProvider>,
        String,
    ) {
        crate::identity_provider::build_identity_provider(&self.home_dir).await
    }

    /// `identity.resolve` — resolve an identifier (email / channel user id) to
    /// its canonical person via the configured identity provider (same trait as
    /// the `identity_resolve` MCP tool). Params: `{ identifier, channel? }`
    /// (channel defaults to "email"). Response: `{ found, provider, channel,
    /// is_project_member, person? }`. A miss is `found: false`, not an error.
    pub(crate) async fn handle_identity_resolve(&self, params: Value) -> WsFrame {
        let identifier = match params.get("identifier").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => return WsFrame::error_response("", "Missing 'identifier' parameter"),
        };
        let channel_str = params
            .get("channel")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("email");
        let channel = duduclaw_identity::ChannelKind::parse_wire(channel_str);

        let (provider, provider_label) = self.build_identity_provider().await;
        match provider
            .resolve_by_channel(channel.clone(), &identifier)
            .await
        {
            Ok(Some(person)) => {
                let is_member = !person.project_ids.is_empty();
                let person_json = serde_json::to_value(&person).unwrap_or_else(|_| json!({}));
                WsFrame::ok_response(
                    "",
                    json!({
                        "found": true,
                        "provider": provider_label,
                        "channel": channel.as_wire(),
                        "is_project_member": is_member,
                        "person": person_json,
                    }),
                )
            }
            Ok(None) => WsFrame::ok_response(
                "",
                json!({
                    "found": false,
                    "provider": provider_label,
                    "channel": channel.as_wire(),
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("Identity provider error: {e}")),
        }
    }
}
