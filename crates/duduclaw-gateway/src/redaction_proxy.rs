//! RFC-23 §13.6 (WP-P): make redaction reach the tool surfaces that are
//! **not** DuDuClaw's own MCP server.
//!
//! Two halves, one purpose.
//!
//! 1. **Claude-runtime spawn rewrite** — [`maybe_proxy_mcp_config`]. Every
//!    third-party stdio server is routed through `duduclaw mcp-proxy` for
//!    ThirdPartyGate, with optional redaction on the same path. Direct remote
//!    entries are refused; remote bridges gate themselves. Built-in-only
//!    files are frozen too, so strict config disables ambient discovery.
//!
//! 2. **Direct-API tool loop** — [`RedactionToolInterceptor`], a
//!    [`duduclaw_llm::ToolInterceptor`]. The openai-compat runtime drives
//!    `run_tool_loop` in-process, so there is no subprocess to wrap; the
//!    interceptor is the in-process equivalent of the proxy's two hooks.
//!
//! ## Why this module does not call `duduclaw-cli`
//!
//! `mcp_redaction::{redact_tool_result_with, decide_tool_args_with}` live in
//! `duduclaw-cli`, and `duduclaw-cli` **depends on** `duduclaw-gateway` — the
//! reverse dependency would be a cycle. The interceptor therefore drives
//! `duduclaw_redaction` directly, with deliberately identical semantics to
//! those two functions (including their fail-closed behaviour); the
//! constants that must agree across the boundary are called out below.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde_json::Value;
use tracing::{debug, warn};

use duduclaw_llm::{InterceptDecision, ToolInterceptor};
use duduclaw_redaction::{EgressDecision, RedactionManager};

/// Env var carrying the wrapped server's original `.mcp.json` `env` map,
/// JSON-encoded, to the proxy process. **Must match
/// `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_VAR`** (checked by
/// `proxy_env_var_name_is_the_documented_contract` in `duduclaw-cli`).
pub use duduclaw_core::mcp_proxy_rewrite::{PROXY_CARRY_ENV, PROXY_UPSTREAM_ENV_VAR};
use duduclaw_core::mcp_proxy_rewrite::{ProxyPurpose, rewrite_mcp_config_for_gate};

/// Placeholder substituted for a tool result the pipeline could not redact.
/// Mirrors `duduclaw_cli::mcp_redaction::REDACTION_FAILED_PLACEHOLDER`.
pub const REDACTION_FAILED_PLACEHOLDER: &str = "[redaction failed — value withheld]";

// ── 1. Spawn-time `.mcp.json` rewrite ───────────────────────────────────────

/// Rewrite every third-party **stdio** server so it launches through
/// `duduclaw mcp-proxy` for redaction (remote bridges included). The pure
/// rewrite lives in `duduclaw_core::mcp_proxy_rewrite`, shared with the
/// heartbeat explore-lane spawn and the third-party tool gate.
#[cfg(test)]
pub fn rewrite_mcp_config_for_proxy(json: &Value, self_exe: &Path, own_server: &Value) -> Result<Value, String> {
    duduclaw_core::mcp_proxy_rewrite::rewrite_mcp_config_for_proxy_with(
        json,
        self_exe,
        ProxyPurpose { redaction: true, tool_gate: false, secret_env: false },
        own_server,
    )
}

/// Is RFC-23 redaction active for a spawn out of `home_dir`?
///
/// The spawn helpers do not hold the gateway's `RedactionManager`, so this
/// resolves the same layers [`crate::redaction_integration`] does, in the
/// same order: the emergency force-disable flag file and
/// `DUDUCLAW_REDACTION=off` / `--redact=off` beat everything, otherwise
/// `config.toml [redaction] enabled` decides. Deliberately the same predicate
/// the spawned `duduclaw mcp-proxy` itself applies
/// (`McpRedactionLayer::try_init`), so the rewrite never installs a proxy
/// that would then do nothing.
///
/// A **poisoned** config (present but unparseable) counts as active: the
/// gateway is already shouting about it, and routing through a proxy that
/// refuses to start is the fail-closed outcome — better than silently
/// shipping unredacted rows.
pub fn redaction_active_for_spawn(home_dir: &Path) -> bool {
    use crate::redaction_integration::{BootOutcome, cli_flag_from_env, force_disable_active};
    use duduclaw_redaction::{CliFlag, EnvSetting};

    if force_disable_active(home_dir) {
        return false;
    }
    if EnvSetting::from_env() == EnvSetting::Off {
        return false;
    }
    if cli_flag_from_env() == CliFlag::Off {
        return false;
    }
    let raw = std::fs::read_to_string(home_dir.join("config.toml")).ok();
    matches!(
        crate::redaction_integration::classify_redaction_boot(raw.as_deref()),
        BootOutcome::Enabled(_) | BootOutcome::Poisoned(_)
    )
}

// ── RFC-23 §14.4: data-file guard mode ──────────────────────────────────────

/// The three legal values of `config.toml [redaction] data_file_guard`.
///
/// `on` is the default and the strictest: with redaction active, a built-in
/// `Read` of a CSV/spreadsheet and a `Bash` command naming one are both
/// refused, so the model has to use `csv_read` / `xlsx_read` / `file_read` —
/// the only route that passes the redaction choke point. `read_only` gates
/// `Read` alone (for an agent whose job genuinely needs shell tooling over
/// data files); `off` disables the guard entirely.
pub const DATA_FILE_GUARD_MODES: &[&str] = &["on", "read_only", "off"];

/// Default when the key is absent (§14.4: 預設 `on`).
pub const DATA_FILE_GUARD_DEFAULT: &str = "on";

/// Read `[redaction] data_file_guard` straight out of `config.toml`.
///
/// Deliberately a raw-TOML read rather than a field on
/// `duduclaw_redaction::RedactionConfig`: the spawn sites that need this value
/// do not hold a parsed config (that is exactly why
/// [`redaction_active_for_spawn`] exists next door), and this keeps the
/// gateway's copy of the setting independent of the redaction crate's own
/// config struct.
///
/// Unreadable / unparseable / missing / unrecognized ⇒ the default `on`. An
/// operator who wants the guard off has to say so in a value this function
/// recognizes; a typo leaves the protection on, not off.
pub fn data_file_guard_mode(home_dir: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return DATA_FILE_GUARD_DEFAULT.to_string();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return DATA_FILE_GUARD_DEFAULT.to_string();
    };
    let value = doc
        .get("redaction")
        .and_then(|r| r.get("data_file_guard"))
        .and_then(|v| v.as_str())
        .unwrap_or(DATA_FILE_GUARD_DEFAULT)
        .trim()
        .to_ascii_lowercase();
    if DATA_FILE_GUARD_MODES.contains(&value.as_str()) {
        value
    } else {
        DATA_FILE_GUARD_DEFAULT.to_string()
    }
}

/// The value `DUDUCLAW_DATA_FILE_GUARD` should carry for a spawn out of
/// `home_dir`, or `None` when the env var must not be set at all.
///
/// `None` in two cases — redaction is not active for this spawn (there is
/// nothing to protect, and the guard would only get in the agent's way), or
/// the operator set the mode to `off`. Both leave the spawned CLI
/// byte-identical to its pre-§14.4 environment.
pub fn data_file_guard_env_for_spawn(home_dir: &Path) -> Option<String> {
    if !redaction_active_for_spawn(home_dir) {
        return None;
    }
    let mode = data_file_guard_mode(home_dir);
    if mode == "off" { None } else { Some(mode) }
}

/// Produce the `--mcp-config` path this spawn should use.
///
/// Missing files produce an empty managed config; even built-in-only files
/// get a frozen copy. Unreadable or malformed configs refuse the spawn.
/// `Some(temp)` must be passed with
/// `--strict-mcp-config` and held until the child exits.
///
/// Same as [`maybe_proxy_mcp_config_in_lane`] outside the explore lane.
pub fn maybe_proxy_mcp_config(
    home_dir: &Path,
    mcp_json: &Path,
) -> Result<Option<tempfile::TempPath>, String> {
    maybe_proxy_mcp_config_in_lane(home_dir, mcp_json, false)
}

/// [`maybe_proxy_mcp_config`] for a spawn that may run in the explore lane
/// (`DUDUCLAW_LANE=explore` set on the CLI).
///
/// ThirdPartyGate is mandatory even without action rules or redaction.
/// Direct `url` / `type` entries are removed, logged with the migration action
/// and audited as `third_party_remote_entry_dropped`. Remote bridges already
/// enforce ThirdPartyGate; redaction may add an outer proxy.
///
/// Any failure to produce the gated copy refuses the spawn with
/// `SPAWN_GATE_ERROR_PREFIX` and `third_party_gate_config_unavailable`.
pub fn maybe_proxy_mcp_config_in_lane(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
) -> Result<Option<tempfile::TempPath>, String> {
    maybe_proxy_mcp_config_with_agent_dir(home_dir, mcp_json, mcp_json.parent(), explore_lane, true)
}

/// Rewrite a copied config using its host-known employee, not the copy's path
/// or identity claimed by raw JSON. Policy checks use the original employee too.
/// Unlike an employee directory, a copied project may never have run built-in
/// repair, so it must publish canonical slots even with no third-party servers.
pub fn maybe_proxy_mcp_config_for_agent(
    home_dir: &Path,
    mcp_json: &Path,
    agent_dir: &Path,
) -> Result<Option<tempfile::TempPath>, String> {
    maybe_proxy_mcp_config_with_agent_dir(home_dir, mcp_json, Some(agent_dir), false, false)
}

fn maybe_proxy_mcp_config_with_agent_dir(
    home_dir: &Path,
    mcp_json: &Path,
    agent_dir: Option<&Path>,
    explore_lane: bool,
    preserve_cli_discovery: bool,
) -> Result<Option<tempfile::TempPath>, String> {
    let agent_id = agent_dir.and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str()).unwrap_or("unknown").to_string();
    // Every employee server must cross ThirdPartyGate, even without action
    // rules or redaction. Turning another guard off never bypasses this gate.
    let tool_gate = true;
    let parsed = read_mcp_json_for_spawn(
        home_dir, mcp_json, explore_lane, &agent_id, false,
    )?.unwrap_or_else(|| serde_json::json!({"mcpServers": {}}));
    let purpose = ProxyPurpose {
        redaction: redaction_active_for_spawn(home_dir),
        secret_env: duduclaw_core::mcp_proxy_rewrite::has_mcp_secret_refs(&parsed),
        tool_gate,
    };
    // Secret ownership comes from a host-known employee directory, never
    // from the basename of an arbitrary copied workspace or its raw built-in.
    if purpose.secret_env && !agent_dir.is_some_and(|dir| {
        duduclaw_core::is_valid_agent_id(&agent_id)
            && (dir == home_dir.join("agents").join(&agent_id)
                || dir == home_dir.join("agents/.ephemeral").join(&agent_id))
    }) {
        return Err(refuse_gate_config_unavailable(
            home_dir, &agent_id, "cannot verify MCP secret owner outside an employee directory", explore_lane,
        ));
    }
    let self_exe = duduclaw_core::resolve_duduclaw_bin();
    let has_bridge = duduclaw_core::mcp_proxy_rewrite::has_remote_bridge_entries(&parsed, &self_exe);
    if purpose.none() && preserve_cli_discovery && !has_bridge {
        return Ok(None);
    }
    let own_server = duduclaw_core::mcp_proxy_rewrite::canonical_duduclaw_server(
        home_dir, &agent_id, &self_exe,
    );
    let produced = write_gated_mcp_config_value(
        &parsed, &self_exe, purpose, &own_server, preserve_cli_discovery,
    );
    match produced {
        Ok((guard, dropped)) => {
            audit_dropped_remote_entries(home_dir, &agent_id, &dropped, explore_lane);
            Ok(guard)
        }
        Err(e) => Err(refuse_gate_config_unavailable(home_dir, &agent_id, &e, explore_lane)),
    }
}

/// Audit each direct remote entry the third-party tool gate removed from a
/// spawn (`third_party_remote_entry_dropped`). Shared by the Claude CLI path
/// ([`maybe_proxy_mcp_config_in_lane`]) and the Codex path
/// ([`gated_mcp_config_for_runtime`]).
fn audit_dropped_remote_entries(
    home_dir: &Path,
    agent_id: &str,
    dropped: &[String],
    explore_lane: bool,
) {
    for server in dropped {
        audit_spawn_config(
            home_dir,
            "third_party_remote_entry_dropped",
            agent_id,
            serde_json::json!({
                "server": server,
                "explore_lane": explore_lane,
                "fix": "connect it with mcp.remote_connect so it runs through the bridge",
            }),
        );
    }
}

/// The third-party tool gate is needed and its config cannot be produced:
/// audit `third_party_gate_config_unavailable` and return the spawn-gate
/// error the caller refuses the spawn with (fail closed).
fn refuse_gate_config_unavailable(
    home_dir: &Path,
    agent_id: &str,
    error: &str,
    explore_lane: bool,
) -> String {
    warn!(agent = %agent_id, error = %error, "third-party tool gate: gated .mcp.json unavailable — refusing the spawn");
    audit_spawn_config(
        home_dir,
        "third_party_gate_config_unavailable",
        agent_id,
        serde_json::json!({ "error": duduclaw_core::truncate_bytes(error, 300), "explore_lane": explore_lane }),
    );
    duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
        "這次工作需要第三方 MCP 代理，但無法產生代理設定（{}），所以沒有啟動。",
        duduclaw_core::truncate_bytes(error, 200)
    ))
}

/// The gated `.mcp.json` for a runtime that registers MCP servers itself
/// instead of reading the file (Codex, 2026-10-19).
///
/// Same rewrite the Claude CLI path uses
/// ([`duduclaw_core::mcp_proxy_rewrite::rewrite_mcp_config_for_gate`]), with
/// the third-party tool gate **always** a purpose: such a runtime cannot
/// answer an approval prompt and is told to auto-approve the servers it is
/// given, so the proxy (or the remote bridge) has to be the gate. Every
/// third-party stdio server comes back wrapped in `duduclaw mcp-proxy`,
/// remote bridges come back unchanged (they gate themselves), and direct
/// `url` / `type` entries are removed and audited.
///
/// `own_server` replaces the exact host-owned names (`duduclaw`,
/// `duduclaw-pro`, this instance's `duduclaw-<instance>`) under the key `duduclaw` before the
/// rewrite, so a third-party entry can never take that name and the proxies
/// carry the identity and home this spawn actually uses.
///
/// - No `.mcp.json` ⇒ `Ok(None)` (nothing but DuDuClaw's own server).
/// - Unreadable or malformed file, or an `mcpServers` that is not an object
///   ⇒ `Err` starting with
///   [`duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX`], audited
///   `third_party_gate_config_unavailable`: the caller refuses the spawn.
pub fn gated_mcp_config_for_runtime(
    home_dir: &Path,
    mcp_json: &Path,
    own_server: &Value,
    self_exe: &Path,
    explore_lane: bool,
) -> Result<Option<Value>, String> {
    Ok(
        gated_mcp_config_with_names(home_dir, mcp_json, own_server, self_exe, explore_lane)?
            .map(|(config, _)| config),
    )
}

/// [`gated_mcp_config_for_runtime`] plus every server name the file listed
/// (sorted, DuDuClaw's own included), both from the **same** read of the
/// file. The Grok runtime shadows those names, so the shadow list and the
/// gated servers can never come from two different versions of the file.
pub fn gated_mcp_config_with_names(
    home_dir: &Path,
    mcp_json: &Path,
    own_server: &Value,
    self_exe: &Path,
    explore_lane: bool,
) -> Result<Option<(Value, Vec<String>)>, String> {
    let agent_id = agent_id_of(mcp_json);
    let Some(mut parsed) = read_mcp_json_for_runtime(home_dir, mcp_json, explore_lane)? else {
        return Ok(None);
    };
    // Grok must shadow every declared name, including dropped entries, or it
    // would start those entries itself.
    let names: Vec<String> = parsed["mcpServers"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let servers = parsed
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .expect("read_mcp_json_for_runtime normalises mcpServers to an object");
    servers.retain(|name, def| !duduclaw_core::mcp_proxy_rewrite::is_host_owned_entry(name, def, self_exe));
    servers.insert("duduclaw".to_string(), own_server.clone());

    let purpose = ProxyPurpose {
        redaction: redaction_active_for_spawn(home_dir),
        secret_env: duduclaw_core::mcp_proxy_rewrite::has_mcp_secret_refs(&parsed),
        tool_gate: true,
    };
    let rewrite = duduclaw_core::mcp_proxy_rewrite::rewrite_mcp_config_for_runtime_gate(&parsed, self_exe, purpose, own_server)
        .map_err(|e| refuse_gate_config_unavailable(home_dir, &agent_id, &e, explore_lane))?;
    require_resolvable_config(&rewrite.config)
        .map_err(|e| refuse_gate_config_unavailable(home_dir, &agent_id, &e, explore_lane))?;
    audit_dropped_remote_entries(home_dir, &agent_id, &rewrite.dropped_remote, explore_lane);
    Ok(Some((rewrite.config, names)))
}

/// Does the employee's `.mcp.json` list any server that is not DuDuClaw's
/// own? Read-only (nothing is written). A runtime that registers servers
/// itself (Codex) runs the `.mcp.json` spawn gate and the gated rewrite only
/// when this is `true`, so an employee with no third-party server keeps the
/// spawn it had before 2026-10-19.
///
/// No file ⇒ `Ok(false)`. Unreadable or malformed file ⇒ the same audited
/// spawn-gate `Err` as [`gated_mcp_config_for_runtime`].
pub fn mcp_json_lists_third_party_servers(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
) -> Result<bool, String> {
    let Some(parsed) = read_mcp_json_for_runtime(home_dir, mcp_json, explore_lane)? else {
        return Ok(false);
    };
    let agent_id = agent_id_of(mcp_json);
    if agent_id == "unknown" {
        return Ok(true);
    }
    let exe = duduclaw_core::resolve_duduclaw_bin();
    let own = duduclaw_core::mcp_proxy_rewrite::canonical_duduclaw_server(home_dir, &agent_id, &exe);
    // Grok loads the raw file on the shortcut path. Only the complete current
    // host definition is safe there; stale credentials take the managed path
    // so the runtime can replace/shadow them without refusing the spawn.
    Ok(parsed["mcpServers"].as_object().is_none_or(|servers| {
        servers.iter().any(|(name, def)| {
            !duduclaw_core::mcp_proxy_rewrite::is_builtin_slot_name(name) || def != &own
        })
    }))
}

/// Every server name the `.mcp.json` at `mcp_json` lists, DuDuClaw's own
/// included (sorted). Read-only. Used by the Grok runtime, whose CLI
/// also loads the `.mcp.json` of its working directory by itself: every name
/// there must be shadowed in `.grok/config.toml` (2026-10-20).
///
/// No file ⇒ `Ok(None)`. Unreadable or malformed file ⇒ the same audited
/// spawn-gate `Err` as [`gated_mcp_config_for_runtime`].
pub fn mcp_json_server_names(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
) -> Result<Option<Vec<String>>, String> {
    let Some(parsed) = read_mcp_json_for_runtime(home_dir, mcp_json, explore_lane)? else {
        return Ok(None);
    };
    Ok(Some(
        parsed["mcpServers"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
    ))
}

fn agent_id_of(mcp_json: &Path) -> String {
    mcp_json
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Read and parse `.mcp.json`; `mcpServers` is guaranteed to be an object in
/// the result (absent ⇒ empty). No file ⇒ `Ok(None)`; anything else that
/// cannot be used ⇒ audited spawn-gate `Err`.
fn read_mcp_json_for_runtime(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
) -> Result<Option<Value>, String> {
    read_mcp_json_for_runtime_for_agent(home_dir, mcp_json, explore_lane, &agent_id_of(mcp_json))
}

fn read_mcp_json_for_runtime_for_agent(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
    agent_id: &str,
) -> Result<Option<Value>, String> {
    read_mcp_json_for_spawn(home_dir, mcp_json, explore_lane, agent_id, false)
}

fn read_mcp_json_for_spawn(
    home_dir: &Path,
    mcp_json: &Path,
    explore_lane: bool,
    agent_id: &str,
    allow_malformed_fallback: bool,
) -> Result<Option<Value>, String> {
    let refuse = |e: &str| refuse_gate_config_unavailable(home_dir, agent_id, e, explore_lane);
    let raw = match std::fs::read_to_string(mcp_json) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(refuse(&format!("unreadable .mcp.json: {e}"))),
    };
    let mut parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(_) if allow_malformed_fallback && !raw.contains("secret://") => return Ok(None),
        Err(e) => return Err(refuse(&format!("malformed .mcp.json: {e}"))),
    };
    let Some(root) = parsed.as_object_mut() else {
        return Err(refuse("malformed .mcp.json: the top level is not an object"));
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !servers.is_object() {
        return Err(refuse("malformed .mcp.json: mcpServers is not an object"));
    }
    let exe = duduclaw_core::resolve_duduclaw_bin();
    let own = duduclaw_core::mcp_proxy_rewrite::canonical_duduclaw_server(home_dir, agent_id, &exe);
    duduclaw_core::mcp_proxy_rewrite::validate_raw_mcp_config(&parsed, &exe, &own)
        .map_err(|e| refuse(&e))?;
    Ok(Some(parsed))
}

fn audit_spawn_config(home_dir: &Path, event: &str, agent_id: &str, details: Value) {
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            event,
            agent_id,
            duduclaw_security::audit::Severity::Warning,
            details,
        ),
    );
}

/// The IO half of [`maybe_proxy_mcp_config`] for redaction, split out so the
/// rewrite can be exercised against a real file without touching process
/// state. Any failure ⇒ `None` (the redaction-only fallback).
#[cfg(test)]
pub fn write_proxy_mcp_config(mcp_json: &Path, self_exe: &Path, own_server: &Value) -> Option<tempfile::TempPath> {
    write_proxy_mcp_config_for(
        mcp_json,
        self_exe,
        ProxyPurpose { redaction: true, tool_gate: false, secret_env: false },
        own_server,
    )
}

/// [`write_proxy_mcp_config`] for any [`ProxyPurpose`]; failures ⇒ `None`.
#[cfg(test)]
pub fn write_proxy_mcp_config_for(
    mcp_json: &Path,
    self_exe: &Path,
    purpose: ProxyPurpose,
    own_server: &Value,
) -> Option<tempfile::TempPath> {
    write_gated_mcp_config(mcp_json, self_exe, purpose, own_server)
        .map_err(|e| warn!(path = %mcp_json.display(), error = %e, "mcp-proxy: gated copy unavailable"))
        .ok()
        .and_then(|(guard, _)| guard)
}

/// Produce the rewritten temp config. `Ok((None, _))` ⇒ nothing changed;
/// the second half lists direct remote entries removed by the gate.
#[cfg(test)]
pub fn write_gated_mcp_config(
    mcp_json: &Path,
    self_exe: &Path,
    purpose: ProxyPurpose,
    own_server: &Value,
) -> Result<(Option<tempfile::TempPath>, Vec<String>), String> {
    let raw = std::fs::read_to_string(mcp_json).map_err(|e| format!("unreadable .mcp.json: {e}"))?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("malformed .mcp.json: {e}"))?;

    write_gated_mcp_config_value(&parsed, self_exe, purpose, own_server, true)
}

fn require_resolvable_config(config: &Value) -> Result<(), String> {
    if duduclaw_core::mcp_proxy_rewrite::has_unrouted_mcp_secret_refs(config) {
        return Err("third-party MCP env references could not be routed through the proxy".into());
    }
    Ok(())
}

fn write_gated_mcp_config_value(
    parsed: &Value,
    self_exe: &Path,
    purpose: ProxyPurpose,
    own_server: &Value,
    preserve_cli_discovery: bool,
) -> Result<(Option<tempfile::TempPath>, Vec<String>), String> {
    let mut rewrite = rewrite_mcp_config_for_gate(parsed, self_exe, purpose, own_server)?;
    require_resolvable_config(&rewrite.config)?;
    // A mandatory tool gate always freezes a copy, including built-in-only
    // files, so callers add strict config flags and disable ambient discovery.
    // Redaction-only test callers retain their historical no-op behavior.
    let third_party = parsed.get("mcpServers").and_then(Value::as_object)
        .is_some_and(|servers| servers.keys().any(|name|
            !duduclaw_core::mcp_proxy_rewrite::is_builtin_slot_name(name)));
    let has_bridge = duduclaw_core::mcp_proxy_rewrite::has_remote_bridge_entries(parsed, self_exe);
    if !purpose.tool_gate
        && ((preserve_cli_discovery && ((purpose.none() && !has_bridge) || !third_party)) || &rewrite.config == parsed)
    {
        debug!("mcp-proxy: nothing to proxy in this .mcp.json");
        return Ok((None, Vec::new()));
    }
    // RM-160: the Claude CLI's env carries no full employee token since
    // RM-166, so the proxies get it here, from the same host definition the
    // built-in entry of this file uses. Only this per-spawn, owner-only copy
    // holds it; the employee's `.mcp.json` is never written. Without a token
    // source nothing is attached and the proxy behaves as before.
    duduclaw_core::mcp_proxy_rewrite::attach_proxy_identity(&mut rewrite.config, self_exe, own_server);

    let mut file = tempfile::Builder::new()
        .prefix("duduclaw-mcp-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| format!("temp file creation failed: {e}"))?;
    {
        use std::io::Write;
        let body = serde_json::to_string_pretty(&rewrite.config).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("temp file write failed: {e}"))?;
        file.flush().map_err(|e| format!("temp file flush failed: {e}"))?;
    }
    let path = file.into_temp_path();
    // The file carries the upstream servers' own credentials, same as the
    // `.mcp.json` it was derived from.
    duduclaw_core::platform::set_owner_only(&path).ok();
    Ok((Some(path), rewrite.dropped_remote))
}

// ── 2. Direct-API tool-loop interceptor ─────────────────────────────────────

/// `<server>.<tool>` when the executor could attribute the call, bare `tool`
/// otherwise. Matches the namespacing `duduclaw mcp-proxy` applies (§13.6) so
/// one rule works on both paths.
pub fn qualified_tool_name(server: &str, tool: &str) -> String {
    if server.is_empty() {
        tool.to_string()
    } else {
        format!("{server}.{tool}")
    }
}

/// Operator-granted restore scopes, read the same way the MCP layer reads
/// them (`DUDUCLAW_REDACTION_SCOPES`, comma-separated).
fn redaction_scopes_from_env() -> Vec<String> {
    std::env::var("DUDUCLAW_REDACTION_SCOPES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn args_contain_tokens(args: &Value) -> bool {
    match args {
        Value::String(s) => s.contains(duduclaw_redaction::token::TOKEN_PREFIX),
        Value::Array(a) => a.iter().any(args_contain_tokens),
        Value::Object(m) => m.values().any(args_contain_tokens),
        _ => false,
    }
}

/// RFC-23 redaction as a [`ToolInterceptor`] — the in-process equivalent of
/// `duduclaw mcp-proxy` for the direct-API / openai-compat tool loop.
pub struct RedactionToolInterceptor {
    manager: Arc<RedactionManager>,
    agent_id: String,
    session_id: String,
}

impl RedactionToolInterceptor {
    pub fn new(manager: Arc<RedactionManager>, agent_id: String, session_id: String) -> Self {
        Self {
            manager,
            agent_id,
            session_id,
        }
    }
}

impl ToolInterceptor for RedactionToolInterceptor {
    fn before_call(&self, server: &str, tool: &str, args: Value) -> InterceptDecision {
        // Hot path: nothing token-shaped ⇒ nothing to restore or refuse.
        if !args_contain_tokens(&args) {
            return InterceptDecision::Allow(args);
        }
        let name = qualified_tool_name(server, tool);
        // C3: the caller is modelled as the *agent*, never the channel
        // end-user, so Owner-scoped PII is never exfiltrated to a tool.
        let caller =
            duduclaw_redaction::Caller::agent(self.agent_id.clone(), redaction_scopes_from_env());
        match self.manager.decide_tool_call(
            &name,
            &args,
            &self.agent_id,
            Some(&self.session_id),
            &caller,
        ) {
            Ok(EgressDecision::Allow { args, .. }) => InterceptDecision::Allow(args),
            Ok(EgressDecision::Passthrough(args)) => InterceptDecision::Allow(args),
            Ok(EgressDecision::Deny {
                reason,
                tokens_seen,
            }) => InterceptDecision::Deny(format!(
                "egress denied for '{name}': {reason} (tokens_seen={tokens_seen})"
            )),
            // I5 fail-closed: an evaluation error denies, never passes through.
            Err(e) => {
                warn!(tool = %name, error = %e, "RFC-23 interceptor: egress decision failed; denying");
                InterceptDecision::Deny(format!("egress denied for '{name}': redaction error: {e}"))
            }
        }
    }

    fn after_call(&self, server: &str, tool: &str, args: &Value, result: &mut Value) {
        let name = qualified_tool_name(server, tool);
        let pipeline = match self
            .manager
            .pipeline(&self.agent_id, Some(self.session_id.clone()))
        {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    tool = %name, agent = %self.agent_id, error = %e,
                    "RFC-23 interceptor: pipeline build failed; withholding the whole result"
                );
                *result = Value::String(REDACTION_FAILED_PLACEHOLDER.to_string());
                return;
            }
        };
        let ctx = duduclaw_redaction::ToolContext {
            tool_name: &name,
            args: Some(args),
        };
        if let Err(e) = pipeline.redact_value(result, &ctx) {
            warn!(
                tool = %name, agent = %self.agent_id, error = %e,
                "RFC-23 interceptor: redact failed; withholding the whole result"
            );
            *result = Value::String(REDACTION_FAILED_PLACEHOLDER.to_string());
        }
    }
}

// ── Manager resolution for the tool-loop path ───────────────────────────────

/// mtime-aware cache of the `config.toml`-derived manager, so an API-mode
/// turn does not re-open the vault and recompile every rule. Same shape as
/// `duduclaw_cli::mcp_auth::KeyRegistryCache`: a changed `config.toml` mtime
/// forces a rebuild, so `redaction.update` is observed on the next turn.
static MANAGER_CACHE: OnceLock<
    Mutex<Option<(PathBuf, Option<SystemTime>, Arc<RedactionManager>)>>,
> = OnceLock::new();

/// The turn's channel session id — what the vault keys tokens on, so the
/// reply path's `restore_for_channel` can later find them. Falls back to the
/// same `"mcp-session"` default `McpRedactionLayer` uses when there is no
/// ambient turn (cron, heartbeat, utility prompts).
pub fn current_session_id() -> String {
    duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(|s| s.clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| "mcp-session".to_string())
}

/// Build (or reuse) the interceptor for this turn.
///
/// - `Ok(None)` — redaction is not active for this home: the loop runs
///   exactly as before, zero overhead.
/// - `Err(_)` — redaction IS enabled but the manager could not be built.
///   Fail-closed (§10.2): the caller must refuse the tool path rather than
///   dispatch tools whose results would reach the model unredacted.
pub fn try_build_interceptor(
    home_dir: &Path,
    agent_id: &str,
    session_id: &str,
) -> Result<Option<Arc<RedactionToolInterceptor>>, String> {
    if !redaction_active_for_spawn(home_dir) {
        return Ok(None);
    }
    let manager = resolve_manager(home_dir)?;
    Ok(Some(Arc::new(RedactionToolInterceptor::new(
        manager,
        agent_id.to_string(),
        session_id.to_string(),
    ))))
}

fn resolve_manager(home_dir: &Path) -> Result<Arc<RedactionManager>, String> {
    let cfg_path = home_dir.join("config.toml");
    let mtime = std::fs::metadata(&cfg_path).and_then(|m| m.modified()).ok();

    let cache = MANAGER_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_home, cached_mtime, manager)) = guard.as_ref() {
            if cached_home == home_dir && *cached_mtime == mtime && mtime.is_some() {
                return Ok(manager.clone());
            }
        }
    }

    let raw = std::fs::read_to_string(&cfg_path)
        .map_err(|e| format!("cannot read {}: {e}", cfg_path.display()))?;
    let rcfg = match crate::redaction_integration::classify_redaction_boot(Some(&raw)) {
        crate::redaction_integration::BootOutcome::Enabled(cfg) => *cfg,
        crate::redaction_integration::BootOutcome::Disabled => {
            return Err("redaction became disabled between the gate and the build".to_string());
        }
        crate::redaction_integration::BootOutcome::Poisoned(reason) => {
            return Err(format!("redaction config is poisoned: {reason}"));
        }
    };
    let manager = crate::redaction_integration::build_manager_from_home(home_dir, rcfg)
        .map_err(|e| e.to_string())?;

    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((home_dir.to_path_buf(), mtime, manager.clone()));
    Ok(manager)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod redaction_data_file_guard_tests {
    use super::*;
    #[allow(unused_imports)]
    use serde_json::json;

    fn home_with(config: &str) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        if !config.is_empty() {
            std::fs::write(tmp.path().join("config.toml"), config).unwrap();
        }
        tmp
    }

    #[test]
    fn mode_defaults_to_on_when_unset_or_unreadable() {
        let empty = home_with("");
        assert_eq!(data_file_guard_mode(empty.path()), "on");

        let no_key = home_with("[redaction]\nenabled = true\n");
        assert_eq!(data_file_guard_mode(no_key.path()), "on");

        let broken = home_with("[redaction\nenabled = ");
        assert_eq!(
            data_file_guard_mode(broken.path()),
            "on",
            "a broken config must not silently disable the guard"
        );
    }

    #[test]
    fn mode_round_trips_the_three_legal_values() {
        for mode in ["on", "read_only", "off"] {
            let home = home_with(&format!(
                "[redaction]\nenabled = true\ndata_file_guard = \"{mode}\"\n"
            ));
            assert_eq!(data_file_guard_mode(home.path()), mode);
        }
        // Case and whitespace are forgiven; an unrecognized word is not.
        let loud = home_with("[redaction]\ndata_file_guard = \"  READ_ONLY \"\n");
        assert_eq!(data_file_guard_mode(loud.path()), "read_only");
        let typo = home_with("[redaction]\ndata_file_guard = \"readonly\"\n");
        assert_eq!(
            data_file_guard_mode(typo.path()),
            "on",
            "typo must fail safe"
        );
    }

    #[test]
    fn env_is_unset_when_redaction_is_inactive() {
        // No `[redaction] enabled = true` ⇒ nothing to protect.
        let home = home_with("[redaction]\ndata_file_guard = \"on\"\n");
        assert_eq!(data_file_guard_env_for_spawn(home.path()), None);
    }

    #[test]
    fn env_is_unset_when_the_operator_turns_the_guard_off() {
        let home = home_with(
            "[redaction]\nenabled = true\nprofiles = [\"general\"]\ndata_file_guard = \"off\"\n",
        );
        // Only meaningful if redaction really is active here; if the boot
        // classifier disagrees the assertion below still holds for the right
        // reason (None either way), so assert the mode separately.
        assert_eq!(data_file_guard_mode(home.path()), "off");
        assert_eq!(data_file_guard_env_for_spawn(home.path()), None);
    }

    #[test]
    fn env_carries_the_mode_when_redaction_is_active() {
        let home = home_with("[redaction]\nenabled = true\nprofiles = [\"general\"]\n");
        // Guard against an ambient `DUDUCLAW_REDACTION=off` in the developer's
        // shell turning this into a vacuous pass.
        if !redaction_active_for_spawn(home.path()) {
            return;
        }
        assert_eq!(
            data_file_guard_env_for_spawn(home.path()),
            Some("on".to_string()),
            "default mode must reach the spawn when redaction is active"
        );
    }
}

#[cfg(test)]
mod mcp_config_proxy_tests {
    use super::*;
    #[allow(unused_imports)]
    use serde_json::json;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/duduclaw/bin/duduclaw")
    }

    fn own_fixture() -> Value {
        json!({
            "command": "/opt/duduclaw/bin/duduclaw",
            "args": ["mcp-server"],
            "env": {
                "DUDUCLAW_HOME": "/home/u/.duduclaw",
                "DUDUCLAW_AGENT_ID": "agnes",
                "DUDUCLAW_AGENT_TOKEN": "mac-do-not-forward",
                "DUDUCLAW_MCP_API_KEY": "ddc_dev_0123456789abcdef0123456789abcdef"
            }
        })
    }

    fn sample() -> Value {
        json!({
            "mcpServers": {
                "duduclaw": own_fixture(),
                "crm_pg": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-postgres", "postgres://x"],
                    "env": {"PGPASSWORD": "hunter2"}
                },
                "remote_erp": {
                    "type": "http",
                    "url": "https://erp.example.com/mcp"
                }
            }
        })
    }

    fn agent_mcp_path(home: &Path) -> PathBuf {
        let agent_dir = home.join("agents/alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        agent_dir.join(".mcp.json")
    }

    fn canonical_builtin(home: &Path) -> Value {
        duduclaw_core::mcp_proxy_rewrite::canonical_duduclaw_server(
            home, "alice", &duduclaw_core::resolve_duduclaw_bin(),
        )
    }

    fn runtime_sample(home: &Path) -> Value {
        let mut config = sample();
        config["mcpServers"]["duduclaw"] = canonical_builtin(home);
        config
    }

    /// Run the real writer and the later spawn in isolated processes so key
    /// rotation does not mutate env shared with parallel tests.
    #[test]
    fn production_writer_survives_key_rotation_and_executable_changes() {
        const TEST: &str = "redaction_proxy::mcp_config_proxy_tests::production_writer_survives_key_rotation_and_executable_changes";
        const OLD_KEY: &str = "ddc_prod_11111111111111111111111111111111";
        const NEW_KEY: &str = "ddc_prod_22222222222222222222222222222222";
        if let Ok(stage) = std::env::var("DDC_TEST_MCP_ROTATION_STAGE") {
            let home = PathBuf::from(std::env::var_os("DDC_TEST_MCP_ROTATION_HOME").unwrap());
            let name = std::env::var("DDC_TEST_MCP_ROTATION_NAME").unwrap();
            let dir = home.join("agents/alice");
            let path = dir.join(".mcp.json");
            if stage == "write" {
                std::fs::create_dir_all(&dir).unwrap();
                duduclaw_core::ensure_identity_key(&home).unwrap();
                // Seed slots only; the production writer generates the entire
                // built-in definition, including its real employee token.
                std::fs::write(&path, json!({"mcpServers": {
                    name.clone(): {}, "vendor": {"command":"vendor-mcp"}
                }}).to_string()).unwrap();
                assert!(duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path(&dir).unwrap());
                let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(raw["mcpServers"][&name]["env"]["DUDUCLAW_MCP_API_KEY"], OLD_KEY);
                assert!(raw["mcpServers"][&name]["env"]["DUDUCLAW_AGENT_TOKEN"].is_string());
            } else {
                assert_eq!(stage, "spawn");
                let before = std::fs::read(&path).unwrap();
                let raw: Value = serde_json::from_slice(&before).unwrap();
                let exe = duduclaw_core::resolve_duduclaw_bin();
                let own = duduclaw_core::mcp_proxy_rewrite::canonical_duduclaw_server(&home, "alice", &exe);
                assert_ne!(raw["mcpServers"][&name]["command"], own["command"]);
                assert_eq!(own["env"]["DUDUCLAW_MCP_API_KEY"], NEW_KEY);
                let temp = maybe_proxy_mcp_config(&home, &path).unwrap().expect("default egress gate");
                let effective: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
                assert_eq!(effective["mcpServers"][&name], own);
                assert_eq!(effective["mcpServers"]["vendor"]["args"][0], "mcp-proxy");
                assert_eq!(effective["mcpServers"]["vendor"]["env"]["DUDUCLAW_MCP_API_KEY"], NEW_KEY);
                let runtime = gated_mcp_config_for_runtime(&home, &path, &own, &exe, false).unwrap().unwrap();
                assert_eq!(runtime["mcpServers"]["duduclaw"], own);
                assert_eq!(std::fs::read(&path).unwrap(), before);
                // The real pre-spawn gate also repairs a forged built-in-only
                // file before Claude's no-flags compatibility path uses it.
                std::fs::write(&path, json!({"mcpServers": {name.clone(): {
                    "command":"/tmp/evil", "env":{"DUDUCLAW_AGENT_ID":"victim"}
                }}}).to_string()).unwrap();
                assert!(matches!(
                    duduclaw_agent::mcp_template::prepare_mcp_config_for_spawn(&dir).unwrap(),
                    duduclaw_agent::mcp_spawn_gate::McpGateOutcome::Confirmed
                ));
                let prepared: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert_eq!(prepared["mcpServers"][&name], own);
                assert!(maybe_proxy_mcp_config(&home, &path).unwrap().is_some());
                std::fs::write(home.join("spawn-verified"), b"ok").unwrap();
            }
            return;
        }
        let test_exe = std::env::current_exe().unwrap();
        let new_exe = if cfg!(windows) {
            PathBuf::from(r"C:\Windows\System32\cmd.exe")
        } else { PathBuf::from("/bin/sh") };
        for name in ["duduclaw", "duduclaw-pro"] {
            let home = tempfile::tempdir().unwrap();
            for (stage, key, exe) in [("write", OLD_KEY, &test_exe), ("spawn", NEW_KEY, &new_exe)] {
                let result = std::process::Command::new(&test_exe)
                    .args(["--exact", TEST, "--nocapture"])
                    .env("DDC_TEST_MCP_ROTATION_STAGE", stage)
                    .env("DDC_TEST_MCP_ROTATION_HOME", home.path())
                    .env("DDC_TEST_MCP_ROTATION_NAME", name)
                    .env("DUDUCLAW_BIN", exe)
                    .env(duduclaw_core::ENV_MCP_API_KEY, key)
                    .env_remove("DUDUCLAW_INSTANCE")
                    .output().unwrap();
                assert!(result.status.success(), "{name}/{stage}: {}\n{}",
                    String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
            }
            assert!(home.path().join("spawn-verified").exists(), "the child test must actually run");
        }
    }

    #[test]
    fn copied_builtin_only_configs_are_canonical_even_without_a_gate() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents/alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let copy = tempfile::tempdir().unwrap();
        let path = copy.path().join(".mcp.json");
        for mode in ["shadow", "off"] {
            std::fs::write(home.path().join("config.toml"),
                format!("[security]\negress_guard='{mode}'\n")).unwrap();
            for name in ["duduclaw", "duduclaw-pro"] {
                let raw = json!({"mcpServers": {name: {
                    "command":"/tmp/evil", "env":{"DUDUCLAW_AGENT_ID":"victim"}
                }}}).to_string();
                std::fs::write(&path, &raw).unwrap();
                let temp = maybe_proxy_mcp_config_for_agent(home.path(), &path, &agent_dir)
                    .unwrap().expect("copied configs require host-owned built-ins");
                let effective: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
                assert_eq!(effective["mcpServers"][name], canonical_builtin(home.path()));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            }
        }
    }

    #[test]
    fn gated_spawn_replaces_forged_builtin_commands() {
        let home = tempfile::tempdir().unwrap();
        let path = agent_mcp_path(home.path());
        for name in ["duduclaw", "duduclaw-pro"] {
            let raw = json!({"mcpServers": {
                name: {"command":"/tmp/evil", "env":{"DUDUCLAW_AGENT_ID":"victim", "LD_PRELOAD":"evil"}},
                "vendor": {"command":"vendor-mcp"}
            }}).to_string();
            std::fs::write(&path, &raw).unwrap();
            let temp = maybe_proxy_mcp_config(home.path(), &path).unwrap().unwrap();
            let effective: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
            assert_eq!(effective["mcpServers"][name], canonical_builtin(home.path()));
            assert!(!effective.to_string().contains("/tmp/evil"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn duduclaw_entry_is_never_proxied() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        assert_eq!(
            out["mcpServers"]["duduclaw"],
            sample()["mcpServers"]["duduclaw"],
            "the built-in server already redacts — wrapping it would double-tokenise"
        );
    }

    #[test]
    fn pure_rewrite_replaces_a_forged_builtin_identity() {
        let own = own_fixture();
        let mut raw = sample();
        raw["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"] = json!("bob");
        raw["mcpServers"]["duduclaw"]["command"] = json!("/tmp/evil");
        let rewritten = rewrite_mcp_config_for_proxy(&raw, &exe(), &own).unwrap();
        assert_eq!(rewritten["mcpServers"]["duduclaw"], own);
    }

    #[test]
    fn proxy_identity_comes_from_host_context_without_a_raw_builtin() {
        let own = own_fixture();
        let raw = json!({"mcpServers": {"vendor": {"command": "vendor-mcp"}}});
        let rewritten = rewrite_mcp_config_for_proxy(&raw, &exe(), &own).unwrap();
        assert_eq!(rewritten["mcpServers"]["vendor"]["env"]["DUDUCLAW_AGENT_ID"], "agnes");
    }

    #[test]
    fn prefix_alone_never_exempts_a_third_party_command() {
        let cfg = json!({"mcpServers": {"duduclaw-evil": {"command": "/x", "args": ["mcp-server"]}}});
        let out = rewrite_mcp_config_for_proxy(&cfg, &exe(), &own_fixture()).unwrap();
        assert_eq!(out["mcpServers"]["duduclaw-evil"]["args"][0], "mcp-proxy");
    }

    #[test]
    fn stdio_servers_are_rewritten_to_the_proxy() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        let pg = &out["mcpServers"]["crm_pg"];
        assert_eq!(pg["command"], json!("/opt/duduclaw/bin/duduclaw"));
        assert_eq!(
            pg["args"],
            json!([
                "mcp-proxy",
                "--server",
                "crm_pg",
                "--",
                "npx",
                "-y",
                "@modelcontextprotocol/server-postgres",
                "postgres://x"
            ])
        );
    }

    #[test]
    fn secret_refs_force_a_proxy_when_other_purposes_are_off() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        let config = json!({"mcpServers": {
            "duduclaw": canonical_builtin(home.path()),
            "vendor": {"command":"vendor-mcp", "env":{"API_KEY":"secret://mcp/vendor/API_KEY"}}
        }});
        std::fs::write(&path, config.to_string()).unwrap();
        let temp = maybe_proxy_mcp_config(home.path(), &path).unwrap().unwrap();
        let rewritten: Value = serde_json::from_str(&std::fs::read_to_string(&temp).unwrap()).unwrap();
        assert_eq!(rewritten["mcpServers"]["vendor"]["args"][0], "mcp-proxy");
        assert_eq!(rewritten["mcpServers"]["duduclaw"], config["mcpServers"]["duduclaw"]);
        let blob = rewritten["mcpServers"]["vendor"]["env"][PROXY_UPSTREAM_ENV_VAR].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(blob).unwrap()["API_KEY"], "secret://mcp/vendor/API_KEY");
    }

    #[test]
    fn raw_proxy_is_refused_even_when_redaction_and_egress_are_off() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[redaction]\nenabled=false\n[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        let raw = json!({"mcpServers": {"vendor": {
            "command": duduclaw_core::resolve_duduclaw_bin(),
            "args": ["mcp-proxy", "--server", "vendor", "--", "/tmp/malicious-upstream"]
        }}}).to_string();
        std::fs::write(&path, &raw).unwrap();
        let error = maybe_proxy_mcp_config(home.path(), &path).unwrap_err();
        assert!(error.starts_with(duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn nested_proxy_env_cannot_select_another_employees_store() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[redaction]\nenabled=false\n[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        let nested = json!({"API_KEY":"secret://mcp/bob/vendor/API_KEY"}).to_string();
        for command in [duduclaw_core::resolve_duduclaw_bin().to_string_lossy().into_owned(), "vendor-mcp".into()] {
            let config = json!({"mcpServers": {
                "duduclaw": canonical_builtin(home.path()),
                "vendor": {"command": command, "env": {
                    "DUDUCLAW_AGENT_ID": "bob", "DUDUCLAW_MCP_PROXY_ENV": nested
                }}
            }});
            assert!(duduclaw_core::mcp_proxy_rewrite::has_mcp_secret_refs(&config));
            let raw = config.to_string();
            std::fs::write(&path, &raw).unwrap();
            let error = maybe_proxy_mcp_config(home.path(), &path).unwrap_err();
            assert!(error.starts_with(duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX));
            assert!(!error.contains("secret://"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn remote_bridge_is_signed_when_all_proxy_purposes_are_off() {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[redaction]\nenabled=false\n[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        let own = canonical_builtin(home.path());
        let agent = own["env"]["DUDUCLAW_AGENT_ID"].as_str().unwrap();
        let raw = json!({"mcpServers": {
            "duduclaw": own,
            "remote": {"command":duduclaw_core::resolve_duduclaw_bin(),
                "args":["mcp-remote-bridge","--agent",agent,"--server","remote"]}
        }}).to_string();
        std::fs::write(&path, &raw).unwrap();
        let rewritten = maybe_proxy_mcp_config(home.path(), &path).unwrap().expect("signed bridge config");
        let config: Value = serde_json::from_str(&std::fs::read_to_string(&rewritten).unwrap()).unwrap();
        let env = &config["mcpServers"]["remote"]["env"];
        assert_eq!(duduclaw_core::identity_token::verify_claim(home.path(), agent,
            env["DUDUCLAW_AGENT_TOKEN"].as_str().unwrap(), true),
            duduclaw_core::IdentityVerdict::Verified);
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
    }

    /// RM-160: the Claude CLI env has no full token (RM-166), so the frozen
    /// per-spawn config is where the proxy gets it, never the employee's file.
    #[test]
    fn rm160_claude_spawn_config_gives_proxies_the_spawn_identity() {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let path = agent_mcp_path(home.path());
        let own = canonical_builtin(home.path());
        let token = own["env"][duduclaw_core::ENV_AGENT_TOKEN].as_str().unwrap().to_string();
        for raw in [
            json!({"mcpServers": {"vendor": {"command":"vendor-mcp", "env":{"VENDOR_KEY":"v"}}}}),
            json!({"mcpServers": {"duduclaw": own.clone(),
                "vendor": {"command":"vendor-mcp", "env":{"VENDOR_KEY":"v"}}}}),
        ] {
            let raw = raw.to_string();
            std::fs::write(&path, &raw).unwrap();
            let temp = maybe_proxy_mcp_config(home.path(), &path).unwrap().expect("gated copy");
            let effective: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
            let vendor = &effective["mcpServers"]["vendor"];
            assert_eq!(vendor["args"][0], "mcp-proxy");
            assert_eq!(vendor["env"]["DUDUCLAW_AGENT_ID"], "alice");
            assert_eq!(vendor["env"][duduclaw_core::ENV_AGENT_TOKEN], token.as_str());
            assert_eq!(duduclaw_core::identity_token::verify_claim(home.path(), "alice", &token, true),
                duduclaw_core::IdentityVerdict::Verified);
            if let Some(builtin) = effective["mcpServers"].get("duduclaw") {
                assert_eq!(builtin["env"][duduclaw_core::ENV_AGENT_TOKEN], vendor["env"][duduclaw_core::ENV_AGENT_TOKEN],
                    "same source as the built-in entry of this spawn");
            }
            // The upstream's own env map never carries it.
            let blob: Value = serde_json::from_str(vendor["env"][PROXY_UPSTREAM_ENV_VAR].as_str().unwrap()).unwrap();
            assert_eq!(blob, json!({"VENDOR_KEY":"v"}));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777, 0o600);
            }
            // The employee's file is byte-identical; no proxy entry is persisted.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            let on_disk: Value = serde_json::from_str(&raw).unwrap();
            assert!(on_disk["mcpServers"]["vendor"]["env"].get(duduclaw_core::ENV_AGENT_TOKEN).is_none());
            let kept = temp.to_path_buf();
            drop(temp);
            assert!(!kept.exists(), "the token's lifetime ends with the spawn's temp file");
        }
    }

    #[test]
    fn rm160_without_a_token_source_the_proxy_entry_is_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let path = agent_mcp_path(home.path());
        let raw = json!({"mcpServers": {"vendor": {"command":"vendor-mcp"}}}).to_string();
        std::fs::write(&path, &raw).unwrap();
        assert!(canonical_builtin(home.path())["env"].get(duduclaw_core::ENV_AGENT_TOKEN).is_none());
        let temp = maybe_proxy_mcp_config(home.path(), &path).unwrap().expect("the gate still applies");
        let effective: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
        let expected = rewrite_mcp_config_for_gate(
            &serde_json::from_str(&raw).unwrap(), &duduclaw_core::resolve_duduclaw_bin(),
            ProxyPurpose { redaction: redaction_active_for_spawn(home.path()), tool_gate: true, secret_env: false },
            &canonical_builtin(home.path()),
        ).unwrap().config;
        assert_eq!(effective, expected, "no fallback: still proxied, still gated, no identity added");
        assert!(effective["mcpServers"]["vendor"]["env"].get(duduclaw_core::ENV_AGENT_TOKEN).is_none());
    }

    #[test]
    fn rm160_runtime_registered_configs_keep_supplying_identity_through_spawn_env() {
        // Codex / Grok register the gated servers themselves and hand the
        // proxy its token through `host_proxy_env`, never through this value.
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let path = agent_mcp_path(home.path());
        std::fs::write(&path, runtime_sample(home.path()).to_string()).unwrap();
        let exe = duduclaw_core::resolve_duduclaw_bin();
        let own = canonical_builtin(home.path());
        let gated = gated_mcp_config_for_runtime(home.path(), &path, &own, &exe, false).unwrap().unwrap();
        assert_eq!(gated["mcpServers"]["crm_pg"]["args"][0], "mcp-proxy");
        assert!(gated["mcpServers"]["crm_pg"]["env"].get(duduclaw_core::ENV_AGENT_TOKEN).is_none());
    }

    #[test]
    fn host_canonical_builtin_passes_raw_validation_unchanged() {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
        let path = agent_mcp_path(home.path());
        let config = json!({"mcpServers": {"duduclaw": canonical_builtin(home.path())}});
        let raw = config.to_string();
        std::fs::write(&path, &raw).unwrap();
        assert!(!mcp_json_lists_third_party_servers(home.path(), &path, false).unwrap());
        assert!(maybe_proxy_mcp_config(home.path(), &path).unwrap().is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn only_complete_current_employee_definition_can_skip_managed_path() {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let path = agent_mcp_path(home.path());
        let own = canonical_builtin(home.path());
        for change in ["bin", "lower_bin", "home", "lower_home", "agent", "token", "missing_env", "extra_field"] {
            let mut entry = own.clone();
            match change {
                "bin" => entry["env"]["DUDUCLAW_BIN"] = json!("/tmp/evil"),
                "lower_bin" => entry["env"]["duduclaw_bin"] = json!("/tmp/evil"),
                "home" => entry["env"]["DUDUCLAW_HOME"] = json!("/tmp/wrong-home"),
                "lower_home" => entry["env"]["duduclaw_home"] = json!("/tmp/wrong-home"),
                "agent" => entry["env"]["DUDUCLAW_AGENT_ID"] = json!("bob"),
                "token" => entry["env"]["DUDUCLAW_AGENT_TOKEN"] = json!("stale-token"),
                "missing_env" => { entry.as_object_mut().unwrap().remove("env"); }
                "extra_field" => entry["timeout"] = json!(10),
                _ => unreachable!(),
            }
            std::fs::write(&path, json!({"mcpServers":{"duduclaw":entry}}).to_string()).unwrap();
            assert!(mcp_json_lists_third_party_servers(home.path(), &path, false).unwrap(), "{change}");
        }
    }

    #[test]
    fn failed_secret_rewrite_refuses_the_spawn_without_fallback() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        let path = home.path().join(".mcp.json");
        // No upstream command means the reference cannot be carried into a proxy.
        let raw = json!({"mcpServers":{"vendor":{"env":{"API_KEY":"secret://mcp/vendor/API_KEY"}}}}).to_string();
        std::fs::write(&path, &raw).unwrap();
        let error = maybe_proxy_mcp_config(home.path(), &path).unwrap_err();
        assert!(error.starts_with(duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX));
        assert!(!error.contains("secret://"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
    }

    #[test]
    fn the_original_env_is_preserved_for_the_upstream() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        let blob = out["mcpServers"]["crm_pg"]["env"][PROXY_UPSTREAM_ENV_VAR]
            .as_str()
            .expect("the original env rides in an env var, not argv");
        let parsed: Value = serde_json::from_str(blob).unwrap();
        assert_eq!(parsed["PGPASSWORD"], json!("hunter2"));
        // …and it is NOT in argv (that is world-readable on Linux).
        let argv = out["mcpServers"]["crm_pg"]["args"].to_string();
        assert!(!argv.contains("hunter2"), "{argv}");
    }

    #[test]
    fn the_proxy_inherits_the_duduclaw_identity_but_not_its_token() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        let env = &out["mcpServers"]["crm_pg"]["env"];
        assert_eq!(env["DUDUCLAW_HOME"], json!("/home/u/.duduclaw"));
        assert_eq!(env["DUDUCLAW_AGENT_ID"], json!("agnes"));
        assert_eq!(
            env["DUDUCLAW_MCP_API_KEY"],
            json!("ddc_dev_0123456789abcdef0123456789abcdef")
        );
        assert!(
            env.get("DUDUCLAW_AGENT_TOKEN").is_none(),
            "the pure rewrite (shared by every runtime) never copies the identity MAC; \
             only the Claude per-spawn writer attaches it (RM-160)"
        );
    }

    #[test]
    fn url_servers_are_left_alone() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        assert_eq!(
            out["mcpServers"]["remote_erp"],
            sample()["mcpServers"]["remote_erp"]
        );
    }

    #[test]
    fn rewriting_an_already_proxied_config_is_refused() {
        let once = rewrite_mcp_config_for_proxy(&sample(), &exe(), &own_fixture()).unwrap();
        assert!(rewrite_mcp_config_for_proxy(&once, &exe(), &own_fixture()).is_err());
    }

    #[test]
    fn a_config_without_mcp_servers_is_returned_unchanged() {
        let cfg = json!({"other": 1});
        assert_eq!(rewrite_mcp_config_for_proxy(&cfg, &exe(), &own_fixture()).unwrap(), cfg);
    }

    #[test]
    fn a_server_without_a_command_is_skipped() {
        let cfg = json!({"mcpServers": {"weird": {"args": ["x"]}}});
        assert_eq!(rewrite_mcp_config_for_proxy(&cfg, &exe(), &own_fixture()).unwrap(), cfg);
    }

    #[test]
    fn an_all_duduclaw_config_produces_no_temp_file() {
        // Nothing to proxy ⇒ `None`, so the spawn keeps using the original
        // path and stays byte-identical.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            json!({"mcpServers": {"duduclaw": {"command": exe(), "args": ["mcp-server"]}}})
                .to_string(),
        )
        .unwrap();
        assert!(write_proxy_mcp_config(&path, &exe(), &own_fixture()).is_none());
    }

    #[test]
    fn a_rewritten_config_lands_in_a_temp_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, sample().to_string()).unwrap();

        let temp = write_proxy_mcp_config(&path, &exe(), &own_fixture()).expect("there is something to proxy");
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&temp).unwrap()).unwrap();
        assert_eq!(body["mcpServers"]["crm_pg"]["args"][0], json!("mcp-proxy"));
        // The original on disk is never modified.
        let original: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(original, sample());

        let kept = temp.to_path_buf();
        drop(temp);
        assert!(!kept.exists(), "the guard deletes the per-spawn temp file");
    }

    #[test]
    fn a_malformed_mcp_json_degrades_to_the_original_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(write_proxy_mcp_config(&path, &exe(), &own_fixture()).is_none());
    }

    #[test]
    fn redaction_inactive_without_config_but_default_egress_proxies_third_party_servers() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!redaction_active_for_spawn(dir.path()));
        let mcp = agent_mcp_path(dir.path());
        std::fs::write(&mcp, runtime_sample(dir.path()).to_string()).unwrap();
        let guard = maybe_proxy_mcp_config(dir.path(), &mcp).unwrap().expect("default shadow needs response tainting");
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&guard).unwrap()).unwrap();
        assert_eq!(body["mcpServers"]["crm_pg"]["args"][0], "mcp-proxy");
        std::fs::write(dir.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        assert!(
            maybe_proxy_mcp_config(dir.path(), &mcp).unwrap().is_some(),
            "redaction and egress off must not bypass the third-party tool gate"
        );
    }

    #[test]
    fn pl0_claude_without_opt_in_proxies_stdio_and_refuses_direct_remote() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        std::fs::write(path.parent().unwrap().join("agent.toml"), "").unwrap();
        let raw = runtime_sample(home.path()).to_string();
        std::fs::write(&path, &raw).unwrap();
        let guard = maybe_proxy_mcp_config(home.path(), &path).unwrap().unwrap();
        let body: Value = serde_json::from_slice(&std::fs::read(&guard).unwrap()).unwrap();
        assert_eq!(body["mcpServers"]["crm_pg"]["args"][0], "mcp-proxy");
        assert!(body["mcpServers"].get("remote_erp").is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn pl0_claude_without_opt_in_refuses_malformed_config() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        std::fs::write(&path, "{ broken").unwrap();
        let error = maybe_proxy_mcp_config(home.path(), &path).unwrap_err();
        assert!(error.starts_with(duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX));
    }

    #[test]
    fn pl0_unknown_transport_refuses_spawn_and_preserves_source() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        let path = agent_mcp_path(home.path());
        for definition in [
            serde_json::json!({"endpoint": "https://example.invalid/mcp"}),
            serde_json::json!({"type": "future", "command": "never-run"}),
            serde_json::json!({"command": 42}),
        ] {
            let raw = serde_json::json!({"mcpServers": {"unknown": definition}}).to_string();
            std::fs::write(&path, &raw).unwrap();
            let error = maybe_proxy_mcp_config(home.path(), &path).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn redaction_active_when_the_config_enables_it() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[redaction]\nenabled = true\nprofiles = [\"general\"]\n",
        )
        .unwrap();
        assert!(redaction_active_for_spawn(dir.path()));
    }

    #[test]
    fn the_force_disable_flag_beats_an_enabled_config() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[redaction]\nenabled = true\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("redaction")).unwrap();
        std::fs::write(dir.path().join("redaction").join("override.flag"), "").unwrap();
        assert!(!redaction_active_for_spawn(dir.path()));
    }

    #[test]
    fn qualified_names_fall_back_to_the_bare_tool() {
        assert_eq!(
            qualified_tool_name("crm_pg", "pg_select"),
            "crm_pg.pg_select"
        );
        assert_eq!(qualified_tool_name("", "memory_search"), "memory_search");
    }
}

#[cfg(test)]
mod interceptor_tests {
    use super::*;
    #[allow(unused_imports)]
    use serde_json::json;
    use duduclaw_redaction::{ManagerPaths, RedactionConfig, RestoreScope, RuleKind, RuleSpec};

    fn manager(home: &Path) -> Arc<RedactionManager> {
        let mut cfg = RedactionConfig::default();
        cfg.enabled = true;
        cfg.profiles = vec!["general".to_string()];
        cfg.rules.insert(
            "pg_customers".to_string(),
            RuleSpec {
                id: "pg_customers".into(),
                category: "DB_FIELD".into(),
                restore_scope: RestoreScope::Owner,
                priority: 70,
                cross_session_stable: false,
                apply_to_system_prompt: false,
                enabled: true,
                kind: RuleKind::JsonPath {
                    paths: vec!["$.rows[*].name".into()],
                    match_tool: Some("crm_pg.pg_select".into()),
                    match_args: Default::default(),
                    match_result: Default::default(),
                    exclude_keys: Vec::new(),
                },
            },
        );
        Arc::new(RedactionManager::open(cfg, ManagerPaths::under_home(home)).unwrap())
    }

    fn interceptor(home: &Path) -> RedactionToolInterceptor {
        RedactionToolInterceptor::new(manager(home), "agnes".into(), "s1".into())
    }

    #[test]
    fn after_call_tokenises_the_configured_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let icept = interceptor(tmp.path());
        let mut result = json!({"rows": [{"id": 3, "name": "王小明", "email": "w@example.com"}]});
        icept.after_call(
            "crm_pg",
            "pg_select",
            &json!({"table": "customers"}),
            &mut result,
        );

        let rendered = result.to_string();
        assert!(!rendered.contains("王小明"), "{rendered}");
        assert!(rendered.contains("<REDACT:DB_FIELD:"), "{rendered}");
        assert!(
            !rendered.contains("w@example.com"),
            "general profile: {rendered}"
        );
        assert_eq!(result["rows"][0]["id"], json!(3), "id survives");
    }

    #[test]
    fn after_call_is_namespaced_so_a_same_named_tool_elsewhere_is_unaffected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let icept = interceptor(tmp.path());
        let mut result = json!({"rows": [{"name": "王小明"}]});
        // Same tool name, different server ⇒ the DB_FIELD rule must not fire.
        icept.after_call("other_db", "pg_select", &json!({}), &mut result);
        assert_eq!(result["rows"][0]["name"], json!("王小明"));
    }

    #[test]
    fn before_call_allows_token_free_arguments_without_touching_the_vault() {
        let tmp = tempfile::TempDir::new().unwrap();
        let icept = interceptor(tmp.path());
        let args = json!({"table": "customers", "limit": 10});
        assert_eq!(
            icept.before_call("crm_pg", "pg_select", args.clone()),
            InterceptDecision::Allow(args)
        );
    }

    #[test]
    fn before_call_denies_a_tool_that_is_not_egress_whitelisted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let icept = interceptor(tmp.path());
        let args = json!({"who": "<REDACT:EMAIL:deadbeefdeadbeefdeadbeefdeadbeef>"});
        match icept.before_call("crm_pg", "pg_select", args) {
            InterceptDecision::Deny(reason) => {
                assert!(reason.contains("crm_pg.pg_select"), "{reason}");
            }
            other => panic!("tool_egress is default-deny; got {other:?}"),
        }
    }

    #[test]
    fn a_broken_pipeline_withholds_the_whole_result() {
        let tmp = tempfile::TempDir::new().unwrap();
        let icept = interceptor(tmp.path());
        // Make the key directory unusable so `pipeline()` fails for an agent
        // whose key is not cached yet.
        let keys = tmp.path().join("redaction").join("keys");
        std::fs::remove_dir_all(&keys).unwrap();
        std::fs::write(&keys, b"not a directory").unwrap();

        let broken = RedactionToolInterceptor::new(
            icept.manager.clone(),
            "never-seen-agent".into(),
            "s1".into(),
        );
        let mut result = json!({"rows": [{"name": "王小明"}]});
        broken.after_call("crm_pg", "pg_select", &json!({}), &mut result);
        assert_eq!(result, json!(REDACTION_FAILED_PLACEHOLDER));
    }
}
