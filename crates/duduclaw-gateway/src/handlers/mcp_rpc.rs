//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── MCP Management ──────────────────────────────────────────

    pub(crate) async fn handle_mcp_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, masked_env, read_mcp_config};

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
                    Err(_) => duduclaw_agent::mcp_template::McpConfig { mcp_servers: Default::default() },
                };
                let mut servers: Vec<Value> = config
                    .mcp_servers
                    .iter()
                    .map(|(k, v)| {
                        // Legacy env may still be literal; new installs store
                        // references. Answer with
                        // set / not_set / reference per name, never the value.
                        json!({
                            "name": k,
                            "command": v.command,
                            "args": v.args,
                            "env": masked_env(&v.env),
                            "source": "mcp_json",
                            "effective_runtimes": ["claude", "codex", "grok", "in_process"],
                            "last_start": crate::mcp_start::last(&self.home_dir, &name, k),
                        })
                    })
                    .collect();
                servers.extend(agent_toml_servers(&dir));
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
                        "env": masked_env(&item.default_def.env),
                    },
                    "required_env": item.required_env,
                    "remote": item.remote,
                })
            })
            .collect();

        WsFrame::ok_response("", json!({ "agents": agents, "catalog": catalog }))
    }

    pub(crate) async fn handle_mcp_probe(&self, params: &Value) -> WsFrame {
        let Some(agent) = params["agent_id"].as_str().filter(|id| is_valid_agent_id(id)) else {
            return WsFrame::error_response("", "Invalid agent_id");
        };
        let Some(server) = params["server_name"].as_str().filter(|name| !name.is_empty()) else {
            return WsFrame::error_response("", "server_name is required");
        };
        let status = crate::mcp_start::probe(&self.home_dir, agent, server).await;
        WsFrame::ok_response("", json!(crate::mcp_start::ProbeResult::from(&status)))
    }

    pub(crate) async fn handle_mcp_update(&self, params: &Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{
            McpServerDef, add_server_to_config, apply_required_env, remove_server_from_config,
        };

        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "agent_id is required"),
        };
        let action = match params.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return WsFrame::error_response("", "action is required (add/edit/remove)"),
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
            "add" | "edit" => {
                let mut def: McpServerDef = match params.get("server_def") {
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
                if let Err(e) = resolve_masked_env(&mut def.env, stored_server_env(&agent_dir, server_name).as_ref()) {
                    return WsFrame::error_response("", &e);
                }
                // A catalogue server installed under its catalogue id must carry
                // supplied values or stored references for every required env name (see
                // `apply_required_env`); refuse with the missing names.
                let def = match self
                    .full_marketplace_catalog()
                    .await
                    .into_iter()
                    .find(|c| c.id == server_name && !c.required_env.is_empty())
                {
                    Some(item) => match apply_required_env(&def, &item.required_env, &Default::default()) {
                        Ok(d) => d,
                        Err(e) => return WsFrame::error_response("", &e),
                    },
                    None => def,
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
                let home = self.home_dir.clone();
                let aid = agent_id.to_string();
                match tokio::task::spawn_blocking(move || {
                    let (installed, _) =
                        crate::remote_mcp::bridge_def::prepare_for_install(&home, &aid, &sn, &def)?;
                    add_server_to_config(&ad, &sn, &installed)?;
                    crate::mcp_start::clear(&home, &aid, Some(&sn));
                    Ok::<_, String>(())
                })
                .await
                {
                    Ok(Ok(())) => {
                        let probe = crate::mcp_start::probe(&self.home_dir, agent_id, server_name).await;
                        WsFrame::ok_response("", json!({ "success": true, "probe": crate::mcp_start::ProbeResult::from(&probe) }))
                    },
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            "remove" => {
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                let home = self.home_dir.clone();
                let aid = agent_id.to_string();
                match tokio::task::spawn_blocking(move || {
                    let was_bridge = duduclaw_agent::mcp_template::read_mcp_config(&ad)
                        .ok()
                        .and_then(|c| c.mcp_servers.get(&sn).cloned())
                        .is_some_and(|d| crate::remote_mcp::bridge_def::is_bridge_def(&d));
                    remove_server_from_config(&ad, &sn)?;
                    crate::mcp_start::clear(&home, &aid, Some(&sn));
                    // Removing a remote server also deletes its stored
                    // credentials (no orphaned tokens), then records the
                    // local-only outcome for the unverified provider grant.
                    let target = if was_bridge {
                        crate::remote_mcp::connect::revocation_target(&home, &aid, &sn)
                    } else {
                        None
                    };
                    if was_bridge && let Err(e) = crate::remote_mcp::connect::disconnect(&home, &aid, &sn, true) {
                        warn!(agent = %aid, server = %sn, error = %e, "remote MCP credentials not removed");
                    }
                    Ok::<_, String>((target, home, aid, sn))
                })
                .await
                {
                    Ok(Ok((target, home, aid, sn))) => {
                        if let Some(t) = target {
                            tokio::spawn(async move {
                                let outcome = crate::remote_mcp::connect::revoke_at_provider(t).await;
                                crate::remote_mcp::audit(
                                    &home,
                                    crate::remote_mcp::AUDIT_TOKEN_REVOCATION,
                                    &aid,
                                    json!({ "agent_id": aid, "server": sn, "outcome": outcome }),
                                );
                            });
                        }
                        let forget_memories = crate::disconnect_forget::after_disconnect(&self.home_dir,
                            &format!("remote_mcp:{agent_id}/{server_name}"), server_name,
                            params.get("forget_memories").and_then(Value::as_bool).unwrap_or(true)).await;
                        WsFrame::ok_response("", json!({ "success": true, "forget_memories":forget_memories }))
                    }
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            _ => WsFrame::error_response(
                "",
                &format!("Unknown action: {action}. Use 'add', 'edit' or 'remove'"),
            ),
        }
    }
}

/// `mcp.list` shows env values as `set` / `not_set` / `reference`. When a
/// dialog echoes those words back, they must never be written as values:
/// `set` keeps the value already stored for that variable on this server (if
/// there is a real one), `not_set` / `reference` are refused with the name.
pub(crate) fn resolve_masked_env(
    env: &mut std::collections::HashMap<String, String>,
    stored: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    for (name, value) in env.iter_mut() {
        match value.trim() {
            "set" => {
                let prev = stored
                    .and_then(|s| s.get(name))
                    .filter(|v| duduclaw_agent::mcp_template::env_value_status(v) == "set");
                match prev {
                    Some(v) => *value = v.clone(),
                    None => {
                        return Err(format!(
                            "{name}: no stored value to keep — enter the real value"
                        ));
                    }
                }
            }
            "not_set" | "reference" => {
                return Err(format!(
                    "{name}: \"{}\" is a status label, not a value — enter the real value",
                    value.trim()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The env of server `name` as stored in the agent's `.mcp.json`, if any.
pub(crate) fn stored_server_env(
    agent_dir: &std::path::Path,
    name: &str,
) -> Option<std::collections::HashMap<String, String>> {
    duduclaw_agent::mcp_template::read_mcp_config(agent_dir)
        .ok()
        .and_then(|c| c.mcp_servers.get(name).map(|d| d.env.clone()))
}

#[cfg(test)]
mod masked_env_tests {
    use super::*;

    #[test]
    fn status_words_are_refused_or_restored() {
        let stored: std::collections::HashMap<String, String> =
            [("K".to_string(), "real-secret".to_string())].into_iter().collect();
        let mut env: std::collections::HashMap<String, String> =
            [("K".to_string(), "set".to_string())].into_iter().collect();
        resolve_masked_env(&mut env, Some(&stored)).unwrap();
        assert_eq!(env["K"], "real-secret");
        let mut env: std::collections::HashMap<String, String> =
            [("BROWSERBASE_API_KEY".to_string(), "not_set".to_string())].into_iter().collect();
        let e = resolve_masked_env(&mut env, None).unwrap_err();
        assert!(e.contains("BROWSERBASE_API_KEY"), "{e}");
        let mut env: std::collections::HashMap<String, String> =
            [("K".to_string(), "set".to_string())].into_iter().collect();
        assert!(resolve_masked_env(&mut env, None).is_err());
    }
}

/// Read-only metadata: credentials and headers never enter the RPC payload.
fn agent_toml_servers(dir: &std::path::Path) -> Vec<Value> {
    let Ok(raw) = duduclaw_core::agent_toml::read_effective_text(dir) else { return Vec::new(); };
    let Ok(value) = raw.parse::<toml::Value>() else { return Vec::new(); };
    value.get("mcp").and_then(|m| m.get("external")).and_then(toml::Value::as_array)
        .into_iter().flatten().enumerate().map(|(index, entry)| {
            let name = entry.get("name").and_then(toml::Value::as_str)
                .or_else(|| entry.get("preset").and_then(toml::Value::as_str))
                .map(str::to_string).unwrap_or_else(|| format!("external-{}", index + 1));
            json!({"name":name, "command":entry.get("command").and_then(toml::Value::as_str).unwrap_or(""),
                "args":[], "env":{}, "source":"agent_toml", "read_only":true,
                "effective_runtimes":["in_process"],
                "enabled":entry.get("enabled").and_then(toml::Value::as_bool).unwrap_or(true)})
        }).collect()
}
