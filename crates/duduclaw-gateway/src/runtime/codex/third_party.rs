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

use duduclaw_core::mcp_proxy_rewrite::{PROXY_UPSTREAM_ENV_FILE_VAR, PROXY_UPSTREAM_ENV_VAR};

use super::{is_safe_config_key_segment, is_secret_env_name, toml_string_literal};

// The runtime-agnostic half (gated extraction, the spawn-time gate, the
// env-file directory and its sweep) moved to `runtime/third_party_mcp.rs`
// on 2026-10-20 so Grok and the tool loop share it; Codex
// behaviour is unchanged.
#[cfg(test)]
use crate::runtime::third_party_mcp::env_files::{
    ENV_FILE_DIR, ENV_FILE_PREFIX, env_file_dir, sweep_env_files_older_than, sweep_stale_env_files,
};
#[cfg(test)]
use crate::runtime::third_party_mcp::gated_servers;
use crate::runtime::third_party_mcp::env_files::{env_file_dir as spawn_env_root, write_env_file};
pub(crate) use crate::runtime::third_party_mcp::{GatedServer, gated_servers_for_spawn};

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
/// gateway start (`third_party_mcp::env_files::sweep_stale_env_files`) in
/// case a crash left one behind.
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
    let env_dir = spawn_env_root(home_dir)?;
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

#[cfg(test)]
mod tests;
