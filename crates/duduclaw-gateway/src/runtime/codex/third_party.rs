//! Third-party MCP servers for a Codex spawn (2026-10-19).
//!
//! Codex never reads an employee's `.mcp.json`: the runtime registers every
//! MCP server itself, as `-c mcp_servers.<id>.…` overrides on the command line
//! (what a spawn actually loads) and in `<agent_dir>/.codex/config.toml` (read
//! when an operator points `CODEX_HOME` there or trusts the directory as a
//! Codex project). Until this module only DuDuClaw's own server was
//! registered, so an employee's other servers (a broker, a database) were
//! silently missing on Codex.
//!
//! The servers come from the same gated rewrite the Claude CLI path uses
//! ([`crate::redaction_proxy::gated_mcp_config_for_runtime`] →
//! `duduclaw_core::mcp_proxy_rewrite::rewrite_mcp_config_for_gate`, with the
//! third-party tool gate always on): stdio servers run through `duduclaw
//! mcp-proxy` (classification from annotations, `action_rules`, explore
//! lane, approvals; redaction when active), DuDuClaw's remote bridges gate
//! themselves, and direct URL entries are removed. Anything else the rewrite
//! leaves behind (an entry without a command, a name that is not a bare TOML
//! key) is dropped, never handed to Codex unproxied.
//!
//! Codex runs with `approval_policy=never`, so each of these servers is
//! registered with `default_tools_approval_mode = "auto"`, like the duduclaw
//! server: the decision is made by the proxy or the bridge one process over,
//! where nobody has to answer a prompt in an unattended terminal.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::Value;
use tracing::warn;

use duduclaw_core::mcp_proxy_rewrite::{
    PROXY_CARRY_ENV, PROXY_UPSTREAM_ENV_FILE_VAR, PROXY_UPSTREAM_ENV_VAR, is_duduclaw_server,
    is_remote_bridge_invocation,
};

use super::{
    env_value_as_string, is_safe_config_key_segment, is_secret_env_name, toml_string_literal,
};

/// Longest server name accepted (same bound as `mcp_scan::is_valid_mcp_server_name`).
const MAX_SERVER_NAME_LEN: usize = 64;

/// One gated third-party server as Codex receives it.
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

/// The spawn-time half: the `.mcp.json` spawn gate first (the file's
/// DuDuClaw entry is confirmed or the spawn is refused, exactly as for a
/// Claude CLI spawn), then [`gated_servers`].
pub(crate) async fn gated_servers_for_spawn(
    agent_dir: &Path,
    home_dir: &Path,
    own_server: &Value,
    explore: bool,
) -> Result<BTreeMap<String, GatedServer>, String> {
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
/// remote bridge, and only under a name that is safe as a bare TOML key.
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
        if name.len() > MAX_SERVER_NAME_LEN || !is_safe_config_key_segment(name) {
            warn!(runtime = "codex", server = %name, "MCP server name is not a bare TOML key — not registered");
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
                runtime = "codex",
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
fn string_env(def: &Value) -> BTreeMap<String, String> {
    def.get("env")
        .and_then(|e| e.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| env_value_as_string(v).map(|s| (k.clone(), s)))
                .collect()
        })
        .unwrap_or_default()
}

/// The `config.toml` form of each server (rendered by
/// `CodexRuntime::render_mcp_toml`). The upstream env stays inline: the
/// file is owner-only, like the `.mcp.json` it comes from. The explore lane
/// is per spawn and travels only in the `-c` overrides.
pub(crate) fn file_entries(servers: &BTreeMap<String, GatedServer>) -> HashMap<String, Value> {
    servers
        .iter()
        .map(|(name, s)| {
            (
                name.clone(),
                serde_json::json!({
                    "command": s.command,
                    "args": s.args,
                    "env": s.env,
                    "default_tools_approval_mode": "auto",
                }),
            )
        })
        .collect()
}

/// What one spawn needs to register the third-party servers.
#[derive(Debug, Default)]
pub(crate) struct ThirdPartyOverrides {
    /// `-c key=value` pairs appended to the Codex argv.
    pub args: Vec<String>,
    /// `(name, value)` pairs to set on the Codex process (credential-shaped
    /// names forwarded with `env_vars`).
    pub process_env: Vec<(String, String)>,
    /// Owner-only per-spawn files holding each proxy's upstream env; deleted
    /// when dropped, so the caller holds them until Codex has exited.
    pub temp_files: Vec<tempfile::TempPath>,
}

/// The `-c` overrides registering `servers` for one spawn.
///
/// - The upstream server's own env ([`PROXY_UPSTREAM_ENV_VAR`]) is written to
///   an owner-only temp file and only its path
///   ([`PROXY_UPSTREAM_ENV_FILE_VAR`]) reaches `argv`.
/// - Credential-shaped names (`DUDUCLAW_MCP_API_KEY`) are forwarded with
///   `env_vars` on Codex versions that support it, unless `taken` (the
///   process env already set for the duduclaw server) holds the same name
///   with a different value; otherwise they stay in the `env` table, the
///   same trade-off the duduclaw server makes on old Codex.
/// - `lane` (`DUDUCLAW_LANE=explore`) is added to every server: Codex hands
///   an MCP child only the variables it is told to, and the proxy reads the
///   lane from its own environment.
///
/// `Err` when a temp file cannot be written; the caller refuses the spawn
/// rather than start a server without its credentials.
pub(crate) fn override_args(
    servers: &BTreeMap<String, GatedServer>,
    supports_env_vars: bool,
    lane: Option<&(String, String)>,
    taken: &[(String, String)],
) -> Result<ThirdPartyOverrides, String> {
    let mut out = ThirdPartyOverrides::default();
    let mut process_env: Vec<(String, String)> = taken.to_vec();
    for (name, s) in servers {
        let key = format!("mcp_servers.{name}");
        push(
            &mut out.args,
            &key,
            "command",
            &toml_string_literal(&s.command),
        );
        let args: Vec<String> = s.args.iter().map(|a| toml_string_literal(a)).collect();
        push(
            &mut out.args,
            &key,
            "args",
            &format!("[{}]", args.join(", ")),
        );
        push(
            &mut out.args,
            &key,
            "default_tools_approval_mode",
            &toml_string_literal("auto"),
        );

        let mut forwarded: Vec<String> = Vec::new();
        for (k, v) in &s.env {
            if !is_safe_config_key_segment(k) {
                warn!(runtime = "codex", server = %name, key = %k, "MCP env key is not a bare TOML key — dropped");
                continue;
            }
            if k == PROXY_UPSTREAM_ENV_VAR {
                let path = write_env_file(v)?;
                let rendered = path.to_string_lossy().into_owned();
                push_env(&mut out.args, &key, PROXY_UPSTREAM_ENV_FILE_VAR, &rendered);
                out.temp_files.push(path);
                continue;
            }
            if supports_env_vars && is_secret_env_name(k) {
                let same_value = process_env
                    .iter()
                    .find(|(pk, _)| pk == k)
                    .map(|(_, pv)| pv == v);
                match same_value {
                    Some(true) => {
                        forwarded.push(k.clone());
                        continue;
                    }
                    None => {
                        process_env.push((k.clone(), v.clone()));
                        out.process_env.push((k.clone(), v.clone()));
                        forwarded.push(k.clone());
                        continue;
                    }
                    Some(false) => {
                        warn!(
                            runtime = "codex",
                            server = %name,
                            key = %k,
                            "credential name already carries another value for this spawn — passed in the env table"
                        );
                    }
                }
            }
            push_env(&mut out.args, &key, k, v);
        }
        if let Some((lk, lv)) = lane {
            push_env(&mut out.args, &key, lk, lv);
        }
        if !forwarded.is_empty() {
            let names: Vec<String> = forwarded.iter().map(|n| toml_string_literal(n)).collect();
            push(
                &mut out.args,
                &key,
                "env_vars",
                &format!("[{}]", names.join(", ")),
            );
        }
    }
    Ok(out)
}

fn push(args: &mut Vec<String>, server_key: &str, field: &str, value: &str) {
    args.push("-c".to_string());
    args.push(format!("{server_key}.{field}={value}"));
}

fn push_env(args: &mut Vec<String>, server_key: &str, name: &str, value: &str) {
    push(
        args,
        server_key,
        &format!("env.{name}"),
        &toml_string_literal(value),
    );
}

/// Write one proxy's upstream env JSON to an owner-only temp file.
fn write_env_file(body: &str) -> Result<tempfile::TempPath, String> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("duduclaw-mcp-env-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| format!("temp file creation failed: {e}"))?;
    file.write_all(body.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|e| format!("temp file write failed: {e}"))?;
    let path = file.into_temp_path();
    duduclaw_core::platform::set_owner_only(&path).ok();
    Ok(path)
}

#[cfg(test)]
mod tests;
