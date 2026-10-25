//! The employee's `.mcp.json` third-party servers for the in-process MCP tool
//! loop (2026-10-20): the openai-compat runtime, the Direct-API reply and
//! the local-inference loop, all built by
//! `claude_runner::build_mcp_tool_registry`.
//!
//! That builder used to mount DuDuClaw's own server plus the `agent.toml
//! [[mcp.external]]` servers only, so `.mcp.json` servers were missing on
//! these paths. The fix lives in the shared builder because all three paths
//! dispatch MCP tools in process with no approval prompt, the same class of
//! omission, and the servers come through the third-party gate either way.
//!
//! - Each server is started as the gated rewrite produced it: `duduclaw
//!   mcp-proxy` for a stdio server, DuDuClaw's bridge for a remote one; a
//!   direct URL entry is removed (audited). The proxy's env (identity
//!   without the MAC, the MCP credential, the upstream env inline in
//!   `DUDUCLAW_MCP_PROXY_ENV`) is handed to the child process only; nothing
//!   is written to disk. The child starts from the agent-CLI allowlisted
//!   environment (`McpClient::connect_scrubbed`), not the gateway's.
//! - Explore lane: mounted, with `DUDUCLAW_LANE=explore` in each child's
//!   env, so the proxy (and the bridge) allow read-only tools only — the
//!   same as a Claude CLI spawn in the lane. `agent.toml` externals stay
//!   unmounted in the lane because nothing gates them.
//! - A server named in `agent.toml` too (by name, or the same stdio command
//!   and arguments) is mounted once, from `agent.toml`: that entry already
//!   carries the operator's tool allowlist and `secret://` references.
//! - Utility calls get none, and read no `.mcp.json`.
//! - A malformed `.mcp.json` or a spawn-gate refusal is an `Err` (spawn-gate
//!   prefixed); see `build_mcp_tool_registry_checked` for what each caller
//!   does with it.

use std::path::Path;

use tracing::info;

use super::{GatedServer, gated_servers_for_spawn};
use crate::mcp_external::ExternalMcpServer;

/// One `.mcp.json` server to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolLoopServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The employee's gated `.mcp.json` servers for one tool-loop build, minus
/// those `externals` already mount.
pub(crate) async fn servers_for_tool_loop(
    agent_id: &str,
    home_dir: &Path,
    agent_dir: &Path,
    externals: &[ExternalMcpServer],
) -> Result<Vec<ToolLoopServer>, String> {
    let Some(own) = crate::runtime::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
        return Ok(Vec::new());
    };
    let explore = crate::explore_lane::in_explore();
    let gated = gated_servers_for_spawn(agent_dir, home_dir, &own, explore).await?;
    let lane = crate::explore_lane::lane_env();
    Ok(gated
        .into_iter()
        .filter(|(name, server)| {
            let dup = mounted_by_externals(name, server, externals);
            if dup {
                info!(
                    agent = %agent_id,
                    server = %name,
                    "MCP server is declared in agent.toml and .mcp.json — mounted once, from agent.toml"
                );
            }
            !dup
        })
        .map(|(name, server)| {
            let mut env: Vec<(String, String)> = server.env.into_iter().collect();
            env.extend(lane.clone());
            ToolLoopServer {
                name,
                command: server.command,
                args: server.args,
                env,
            }
        })
        .collect())
}

/// Is this `.mcp.json` server already among the `agent.toml` externals?
/// Same name, or (stdio) the same upstream command and arguments as the
/// proxy wraps.
fn mounted_by_externals(name: &str, server: &GatedServer, externals: &[ExternalMcpServer]) -> bool {
    let upstream = upstream_invocation(server);
    externals.iter().any(|ext| {
        ext.name == name
            || (ext.url.is_none()
                && upstream.as_ref().is_some_and(|(cmd, args)| {
                    *cmd == ext.command && args.as_slice() == ext.args.as_slice()
                }))
    })
}

/// `(command, args)` after the proxy's `--`, for a proxied server.
fn upstream_invocation(server: &GatedServer) -> Option<(String, Vec<String>)> {
    if server.args.first().map(String::as_str) != Some("mcp-proxy") {
        return None;
    }
    let sep = server.args.iter().position(|a| a == "--")?;
    let mut rest = server.args.iter().skip(sep + 1).cloned();
    let cmd = rest.next()?;
    Some((cmd, rest.collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gated(args: &[&str]) -> GatedServer {
        GatedServer {
            command: "/opt/duduclaw".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Default::default(),
        }
    }

    fn ext(name: &str, command: &str, args: &[&str]) -> ExternalMcpServer {
        ExternalMcpServer {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Vec::new(),
            url: None,
            bearer_token: None,
            headers: Vec::new(),
            filter: Default::default(),
            google_guard: None,
            google_closed: None,
            source_ordinal: 0,
        }
    }

    #[test]
    fn a_server_in_both_files_is_mounted_once() {
        let proxied = gated(&[
            "mcp-proxy",
            "--server",
            "broker",
            "--",
            "python3",
            "-m",
            "broker_mcp",
        ]);
        // Same name.
        assert!(mounted_by_externals(
            "broker",
            &proxied,
            &[ext("broker", "npx", &["x"])]
        ));
        // Same upstream invocation under another name.
        assert!(mounted_by_externals(
            "broker",
            &proxied,
            &[ext("other", "python3", &["-m", "broker_mcp"])]
        ));
        // Different server.
        assert!(!mounted_by_externals(
            "broker",
            &proxied,
            &[ext("pg", "npx", &["pg-mcp"])]
        ));
        assert!(!mounted_by_externals("broker", &proxied, &[]));
    }

    fn employee_with_broker() -> (tempfile::TempDir, std::path::PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("trader");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            serde_json::json!({ "mcpServers": {
                "duduclaw": { "command": "duduclaw", "args": ["mcp-server"] },
                "masterlink": { "command": "python3", "args": ["-m", "m"], "env": { "ML_PASSWORD": "pw" } }
            }})
            .to_string(),
        )
        .unwrap();
        (home, dir)
    }

    /// The read-only lane reaches the proxy's environment, so it lists and
    /// runs only read-only tools.
    #[tokio::test]
    async fn the_explore_lane_reaches_the_child_env() {
        let (home, dir) = employee_with_broker();
        let lane = |servers: &[ToolLoopServer]| {
            servers[0]
                .env
                .iter()
                .any(|(k, v)| k == duduclaw_core::ENV_LANE && v == duduclaw_core::LANE_EXPLORE)
        };
        let normal = servers_for_tool_loop("trader", home.path(), &dir, &[])
            .await
            .unwrap();
        assert_eq!(normal.len(), 1);
        assert!(!lane(&normal));
        let explore = crate::explore_lane::EXPLORE
            .scope(
                true,
                servers_for_tool_loop("trader", home.path(), &dir, &[]),
            )
            .await
            .unwrap();
        assert_eq!(explore[0].args[0], "mcp-proxy");
        assert!(lane(&explore), "{:?}", explore[0].env);
        // The upstream env rides inline in the child env, never on argv.
        assert!(!explore[0].args.iter().any(|a| a.contains("pw")));
    }

    #[tokio::test]
    async fn a_utility_call_mounts_nothing() {
        let (home, dir) = employee_with_broker();
        let got = crate::runtime_dispatch::UTILITY_CALL
            .scope((), servers_for_tool_loop("trader", home.path(), &dir, &[]))
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn upstream_of_a_bridge_is_not_compared() {
        let bridge = gated(&["mcp-remote-bridge", "--agent", "a", "--server", "zap"]);
        assert!(upstream_invocation(&bridge).is_none());
        let proxied = gated(&["mcp-proxy", "--server", "x", "--", "cmd", "a"]);
        assert_eq!(
            upstream_invocation(&proxied),
            Some(("cmd".to_string(), vec!["a".to_string()]))
        );
    }
}

/// Live check (manual): the tool loop's `.mcp.json` server really starts
/// behind `duduclaw mcp-proxy` from the scrubbed environment and gets its
/// declared env. Needs a `duduclaw` built from this tree in `DUDUCLAW_BIN`
/// and python3; `DUDUCLAW_LIVE_SENTINEL_API_KEY` (any value) set on the test
/// process stands in for a gateway vendor key that must not reach the
/// upstream. Run with:
///   DUDUCLAW_BIN=<built duduclaw> DUDUCLAW_LIVE_SENTINEL_API_KEY=x \
///     cargo test -p duduclaw-gateway --lib live_tool_loop -- --ignored --nocapture
#[cfg(test)]
mod live {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    #[ignore = "starts a built duduclaw mcp-proxy and a python stub"]
    async fn live_tool_loop_mounts_a_dot_mcp_server_behind_the_proxy() {
        if std::env::var("DUDUCLAW_BIN").is_err() {
            eprintln!("DUDUCLAW_BIN missing — skipping");
            return;
        }
        let python = [
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
            "/usr/bin/python3",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
        .expect("python3");
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dir = home.join("agents").join("trader");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = tmp.path().join("upstream.jsonl");
        let script = tmp.path().join("stub.py");
        std::fs::write(
            &script,
            format!(
                r#"import json, os, sys
with open({marker:?}, "a") as f:
    f.write(json.dumps(dict(os.environ)) + "\n")
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m = msg.get("method")
    if m == "initialize":
        r = {{"protocolVersion": msg["params"].get("protocolVersion", "2024-11-05"), "capabilities": {{"tools": {{}}}}, "serverInfo": {{"name": "stub", "version": "0"}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": r}}
    elif m == "tools/list":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": [{{"name": "ping", "description": "p", "inputSchema": {{"type": "object"}}, "annotations": {{"readOnlyHint": True}}}}]}}}}
    else:
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "no"}}}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#,
                marker = marker.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            json!({ "mcpServers": {
                "duduclaw": { "command": std::env::var("DUDUCLAW_BIN").unwrap(), "args": ["mcp-server"] },
                "masterlink": { "command": python.to_string_lossy(), "args": [script.to_string_lossy()],
                                "env": { "ML_PASSWORD": "broker-secret-pw" } }
            }})
            .to_string(),
        )
        .unwrap();
        let servers = servers_for_tool_loop("trader", &home, &dir, &[])
            .await
            .unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].args[0], "mcp-proxy");
        let s = &servers[0];
        let client = duduclaw_llm::McpClient::connect_scrubbed(
            &s.command,
            &s.args,
            &s.env,
            duduclaw_llm::DEFAULT_MCP_TIMEOUT,
        )
        .await
        .expect("proxy + upstream handshake");
        drop(client);
        let seen = std::fs::read_to_string(&marker).expect("upstream started");
        let env: serde_json::Value = serde_json::from_str(seen.lines().next().unwrap()).unwrap();
        eprintln!(
            "upstream env keys: {:?}",
            env.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        assert_eq!(env["ML_PASSWORD"], "broker-secret-pw");
        assert!(env.get("DUDUCLAW_AGENT_TOKEN").is_none());
        assert!(env.get("DUDUCLAW_MCP_PROXY_ENV").is_none());
        assert!(
            env.get("DUDUCLAW_LIVE_SENTINEL_API_KEY").is_none(),
            "gateway env must not leak"
        );
    }
}
