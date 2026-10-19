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

/// The spawn-time half. Nothing changes for a spawn that has no third-party
/// server to register:
///
/// - a utility call ([`crate::runtime_dispatch::in_utility_call`]: summaries,
///   classifiers, judges on the employee's runtime) never gets the
///   employee's servers, as on the Claude utility path, and does not read
///   `.mcp.json` at all;
/// - no `.mcp.json`, or one listing only DuDuClaw's own server (team role
///   scaffolds write exactly that) ⇒ no spawn gate, no write, the pre-change
///   spawn;
/// - an unreadable or malformed `.mcp.json` ⇒ refused (fail closed; before
///   2026-10-19 Codex did not read the file at all).
///
/// Only when third-party servers are listed does the `.mcp.json` spawn gate
/// run (as for a Claude CLI spawn), then [`gated_servers`]. The gate writes
/// nothing when it fails; when it succeeds it may rewrite only the file's
/// DuDuClaw entries (atomically, owner-only, other entries kept as they are),
/// the same repair every Claude spawn and every gateway boot performs.
pub(crate) async fn gated_servers_for_spawn(
    agent_dir: &Path,
    home_dir: &Path,
    own_server: &Value,
    explore: bool,
) -> Result<BTreeMap<String, GatedServer>, String> {
    if crate::runtime_dispatch::in_utility_call() {
        return Ok(BTreeMap::new());
    }
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
/// `CodexRuntime::render_mcp_toml`). The upstream server's own env
/// ([`PROXY_UPSTREAM_ENV_VAR`], its credentials) is **not** written to this
/// persistent file: a spawn supplies it through the per-spawn file named in
/// the `-c` overrides, which Codex merges over this table. A manual `codex`
/// run from this file alone starts the server without its declared env. The
/// explore lane is per spawn and also travels only in the `-c` overrides.
pub(crate) fn file_entries(servers: &BTreeMap<String, GatedServer>) -> HashMap<String, Value> {
    servers
        .iter()
        .map(|(name, s)| {
            let env: BTreeMap<&String, &String> = s
                .env
                .iter()
                .filter(|(k, _)| k.as_str() != PROXY_UPSTREAM_ENV_VAR)
                .collect();
            (
                name.clone(),
                serde_json::json!({
                    "command": s.command,
                    "args": s.args,
                    "env": env,
                    "default_tools_approval_mode": "auto",
                }),
            )
        })
        .collect()
}

/// Directory under the DuDuClaw home holding the per-spawn upstream-env files.
pub(crate) const ENV_FILE_DIR: &str = "mcp-proxy-env";
/// Name prefix of a per-spawn upstream-env file.
const ENV_FILE_PREFIX: &str = "duduclaw-mcp-env-";
/// A file older than this at gateway start belongs to no running spawn (a
/// Codex run is cut off after two minutes).
pub(crate) const ENV_FILE_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// `<home>/tmp/mcp-proxy-env`, created owner-only (0700 on Unix). A symbolic
/// link or a non-directory there is refused rather than followed.
fn env_file_dir(home_dir: &Path) -> Result<std::path::PathBuf, String> {
    let dir = home_dir.join("tmp").join(ENV_FILE_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Err(format!("{} is not a plain directory", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return env_file_dir(home_dir);
                }
                Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
            }
        }
        Err(e) => return Err(format!("cannot inspect {}: {e}", dir.display())),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    }
    Ok(dir)
}

/// Remove upstream-env files left behind by a gateway that stopped while a
/// Codex spawn was running (a dropped guard normally deletes them). Called
/// once at gateway start; only files with this module's prefix and older
/// than [`ENV_FILE_STALE_AFTER`] are removed. Returns how many.
pub fn sweep_stale_env_files(home_dir: &Path) -> usize {
    sweep_env_files_older_than(home_dir, ENV_FILE_STALE_AFTER)
}

fn sweep_env_files_older_than(home_dir: &Path, max_age: std::time::Duration) -> usize {
    let dir = home_dir.join("tmp").join(ENV_FILE_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(ENV_FILE_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= max_age);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
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
/// The env files live in `<home_dir>/tmp/mcp-proxy-env` (0700), swept at
/// gateway start ([`sweep_stale_env_files`]) in case a crash left one behind.
///
/// `Err` when a temp file cannot be written; the caller refuses the spawn
/// rather than start a server without its credentials.
pub(crate) fn override_args(
    servers: &BTreeMap<String, GatedServer>,
    supports_env_vars: bool,
    lane: Option<&(String, String)>,
    taken: &[(String, String)],
    home_dir: &Path,
) -> Result<ThirdPartyOverrides, String> {
    let mut out = ThirdPartyOverrides::default();
    if servers.is_empty() {
        return Ok(out);
    }
    let env_dir = env_file_dir(home_dir)?;
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
                let path = write_env_file(&env_dir, v)?;
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

/// Write one proxy's upstream env JSON to an owner-only temp file in `dir`
/// (`tempfile` creates it 0600 before anything is written).
fn write_env_file(dir: &Path, body: &str) -> Result<tempfile::TempPath, String> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix(ENV_FILE_PREFIX)
        .suffix(".json")
        .tempfile_in(dir)
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
