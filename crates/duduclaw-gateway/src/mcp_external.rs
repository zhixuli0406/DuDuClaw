//! MCP Bridge Adapter Framework — mount external third-party MCP servers.
//!
//! Rather than hand-writing a Rust connector for every SaaS (Plane, Chatwoot,
//! Invoice Ninja, Gmail, …), an agent can declare external MCP servers in its
//! `agent.toml` and DuDuClaw spawns them alongside the internal duduclaw MCP
//! server, exposing their tools to the agent's tool loop. This reuses the
//! existing `duduclaw_llm::{McpClient, ToolRegistry}` transport — the only new
//! pieces are this config reader, credential resolution, and a per-server tool
//! allow/deny filter ([`duduclaw_llm::ToolFilter`]).
//!
//! ## `agent.toml` schema
//!
//! ```toml
//! [[mcp.external]]
//! name = "chatwoot"
//! command = "npx"
//! args = ["-y", "@chatwoot/mcp-server-chatwoot"]
//! enabled = true
//! # env values: plain literal; `env://VAR` to pull from the gateway process
//! # environment; or `secret://<backend>/<name>` to pull from the configured
//! # secret manager (vault / onepassword / infisical / local / env) at spawn
//! # time — keeps secrets out of both agent.toml and the process environment.
//! env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
//! allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation"]  # allowlist (deny-by-default)
//! denied_tools  = []                                                            # always removed
//! ```
//!
//! ## Safety
//!
//! - A server with an unresolvable `env://` **or** `secret://` credential is
//!   **skipped** (a server spawned without its token would misbehave) —
//!   fail-safe, logged.
//! - `allowed_tools` is deny-by-default: if set, only those tools are exposed.
//! - The internal duduclaw server always wins name collisions (it is client 0).

use std::path::Path;

use duduclaw_llm::ToolFilter;

/// One resolved external MCP server ready to spawn.
#[derive(Debug, Clone)]
pub struct ExternalMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Fully-resolved child environment (`env://` refs already pulled).
    pub env: Vec<(String, String)>,
    /// Per-server tool visibility filter.
    pub filter: ToolFilter,
}

/// Resolve one env value: `env://VAR` → the gateway's env (None if unset),
/// anything else → itself. `secret://` values pass through here verbatim and
/// are resolved later by the async [`resolve_secret_refs`] pass (the secret
/// backend is async and needs the DuDuClaw home, neither available in this pure
/// sync parse).
fn resolve_env_value(raw: &str) -> Option<String> {
    if let Some(var) = raw.strip_prefix("env://") {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None, // missing/empty → signal "skip this server"
        }
    } else {
        Some(raw.to_string())
    }
}

/// Resolve any `secret://<backend>/<name>` env values against the configured
/// secret manager, returning only the servers whose secrets all resolved.
///
/// A server holding an unresolvable `secret://` ref is dropped (fail-safe,
/// mirroring the `env://` skip in [`parse_external_servers`]). `home_dir` is the
/// DuDuClaw home used to load `config.toml`'s `[secret_manager]` section and the
/// keyfile for encrypted tokens. Kept separate from the pure sync parse so the
/// parse stays unit-testable and the async secret backend lives in one place.
pub async fn resolve_secret_refs(
    servers: Vec<ExternalMcpServer>,
    home_dir: &Path,
) -> Vec<ExternalMcpServer> {
    use duduclaw_security::secret_manager::{resolve_secret_reference, SecretManagerConfig};

    // Fast path: nothing to resolve ⇒ don't even read config.toml.
    if !servers
        .iter()
        .any(|s| s.env.iter().any(|(_, v)| v.starts_with("secret://")))
    {
        return servers;
    }

    // Load [secret_manager] once; absent / malformed ⇒ default (local).
    let sm_cfg = match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(s) => SecretManagerConfig::from_toml_str(&s).unwrap_or_default(),
        Err(_) => SecretManagerConfig::default(),
    };

    let mut out = Vec::with_capacity(servers.len());
    'server: for mut server in servers {
        for (key, val) in server.env.iter_mut() {
            if val.starts_with("secret://") {
                match resolve_secret_reference(val, &sm_cfg, home_dir).await {
                    Some(resolved) => *val = resolved,
                    None => {
                        tracing::warn!(
                            server = %server.name, key = %key,
                            "external MCP secret:// credential unresolved — skipping server"
                        );
                        continue 'server;
                    }
                }
            }
        }
        out.push(server);
    }
    out
}

/// Load + fully resolve external MCP servers for an agent: parse `agent.toml`,
/// then resolve any `secret://` refs against the secret manager rooted at
/// `home_dir`. This is the entry point spawn paths should call.
pub async fn load_external_mcp_servers_resolved(
    agent_dir: &Path,
    home_dir: &Path,
) -> Vec<ExternalMcpServer> {
    resolve_secret_refs(load_external_mcp_servers(agent_dir), home_dir).await
}

/// Parse a security-relevant tool-filter list (`allowed_tools`/`denied_tools`).
/// Absent ⇒ `Ok(empty)`. Present and an array ⇒ `Ok(strings)`. Present but the
/// WRONG type ⇒ `Err(())` (caller skips the whole server, fail-closed) with a
/// loud warning, so a `"x"`-instead-of-`["x"]` typo can never silently widen the
/// exposed tool surface.
fn tool_list_field(entry: &toml::Value, key: &str, server: &str) -> Result<Vec<String>, ()> {
    match entry.get(key) {
        None => Ok(Vec::new()),
        Some(v) if v.as_array().is_some() => Ok(str_array(Some(v))),
        Some(_) => {
            tracing::warn!(
                server = %server, key = %key,
                "external MCP {key} must be an array of tool names — skipping server (fail-closed)"
            );
            Err(())
        }
    }
}

fn str_array(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `[[mcp.external]]` entries out of an already-parsed `agent.toml`.
/// Pure (no filesystem / env) except for `resolve_env_value`; split from the
/// file read so the parsing + skip logic is unit-testable.
pub fn parse_external_servers(toml_value: &toml::Value) -> Vec<ExternalMcpServer> {
    let entries = toml_value
        .get("mcp")
        .and_then(|m| m.get("external"))
        .and_then(|e| e.as_array());
    let Some(entries) = entries else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries {
        let enabled = entry
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let command = entry
            .get("command")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if command.trim().is_empty() {
            tracing::warn!(server = %name, "external MCP server missing 'command' — skipping");
            continue;
        }
        let args = str_array(entry.get("args"));

        // Resolve env; a missing `env://` credential disables the whole server.
        let mut env = Vec::new();
        let mut skip = false;
        if let Some(tbl) = entry.get("env").and_then(|e| e.as_table()) {
            for (k, raw) in tbl {
                let Some(raw) = raw.as_str() else { continue };
                match resolve_env_value(raw) {
                    Some(v) => env.push((k.clone(), v)),
                    None => {
                        tracing::warn!(
                            server = %name, key = %k,
                            "external MCP env credential unresolved (env:// unset) — skipping server"
                        );
                        skip = true;
                        break;
                    }
                }
            }
        }
        if skip {
            continue;
        }

        // Tool filter lists are security-relevant: a present-but-wrong-type
        // value (e.g. `allowed_tools = "x"` instead of `["x"]`) must NOT silently
        // become an empty (permissive) allowlist. Fail closed — skip the server
        // loudly so a typo can't expose the whole external tool surface.
        let allowed = match tool_list_field(entry, "allowed_tools", &name) {
            Ok(v) => v,
            Err(()) => continue,
        };
        let denied = match tool_list_field(entry, "denied_tools", &name) {
            Ok(v) => v,
            Err(()) => continue,
        };
        let filter = ToolFilter { allowed, denied };

        out.push(ExternalMcpServer {
            name,
            command,
            args,
            env,
            filter,
        });
    }
    out
}

/// Load external MCP servers declared in `<agent_dir>/agent.toml`. Missing /
/// malformed file ⇒ empty (no externals; behavior unchanged).
pub fn load_external_mcp_servers(agent_dir: &Path) -> Vec<ExternalMcpServer> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("agent.toml")) else {
        return Vec::new();
    };
    let Ok(v) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    parse_external_servers(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<ExternalMcpServer> {
        parse_external_servers(&s.parse::<toml::Value>().unwrap())
    }

    #[test]
    fn no_section_is_empty() {
        assert!(parse("[agent]\nname='x'\n").is_empty());
    }

    #[test]
    fn parses_basic_server_with_filter() {
        let s = r#"
[[mcp.external]]
name = "plane"
command = "npx"
args = ["-y", "plane-mcp"]
allowed_tools = ["plane_list_issues"]
denied_tools = ["plane_delete_issue"]
env = { PLANE_BASE_URL = "https://plane.example.com" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        let sv = &servers[0];
        assert_eq!(sv.name, "plane");
        assert_eq!(sv.command, "npx");
        assert_eq!(sv.args, vec!["-y", "plane-mcp"]);
        assert_eq!(sv.env, vec![("PLANE_BASE_URL".into(), "https://plane.example.com".into())]);
        assert!(sv.filter.permits("plane_list_issues"));
        assert!(!sv.filter.permits("plane_delete_issue"));
        assert!(!sv.filter.permits("plane_other"), "allowlist is deny-by-default");
    }

    #[test]
    fn disabled_server_skipped() {
        let s = r#"
[[mcp.external]]
name = "off"
command = "x"
enabled = false
"#;
        assert!(parse(s).is_empty());
    }

    #[test]
    fn missing_command_skipped() {
        let s = "[[mcp.external]]\nname = \"nocmd\"\n";
        assert!(parse(s).is_empty());
    }

    #[test]
    fn env_ref_missing_skips_server() {
        // env:// pointing at an almost-certainly-unset var disables the server.
        let s = r#"
[[mcp.external]]
name = "needsauth"
command = "x"
env = { TOKEN = "env://DUDUCLAW_TEST_DEFINITELY_UNSET_VAR_XYZ" }
"#;
        assert!(parse(s).is_empty(), "unresolved credential ⇒ server skipped");
    }

    #[test]
    fn plain_env_value_passes_through() {
        let s = r#"
[[mcp.external]]
name = "plain"
command = "x"
env = { BASE = "literal-value" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].env, vec![("BASE".into(), "literal-value".into())]);
    }

    #[test]
    fn malformed_allowlist_fails_closed() {
        // allowed_tools as a bare string (typo) must NOT silently expose all
        // tools — the server is skipped entirely.
        let s = r#"
[[mcp.external]]
name = "typo"
command = "x"
allowed_tools = "just_one"
"#;
        assert!(parse(s).is_empty(), "malformed allowlist ⇒ server skipped (fail-closed)");

        // denied_tools as a wrong type likewise skips the server.
        let s2 = r#"
[[mcp.external]]
name = "typo2"
command = "x"
denied_tools = 42
"#;
        assert!(parse(s2).is_empty());
    }

    #[test]
    fn multiple_servers() {
        let s = r#"
[[mcp.external]]
name = "a"
command = "x"
[[mcp.external]]
name = "b"
command = "y"
"#;
        assert_eq!(parse(s).len(), 2);
    }

    #[test]
    fn secret_ref_passes_through_parse_verbatim() {
        // The sync parse keeps `secret://` values untouched (resolved later).
        let s = r#"
[[mcp.external]]
name = "s"
command = "x"
env = { TOKEN = "secret://vault/tok" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].env[0].1, "secret://vault/tok");
    }

    #[tokio::test]
    async fn resolve_no_secret_refs_is_passthrough() {
        let s = r#"
[[mcp.external]]
name = "plain"
command = "x"
env = { BASE = "literal" }
"#;
        let servers = parse(s);
        // No secret:// ⇒ fast path, home_dir never read.
        let out = resolve_secret_refs(servers, Path::new("/nonexistent-home")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].env[0].1, "literal");
    }

    #[tokio::test]
    async fn unresolvable_secret_ref_drops_server() {
        // `secret://local/<name>` against an empty ephemeral local store cannot
        // resolve ⇒ the server is dropped fail-safe (never spawned token-less).
        let s = r#"
[[mcp.external]]
name = "needssecret"
command = "x"
env = { TOKEN = "secret://local/definitely-absent-secret" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1, "parse keeps secret:// verbatim");
        let out = resolve_secret_refs(servers, Path::new("/nonexistent-home")).await;
        assert!(out.is_empty(), "unresolvable secret ⇒ server dropped");
    }
}
