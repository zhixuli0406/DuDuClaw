//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── MCP Management ──────────────────────────────────────────

    pub(crate) async fn handle_mcp_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, read_mcp_config};

        let agents_dir = self.home_dir.join("agents");
        let mut agents = Vec::new();

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
                let config = match read_mcp_config(&dir) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let servers: Vec<Value> = config
                    .mcp_servers
                    .iter()
                    .map(|(k, v)| {
                        json!({
                            "name": k,
                            "command": v.command,
                            "args": v.args,
                            "env": v.env,
                        })
                    })
                    .collect();
                agents.push(json!({
                    "agent_id": name,
                    "servers": servers,
                }));
            }
        }

        let catalog: Vec<Value> = marketplace_catalog()
            .iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "name": item.name,
                    "description": item.description,
                    "category": item.category,
                    "requires_oauth": item.requires_oauth,
                    "default_def": {
                        "command": item.default_def.command,
                        "args": item.default_def.args,
                        "env": item.default_def.env,
                    },
                    "required_env": item.required_env,
                })
            })
            .collect();

        WsFrame::ok_response("", json!({ "agents": agents, "catalog": catalog }))
    }

    pub(crate) async fn handle_mcp_update(&self, params: &Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{
            McpServerDef, add_server_to_config, remove_server_from_config,
        };

        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "agent_id is required"),
        };
        let action = match params.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return WsFrame::error_response("", "action is required (add/remove)"),
        };
        let server_name = match params.get("server_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "server_name is required"),
        };

        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }

        let agent_dir = self.home_dir.join("agents").join(agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }

        match action {
            "add" => {
                let def: McpServerDef = match params.get("server_def") {
                    Some(v) => match serde_json::from_value(v.clone()) {
                        Ok(d) => d,
                        Err(e) => {
                            return WsFrame::error_response(
                                "",
                                &format!("Invalid server_def: {e}"),
                            );
                        }
                    },
                    None => {
                        return WsFrame::error_response(
                            "",
                            "server_def is required for add action",
                        );
                    }
                };
                // Same fail-closed gate as mcp.import.install — spawning an MCP
                // server runs a real process, so a shell/downloader definition
                // is rejected regardless of which RPC carries it.
                let scan = crate::mcp_scan::scan_mcp_server_def(server_name, &def);
                if !scan.passed {
                    warn!(server = %server_name, risk = ?scan.risk_level, "mcp.update add DENIED by security scan");
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
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || add_server_to_config(&ad, &sn, &def))
                    .await
                {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            "remove" => {
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || remove_server_from_config(&ad, &sn)).await
                {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            _ => WsFrame::error_response(
                "",
                &format!("Unknown action: {action}. Use 'add' or 'remove'"),
            ),
        }
    }
}
