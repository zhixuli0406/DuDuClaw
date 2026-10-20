//! An employee's third-party `.mcp.json` servers for runtimes that register
//! MCP servers themselves instead of reading `.mcp.json` (2026-10-19 Codex,
//! 2026-10-20 Grok, Antigravity, and the openai-compat / Direct-API / local
//! tool loop).
//!
//! The runtime-agnostic half lives here; each runtime turns [`GatedServer`]s
//! into its own config format:
//!
//! - Codex: `-c` overrides + `.codex/config.toml` (`codex/third_party.rs`);
//! - Grok: `<cwd>/.grok/config.toml` (`grok/third_party.rs`);
//! - Antigravity: `<cwd>/.agents/mcp_config.json`
//!   (`antigravity/third_party.rs`);
//! - the in-process tool loop: one `McpClient` per server
//!   ([`tool_loop`]).
//!
//! Every server comes from the same gated rewrite the Claude CLI path uses
//! ([`crate::redaction_proxy::gated_mcp_config_for_runtime`], third-party
//! tool gate always on): stdio servers run through `duduclaw mcp-proxy`,
//! DuDuClaw's remote bridges gate themselves, direct URL entries are removed
//! and audited, and anything else is dropped, never handed to a CLI
//! unproxied.
//!
//! Grok and Antigravity keep their registration in a persistent file that
//! every spawn of the employee (and anyone running the CLI in that directory)
//! shares. For them each server is wrapped once more as a *spawn-scoped*
//! proxy ([`spawn_scoped`]): the file names no credential and nothing per
//! spawn, the upstream env travels in a per-spawn owner-only directory
//! ([`env_files::write_spawn_env_dir`]), and a proxy started without that
//! directory starts nothing. That is also how a utility call on those
//! runtimes gets none of the employee's servers.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use tracing::warn;

use duduclaw_core::mcp_proxy_rewrite::{
    PROXY_CARRY_ENV, PROXY_SPAWN_SCOPED_FLAG, PROXY_UPSTREAM_ENV_VAR, is_duduclaw_server,
    is_portable_server_name, is_remote_bridge_invocation,
};

pub(crate) mod env_files;

/// One gated third-party server as a runtime receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatedServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// The gated third-party servers of the employee in `agent_dir`, keyed by
/// server name. DuDuClaw's own server is never among them (`own_server`
/// takes the `duduclaw` key before the rewrite and is registered separately).
///
/// `Err` (spawn-gate prefixed, audited) when `.mcp.json` exists but cannot be
/// read or parsed: the caller refuses the spawn.
pub(crate) fn gated_servers(
    agent_dir: &Path,
    home_dir: &Path,
    own_server: &Value,
    explore: bool,
) -> Result<BTreeMap<String, GatedServer>, String> {
    let Some(self_exe) = own_server.get("command").and_then(|c| c.as_str()) else {
        return Ok(BTreeMap::new());
    };
    let self_exe = Path::new(self_exe);
    let Some(config) = crate::redaction_proxy::gated_mcp_config_for_runtime(
        home_dir,
        &agent_dir.join(".mcp.json"),
        own_server,
        self_exe,
        explore,
    )?
    else {
        return Ok(BTreeMap::new());
    };
    Ok(extract_gated(&config, self_exe, own_server))
}

/// The spawn-time half. Nothing changes for a spawn that has no third-party
/// server to register:
///
/// - a utility call ([`crate::runtime_dispatch::in_utility_call`]: summaries,
///   classifiers, judges on the employee's runtime) never gets the
///   employee's servers, as on the Claude utility path, and does not read
///   `.mcp.json` at all;
/// - otherwise [`gated_servers_checked`].
pub(crate) async fn gated_servers_for_spawn(
    agent_dir: &Path,
    home_dir: &Path,
    own_server: &Value,
    explore: bool,
) -> Result<BTreeMap<String, GatedServer>, String> {
    if crate::runtime_dispatch::in_utility_call() {
        return Ok(BTreeMap::new());
    }
    gated_servers_checked(agent_dir, home_dir, own_server, explore).await
}

/// [`gated_servers`] behind the `.mcp.json` spawn gate, without the
/// utility-call shortcut (Grok writes the same persistent file for a utility
/// call, see `grok/third_party.rs`):
///
/// - no `.mcp.json`, or one listing only DuDuClaw's own server (team role
///   scaffolds write exactly that) ⇒ no spawn gate, no write, empty;
/// - an unreadable or malformed `.mcp.json` ⇒ refused (fail closed);
/// - third-party servers listed ⇒ the `.mcp.json` spawn gate runs (as for a
///   Claude CLI spawn), then [`gated_servers`]. The gate writes nothing when
///   it fails; when it succeeds it may rewrite only the file's DuDuClaw
///   entries (atomically, owner-only, other entries kept as they are), the
///   same repair every Claude spawn and every gateway boot performs.
pub(crate) async fn gated_servers_checked(
    agent_dir: &Path,
    home_dir: &Path,
    own_server: &Value,
    explore: bool,
) -> Result<BTreeMap<String, GatedServer>, String> {
    if !crate::redaction_proxy::mcp_json_lists_third_party_servers(
        home_dir,
        &agent_dir.join(".mcp.json"),
        explore,
    )? {
        return Ok(BTreeMap::new());
    }
    let dir_owned = agent_dir.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        duduclaw_agent::mcp_template::prepare_mcp_config_for_spawn(&dir_owned)
    })
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(msg)) => return Err(msg),
        Err(e) => {
            return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
                "MCP 設定檢查無法執行：{e}"
            )));
        }
    }
    gated_servers(agent_dir, home_dir, own_server, explore)
}

/// Pick the gated third-party entries out of a rewritten config. An entry is
/// kept only when it launches `self_exe` as `mcp-proxy` or as DuDuClaw's
/// remote bridge, and only under a name that is safe as a bare TOML key, a
/// JSON key, an Antigravity permission rule and a file name
/// ([`is_portable_server_name`]).
///
/// A bridge entry carries only `DUDUCLAW_HOME`; under the Claude CLI it
/// inherits the rest from the CLI, but Codex clears an MCP child's
/// environment, so the bridge gets the same [`PROXY_CARRY_ENV`] names a
/// proxy gets, from `own_server` (never the identity MAC).
fn extract_gated(
    config: &Value,
    self_exe: &Path,
    own_server: &Value,
) -> BTreeMap<String, GatedServer> {
    let mut out = BTreeMap::new();
    let Some(servers) = config.get("mcpServers").and_then(|s| s.as_object()) else {
        return out;
    };
    let exe = self_exe.to_string_lossy();
    for (name, def) in servers {
        if is_duduclaw_server(name) {
            continue;
        }
        if !is_portable_server_name(name) {
            warn!(server = %name, "MCP server name is not a plain name (letters, digits, _ and -, at most 64) — not registered");
            continue;
        }
        let command = def.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let args: Vec<String> = def
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let proxied = command == exe && args.first().map(String::as_str) == Some("mcp-proxy");
        if !proxied && !is_remote_bridge_invocation(command, &args, self_exe) {
            warn!(
                server = %name,
                "MCP server could not be put behind duduclaw mcp-proxy — not registered"
            );
            continue;
        }
        let mut env = string_env(def);
        if !proxied {
            for (k, v) in string_env(own_server) {
                if PROXY_CARRY_ENV.contains(&k.as_str()) {
                    env.entry(k).or_insert(v);
                }
            }
        }
        out.insert(
            name.clone(),
            GatedServer {
                command: command.to_string(),
                args,
                env,
            },
        );
    }
    out
}

/// A definition's `env` as strings (non-scalar values dropped).
pub(crate) fn string_env(def: &Value) -> BTreeMap<String, String> {
    def.get("env")
        .and_then(|e| e.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| super::codex::env_value_as_string(v).map(|s| (k.clone(), s)))
                .collect()
        })
        .unwrap_or_default()
}

/// One server as a persistent CLI config registers it (Grok, Antigravity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedServer {
    /// The DuDuClaw binary.
    pub command: String,
    /// `mcp-proxy --spawn-scoped --server <name> -- <upstream…>`.
    pub args: Vec<String>,
    /// What the proxy itself needs ([`PROXY_CARRY_ENV`] names only: home,
    /// port, instance, agent id, MCP credential). Never the identity MAC,
    /// never the upstream server's own env.
    pub proxy_env: BTreeMap<String, String>,
    /// The upstream server's env as a JSON object, written to the per-spawn
    /// directory, never to the persistent file.
    pub upstream_env: String,
}

/// Wrap each gated server as a spawn-scoped proxy.
///
/// - A proxied stdio server keeps its proxy command; the flag is added and
///   its upstream env ([`PROXY_UPSTREAM_ENV_VAR`]) moves to `upstream_env`.
/// - A remote bridge (which gates itself) is put behind a spawn-scoped proxy
///   too, so the persistent entry is inert outside a spawn that registered
///   it; the proxy does not gate a bridge a second time (`run_mcp_proxy`
///   skips its gate for DuDuClaw's own bridge) and hands the bridge its env
///   through `upstream_env`.
pub(crate) fn spawn_scoped(
    servers: &BTreeMap<String, GatedServer>,
) -> BTreeMap<String, ScopedServer> {
    servers
        .iter()
        .map(|(name, s)| (name.clone(), scope_one(name, s)))
        .collect()
}

fn scope_one(name: &str, s: &GatedServer) -> ScopedServer {
    let proxied = s.args.first().map(String::as_str) == Some("mcp-proxy");
    let proxy_env: BTreeMap<String, String> = s
        .env
        .iter()
        .filter(|(k, _)| PROXY_CARRY_ENV.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (args, upstream) = if proxied {
        let mut args = vec!["mcp-proxy".to_string()];
        if s.args.get(1).map(String::as_str) != Some(PROXY_SPAWN_SCOPED_FLAG) {
            args.push(PROXY_SPAWN_SCOPED_FLAG.to_string());
        }
        args.extend(s.args.iter().skip(1).cloned());
        // The declared upstream env, plus anything else an already-proxied
        // entry carried beyond the proxy's own names (it would have reached
        // the upstream by inheritance).
        let mut upstream: serde_json::Map<String, Value> = s
            .env
            .get(PROXY_UPSTREAM_ENV_VAR)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in &s.env {
            if k != PROXY_UPSTREAM_ENV_VAR && !PROXY_CARRY_ENV.contains(&k.as_str()) {
                upstream
                    .entry(k.clone())
                    .or_insert_with(|| Value::String(v.clone()));
            }
        }
        (args, upstream)
    } else {
        let mut args = vec![
            "mcp-proxy".to_string(),
            PROXY_SPAWN_SCOPED_FLAG.to_string(),
            "--server".to_string(),
            name.to_string(),
            "--".to_string(),
            s.command.clone(),
        ];
        args.extend(s.args.iter().cloned());
        let upstream = s
            .env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        (args, upstream)
    };
    ScopedServer {
        command: s.command.clone(),
        args,
        proxy_env,
        upstream_env: Value::Object(upstream).to_string(),
    }
}

/// Is `(command, args)` a spawn-scoped proxy entry written by this binary?
/// Used to recognise (and prune) the entries DuDuClaw manages in a persistent
/// CLI config without touching anything an operator added by hand.
pub(crate) fn is_managed_entry(command: &str, args: &[String], self_exe: &Path) -> bool {
    Path::new(command) == self_exe
        && args.first().map(String::as_str) == Some("mcp-proxy")
        && args.get(1).map(String::as_str) == Some(PROXY_SPAWN_SCOPED_FLAG)
}

#[cfg(test)]
mod tests;
