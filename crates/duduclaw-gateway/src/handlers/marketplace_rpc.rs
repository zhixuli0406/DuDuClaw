//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Marketplace ─────────────────────────────────────────────

    /// Build the Marketplace catalog JSON: built-in entries plus
    /// optional user-contributed entries from `~/.duduclaw/marketplace.json`.
    ///
    /// Schema of the optional file:
    /// ```json
    /// { "servers": [ { "id": "...", "name": "...", ... } ] }
    /// ```
    /// Each entry follows the `McpCatalogItem` JSON shape. Invalid files
    /// are skipped with a warning so a malformed user file never breaks
    /// the dashboard.
    pub(crate) async fn handle_marketplace_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{McpCatalogItem, marketplace_catalog};

        let mut servers: Vec<McpCatalogItem> = marketplace_catalog();

        // Merge optional user-contributed catalog entries.
        let user_path = self.home_dir.join("marketplace.json");
        if user_path.exists() {
            match tokio::fs::read_to_string(&user_path).await {
                Ok(content) => {
                    #[derive(serde::Deserialize)]
                    struct UserCatalog {
                        #[serde(default)]
                        servers: Vec<McpCatalogItem>,
                    }
                    match serde_json::from_str::<UserCatalog>(&content) {
                        Ok(user) => {
                            info!(
                                path = %user_path.display(),
                                count = user.servers.len(),
                                "Merged user marketplace catalog"
                            );
                            servers.extend(user.servers);
                        }
                        Err(e) => warn!(
                            path = %user_path.display(),
                            error = %e,
                            "Failed to parse user marketplace.json; skipping"
                        ),
                    }
                }
                Err(e) => warn!(
                    path = %user_path.display(),
                    error = %e,
                    "Failed to read user marketplace.json; skipping"
                ),
            }
        }

        // Build a map of catalog id -> agents that already have it installed.
        // Install writes the catalog `id` as the server key in the agent's
        // `.mcp.json` (see handle_marketplace_install), so a server counts as
        // installed for an agent when that id appears among its mcp_servers.
        let installed_by = self.marketplace_installed_map().await;

        let mut servers_json = match serde_json::to_value(&servers) {
            Ok(Value::Array(arr)) => arr,
            Ok(_) => Vec::new(),
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Failed to serialize marketplace catalog: {e}"),
                );
            }
        };

        // Annotate each server with its backend-derived installed_by list so the
        // dashboard reflects real `.mcp.json` state instead of ephemeral UI state.
        for entry in servers_json.iter_mut() {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let agents = installed_by.get(&id).cloned().unwrap_or_default();
            if let Value::Object(map) = entry {
                map.insert("installed_by".to_string(), json!(agents));
            }
        }

        WsFrame::ok_response("", json!({ "servers": servers_json }))
    }

    /// Scan every agent's `.mcp.json` and return a map of server key (catalog id)
    /// -> sorted list of agent ids that have it installed. Used by the Marketplace
    /// page to render an accurate, reload-safe "installed" state.
    pub(crate) async fn marketplace_installed_map(&self) -> std::collections::HashMap<String, Vec<String>> {
        use duduclaw_agent::mcp_template::read_mcp_config;

        let mut map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let agents_dir = self.home_dir.join("agents");
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = match dir.file_name().and_then(|n| n.to_str()) {
                    Some(n) if !n.starts_with('_') && !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                if let Ok(config) = read_mcp_config(&dir) {
                    for key in config.mcp_servers.keys() {
                        map.entry(key.clone()).or_default().push(name.clone());
                    }
                }
            }
        }
        for agents in map.values_mut() {
            agents.sort();
            agents.dedup();
        }
        map
    }

    /// Built-in catalogue plus the optional user-contributed
    /// `~/.duduclaw/marketplace.json` (unparseable file ⇒ built-in only).
    pub(crate) async fn full_marketplace_catalog(
        &self,
    ) -> Vec<duduclaw_agent::mcp_template::McpCatalogItem> {
        use duduclaw_agent::mcp_template::{McpCatalogItem, marketplace_catalog};

        let mut catalog: Vec<McpCatalogItem> = marketplace_catalog();
        let user_path = self.home_dir.join("marketplace.json");
        if let Ok(content) = tokio::fs::read_to_string(&user_path).await {
            #[derive(serde::Deserialize)]
            struct UserCatalog {
                #[serde(default)]
                servers: Vec<McpCatalogItem>,
            }
            if let Ok(user) = serde_json::from_str::<UserCatalog>(&content) {
                catalog.extend(user.servers);
            }
        }
        catalog
    }

    /// Install a marketplace catalog server into an agent's `.mcp.json`.
    ///
    /// Params: `{ "id": "<catalog id>", "agent_id": "<agent>",
    /// "env"?: { "<NAME>": "<value>" } }` — `env` must hold a non-empty
    /// literal for every name in the item's `required_env`.
    /// Looks the item up in the built-in catalog plus the optional
    /// user-contributed `~/.duduclaw/marketplace.json`, then reuses the
    /// same `add_server_to_config` path as `mcp.update`.
    pub(crate) async fn handle_marketplace_install(&self, params: Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{McpCatalogItem, add_server_to_config, apply_required_env};

        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }

        let catalog: Vec<McpCatalogItem> = self.full_marketplace_catalog().await;

        let item = match catalog.into_iter().find(|c| c.id == id) {
            Some(c) => c,
            None => {
                return WsFrame::error_response(
                    "",
                    &format!("Marketplace server '{id}' not found"),
                );
            }
        };

        // Required env values arrive as `env: { NAME: value }` and are written
        // into `.mcp.json` as literals (owner-only file, never logged). A
        // missing one is refused with the variable names.
        let supplied: std::collections::HashMap<String, String> = match params.get("env") {
            None | Some(Value::Null) => Default::default(),
            Some(Value::Object(map)) => {
                let mut out = std::collections::HashMap::new();
                for (k, v) in map {
                    match v.as_str() {
                        Some(s) => {
                            out.insert(k.clone(), s.to_string());
                        }
                        None => {
                            return WsFrame::error_response(
                                "",
                                &format!("env value for '{}' must be a string", duduclaw_core::truncate_chars(k, 64)),
                            );
                        }
                    }
                }
                out
            }
            Some(_) => return WsFrame::error_response("", "env must be an object of NAME: value"),
        };
        let server_name = item.id.clone();
        let mut supplied = supplied;
        if let Err(e) = super::mcp_rpc::resolve_masked_env(
            &mut supplied,
            super::mcp_rpc::stored_server_env(&agent_dir, &server_name).as_ref(),
        ) {
            return WsFrame::error_response("", &e);
        }
        let def = match apply_required_env(&item.default_def, &item.required_env, &supplied) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // Defense in depth: the user-contributed marketplace.json is plain
        // config on disk — scan the definition it hands us before spawning it
        // into an agent, same fail-closed policy as the import path.
        let scan = crate::mcp_scan::scan_mcp_server_def(&server_name, &def);
        if !scan.passed {
            warn!(server = %server_name, risk = ?scan.risk_level, "marketplace.install DENIED by security scan");
            return WsFrame::error_response(
                "",
                &format!(
                    "Security scan rejected MCP server '{server_name}': risk {:?}: {}",
                    scan.risk_level,
                    scan.findings
                        .iter()
                        .take(5)
                        .map(|f| f.description.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            );
        }
        match tokio::task::spawn_blocking(move || {
            add_server_to_config(&agent_dir, &server_name, &def)
        })
        .await
        {
            Ok(Ok(())) => {
                info!(server = %id, agent = %agent_id, "Marketplace server installed");
                WsFrame::ok_response("", json!({ "success": true, "agent_id": agent_id }))
            }
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
        }
    }
}
