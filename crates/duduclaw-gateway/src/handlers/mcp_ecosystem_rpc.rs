//! MCP Registry search / install and native remote MCP connections
//! (`mcp.registry_search`, `mcp.registry_install`, `mcp.remote_connect`,
//! `mcp.remote_complete`, `mcp.remote_status`, `mcp.remote_disconnect`).
//!
//! Registry installs reuse the existing install path end to end: the fetched
//! `server.json` goes through `parse_mcp_manifest_value`, then
//! `handle_mcp_import_install` (Admin: re-scan, write through the locked
//! `.mcp.json` writer) or `handle_mcp_install_request` (others: scan, then the
//! manager → admin approval chain). Nothing here writes `.mcp.json` except
//! [`install_remote_entry`], which writes the bridge entry for a server an
//! Admin has just connected.

#[allow(unused_imports)]
use super::*;

use crate::remote_mcp::{self, bridge_def, connect, store};

/// Write (or confirm) the `.mcp.json` bridge entry for a connected remote
/// server. Refuses to replace a different server already installed under the
/// same name. Same scan as every other install.
pub(crate) fn install_remote_entry(home: &Path, agent_id: &str, server: &str) -> Result<(), String> {
    use duduclaw_agent::mcp_template::{add_server_to_config, read_mcp_config};
    let agent_dir = home.join("agents").join(agent_id);
    if !agent_dir.is_dir() {
        return Err(format!("Agent '{agent_id}' not found"));
    }
    let def = bridge_def::installed_def(home, agent_id, server)?;
    if let Some(existing) = read_mcp_config(&agent_dir)?.mcp_servers.get(server) {
        if !bridge_def::is_bridge_def(existing) {
            return Err(format!(
                "employee '{agent_id}' already has a different MCP server named '{server}'; remove it or pick another name"
            ));
        }
        if existing.command == def.command && existing.args == def.args && existing.env == def.env {
            return Ok(());
        }
    }
    let scan = crate::mcp_scan::scan_mcp_server_def(server, &def);
    if !scan.passed {
        return Err(format!("Security scan rejected the bridge entry: risk {:?}", scan.risk_level));
    }
    add_server_to_config(&agent_dir, server, &def)
}

/// [`install_remote_entry`] for the HTTP callback in `server.rs`.
pub fn install_remote_entry_for_callback(home: &Path, agent_id: &str, server: &str) -> Result<(), String> {
    install_remote_entry(home, agent_id, server)
}

/// After a finished OAuth sign-in (the HTTP callback or a pasted callback
/// URL): write the bridge entry and audit. Both routes call this, so they
/// cannot drift apart.
pub async fn finish_remote_sign_in(home: &Path, done: &connect::Completed) -> Result<(), String> {
    let (h, a, s) = (home.to_path_buf(), done.agent_id.clone(), done.server.clone());
    let installed = tokio::task::spawn_blocking(move || install_remote_entry(&h, &a, &s))
        .await
        .unwrap_or_else(|e| Err(format!("internal error: {e}")));
    remote_mcp::audit(
        home,
        remote_mcp::AUDIT_CONNECTED,
        &done.agent_id,
        json!({ "agent_id": done.agent_id, "server": done.server, "auth": "oauth", "entry_written": installed.is_ok() }),
    );
    installed
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

impl MethodHandler {
    /// `mcp.registry_search { query, cursor? }` — any signed-in user (same as
    /// `mcp.import.fetch`): read-only, fixed host.
    pub(crate) async fn handle_mcp_registry_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let cursor = params.get("cursor").and_then(|v| v.as_str());
        match crate::mcp_registry::search(query, cursor).await {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("MCP Registry search failed: {e}")),
        }
    }

    /// `mcp.registry_install { name, version?, agent_id, remote?, server_name?,
    /// env?, add_to_catalog? }`. Admin installs directly; anyone else files an
    /// install request (operator access to the employee required there).
    pub(crate) async fn handle_mcp_registry_install(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(name) = str_param(&params, "name") else {
            return WsFrame::error_response("", "Missing 'name' parameter");
        };
        let name = name.to_string();
        let version = str_param(&params, "version").map(str::to_string);
        let agent_id = match str_param(&params, "agent_id") {
            Some(a) if is_valid_agent_id(a) => a.to_string(),
            Some(_) => return WsFrame::error_response("", "Invalid agent_id"),
            None => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !self.home_dir.join("agents").join(&agent_id).is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }
        let want_remote = params.get("remote").and_then(|v| v.as_bool());

        let (server, meta) = match crate::mcp_registry::fetch_server(&name, version.as_deref()).await {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("MCP Registry: {e}")),
        };
        let hit = crate::mcp_registry::normalize_server(&server, meta.as_ref());
        if let Some(reason) = &hit.reason {
            return WsFrame::error_response("", &format!("This registry server cannot be installed ({reason})"));
        }
        let fallback = Self::sanitize_mcp_name(&name, "registry-server");
        let candidates = match Self::parse_mcp_manifest_value(&server, &fallback) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("Cannot use this server.json: {e}")),
        };
        // Package candidates first (registry order), then native-bridge
        // remotes; the legacy SSE fallback is never offered here.
        let is_remote = |d: &duduclaw_agent::mcp_template::McpServerDef| bridge_def::is_bridge_def(d);
        let is_legacy = |d: &duduclaw_agent::mcp_template::McpServerDef| {
            d.args.iter().any(|a| a == "mcp-remote")
        };
        let pick = match want_remote {
            Some(true) => candidates.iter().find(|(_, d, _)| is_remote(d)),
            Some(false) => candidates.iter().find(|(_, d, _)| !is_remote(d) && !is_legacy(d)),
            None => candidates
                .iter()
                .find(|(_, d, _)| !is_remote(d) && !is_legacy(d))
                .or_else(|| candidates.iter().find(|(_, d, _)| is_remote(d))),
        };
        let Some((cand_name, cand_def, _)) = pick.cloned() else {
            return WsFrame::error_response(
                "",
                if want_remote == Some(true) {
                    "This server has no remote endpoint DuDuClaw can connect to"
                } else {
                    "This server has no npm / PyPI / OCI package DuDuClaw can run"
                },
            );
        };
        let remote = is_remote(&cand_def);
        let server_name = str_param(&params, "server_name").map(str::to_string).unwrap_or(cand_name);
        if !crate::mcp_scan::is_valid_mcp_server_name(&server_name)
            || server_name.to_ascii_lowercase().starts_with("duduclaw")
        {
            return WsFrame::error_response("", "Invalid server_name (A-Za-z0-9._- max 64, not starting with duduclaw)");
        }

        let mut def = cand_def;
        if !remote && let Some(pkg) = crate::mcp_registry::first_supported_package(&server) {
            crate::mcp_registry::pin_package_version(&mut def, pkg);
            // Declared env: required names must be filled; optional names may
            // be. Any other name is refused by `apply_required_env`.
            let declared = crate::mcp_registry::package_env(pkg);
            let supplied: std::collections::HashMap<String, String> = match params.get("env") {
                None | Some(Value::Null) => Default::default(),
                Some(Value::Object(m)) => {
                    let mut out = std::collections::HashMap::new();
                    for (k, v) in m {
                        match v.as_str() {
                            Some(s) if !s.trim().is_empty() => {
                                out.insert(k.clone(), s.to_string());
                            }
                            Some(_) => {}
                            None => return WsFrame::error_response("", &format!("env value for {k} must be a string")),
                        }
                    }
                    out
                }
                Some(_) => return WsFrame::error_response("", "env must be an object"),
            };
            let mut names: Vec<String> = declared.iter().filter(|e| e.required).map(|e| e.name.clone()).collect();
            for e in declared.iter().filter(|e| !e.required) {
                if supplied.contains_key(&e.name) {
                    names.push(e.name.clone());
                }
            }
            def = match duduclaw_agent::mcp_template::apply_required_env(&def, &names, &supplied) {
                Ok(d) => d,
                Err(e) => return WsFrame::error_response("", &e),
            };
        }

        let version_label = if hit.version.is_empty() { "latest".to_string() } else { hit.version.clone() };
        let display = if hit.title.is_empty() { hit.name.clone() } else { hit.title.clone() };
        let description = duduclaw_core::truncate_chars(
            &format!("{display} — {} (MCP Registry {}@{version_label})", hit.description, hit.name),
            300,
        );
        let mut install_params = json!({
            "agent_id": agent_id,
            "server_name": server_name,
            "server_def": def,
            "description": description,
            "source_url": format!("{}/v0/servers/{}", crate::mcp_registry::REGISTRY_BASE, hit.name),
            "add_to_catalog": params.get("add_to_catalog").and_then(|v| v.as_bool()).unwrap_or(false),
        });
        let is_admin = ctx.role == UserRole::Admin;
        let frame = if is_admin {
            self.handle_mcp_import_install(install_params.take()).await
        } else {
            self.handle_mcp_install_request(install_params.take(), ctx).await
        };
        match frame {
            WsFrame::Response { id, ok: true, payload: Some(Value::Object(mut map)), error } => {
                map.insert("registry_name".into(), json!(hit.name));
                map.insert("registry_version".into(), json!(version_label));
                map.insert("remote".into(), json!(remote));
                map.insert("remote_needs_bearer".into(), json!(hit.remote_needs_bearer));
                map.insert("mode".into(), json!(if is_admin { "installed" } else { "requested" }));
                WsFrame::Response { id, ok: true, payload: Some(Value::Object(map)), error }
            }
            other => other,
        }
    }

    /// `mcp.remote_connect { agent_id, name, url?, auth, bearer?,
    /// redirect_origin?, client_id?, client_secret? }` — Admin only.
    pub(crate) async fn handle_mcp_remote_connect(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = str_param(&params, "agent_id").unwrap_or("").to_string();
        let server = str_param(&params, "name").unwrap_or("").to_string();
        if let Err(e) = store::validate_ids(&agent_id, &server) {
            return WsFrame::error_response("", &e);
        }
        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }
        // Refuse before any network call when the name is taken by a
        // different (non-bridge) server.
        if let Ok(cfg) = duduclaw_agent::mcp_template::read_mcp_config(&agent_dir)
            && let Some(existing) = cfg.mcp_servers.get(&server)
            && !bridge_def::is_bridge_def(existing)
        {
            return WsFrame::error_response(
                "",
                &format!("employee '{agent_id}' already has a different MCP server named '{server}'"),
            );
        }
        let Some(auth) = str_param(&params, "auth").and_then(store::AuthKind::parse) else {
            return WsFrame::error_response("", "auth must be one of: oauth, bearer, none");
        };
        let req = connect::ConnectRequest {
            agent_id: agent_id.clone(),
            server: server.clone(),
            url: str_param(&params, "url").map(str::to_string),
            auth,
            bearer: params.get("bearer").and_then(|v| v.as_str()).map(str::to_string),
            redirect_origin: str_param(&params, "redirect_origin").map(str::to_string),
            client_id: str_param(&params, "client_id").map(str::to_string),
            client_secret: params.get("client_secret").and_then(|v| v.as_str()).map(str::to_string),
            allowed_origins: crate::server::allowed_origins_snapshot(),
        };
        let host = req
            .url
            .as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .map(|u| remote_mcp::http::host_label(&u))
            .unwrap_or_default();
        let details = |extra: Value| {
            let mut d = json!({
                "agent_id": agent_id,
                "server": server,
                "auth": auth.as_str(),
                "host": host,
                "actor": ctx.email,
            });
            if let (Some(obj), Value::Object(more)) = (d.as_object_mut(), extra) {
                obj.extend(more);
            }
            d
        };
        match connect::start_connect(&self.home_dir, req).await {
            Ok(connect::ConnectOutcome::Connected) => {
                let (home, a, s) = (self.home_dir.clone(), agent_id.clone(), server.clone());
                match tokio::task::spawn_blocking(move || install_remote_entry(&home, &a, &s)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return WsFrame::error_response("", &format!("Connected, but the .mcp.json entry could not be written: {e}")),
                    Err(e) => return WsFrame::error_response("", &format!("Internal error: {e}")),
                }
                remote_mcp::audit(&self.home_dir, remote_mcp::AUDIT_CONNECTED, &agent_id, details(json!({})));
                info!(agent = %agent_id, server = %server, "remote MCP server connected");
                WsFrame::ok_response("", json!({ "status": "connected", "agent_id": agent_id, "server": server }))
            }
            Ok(connect::ConnectOutcome::Authorize { authorize_url, completion, redirect_uri }) => {
                remote_mcp::audit(
                    &self.home_dir,
                    remote_mcp::AUDIT_CONNECT_STARTED,
                    &agent_id,
                    details(json!({ "completion": completion.as_str() })),
                );
                WsFrame::ok_response(
                    "",
                    json!({
                        "status": "authorize",
                        "authorize_url": authorize_url,
                        // `paste`: the dashboard address cannot receive the
                        // OAuth redirect (plain http on a LAN address); the
                        // operator pastes the address-bar URL into
                        // `mcp.remote_complete`.
                        "completion": completion.as_str(),
                        "redirect_uri": redirect_uri,
                        "expires_in": connect::PENDING_TTL.as_secs(),
                        "agent_id": agent_id,
                        "server": server,
                    }),
                )
            }
            Err(e) => {
                remote_mcp::audit(
                    &self.home_dir,
                    remote_mcp::AUDIT_CONNECT_FAILED,
                    &agent_id,
                    details(json!({ "error": duduclaw_core::truncate_chars(&e, 300) })),
                );
                WsFrame::error_response("", &e)
            }
        }
    }

    /// `mcp.remote_complete { callback_url }` — Admin only. Finishes a
    /// sign-in started with `completion: "paste"` from the address the
    /// browser showed after the provider redirected to the loopback address.
    pub(crate) async fn handle_mcp_remote_complete(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(pasted) = params.get("callback_url").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "callback_url is required");
        };
        match connect::complete_pasted(&self.home_dir, pasted).await {
            Ok(done) => match finish_remote_sign_in(&self.home_dir, &done).await {
                Ok(()) => {
                    info!(agent = %done.agent_id, server = %done.server, actor = %ctx.email, "remote MCP sign-in completed (pasted callback)");
                    WsFrame::ok_response(
                        "",
                        json!({ "status": "connected", "agent_id": done.agent_id, "server": done.server }),
                    )
                }
                Err(e) => WsFrame::error_response(
                    "",
                    &format!("Signed in, but the employee's MCP settings could not be updated: {e}"),
                ),
            },
            Err(e) => {
                warn!(error = %e, "pasted remote MCP callback refused");
                WsFrame::error_response("", &e)
            }
        }
    }

    /// `mcp.remote_status { agent_id? }` — Admin only. No secrets.
    pub(crate) async fn handle_mcp_remote_status(&self, params: Value) -> WsFrame {
        let filter = str_param(&params, "agent_id").map(str::to_string);
        if let Some(a) = &filter
            && !is_valid_agent_id(a)
        {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        let home = self.home_dir.clone();
        match tokio::task::spawn_blocking(move || connect::status_list(&home, filter.as_deref())).await {
            Ok(Ok(list)) => WsFrame::ok_response("", json!({ "servers": list })),
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
        }
    }

    /// `mcp.tool_effects { agent_id }` — Admin only. Every third-party
    /// server whose `tools/list` passed `duduclaw mcp-proxy` or `duduclaw
    /// mcp-remote-bridge` for this employee, each tool with the effect class
    /// and verdict the current policy gives it (recomputed now from the
    /// recorded annotations; `observed_at` says when the list was seen).
    pub(crate) async fn handle_mcp_tool_effects(&self, params: Value) -> WsFrame {
        let agent_id = str_param(&params, "agent_id").unwrap_or("").to_string();
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        let home = self.home_dir.clone();
        match tokio::task::spawn_blocking(move || {
            crate::third_party_tools::load_snapshots(&home, &agent_id)
        })
        .await
        {
            Ok(servers) => WsFrame::ok_response("", json!({ "servers": servers })),
            Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
        }
    }

    /// `mcp.remote_disconnect { agent_id, name, forget? }` — Admin only.
    /// Deletes the stored credentials (local only). `forget: true` also
    /// removes the record and the `.mcp.json` entry.
    pub(crate) async fn handle_mcp_remote_disconnect(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = str_param(&params, "agent_id").unwrap_or("").to_string();
        let server = str_param(&params, "name").unwrap_or("").to_string();
        if let Err(e) = store::validate_ids(&agent_id, &server) {
            return WsFrame::error_response("", &e);
        }
        let forget = params.get("forget").and_then(|v| v.as_bool()).unwrap_or(false);
        let (home, a, s) = (self.home_dir.clone(), agent_id.clone(), server.clone());
        let res = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            let existed = connect::disconnect(&home, &a, &s, forget)?;
            if forget {
                let agent_dir = home.join("agents").join(&a);
                let is_bridge = duduclaw_agent::mcp_template::read_mcp_config(&agent_dir)
                    .ok()
                    .and_then(|c| c.mcp_servers.get(&s).cloned())
                    .is_some_and(|d| bridge_def::is_bridge_def(&d));
                if is_bridge {
                    duduclaw_agent::mcp_template::remove_server_from_config(&agent_dir, &s)?;
                }
            }
            Ok(existed)
        })
        .await;
        match res {
            Ok(Ok(existed)) => {
                remote_mcp::audit(
                    &self.home_dir,
                    remote_mcp::AUDIT_DISCONNECTED,
                    &agent_id,
                    json!({ "agent_id": agent_id, "server": server, "forget": forget, "existed": existed, "actor": ctx.email }),
                );
                WsFrame::ok_response("", json!({ "success": true, "existed": existed, "forget": forget }))
            }
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_remote_entry_writes_the_bridge_and_refuses_name_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let agent_dir = home.join("agents").join("a1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        install_remote_entry(home, "a1", "zap").unwrap();
        let cfg = duduclaw_agent::mcp_template::read_mcp_config(&agent_dir).unwrap();
        let def = &cfg.mcp_servers["zap"];
        assert!(bridge_def::is_bridge_def(def));
        // Idempotent.
        install_remote_entry(home, "a1", "zap").unwrap();
        // A different server under the same name is not replaced.
        duduclaw_agent::mcp_template::add_server_to_config(
            &agent_dir,
            "fs",
            &duduclaw_agent::mcp_template::McpServerDef { command: "npx".into(), args: vec!["-y".into(), "x".into()], env: Default::default() },
        )
        .unwrap();
        assert!(install_remote_entry(home, "a1", "fs").is_err());
        assert!(install_remote_entry(home, "nope", "zap").is_err());
    }
}
