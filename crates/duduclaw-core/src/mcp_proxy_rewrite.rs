//! Spawn-time `.mcp.json` rewrite that routes third-party **stdio** MCP
//! servers through `duduclaw mcp-proxy`.
//!
//! Pure (no IO, no env reads). Lives in `duduclaw-core` so both the gateway
//! (`redaction_proxy::maybe_proxy_mcp_config*`, every Claude CLI spawn) and
//! `duduclaw-agent` (the heartbeat proactive check, which runs in the explore
//! lane) can apply it; `duduclaw-agent` does not depend on the gateway.
//!
//! Two reasons put the proxy in the path, and the rewrite differs only in
//! what it does with `duduclaw mcp-remote-bridge` entries:
//!
//! - **Redaction** (RFC-23 §13.6): every third-party stdio entry is wrapped,
//!   bridges included, because the bridge itself does not redact.
//! - **Third-party tool gate** (2026-10-08: `action_rules`, explore lane):
//!   bridges are left alone, because the bridge applies the same gate itself
//!   and wrapping it would ask twice; direct remote entries (`url` / `type`)
//!   are removed, because nothing can gate them
//!   ([`rewrite_mcp_config_for_gate`]).

use std::path::Path;

use serde_json::{Map, Value, json};

/// Env var carrying the wrapped server's original `.mcp.json` `env` map,
/// JSON-encoded, to the proxy process. Must match
/// `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_VAR`.
pub const PROXY_UPSTREAM_ENV_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV";

/// Env names copied from the `.mcp.json` `duduclaw` entry onto every
/// rewritten proxy entry, so the proxy process resolves the same home, the
/// same agent identity and the same MCP credential the built-in server does.
///
/// `DUDUCLAW_AGENT_TOKEN` is deliberately NOT carried: it proves an agent id
/// to the MCP server, and the proxy makes no MCP tool calls of its own;
/// handing it (and, by inheritance, a third-party server) that token would
/// widen the blast radius for nothing.
pub const PROXY_CARRY_ENV: &[&str] = &[
    "DUDUCLAW_HOME",
    "DUDUCLAW_PORT",
    "DUDUCLAW_INSTANCE",
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
];

/// The `duduclaw` subcommand of a remote MCP bridge entry.
pub const REMOTE_BRIDGE_SUBCOMMAND: &str = "mcp-remote-bridge";

/// Is this `.mcp.json` entry DuDuClaw's own MCP server? `duduclaw` is the
/// default key, `duduclaw-pro` the legacy name and `duduclaw-<instance>` the
/// multi-instance form. Never proxied.
pub fn is_duduclaw_server(name: &str) -> bool {
    name == "duduclaw" || name.starts_with("duduclaw-")
}

/// Is `(command, args)` DuDuClaw's own remote MCP bridge? The command must
/// be exactly `self_exe` (the binary that writes the entry), so another
/// program that happens to take `mcp-remote-bridge` as its first argument is
/// not mistaken for it.
pub fn is_remote_bridge_invocation(command: &str, args: &[String], self_exe: &Path) -> bool {
    args.first().map(String::as_str) == Some(REMOTE_BRIDGE_SUBCOMMAND)
        && Path::new(command) == self_exe
}

/// Why the proxy is put in the path (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyPurpose {
    /// RFC-23 redaction is active: wrap every third-party stdio entry.
    pub redaction: bool,
    /// The third-party tool gate needs the proxy: wrap every entry except
    /// remote bridges (which gate themselves).
    pub tool_gate: bool,
}

impl ProxyPurpose {
    /// Nothing to do.
    pub fn none(&self) -> bool {
        !self.redaction && !self.tool_gate
    }
}

/// Rewrite every third-party **stdio** server so it launches through
/// `duduclaw mcp-proxy` for `purpose`.
///
/// Left untouched: DuDuClaw's own entry, `type` / `url` entries (no child
/// process to wrap), entries with no `command`, entries already pointing at
/// `mcp-proxy` (idempotent), and, when redaction is not a purpose, remote
/// bridge entries. The original `env` map rides along in
/// [`PROXY_UPSTREAM_ENV_VAR`] rather than in argv.
pub fn rewrite_mcp_config_for_proxy_with(json: &Value, self_exe: &Path, purpose: ProxyPurpose) -> Value {
    rewrite_mcp_config_for_gate(json, self_exe, purpose).config
}

/// What [`rewrite_mcp_config_for_gate`] produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyRewrite {
    /// The config to hand the Claude CLI.
    pub config: Value,
    /// Direct remote entries (`url` / `type`) removed because the
    /// third-party tool gate was a purpose and nothing can gate them. Sorted
    /// server names; the caller audits each one.
    pub dropped_remote: Vec<String>,
}

/// Is this `.mcp.json` entry a direct remote server (`url`, or a `type`
/// such as `http` / `sse`) that the Claude CLI would reach without any
/// child process DuDuClaw could wrap?
pub fn is_direct_remote_entry(def: &Value) -> bool {
    def.get("url").is_some() || def.get("type").is_some()
}

/// [`rewrite_mcp_config_for_proxy_with`] that also reports what it removed.
///
/// Direct remote entries (`url` / `type: http|sse`, not DuDuClaw's
/// `mcp-remote-bridge`) are reached by the Claude CLI itself, so neither
/// redaction nor the third-party tool gate sees their tools. When the tool
/// gate is a purpose (`action_rules` present or the explore lane) they are
/// **removed** from the spawn config (2026-10-08 close-out): the operator
/// connects them with `mcp.remote_connect`, which writes a bridge entry the
/// gate does cover. They are not rewritten to the bridge on the fly because
/// the bridge reads its URL and credentials from the encrypted remote store,
/// and a spawn must not create store records or probe the network. With
/// redaction as the only purpose they stay (logged), as before.
pub fn rewrite_mcp_config_for_gate(json: &Value, self_exe: &Path, purpose: ProxyPurpose) -> ProxyRewrite {
    let mut out = json.clone();
    let mut dropped_remote = Vec::new();
    if purpose.none() {
        return ProxyRewrite { config: out, dropped_remote };
    }
    let exe = self_exe.to_string_lossy().into_owned();

    let Some(servers) = out.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return ProxyRewrite { config: out, dropped_remote };
    };

    let carry = carry_env(servers);
    let names: Vec<String> = servers.keys().cloned().collect();

    for name in names {
        if is_duduclaw_server(&name) {
            continue;
        }
        let Some(def) = servers.get(&name).cloned() else {
            continue;
        };
        if is_direct_remote_entry(&def) {
            if purpose.tool_gate {
                tracing::warn!(
                    server = %name,
                    "direct HTTP/SSE MCP server removed from this spawn — the third-party tool gate cannot reach it; connect it with mcp.remote_connect"
                );
                servers.remove(&name);
                dropped_remote.push(name);
            } else {
                tracing::warn!(
                    server = %name,
                    "HTTP/SSE MCP server is NOT proxied — redaction does not reach it"
                );
            }
            continue;
        }
        let Some(command) = def.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        if is_already_proxied(&def, &exe) {
            continue;
        }
        let orig_args: Vec<String> = def
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if !purpose.redaction && is_remote_bridge_invocation(command, &orig_args, self_exe) {
            continue;
        }

        let mut args = vec![
            json!("mcp-proxy"),
            json!("--server"),
            json!(name),
            json!("--"),
            json!(command),
        ];
        if let Some(orig) = def.get("args").and_then(|a| a.as_array()) {
            args.extend(orig.iter().cloned());
        }

        let original_env = def
            .get("env")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let mut env = carry.clone();
        env.insert(
            PROXY_UPSTREAM_ENV_VAR.to_string(),
            Value::String(original_env.to_string()),
        );

        servers.insert(
            name,
            json!({ "command": exe, "args": args, "env": Value::Object(env) }),
        );
    }
    dropped_remote.sort();
    ProxyRewrite { config: out, dropped_remote }
}

/// The env pairs the proxy needs, lifted off whichever DuDuClaw entry this
/// config carries. Absent entry ⇒ empty.
fn carry_env(servers: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some((_, def)) = servers.iter().find(|(k, _)| is_duduclaw_server(k)) else {
        return out;
    };
    let Some(env) = def.get("env").and_then(|e| e.as_object()) else {
        return out;
    };
    for key in PROXY_CARRY_ENV {
        if let Some(v) = env.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    out
}

fn is_already_proxied(def: &Value, exe: &str) -> bool {
    def.get("command").and_then(|c| c.as_str()) == Some(exe)
        && def
            .get("args")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|a| a.as_str())
            == Some("mcp-proxy")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(exe: &str) -> Value {
        json!({ "mcpServers": {
            "duduclaw": { "command": exe, "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "a1", "DUDUCLAW_AGENT_TOKEN": "t" } },
            "pg": { "command": "npx", "args": ["-y", "pg-mcp"], "env": { "PGPASSWORD": "x" } },
            "zap": { "command": exe, "args": ["mcp-remote-bridge", "--agent", "a1", "--server", "zap"] },
            "web": { "url": "https://example.com/mcp" }
        }})
    }

    #[test]
    fn gate_only_rewrite_skips_bridges_and_redaction_wraps_them() {
        let exe = "/opt/duduclaw";
        let p = Path::new(exe);
        let gate = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: false, tool_gate: true });
        assert_eq!(gate["mcpServers"]["pg"]["args"][0], "mcp-proxy");
        assert_eq!(gate["mcpServers"]["zap"]["args"][0], "mcp-remote-bridge");
        assert!(gate["mcpServers"].get("web").is_none(), "a direct remote entry cannot be gated");
        assert!(gate["mcpServers"]["pg"]["env"].get("DUDUCLAW_AGENT_TOKEN").is_none());
        assert_eq!(gate["mcpServers"]["pg"]["env"]["DUDUCLAW_AGENT_ID"], "a1");
        let red = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: true, tool_gate: false });
        assert_eq!(red["mcpServers"]["zap"]["args"][0], "mcp-proxy");
        let none = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: false, tool_gate: false });
        assert_eq!(none, cfg(exe));
    }

    #[test]
    fn the_gate_reports_every_direct_remote_entry_it_drops() {
        let exe = "/opt/duduclaw";
        let mut c = cfg(exe);
        c["mcpServers"]["sse"] = json!({ "type": "sse", "url": "https://x.example/sse" });
        c["mcpServers"]["http"] = json!({ "type": "http", "url": "https://y.example/mcp" });
        let r = rewrite_mcp_config_for_gate(&c, Path::new(exe), ProxyPurpose { redaction: true, tool_gate: true });
        assert_eq!(r.dropped_remote, vec!["http", "sse", "web"]);
        for n in ["http", "sse", "web"] {
            assert!(r.config["mcpServers"].get(n).is_none());
        }
        // The bridge and the duduclaw entry survive.
        assert!(r.config["mcpServers"].get("zap").is_some());
        assert!(r.config["mcpServers"].get("duduclaw").is_some());
        // Redaction alone keeps them (the old behaviour).
        let r = rewrite_mcp_config_for_gate(&c, Path::new(exe), ProxyPurpose { redaction: true, tool_gate: false });
        assert!(r.dropped_remote.is_empty());
        assert_eq!(r.config["mcpServers"]["web"], c["mcpServers"]["web"]);
    }

    #[test]
    fn a_bridge_is_recognised_only_from_this_binary() {
        let args = vec!["mcp-remote-bridge".to_string()];
        assert!(is_remote_bridge_invocation("/opt/duduclaw", &args, Path::new("/opt/duduclaw")));
        assert!(!is_remote_bridge_invocation("/tmp/evil", &args, Path::new("/opt/duduclaw")));
        assert!(!is_remote_bridge_invocation("/opt/duduclaw", &["mcp-server".into()], Path::new("/opt/duduclaw")));
    }
}
