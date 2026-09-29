//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── MCP import from GitHub / URL (scanned, fail-closed) ─────

    /// Parse a fetched manifest into `(name, def, description)` candidates.
    ///
    /// Accepted shapes:
    /// 1. `.mcp.json`: `{ "mcpServers": { "<name>": { command, args, env } } }`
    ///    — entries with a `url` instead of a `command` (remote HTTP/SSE
    ///    servers) are bridged via `npx -y mcp-remote <url>`, the standard
    ///    stdio adapter those projects themselves document.
    /// 2. user catalog: `{ "servers": [ { id, name?, description?, default_def } ] }`
    /// 3. single catalog item: `{ "id": ..., "default_def": { ... } }`
    /// 4. bare def: `{ "command": ..., "args": [...], "env": {...} }`
    /// 5. MCP Registry `server.json` (2025 schema): `packages[]` (npm → npx,
    ///    pypi → uvx, oci → docker) and `remotes[]` (→ mcp-remote bridge).
    pub(crate) fn parse_mcp_manifest(
        text: &str,
        fallback_name: &str,
    ) -> Result<Vec<(String, duduclaw_agent::mcp_template::McpServerDef, String)>, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        Self::parse_mcp_manifest_value(&value, fallback_name)
    }

    /// Build the `npx -y mcp-remote <url>` bridge definition for a remote
    /// HTTP/SSE MCP server so stdio-only runtimes can use it.
    pub(crate) fn mcp_remote_bridge_def(url: &str) -> duduclaw_agent::mcp_template::McpServerDef {
        duduclaw_agent::mcp_template::McpServerDef {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "mcp-remote".to_string(), url.to_string()],
            env: std::collections::HashMap::new(),
        }
    }

    /// Sanitize an arbitrary string into a valid MCP server name.
    pub(crate) fn sanitize_mcp_name(raw: &str, fallback: &str) -> String {
        let cleaned: String = raw
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
        if cleaned.is_empty() {
            fallback.to_string()
        } else {
            cleaned
        }
    }

    pub(crate) fn parse_mcp_manifest_value(
        value: &Value,
        fallback_name: &str,
    ) -> Result<Vec<(String, duduclaw_agent::mcp_template::McpServerDef, String)>, String> {
        use duduclaw_agent::mcp_template::McpServerDef;

        // Shape 1: standard .mcp.json
        if let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()) {
            let mut out = Vec::new();
            let mut skipped = Vec::new();
            for (name, def_val) in map {
                if def_val.get("command").is_some() {
                    match serde_json::from_value::<McpServerDef>(def_val.clone()) {
                        Ok(def) => out.push((name.clone(), def, String::new())),
                        Err(e) => skipped.push(format!("{name}: {e}")),
                    }
                } else if let Some(url) = def_val.get("url").and_then(|v| v.as_str()) {
                    out.push((
                        name.clone(),
                        Self::mcp_remote_bridge_def(url),
                        format!("Remote MCP ({url}) bridged via mcp-remote"),
                    ));
                } else {
                    skipped.push(format!("{name}: no command or url"));
                }
            }
            if out.is_empty() {
                return Err(format!(
                    "mcpServers map has no usable entries ({})",
                    skipped.join("; ")
                ));
            }
            return Ok(out);
        }

        // Shape 5: MCP Registry server.json — identified by packages/remotes
        // arrays plus a name (the $schema URL is optional in the wild).
        let has_registry_arrays = value.get("packages").map(|v| v.is_array()).unwrap_or(false)
            || value.get("remotes").map(|v| v.is_array()).unwrap_or(false);
        if has_registry_arrays {
            let reg_name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(fallback_name);
            let base_name = Self::sanitize_mcp_name(reg_name, fallback_name);
            let description = value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mut out = Vec::new();

            for pkg in value
                .get("packages")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                let registry = pkg
                    .get("registryType")
                    .or_else(|| pkg.get("registry_name"))
                    .or_else(|| pkg.get("registryName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let identifier = pkg
                    .get("identifier")
                    .or_else(|| pkg.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if identifier.is_empty() {
                    continue;
                }
                let def = match registry {
                    "npm" => McpServerDef {
                        command: "npx".into(),
                        args: vec!["-y".into(), identifier.to_string()],
                        env: std::collections::HashMap::new(),
                    },
                    "pypi" => McpServerDef {
                        command: "uvx".into(),
                        args: vec![identifier.to_string()],
                        env: std::collections::HashMap::new(),
                    },
                    "oci" => McpServerDef {
                        command: "docker".into(),
                        args: vec![
                            "run".into(),
                            "-i".into(),
                            "--rm".into(),
                            identifier.to_string(),
                        ],
                        env: std::collections::HashMap::new(),
                    },
                    _ => continue,
                };
                out.push((base_name.clone(), def, description.clone()));
            }

            for remote in value
                .get("remotes")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                if let Some(url) = remote.get("url").and_then(|v| v.as_str()) {
                    out.push((
                        base_name.clone(),
                        Self::mcp_remote_bridge_def(url),
                        if description.is_empty() {
                            format!("Remote MCP ({url}) bridged via mcp-remote")
                        } else {
                            format!("{description} (remote, bridged via mcp-remote)")
                        },
                    ));
                }
            }

            if out.is_empty() {
                return Err("registry server.json has no npm/pypi/oci packages or remotes".into());
            }
            return Ok(out);
        }

        // Shape 2: catalog list
        if let Some(list) = value.get("servers").and_then(|v| v.as_array()) {
            let mut out = Vec::new();
            for item in list {
                let def_val = item
                    .get("default_def")
                    .ok_or_else(|| "catalog entry missing default_def".to_string())?;
                let def: McpServerDef = serde_json::from_value(def_val.clone())
                    .map_err(|e| format!("catalog entry has an invalid default_def: {e}"))?;
                let name = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(fallback_name)
                    .to_string();
                let desc = item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                out.push((name, def, desc));
            }
            if out.is_empty() {
                return Err("manifest has an empty servers list".into());
            }
            return Ok(out);
        }

        // Shape 3: single catalog item
        if let Some(def_val) = value.get("default_def") {
            let def: McpServerDef = serde_json::from_value(def_val.clone())
                .map_err(|e| format!("invalid default_def: {e}"))?;
            let name = value
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(fallback_name)
                .to_string();
            let desc = value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            return Ok(vec![(name, def, desc)]);
        }

        // Shape 4: bare server definition
        if value.get("command").is_some() {
            let def: McpServerDef = serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid server definition: {e}"))?;
            return Ok(vec![(fallback_name.to_string(), def, String::new())]);
        }

        Err("unrecognized manifest shape — expected an .mcp.json (mcpServers map), an MCP Registry server.json (packages/remotes), a catalog ({servers: [...]}) or a single {command, args, env} definition".into())
    }

    /// Serialize a scan result into the same JSON shape as skill vetting.
    pub(crate) fn mcp_scan_to_json(
        scan: &crate::skill_lifecycle::security_scanner::SecurityScanResult,
    ) -> Value {
        json!({
            "passed": scan.passed,
            "risk_level": format!("{:?}", scan.risk_level),
            "findings": scan.findings.iter().map(|f| json!({
                "category": format!("{:?}", f.category),
                "severity": format!("{:?}", f.severity).to_lowercase(),
                "description": f.description,
                "pattern": f.matched_pattern,
            })).collect::<Vec<_>>(),
        })
    }
}
