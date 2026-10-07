//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Fetch and scan MCP server definitions from a GitHub repo or direct URL.
    ///
    /// Params: `{ "url": "<github repo | blob | raw json url>" }`.
    /// Returns candidates with their individual scan verdicts; nothing is
    /// installed by this call.
    pub(crate) async fn handle_mcp_import_fetch(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.trim().is_empty() => u.trim().to_string(),
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };

        let parsed = match reqwest::Url::parse(url.trim_end_matches('/')) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Invalid URL: {e}")),
        };
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let path = parsed.path().trim_end_matches('/').to_string();

        // Repo root -> try well-known manifest filenames, then README (config
        // snippets in fenced code blocks — how most MCP repos document setup);
        // blob/direct -> one URL (markdown content also gets snippet
        // extraction via manifest_from_text).
        const MANIFEST_FILES: [&str; 5] = [
            ".mcp.json",
            "mcp.json",
            "server.json",
            "mcp-server.json",
            "README.md",
        ];
        let is_github = host == "github.com" || host == "www.github.com";
        let is_gitlab = host == "gitlab.com" || host == "www.gitlab.com";
        let github_repo_root = is_github && !path.contains("/blob/");
        let gitlab_repo_root = is_gitlab && !path.contains("/-/blob/");
        let raw_base: Option<String> = if github_repo_root {
            Some(format!("https://raw.githubusercontent.com{path}/HEAD"))
        } else if gitlab_repo_root {
            Some(format!("https://gitlab.com{path}/-/raw/HEAD"))
        } else {
            None
        };
        let candidates: Vec<String> = if let Some(base) = &raw_base {
            MANIFEST_FILES
                .iter()
                .map(|f| format!("{base}/{f}"))
                .collect()
        } else if is_github {
            vec![format!(
                "https://raw.githubusercontent.com{}",
                path.replacen("/blob/", "/", 1)
            )]
        } else if is_gitlab {
            vec![format!(
                "https://gitlab.com{}",
                path.replacen("/-/blob/", "/-/raw/", 1)
            )]
        } else {
            vec![url.clone()]
        };

        // Fallback server name: last URL path segment (repo name), sanitized.
        let fallback_name: String = path
            .rsplit('/')
            .next()
            .unwrap_or("imported")
            .trim_end_matches(".json")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .take(64)
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let fallback_name = if fallback_name.is_empty() {
            "imported".to_string()
        } else {
            fallback_name
        };

        let mut tried: Vec<String> = Vec::new();
        let mut found: Option<(
            String,
            Vec<(String, duduclaw_agent::mcp_template::McpServerDef, String)>,
        )> = None;

        for candidate in &candidates {
            if let Err(e) = crate::web_fetch::validate_url(candidate) {
                tried.push(format!("URL rejected: {e}"));
                continue;
            }
            let text = match Self::fetch_remote_text(candidate, Self::SKILL_FETCH_MAX_BYTES).await {
                Ok(t) => t,
                Err(e) => {
                    tried.push(format!("{}: {e}", Self::short_candidate_label(candidate)));
                    continue;
                }
            };
            match Self::manifest_from_text(&text, &fallback_name) {
                Ok(servers) => {
                    found = Some((candidate.clone(), servers));
                    break;
                }
                Err(e) => tried.push(format!("{}: {e}", Self::short_candidate_label(candidate))),
            }
        }

        // Last-resort fallback for repo roots: an npm package.json with a
        // `bin` entry is npx-runnable — synthesize `npx -y <pkg>`.
        if found.is_none() {
            if let Some(base) = &raw_base {
                let pkg_url = format!("{base}/package.json");
                match Self::fetch_remote_text(&pkg_url, Self::SKILL_FETCH_MAX_BYTES).await {
                    Ok(text) => match serde_json::from_str::<Value>(&text) {
                        Ok(pkg) => {
                            let pkg_name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if !pkg_name.is_empty() && pkg.get("bin").is_some() {
                                let server_name = Self::sanitize_mcp_name(
                                    pkg_name.rsplit('/').next().unwrap_or(pkg_name),
                                    &fallback_name,
                                );
                                let desc = pkg
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let def = duduclaw_agent::mcp_template::McpServerDef {
                                    command: "npx".into(),
                                    args: vec!["-y".into(), pkg_name.to_string()],
                                    env: std::collections::HashMap::new(),
                                };
                                found = Some((
                                    pkg_url.clone(),
                                    vec![(
                                        server_name,
                                        def,
                                        format!("{desc} (inferred from package.json)"),
                                    )],
                                ));
                            } else {
                                tried.push("package.json: no bin field (not npx-runnable)".into());
                            }
                        }
                        Err(e) => tried.push(format!("package.json: {e}")),
                    },
                    Err(e) => tried.push(format!("package.json: {e}")),
                }
            }
        }

        let Some((resolved, servers)) = found else {
            return WsFrame::error_response(
                "",
                &format!(
                    "No MCP manifest found. Tried: {}",
                    duduclaw_core::truncate_chars(&tried.join(" | "), 600),
                ),
            );
        };

        let servers_json: Vec<Value> = servers
            .iter()
            .map(|(name, def, desc)| {
                let scan = crate::mcp_scan::scan_mcp_server_def(name, def);
                json!({
                    "name": name,
                    "description": desc,
                    "command": def.command,
                    "args": def.args,
                    "env": def.env,
                    "scan": Self::mcp_scan_to_json(&scan),
                    "passed": scan.passed,
                })
            })
            .collect();
        info!(url = %resolved, servers = servers_json.len(), "MCP import manifest fetched and scanned");
        WsFrame::ok_response(
            "",
            json!({
                "source_url": url,
                "resolved_url": resolved,
                "servers": servers_json,
            }),
        )
    }

    /// Trailing path segment of a candidate URL for compact error reporting.
    pub(crate) fn short_candidate_label(url: &str) -> &str {
        url.rsplit('/').next().unwrap_or(url)
    }

    /// Parse manifest text: strict JSON shapes first, then markdown config
    /// snippet extraction (fenced ```json blocks containing `mcpServers`).
    pub(crate) fn manifest_from_text(
        text: &str,
        fallback_name: &str,
    ) -> Result<Vec<(String, duduclaw_agent::mcp_template::McpServerDef, String)>, String> {
        match Self::parse_mcp_manifest(text, fallback_name) {
            Ok(servers) => Ok(servers),
            Err(json_err) => {
                let extracted = Self::extract_mcp_from_markdown(text, fallback_name);
                if extracted.is_empty() {
                    Err(json_err)
                } else {
                    Ok(extracted)
                }
            }
        }
    }

    /// Extract `mcpServers` config snippets from markdown fenced code blocks.
    /// Deduplicates identical (name, command, args) entries — READMEs repeat
    /// the same snippet for Claude Desktop / Cursor / VS Code.
    pub(crate) fn extract_mcp_from_markdown(
        text: &str,
        fallback_name: &str,
    ) -> Vec<(String, duduclaw_agent::mcp_template::McpServerDef, String)> {
        let mut blocks: Vec<String> = Vec::new();
        let mut in_block = false;
        let mut current = String::new();
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                if in_block {
                    blocks.push(std::mem::take(&mut current));
                    in_block = false;
                } else {
                    in_block = true;
                }
            } else if in_block {
                current.push_str(line);
                current.push('\n');
            }
        }

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for block in &blocks {
            if !block.contains("\"mcpServers\"") {
                continue;
            }
            let Some(json_str) = Self::first_json_object(block) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&json_str) else {
                continue;
            };
            let Ok(servers) = Self::parse_mcp_manifest_value(&value, fallback_name) else {
                continue;
            };
            for (name, def, desc) in servers {
                let key = format!("{name}|{}|{}", def.command, def.args.join("\u{1f}"));
                if seen.insert(key) {
                    out.push((name, def, desc));
                }
            }
        }
        out
    }

    /// Return the first balanced `{...}` JSON object in a text block
    /// (string-literal aware, so braces inside quoted values don't miscount).
    pub(crate) fn first_json_object(text: &str) -> Option<String> {
        let start = text.find('{')?;
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (i, c) in text[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..start + i + c.len_utf8()].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Install a previously fetched (and re-scanned) MCP server definition.
    ///
    /// Params: `{ agent_id, server_name, server_def, add_to_catalog?, description?, source_url? }`.
    /// The definition is re-scanned server-side and rejected fail-closed at
    /// risk ≥ High — the fetch RPC's verdict cannot be replayed or tampered.
    pub(crate) async fn handle_mcp_import_install(&self, params: Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{McpServerDef, add_server_to_config};

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
        let server_name = match params.get("server_name").and_then(|v| v.as_str()) {
            Some(s) if crate::mcp_scan::is_valid_mcp_server_name(s) => s.to_string(),
            _ => {
                return WsFrame::error_response(
                    "",
                    "Invalid server_name (allowed: A-Za-z0-9._- max 64)",
                );
            }
        };
        let def: McpServerDef = match params.get("server_def") {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(d) => d,
                Err(e) => return WsFrame::error_response("", &format!("Invalid server_def: {e}")),
            },
            None => return WsFrame::error_response("", "Missing 'server_def' parameter"),
        };

        let scan = crate::mcp_scan::scan_mcp_server_def(&server_name, &def);
        if !scan.passed {
            warn!(
                server = %server_name,
                risk = ?scan.risk_level,
                findings = scan.findings.len(),
                "mcp.import.install DENIED by security scan"
            );
            return WsFrame::error_response(
                "",
                &format!(
                    "Security scan rejected MCP server '{server_name}': risk {:?}, {} finding(s): {}",
                    scan.risk_level,
                    scan.findings.len(),
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
        let sn = server_name.clone();
        let d = def.clone();
        let home = self.home_dir.clone();
        let aid = agent_id.clone();
        // A remote (bridge) candidate records its URL in the encrypted
        // remote-server store and is written in its installed form, which
        // carries no URL or token (`remote_mcp::bridge_def`).
        let needs_connect = match tokio::task::spawn_blocking(move || {
            let (installed, needs_connect) =
                crate::remote_mcp::bridge_def::prepare_for_install(&home, &aid, &sn, &d)?;
            add_server_to_config(&ad, &sn, &installed)?;
            Ok::<bool, String>(needs_connect)
        })
        .await
        {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return WsFrame::error_response("", &e),
            Err(e) => return WsFrame::error_response("", &format!("Internal error: {e}")),
        };

        // Optionally persist into the user marketplace catalog so the server
        // shows up alongside the built-ins for future installs.
        let mut catalog_added = false;
        if params
            .get("add_to_catalog")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let description = params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let source_url = params
                .get("source_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            match self
                .append_to_user_marketplace(&server_name, &def, &description, &source_url)
                .await
            {
                Ok(added) => catalog_added = added,
                Err(e) => {
                    // Install already succeeded — report the partial failure
                    // honestly instead of failing the whole call.
                    warn!(server = %server_name, error = %e, "installed but failed to update marketplace.json");
                    return WsFrame::ok_response(
                        "",
                        json!({
                            "success": true,
                            "agent_id": agent_id,
                            "server_name": server_name,
                            "catalog_added": false,
                            "needs_connect": needs_connect,
                            "warning": format!("installed, but adding to the marketplace catalog failed: {e}"),
                        }),
                    );
                }
            }
        }

        info!(server = %server_name, agent = %agent_id, catalog = catalog_added, "MCP server imported from URL");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "server_name": server_name,
                "catalog_added": catalog_added,
                "needs_connect": needs_connect,
            }),
        )
    }

    /// Append an imported server to `~/.duduclaw/marketplace.json` (dedup by id).
    /// Returns Ok(false) when an entry with the same id already exists.
    pub(crate) async fn append_to_user_marketplace(
        &self,
        id: &str,
        def: &duduclaw_agent::mcp_template::McpServerDef,
        description: &str,
        source_url: &str,
    ) -> Result<bool, String> {
        use duduclaw_agent::mcp_template::McpCatalogItem;

        #[derive(serde::Serialize, serde::Deserialize, Default)]
        struct UserCatalog {
            #[serde(default)]
            servers: Vec<McpCatalogItem>,
        }

        let user_path = self.home_dir.join("marketplace.json");
        let mut catalog: UserCatalog = if user_path.exists() {
            let content = tokio::fs::read_to_string(&user_path)
                .await
                .map_err(|e| format!("read marketplace.json: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("parse marketplace.json: {e}"))?
        } else {
            UserCatalog::default()
        };

        if catalog.servers.iter().any(|s| s.id == id) {
            return Ok(false);
        }
        catalog.servers.push(McpCatalogItem {
            id: id.to_string(),
            name: id.to_string(),
            description: if description.is_empty() {
                format!("Imported from {source_url}")
            } else {
                description.to_string()
            },
            category: "imported".to_string(),
            author: source_url.to_string(),
            tags: vec!["imported".to_string()],
            featured: false,
            requires_oauth: false,
            default_def: def.clone(),
            required_env: Vec::new(),
            remote: None,
        });

        let serialized = serde_json::to_string_pretty(&catalog)
            .map_err(|e| format!("serialize marketplace.json: {e}"))?;
        // Atomic write: temp + rename, same convention as other config writers.
        let tmp = user_path.with_extension("json.tmp");
        tokio::fs::write(&tmp, serialized)
            .await
            .map_err(|e| format!("write marketplace.json: {e}"))?;
        tokio::fs::rename(&tmp, &user_path)
            .await
            .map_err(|e| format!("rename marketplace.json: {e}"))?;
        Ok(true)
    }
}
