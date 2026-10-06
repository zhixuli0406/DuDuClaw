//! MCP server configuration template generator.
//!
//! Generates `.mcp.json` files for agent directories to connect
//! external MCP servers (e.g., Playwright for browser automation).

use std::path::Path;
use serde::{Serialize, Deserialize};
use tracing::info;

/// MCP server configuration for an agent directory's `.mcp.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::HashMap<String, McpServerDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDef {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Generate a Playwright MCP server configuration.
pub fn playwright_mcp_config(headless: bool) -> McpConfig {
    let mut args = vec!["-y".to_string(), PLAYWRIGHT_MCP_PACKAGE.to_string()];
    if headless {
        args.push("--headless".to_string());
    }

    let mut servers = std::collections::HashMap::new();
    servers.insert("playwright".to_string(), McpServerDef {
        command: "npx".to_string(),
        args,
        env: std::collections::HashMap::new(),
    });

    McpConfig { mcp_servers: servers }
}

/// Write `.mcp.json` to an agent directory.
/// Returns Ok(true) if written, Ok(false) if file already exists.
pub fn write_mcp_config(agent_dir: &Path, config: &McpConfig) -> Result<bool, String> {
    use std::io::Write;

    let path = agent_dir.join(".mcp.json");
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;

    // Owner-only from the first byte (0600 at create on Unix), then the
    // platform helper for Windows ACLs.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut f) => {
            duduclaw_core::platform::set_owner_only(&path).ok();
            f.write_all(json.as_bytes()).map_err(|e| format!("Failed to write MCP config: {e}"))?;
            info!(path = %path.display(), "MCP config written");
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            info!(path = %path.display(), "MCP config already exists, skipping");
            Ok(false)
        }
        Err(e) => Err(format!("Failed to create MCP config: {e}")),
    }
}

/// Merge Playwright server into an existing `.mcp.json`, preserving other servers.
pub fn ensure_playwright_in_config(agent_dir: &Path, headless: bool) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");

    let mut config = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read MCP config: {e}"))?;
        serde_json::from_str::<McpConfig>(&content)
            .map_err(|e| format!("Failed to parse MCP config: {e}"))?
    } else {
        McpConfig { mcp_servers: std::collections::HashMap::new() }
    };

    if config.mcp_servers.contains_key("playwright") {
        return Ok(()); // Already configured
    }

    let playwright = playwright_mcp_config(headless);
    config.mcp_servers.extend(playwright.mcp_servers);

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;
    duduclaw_core::platform::set_owner_only(&path).ok();

    info!(path = %path.display(), "Playwright MCP server added to config");
    Ok(())
}

/// Generate a Browserbase MCP server configuration.
///
/// The three values are written into `env` as **literal strings**. A
/// `${NAME}` reference cannot work here: the Claude CLI expands such a
/// reference from its own process environment, and the gateway starts every
/// employee CLI with the allowlisted environment of
/// `duduclaw_core::spawn_env`, which drops every `*_API_KEY`-shaped name — so
/// the reference resolved to an empty string and the server started without
/// credentials. Literal values are how `claude mcp add -e NAME=value` stores
/// them too; the file is written owner-only (0600) and the values are never
/// logged. Fails when any value is empty or contains `${`.
pub fn browserbase_mcp_config(
    api_key: &str,
    project_id: &str,
    gemini_api_key: &str,
) -> Result<McpConfig, String> {
    let template = McpServerDef {
        command: "npx".to_string(),
        args: vec!["-y".to_string(), BROWSERBASE_MCP_PACKAGE.to_string()],
        env: env_placeholders(&BROWSERBASE_REQUIRED_ENV),
    };
    let supplied: std::collections::HashMap<String, String> = [
        (BROWSERBASE_REQUIRED_ENV[0], api_key),
        (BROWSERBASE_REQUIRED_ENV[1], project_id),
        (BROWSERBASE_REQUIRED_ENV[2], gemini_api_key),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let required: Vec<String> = BROWSERBASE_REQUIRED_ENV.iter().map(|s| s.to_string()).collect();
    let def = apply_required_env(&template, &required, &supplied)?;

    let mut servers = std::collections::HashMap::new();
    servers.insert("browserbase".to_string(), def);
    Ok(McpConfig { mcp_servers: servers })
}

/// Merge Browserbase server into an existing `.mcp.json`, preserving other servers.
pub fn ensure_browserbase_in_config(
    agent_dir: &Path,
    api_key: &str,
    project_id: &str,
    gemini_api_key: &str,
) -> Result<(), String> {
    if read_mcp_config(agent_dir)?.mcp_servers.contains_key("browserbase") {
        return Ok(());
    }
    let bb = browserbase_mcp_config(api_key, project_id, gemini_api_key)?;
    let def = bb.mcp_servers.get("browserbase").cloned().ok_or("browserbase entry missing")?;
    add_server_to_config(agent_dir, "browserbase", &def)?;
    info!(dir = %agent_dir.display(), "Browserbase MCP server added to config");
    Ok(())
}

/// Ensure the `duduclaw` MCP server is registered in Claude Code's **global**
/// settings (`~/.claude/settings.json`), not per-agent `.mcp.json`.
///
/// The DuDuClaw MCP server provides platform-level tools (send_to_agent,
/// list_cron_tasks, create_agent, etc.) that ALL agents need. Placing it
/// globally avoids per-agent `.mcp.json` maintenance and the production bugs
/// caused by missing or stale configs.
///
/// Agent-specific MCP servers (Playwright, Browserbase, etc.) stay in
/// per-agent `.mcp.json` — Claude CLI merges both layers.
///
/// Returns `Ok(true)` if settings.json was updated, `Ok(false)` if no change needed.
pub fn ensure_global_mcp_server() -> Result<bool, String> {
    let abs_bin = duduclaw_core::resolve_duduclaw_bin();
    let abs_str = abs_bin.to_string_lossy().into_owned();
    if !std::path::Path::new(&abs_str).is_absolute() {
        return Ok(false);
    }

    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let settings_path = home.join(".claude").join("settings.json");

    // Read existing settings (or create empty)
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("Failed to read {}: {e}", settings_path.display()))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {e}", settings_path.display()))?
    } else {
        serde_json::json!({})
    };

    // The registration key is namespaced per instance (`duduclaw` by default,
    // `duduclaw-<instance>` when DUDUCLAW_INSTANCE is set) so that several
    // instances sharing this `~/.claude/settings.json` don't overwrite each
    // other (multi-instance isolation — Plan A).
    let key = duduclaw_core::mcp_server_key();

    // Build the desired launch spec, carrying THIS instance's env into it so the
    // Claude-CLI-spawned `duduclaw mcp-server` connects to this instance's state
    // root / port even when several entries coexist. Only non-empty overrides
    // are written, keeping the single-instance spec byte-identical to before.
    let mut desired = serde_json::json!({
        "command": abs_str,
        "args": ["mcp-server"],
    });
    let mut env = serde_json::Map::new();
    for (k, v) in duduclaw_core::mcp_forward_env_vars() {
        env.insert(k, serde_json::Value::String(v));
    }
    if !env.is_empty() {
        desired
            .as_object_mut()
            .expect("desired is an object")
            .insert("env".to_string(), serde_json::Value::Object(env));
    }

    // Idempotent: skip the write only when the existing entry already equals the
    // desired one (command + args + env).
    if settings.get("mcpServers").and_then(|s| s.get(&key)) == Some(&desired) {
        return Ok(false);
    }

    // Upsert mcpServers.<key>
    let mcp_servers = settings
        .as_object_mut()
        .ok_or("settings.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert(serde_json::json!({}));

    mcp_servers
        .as_object_mut()
        .ok_or("mcpServers is not a JSON object")?
        .insert(key.clone(), desired);

    // Write back atomically
    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    let tmp = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &settings_path)
        .map_err(|e| format!("Failed to rename {}: {e}", tmp.display()))?;

    info!(
        path = %settings_path.display(),
        key = %key,
        command = %abs_str,
        "Registered duduclaw MCP server in global Claude settings"
    );
    Ok(true)
}

/// Audit event written when an employee's `.mcp.json` cannot be confirmed
/// before a spawn (the spawn does not happen).
pub const AUDIT_MCP_CONFIG_UNVERIFIED: &str = "mcp_config_unverified";

/// Run `f` holding the advisory lock every writer of an agent's `.mcp.json`
/// shares (this module's repair and `add_server_to_config` /
/// `remove_server_from_config`, the expert-pack merge), so two read–modify–
/// write sequences cannot overwrite each other. For an employee directory the
/// lock lives under `<home>/locks/`, not in the employee directory
/// ([`crate::mcp_spawn_gate::mcp_config_lock_base`]), so nothing the
/// employee creates there can block or redirect it.
pub fn with_mcp_config_lock<T>(
    path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let lock_base = crate::mcp_spawn_gate::mcp_config_lock_base(path);
    let mut inner: Option<Result<T, String>> = None;
    duduclaw_core::with_file_lock(&lock_base, || {
        inner = Some(f());
        Ok(())
    })
    .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    inner.unwrap_or_else(|| Err("lock callback did not run".to_string()))
}

/// Derive the DuDuClaw home directory from an agent directory path.
///
/// Normally `agent_dir` is `<home>/agents/<id>`, so walking up two levels
/// (`agents`, then `<home>`) recovers home — the shape the old inline
/// `agent_dir.parent().and_then(|p| p.parent())` assumed everywhere. Ephemeral
/// agents live one level deeper at `<home>/agents/.ephemeral/<id>`
/// (`spawn_ephemeral`), so that same two-hop walk lands on `<home>/agents`
/// instead of `<home>` — every signed identity token then embeds the wrong
/// key root. This detects the `.ephemeral` directory name in the parent chain
/// and peels one extra level for that case.
///
/// Falls back to `duduclaw_core::duduclaw_home()` when `agent_dir` is too
/// shallow to have the expected ancestors (matches the previous inline
/// fail-safe behaviour — never panics on a malformed path).
pub(crate) fn derive_home_from_agent_dir(agent_dir: &Path) -> std::path::PathBuf {
    let Some(parent) = agent_dir.parent() else {
        return duduclaw_core::duduclaw_home();
    };
    let is_ephemeral = parent.file_name().and_then(|n| n.to_str()) == Some(".ephemeral");
    let levels_up = if is_ephemeral { 3 } else { 2 };

    let mut home = agent_dir.to_path_buf();
    for _ in 0..levels_up {
        match home.parent() {
            Some(p) => home = p.to_path_buf(),
            None => return duduclaw_core::duduclaw_home(),
        }
    }
    home
}

/// The `.mcp.json` entry names this module owns: `duduclaw`, the legacy
/// `duduclaw-pro`, and this instance's `duduclaw-<instance>`.
fn owned_entry_names() -> Vec<String> {
    let mut names = vec!["duduclaw".to_string(), "duduclaw-pro".to_string()];
    let key = duduclaw_core::mcp_server_key();
    if !names.contains(&key) {
        names.push(key);
    }
    names
}

/// Repair an agent's `.mcp.json` so its DuDuClaw MCP server entry is exactly
/// what DuDuClaw writes.
///
/// The `duduclaw` (and `duduclaw-pro` / `duduclaw-<instance>`, when present)
/// entry is **regenerated whole** on every call: `command` = the resolved
/// binary, `args` = `["mcp-server"]`, `env` = this agent's identity pair
/// (`DUDUCLAW_AGENT_ID`, plus `DUDUCLAW_AGENT_TOKEN` when `<home>/identity.key`
/// exists) and the forward set (`DUDUCLAW_HOME` derived from the agent
/// directory, and whichever of `DUDUCLAW_PORT` / `DUDUCLAW_INSTANCE` /
/// `DUDUCLAW_MCP_API_KEY` / `DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED` this process
/// has). Nothing else in that entry survives — not another `DUDUCLAW_*` key,
/// not `PATH` / `LD_PRELOAD` / `NODE_OPTIONS`, not a managed key this process
/// has no value for — because the Claude CLI lets a server's configured env
/// override the inherited one. Dropped key names (never values) are logged.
/// Missing entry ⇒ a `duduclaw` entry is added; missing file ⇒ created.
///
/// Other entries are copied through untouched (as JSON values, so an entry
/// with fields this module does not model, such as an HTTP server, is kept).
///
/// Errors (nothing written): the file exists but is not a regular file
/// (directory, symbolic link), cannot be read, is not valid JSON, or its
/// `mcpServers` is not an object; the duduclaw binary path is not absolute;
/// or the write fails. Holds [`with_mcp_config_lock`]. Returns whether the
/// file was written.
pub fn ensure_duduclaw_absolute_path(agent_dir: &Path) -> Result<bool, String> {
    let path = agent_dir.join(".mcp.json");

    let abs_bin = duduclaw_core::resolve_duduclaw_bin();
    let abs_str = abs_bin.to_string_lossy().into_owned();

    // Still relative after resolution (fallback "duduclaw"): the entry
    // cannot be regenerated, so the file counts as unconfirmable (fail
    // closed) instead of being passed on unchecked.
    if !std::path::Path::new(&abs_str).is_absolute() {
        return Err(format!(
            "the duduclaw binary path could not be resolved to an absolute path ({abs_str})"
        ));
    }

    // Agent identity = directory name (matches the rest of the codebase,
    // e.g. `can_delegate`, `is_valid_agent_id`).
    let agent_id = agent_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("agent dir has no file_name: {}", agent_dir.display()))?
        .to_string();

    // Shared forward set (home/port/instance + MCP auth). This function also
    // scaffolds members under a cloned eval home. An inherited DUDUCLAW_HOME
    // can name the source instance, so the MCP child must always use the same
    // derived home as its identity token.
    let home_dir = derive_home_from_agent_dir(agent_dir);
    let mut forward_env = duduclaw_core::mcp_forward_env_vars();
    forward_env.retain(|(name, _)| name != "DUDUCLAW_HOME");
    forward_env.push((
        "DUDUCLAW_HOME".to_string(),
        home_dir.to_string_lossy().to_string(),
    ));
    let identity_env = duduclaw_core::agent_identity_env_vars(&home_dir, &agent_id);

    let mut env = serde_json::Map::new();
    for (k, v) in identity_env.iter().chain(forward_env.iter()) {
        env.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    let desired = serde_json::json!({
        "command": abs_str,
        "args": ["mcp-server"],
        "env": serde_json::Value::Object(env),
    });

    with_mcp_config_lock(&path, || {
        let (mut doc, created) = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (serde_json::json!({ "mcpServers": {} }), true)
            }
            Err(e) => return Err(format!("Failed to inspect {}: {e}", path.display())),
            Ok(m) if !m.file_type().is_file() => {
                return Err(format!("{} is not a regular file", path.display()));
            }
            Ok(_) => {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
                let doc: serde_json::Value = serde_json::from_str(&content)
                    .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;
                (doc, false)
            }
        };
        let root = doc
            .as_object_mut()
            .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
        let servers = root
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| format!("{}: mcpServers is not a JSON object", path.display()))?;

        let mut targets: Vec<String> = owned_entry_names()
            .into_iter()
            .filter(|k| servers.contains_key(k))
            .collect();
        if targets.is_empty() {
            targets.push("duduclaw".to_string());
        }
        // A key this process cannot supply must not be written away: an
        // entry that carries the MCP API key while this process has none
        // (the internal key not provisioned yet, a process without the
        // gateway env) cannot be regenerated without breaking the server's
        // authentication, so the file counts as unconfirmable.
        let has_key_now = desired["env"].get(duduclaw_core::ENV_MCP_API_KEY).is_some();
        if !has_key_now {
            for key in &targets {
                let had_key = servers
                    .get(key)
                    .and_then(|e| e.get("env"))
                    .and_then(|e| e.get(duduclaw_core::ENV_MCP_API_KEY))
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty());
                if had_key {
                    return Err(format!(
                        "{}: the {key} entry carries {} but this process has no value for it; \
                         not rewriting it",
                        path.display(),
                        duduclaw_core::ENV_MCP_API_KEY
                    ));
                }
            }
        }
        let mut changed = created;
        for key in &targets {
            if servers.get(key) == Some(&desired) {
                continue;
            }
            if let Some(old) = servers.get(key) {
                let dropped: Vec<String> = old
                    .get("env")
                    .and_then(|e| e.as_object())
                    .map(|e| {
                        e.keys()
                            .filter(|k| desired["env"].get(k.as_str()).is_none())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if !dropped.is_empty() {
                    tracing::warn!(
                        agent = %agent_id,
                        server = %key,
                        dropped = ?dropped,
                        "Removed env keys DuDuClaw does not write from its MCP server entry"
                    );
                }
            }
            servers.insert(key.clone(), desired.clone());
            changed = true;
        }
        if !changed {
            return Ok(false);
        }
        let json = serde_json::to_string_pretty(&doc)
            .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
        write_owner_only_atomic(&path, json.as_bytes())?;
        info!(
            path = %path.display(),
            command = %abs_str,
            agent_id = %agent_id,
            "Repaired duduclaw MCP server entry (command, args, env regenerated)"
        );
        Ok(true)
    })
}

/// Whether `agent_dir` is an employee directory this module repairs:
/// `<home>/agents/<id>` (not `_` / `.` prefixed) or
/// `<home>/agents/.ephemeral/<id>` (ephemeral employees and team role
/// members).
pub(crate) fn is_repairable_agent_dir(agent_dir: &Path) -> bool {
    let Some(name) = agent_dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('_') || name.starts_with('.') {
        return false;
    }
    let Some(parent) = agent_dir.parent() else {
        return false;
    };
    let parent_name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if parent_name.eq_ignore_ascii_case("agents") {
        return true;
    }
    parent_name.eq_ignore_ascii_case(".ephemeral")
        && parent
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("agents"))
}

/// [`ensure_duduclaw_absolute_path`] for one working directory right before
/// a spawn, so a `.mcp.json` changed since boot is repaired before the next
/// Claude CLI reads it.
///
/// - `<home>/agents/<id>` or `<home>/agents/.ephemeral/<id>`: always
///   confirmed. A missing directory is an error (the CLI would run with
///   nothing confirmed); a failure is retried twice (50 ms apart) for
///   transient IO, then returned.
/// - Any other directory that holds an `agent.toml`: an error. It looks like
///   an employee directory this module cannot place, so it is not passed on
///   unchecked.
/// - Any other directory: [`McpGateOutcome::NotApplicable`] with the reason.
///
/// [`McpGateOutcome::NotApplicable`]: crate::mcp_spawn_gate::McpGateOutcome::NotApplicable
pub fn refresh_for_spawn(
    agent_dir: &Path,
) -> Result<crate::mcp_spawn_gate::McpGateOutcome, String> {
    use crate::mcp_spawn_gate::McpGateOutcome;
    if !is_repairable_agent_dir(agent_dir) {
        if agent_dir.join("agent.toml").exists() {
            return Err(format!(
                "{} has an agent.toml but is not under <home>/agents/; its .mcp.json cannot be confirmed",
                agent_dir.display()
            ));
        }
        return Ok(McpGateOutcome::NotApplicable("not an employee directory"));
    }
    if !agent_dir.is_dir() {
        return Err(format!("{} is not a directory", agent_dir.display()));
    }
    let mut last = String::new();
    for attempt in 0..3 {
        match ensure_duduclaw_absolute_path(agent_dir) {
            Ok(_) => return Ok(McpGateOutcome::Confirmed),
            Err(e) => last = e,
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    Err(last)
}

/// The spawn gate every Claude CLI spawn that hands an employee's
/// `.mcp.json` to the CLI runs first: [`refresh_for_spawn`], and on failure
/// an audit event ([`AUDIT_MCP_CONFIG_UNVERIFIED`], agent id and reason) plus
/// an error for the caller to abort the spawn with (fail closed: the CLI
/// would otherwise start every server listed in a file DuDuClaw could not
/// confirm). The error starts with
/// [`crate::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX`]: spawn loops stop on it
/// without booking it against the account, and
/// [`crate::mcp_spawn_gate::spawn_gate_user_message`] gives the sentence to
/// show a person.
pub fn prepare_mcp_config_for_spawn(
    agent_dir: &Path,
) -> Result<crate::mcp_spawn_gate::McpGateOutcome, String> {
    match refresh_for_spawn(agent_dir) {
        Ok(outcome) => {
            if let crate::mcp_spawn_gate::McpGateOutcome::NotApplicable(why) = &outcome {
                tracing::debug!(dir = %agent_dir.display(), reason = %why, "MCP spawn gate not applicable");
            }
            Ok(outcome)
        }
        Err(reason) => {
            let agent_id = agent_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let home = derive_home_from_agent_dir(agent_dir);
            duduclaw_security::audit::append_audit_event(
                &home,
                &duduclaw_security::audit::AuditEvent::new(
                    AUDIT_MCP_CONFIG_UNVERIFIED,
                    &agent_id,
                    duduclaw_security::audit::Severity::Warning,
                    serde_json::json!({ "agent_id": agent_id, "reason": reason }),
                ),
            );
            tracing::warn!(agent = %agent_id, %reason, "MCP config could not be confirmed; spawn refused");
            Err(crate::mcp_spawn_gate::spawn_gate_error(&format!(
                "員工 {agent_id} 的 MCP 設定（.mcp.json）無法確認，這次不啟動：{reason}。\
                 請管理者檢查或修復這個檔案。"
            )))
        }
    }
}

/// Scan all agent directories and fix relative `duduclaw` MCP server paths.
///
/// Called on gateway startup to ensure subprocess-spawned Claude CLI can
/// discover the MCP server without PATH inheritance.
pub fn ensure_mcp_absolute_paths_all(agents_dir: &Path) -> usize {
    let mut fixed = 0usize;
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %agents_dir.display(),
                error = %e,
                "Cannot read agents directory for MCP path fixup"
            );
            return 0;
        }
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Skip trash / defaults directories
        if let Some(name) = dir.file_name().and_then(|n| n.to_str())
            && (name.starts_with('_') || name.starts_with('.'))
        {
            continue;
        }
        match ensure_duduclaw_absolute_path(&dir) {
            Ok(true) => fixed += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    agent_dir = %dir.display(),
                    error = %e,
                    "Failed to fix MCP path"
                );
            }
        }
    }

    if fixed > 0 {
        info!(count = fixed, "Fixed relative MCP paths on startup");
    }
    fixed
}

/// An entry in the MCP marketplace catalog.
///
/// Honest-fields-only: no fake stars, download counts, or prices.
/// - `author`: who maintains the MCP server package.
/// - `tags`: keyword tags used for search and filtering.
/// - `featured`: flag for flagship items highlighted on the Marketplace page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCatalogItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub featured: bool,
    pub requires_oauth: bool,
    pub default_def: McpServerDef,
    pub required_env: Vec<String>,
}

/// npm package that serves the Playwright MCP server (Microsoft; `--headless` supported).
pub const PLAYWRIGHT_MCP_PACKAGE: &str = "@playwright/mcp";
/// npm package that serves the Browserbase MCP server. The older
/// `@browserbasehq/mcp-server-browserbase` is deprecated in favour of this one.
pub const BROWSERBASE_MCP_PACKAGE: &str = "@browserbasehq/mcp";
/// Environment the Browserbase server needs: its own key + project, plus a key
/// for the default Stagehand model (Gemini) per the package README.
pub const BROWSERBASE_REQUIRED_ENV: [&str; 3] =
    ["BROWSERBASE_API_KEY", "BROWSERBASE_PROJECT_ID", "GEMINI_API_KEY"];

/// Empty placeholder value for each required env name. The catalogue only
/// declares which names a server needs; the operator supplies the values at
/// install time (see [`apply_required_env`]).
fn env_placeholders(names: &[&str]) -> std::collections::HashMap<String, String> {
    names.iter().map(|n| (n.to_string(), String::new())).collect()
}

/// Longest env value accepted at install time.
pub const MAX_ENV_VALUE_BYTES: usize = 4096;

/// Why a supplied env value cannot be used. Never carries the value itself.
fn env_value_problem(value: &str) -> Option<&'static str> {
    if value.trim().is_empty() {
        return Some("missing");
    }
    // The Claude CLI expands `${NAME}` (and `${NAME:-default}`) inside
    // `.mcp.json` env values from its own environment, which the gateway
    // strips of secret-shaped names. A reference therefore resolves to
    // nothing; refuse it instead of writing a server that starts unkeyed.
    if value.contains("${") {
        return Some("reference");
    }
    if value.len() > MAX_ENV_VALUE_BYTES {
        return Some("too_long");
    }
    if value.chars().any(|c| c.is_control()) {
        return Some("control_chars");
    }
    None
}

/// Fill a server definition's required env names with literal values.
///
/// For each name in `required`, the value comes from `supplied` first and
/// otherwise from `def.env` (so a full `server_def` from `mcp.update` is
/// checked the same way as `marketplace.install`'s separate `env` map).
/// Fails closed:
/// - a required name whose value is absent, empty or a `${...}` reference →
///   error naming every such variable (`Missing required environment values: …`);
/// - a value that is too long or contains control characters → error naming it;
/// - a `supplied` name that is not in `required` → error naming it (the
///   install form cannot smuggle `NODE_OPTIONS` or similar into the server).
///
/// Error text names variables only, never values.
pub fn apply_required_env(
    def: &McpServerDef,
    required: &[String],
    supplied: &std::collections::HashMap<String, String>,
) -> Result<McpServerDef, String> {
    let mut unknown: Vec<&str> = supplied
        .keys()
        .filter(|k| !required.iter().any(|r| r == *k))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(format!(
            "Unexpected environment variables for this server: {}",
            unknown.join(", ")
        ));
    }

    let mut out = def.clone();
    let mut missing: Vec<&str> = Vec::new();
    let mut invalid: Vec<&str> = Vec::new();
    for name in required {
        let value = supplied
            .get(name)
            .or_else(|| def.env.get(name))
            .map(String::as_str)
            .unwrap_or("");
        match env_value_problem(value) {
            None => {
                out.env.insert(name.clone(), value.to_string());
            }
            Some("missing") | Some("reference") => missing.push(name),
            Some(_) => invalid.push(name),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Missing required environment values: {} (enter the values themselves; \
             ${{NAME}} references are not passed to the employee's CLI)",
            missing.join(", ")
        ));
    }
    if !invalid.is_empty() {
        return Err(format!(
            "Invalid environment values (over {MAX_ENV_VALUE_BYTES} bytes or containing control characters): {}",
            invalid.join(", ")
        ));
    }
    Ok(out)
}

/// What the dashboard may learn about one env value: `set`, `not_set`, or
/// `reference` (a `${NAME}` the CLI expands at spawn). Never the value.
pub fn env_value_status(value: &str) -> &'static str {
    if value.trim().is_empty() {
        "not_set"
    } else if value.contains("${") {
        "reference"
    } else {
        "set"
    }
}

/// Mask every env value of a server definition for an RPC answer.
pub fn masked_env(
    env: &std::collections::HashMap<String, String>,
) -> std::collections::BTreeMap<String, &'static str> {
    env.iter().map(|(k, v)| (k.clone(), env_value_status(v))).collect()
}

/// [`write_owner_only_atomic`] for writers outside this module that already
/// hold [`with_mcp_config_lock`] (the expert-pack merge).
pub fn write_mcp_config_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_owner_only_atomic(path, bytes)
}

/// Write `bytes` to `path` atomically with owner-only permissions from the
/// first byte: the temp file is restricted before anything is written, so a
/// secret never sits world-readable between write and chmod.
fn write_owner_only_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    // Unpredictable temp name in the same directory: a fixed name could be
    // pre-created by the employee (as a directory, a file or a link) and make
    // every repair fail. `create_new` refuses an existing entry, links
    // included.
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mcp.json".to_string());
    let tmp_path = path.with_file_name(format!("{file_name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp_path)
        .map_err(|e| format!("Failed to create temp MCP config: {e}"))?;
    duduclaw_core::platform::set_owner_only(&tmp_path)
        .map_err(|e| format!("Failed to restrict temp MCP config: {e}"))?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("Failed to write temp MCP config: {e}"));
    }
    drop(f);
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("Failed to rename temp MCP config: {e}"));
    }
    duduclaw_core::platform::set_owner_only(path).ok();
    Ok(())
}

/// Return the built-in MCP marketplace catalog.
///
/// Only packages confirmed on the npm registry and not marked deprecated /
/// unsupported are listed (checked 2026-10-03). The former `@anthropic-ai/
/// mcp-server-*` names never existed, and the github / slack / postgres /
/// brave-search / sqlite / fetch cards were removed: their
/// `@modelcontextprotocol/server-*` packages are unsupported or absent.
pub fn marketplace_catalog() -> Vec<McpCatalogItem> {
    vec![
        McpCatalogItem {
            id: "playwright".into(),
            name: "Playwright".into(),
            description: "Browser automation".into(),
            category: "browser".into(),
            author: "Microsoft".into(),
            tags: vec!["browser".into(), "automation".into(), "testing".into()],
            featured: true,
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["-y".into(), PLAYWRIGHT_MCP_PACKAGE.into(), "--headless".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "browserbase".into(),
            name: "Browserbase".into(),
            description: "Cloud browser".into(),
            category: "browser".into(),
            author: "Browserbase".into(),
            tags: vec!["browser".into(), "cloud".into(), "automation".into()],
            featured: false,
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["-y".into(), BROWSERBASE_MCP_PACKAGE.into()],
                env: env_placeholders(&BROWSERBASE_REQUIRED_ENV),
            },
            required_env: BROWSERBASE_REQUIRED_ENV.iter().map(|s| s.to_string()).collect(),
        },
        McpCatalogItem {
            id: "filesystem".into(),
            name: "Filesystem".into(),
            description: "File access".into(),
            category: "data".into(),
            author: "Model Context Protocol".into(),
            tags: vec!["files".into(), "storage".into(), "local".into()],
            featured: true,
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                    ".".into(),
                ],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "memory".into(),
            name: "Memory".into(),
            description: "Persistent memory".into(),
            category: "data".into(),
            author: "Model Context Protocol".into(),
            tags: vec!["memory".into(), "storage".into(), "knowledge".into()],
            featured: false,
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
    ]
}

/// Read and parse `.mcp.json` from an agent directory.
/// Returns an empty config if the file does not exist.
pub fn read_mcp_config(agent_dir: &Path) -> Result<McpConfig, String> {
    let path = agent_dir.join(".mcp.json");
    if !path.exists() {
        return Ok(McpConfig { mcp_servers: std::collections::HashMap::new() });
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read MCP config: {e}"))?;
    serde_json::from_str::<McpConfig>(&content)
        .map_err(|e| format!("Failed to parse MCP config: {e}"))
}

/// Add a server entry to an agent's `.mcp.json`, creating the file if needed.
/// Writes atomically via temp file + rename.
pub fn add_server_to_config(agent_dir: &Path, name: &str, def: &McpServerDef) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");
    with_mcp_config_lock(&path, || {
        let mut config = read_mcp_config(agent_dir)?;
        config.mcp_servers.insert(name.to_string(), def.clone());
        let json = serde_json::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
        write_owner_only_atomic(&path, json.as_bytes())
    })?;

    info!(path = %path.display(), server = name, "MCP server added to config");
    Ok(())
}

/// Remove a server entry from an agent's `.mcp.json`.
/// Returns an error if the server does not exist.
pub fn remove_server_from_config(agent_dir: &Path, server_name: &str) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");
    with_mcp_config_lock(&path, || {
        let mut config = read_mcp_config(agent_dir)?;
        if config.mcp_servers.remove(server_name).is_none() {
            return Err(format!("MCP server '{server_name}' not found in config"));
        }
        let json = serde_json::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
        write_owner_only_atomic(&path, json.as_bytes())
    })?;

    info!(path = %path.display(), server = server_name, "MCP server removed from config");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn playwright_config_headless() {
        let config = playwright_mcp_config(true);
        assert!(config.mcp_servers.contains_key("playwright"));
        let server = &config.mcp_servers["playwright"];
        assert_eq!(server.command, "npx");
        assert!(server.args.contains(&"--headless".to_string()));
    }

    #[test]
    fn write_and_read_config() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let config = playwright_mcp_config(true);
        assert!(write_mcp_config(dir.path(), &config).expect("first write should succeed"));
        // Second write should return false (already exists)
        assert!(!write_mcp_config(dir.path(), &config).expect("second write should return false"));
    }

    #[test]
    fn browserbase_config_has_literal_env() {
        let config = browserbase_mcp_config("key123", "proj456", "gem789").unwrap();
        let server = &config.mcp_servers["browserbase"];
        // Literal values: a `${NAME}` reference would resolve to nothing in
        // the allowlisted environment the gateway spawns the CLI with.
        assert_eq!(server.env["BROWSERBASE_API_KEY"], "key123");
        assert_eq!(server.env["BROWSERBASE_PROJECT_ID"], "proj456");
        assert_eq!(server.env["GEMINI_API_KEY"], "gem789");
        assert!(server.args.contains(&BROWSERBASE_MCP_PACKAGE.to_string()));
    }

    #[test]
    fn browserbase_config_refuses_missing_or_reference_values() {
        let err = browserbase_mcp_config("", "proj", "${GEMINI_API_KEY}").unwrap_err();
        assert!(err.contains("BROWSERBASE_API_KEY"), "{err}");
        assert!(err.contains("GEMINI_API_KEY"), "{err}");
        assert!(!err.contains("BROWSERBASE_PROJECT_ID"), "{err}");
    }

    fn browserbase_item() -> McpCatalogItem {
        marketplace_catalog().into_iter().find(|c| c.id == "browserbase").unwrap()
    }

    #[test]
    fn apply_required_env_fills_supplied_values() {
        let item = browserbase_item();
        let supplied: std::collections::HashMap<String, String> = [
            ("BROWSERBASE_API_KEY", "bb-key"),
            ("BROWSERBASE_PROJECT_ID", "bb-proj"),
            ("GEMINI_API_KEY", "gm-key"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let def = apply_required_env(&item.default_def, &item.required_env, &supplied).unwrap();
        assert_eq!(def.env["BROWSERBASE_API_KEY"], "bb-key");
        assert_eq!(def.env.len(), 3);
    }

    #[test]
    fn apply_required_env_names_every_missing_variable_without_values() {
        let item = browserbase_item();
        let supplied: std::collections::HashMap<String, String> =
            [("BROWSERBASE_API_KEY".to_string(), "secret-value-xyz".to_string())].into();
        let err = apply_required_env(&item.default_def, &item.required_env, &supplied).unwrap_err();
        assert!(err.contains("BROWSERBASE_PROJECT_ID") && err.contains("GEMINI_API_KEY"), "{err}");
        assert!(!err.contains("secret-value-xyz"), "error must not echo values: {err}");
    }

    #[test]
    fn apply_required_env_refuses_unknown_names_and_control_chars() {
        let item = browserbase_item();
        let mut supplied: std::collections::HashMap<String, String> = item
            .required_env
            .iter()
            .map(|k| (k.clone(), "v".to_string()))
            .collect();
        supplied.insert("NODE_OPTIONS".into(), "--require /tmp/x".into());
        let err = apply_required_env(&item.default_def, &item.required_env, &supplied).unwrap_err();
        assert!(err.contains("NODE_OPTIONS"), "{err}");

        supplied.remove("NODE_OPTIONS");
        supplied.insert("GEMINI_API_KEY".into(), "a\nb".into());
        let err = apply_required_env(&item.default_def, &item.required_env, &supplied).unwrap_err();
        assert!(err.contains("GEMINI_API_KEY"), "{err}");
    }

    #[test]
    fn apply_required_env_accepts_values_inside_def() {
        // `mcp.update` carries the values in `server_def.env`.
        let item = browserbase_item();
        let mut def = item.default_def.clone();
        for k in &item.required_env {
            def.env.insert(k.clone(), format!("lit-{k}"));
        }
        let out = apply_required_env(&def, &item.required_env, &Default::default()).unwrap();
        assert_eq!(out.env["GEMINI_API_KEY"], "lit-GEMINI_API_KEY");
        // The old `${NAME}` default is refused.
        let mut refs = item.default_def.clone();
        for k in &item.required_env {
            refs.env.insert(k.clone(), format!("${{{k}}}"));
        }
        assert!(apply_required_env(&refs, &item.required_env, &Default::default()).is_err());
    }

    #[test]
    fn masked_env_never_returns_values() {
        let env: std::collections::HashMap<String, String> = [
            ("A".to_string(), "sk-live-123".to_string()),
            ("B".to_string(), String::new()),
            ("C".to_string(), "${HOME}".to_string()),
        ]
        .into();
        let m = masked_env(&env);
        assert_eq!(m["A"], "set");
        assert_eq!(m["B"], "not_set");
        assert_eq!(m["C"], "reference");
        assert!(!serde_json::to_string(&m).unwrap().contains("sk-live-123"));
    }

    #[test]
    fn installed_browserbase_entry_is_literal_and_owner_only() {
        let dir = TempDir::new().unwrap();
        ensure_browserbase_in_config(dir.path(), "k1", "p1", "g1").unwrap();
        let cfg = read_mcp_config(dir.path()).unwrap();
        assert_eq!(cfg.mcp_servers["browserbase"].env["BROWSERBASE_API_KEY"], "k1");
        let tmp_left = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!tmp_left, "no temp file left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(".mcp.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// npm scopes/prefixes confirmed not to exist on the registry. A catalogue
    /// entry under one of these installs nothing and fails at first spawn.
    const DEAD_PACKAGE_PREFIXES: &[&str] = &["@anthropic-ai/mcp-server-"];

    /// The npm package an `npx` definition runs: the first non-flag argument.
    fn npx_package(def: &McpServerDef) -> Option<&str> {
        def.args.iter().map(String::as_str).find(|a| !a.starts_with('-'))
    }

    /// Every `${NAME}` reference inside the definition's env values.
    fn referenced_env_names(def: &McpServerDef) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for v in def.env.values() {
            let mut rest = v.as_str();
            while let Some(start) = rest.find("${") {
                let after = &rest[start + 2..];
                match after.find('}') {
                    Some(end) => {
                        out.insert(after[..end].to_string());
                        rest = &after[end + 1..];
                    }
                    None => break,
                }
            }
        }
        out
    }

    fn assert_def_sound(label: &str, def: &McpServerDef) {
        assert!(!def.command.trim().is_empty(), "{label}: empty command");
        assert!(!def.args.is_empty(), "{label}: empty args");
        let pkg = npx_package(def).unwrap_or_else(|| panic!("{label}: no package argument"));
        for dead in DEAD_PACKAGE_PREFIXES {
            assert!(
                !pkg.starts_with(dead),
                "{label}: package '{pkg}' uses the non-existent prefix '{dead}'"
            );
        }
    }

    #[test]
    fn catalog_entries_name_real_packages_and_declare_env() {
        let catalog = marketplace_catalog();
        assert!(!catalog.is_empty());
        let mut ids = std::collections::BTreeSet::new();
        for item in &catalog {
            assert!(ids.insert(item.id.clone()), "duplicate catalogue id '{}'", item.id);
            assert_def_sound(&item.id, &item.default_def);
            assert!(!item.author.trim().is_empty(), "{}: empty author", item.id);

            // Declared env == the env the definition passes, and the catalogue
            // carries no value at all: neither a literal secret nor a `${NAME}`
            // reference (which the allowlisted spawn env cannot resolve).
            let declared: std::collections::BTreeSet<String> =
                item.required_env.iter().cloned().collect();
            let passed: std::collections::BTreeSet<String> =
                item.default_def.env.keys().cloned().collect();
            assert_eq!(declared, passed, "{}: required_env != env keys", item.id);
            assert!(referenced_env_names(&item.default_def).is_empty(), "{}: env reference", item.id);
            assert!(
                item.default_def.env.values().all(|v| v.is_empty()),
                "{}: catalogue env must be empty placeholders",
                item.id
            );
            for name in &item.required_env {
                assert!(!name.trim().is_empty(), "{}: empty env name", item.id);
            }
        }
    }

    #[test]
    fn generated_browser_configs_name_real_packages() {
        for (name, def) in playwright_mcp_config(true)
            .mcp_servers
            .into_iter()
            .chain(browserbase_mcp_config("k", "p", "g").unwrap().mcp_servers)
        {
            assert_def_sound(&name, &def);
        }
        let bb = browserbase_mcp_config("k", "p", "g").unwrap();
        let env: std::collections::BTreeSet<&str> =
            bb.mcp_servers["browserbase"].env.keys().map(String::as_str).collect();
        assert_eq!(env, BROWSERBASE_REQUIRED_ENV.into_iter().collect());
    }

    #[test]
    fn dead_prefix_check_catches_old_names() {
        let def = McpServerDef {
            command: "npx".into(),
            args: vec!["-y".into(), "@anthropic-ai/mcp-server-playwright".into()],
            env: Default::default(),
        };
        let caught = std::panic::catch_unwind(|| assert_def_sound("legacy", &def));
        assert!(caught.is_err(), "the dead-prefix guard must reject the old package name");
    }

    #[test]
    fn ensure_playwright_merges() {
        let dir = TempDir::new().unwrap();
        // Write initial config with another server
        let mut initial = McpConfig { mcp_servers: std::collections::HashMap::new() };
        initial.mcp_servers.insert("memory".to_string(), McpServerDef {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-memory".to_string()],
            env: std::collections::HashMap::new(),
        });
        write_mcp_config(dir.path(), &initial).expect("initial write should succeed");
        // Need to remove the file first since write_mcp_config skips existing
        std::fs::remove_file(dir.path().join(".mcp.json")).expect("remove should succeed");
        write_mcp_config(dir.path(), &initial).expect("second write should succeed");

        ensure_playwright_in_config(dir.path(), true).expect("ensure playwright should succeed");

        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).expect("read config should succeed");
        let config: McpConfig = serde_json::from_str(&content).expect("config should be valid JSON");
        assert!(config.mcp_servers.contains_key("playwright"));
        assert!(config.mcp_servers.contains_key("memory"));
    }

    // ── Home derivation (T3 fix) ──────────────────────────────
    //
    // `derive_home_from_agent_dir` must recover the same DuDuClaw home for a
    // normal agent (`<home>/agents/<id>`, two parents up) and an ephemeral
    // one (`<home>/agents/.ephemeral/<id>`, three parents up) — the bug fixed
    // here was that the naive two-parent walk landed on `<home>/agents`
    // instead of `<home>` for the ephemeral shape, poisoning the derived
    // identity token's key root.

    #[test]
    fn derive_home_normal_agent_path() {
        let home = std::path::Path::new("/tmp/duduclaw-home");
        let agent_dir = home.join("agents").join("sales-rep");
        assert_eq!(derive_home_from_agent_dir(&agent_dir), home);
    }

    #[test]
    fn derive_home_ephemeral_agent_path_matches_normal() {
        let home = std::path::Path::new("/tmp/duduclaw-home");
        let normal_dir = home.join("agents").join("sales-rep");
        let ephemeral_dir = home.join("agents").join(".ephemeral").join("eph-abc123");

        let normal_home = derive_home_from_agent_dir(&normal_dir);
        let ephemeral_home = derive_home_from_agent_dir(&ephemeral_dir);

        assert_eq!(normal_home, home);
        assert_eq!(
            ephemeral_home, home,
            "ephemeral scaffold sits one level deeper than a normal agent dir; \
             the derived home must still land on the same root"
        );
        assert_eq!(ephemeral_home, normal_home);
    }

    // ── Agent-ID env migration tests ──────────────────────────
    //
    // Each test creates an agent directory named so `ensure_duduclaw_absolute_path`
    // derives the expected `DUDUCLAW_AGENT_ID`. We set `DUDUCLAW_BIN` (via the
    // env used by `duduclaw_core::resolve_duduclaw_bin`) so the "command must be
    // absolute AND must exist" invariant is satisfied under test.

    /// Return a usable absolute path to `/bin/sh` (exists on Linux + macOS),
    /// which we use as a placeholder duduclaw binary in tests — it satisfies
    /// the `exists()` check inside `ensure_duduclaw_absolute_path`.
    fn fake_bin_path() -> std::path::PathBuf {
        // Must be absolute *and* existing on the host: `ensure_duduclaw_absolute_path`
        // early-returns when the resolved bin isn't absolute, and treats a
        // non-existent command as "needs update". A Unix path like `/bin/sh` is
        // NOT absolute on Windows (`Path::is_absolute()` requires a drive/UNC),
        // which silently skipped the migration and failed these tests on Windows.
        if cfg!(windows) {
            std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe")
        } else {
            std::path::PathBuf::from("/bin/sh")
        }
    }

    /// Scoped `DUDUCLAW_BIN` override. Sets the env on construction, removes it
    /// on drop. Tests that use this must hold `BIN_ENV_LOCK` so parallel runs
    /// don't clobber each other.
    struct BinEnvOverride;
    impl BinEnvOverride {
        fn new(path: &std::path::Path) -> Self {
            // SAFETY: serialized via `BIN_ENV_LOCK` in each test.
            unsafe { std::env::set_var("DUDUCLAW_BIN", path); }
            Self
        }
    }
    impl Drop for BinEnvOverride {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("DUDUCLAW_BIN"); }
        }
    }

    /// Scoped `DUDUCLAW_HOME` override — the forward-set var that tells an
    /// MCP child which home to serve. Same locking contract as
    /// [`BinEnvOverride`].
    struct HomeEnvOverride;
    impl HomeEnvOverride {
        fn set(path: &std::path::Path) -> Self {
            // SAFETY: serialized via `BIN_ENV_LOCK` in each test.
            unsafe { std::env::set_var("DUDUCLAW_HOME", path); }
            Self
        }
    }
    impl Drop for HomeEnvOverride {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("DUDUCLAW_HOME"); }
        }
    }

    /// Scoped `DUDUCLAW_MCP_API_KEY` override — the forward-set var whose
    /// value `ensure_duduclaw_absolute_path` must keep in sync inside
    /// `.mcp.json`. Same locking contract as [`BinEnvOverride`].
    struct ApiKeyEnvOverride;
    impl ApiKeyEnvOverride {
        fn set(value: &str) -> Self {
            // SAFETY: serialized via `BIN_ENV_LOCK` in each test.
            unsafe { std::env::set_var(duduclaw_core::ENV_MCP_API_KEY, value); }
            Self
        }
    }
    impl Drop for ApiKeyEnvOverride {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(duduclaw_core::ENV_MCP_API_KEY); }
        }
    }

    static BIN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Acquire `BIN_ENV_LOCK`, tolerating poisoning. The guarded data is `()`, so
    /// if one test panics while holding the lock the others can still serialize on
    /// it — a single real failure stays a single failure instead of cascading into
    /// `PoisonError` noise across the whole `DUDUCLAW_BIN` test group.
    fn lock_bin_env() -> std::sync::MutexGuard<'static, ()> {
        BIN_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_json(path: &std::path::Path, value: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(value).unwrap();
        std::fs::write(path, pretty).unwrap();
    }

    fn read_mcp_json(path: &std::path::Path) -> serde_json::Value {
        let content = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn mcp_json_migration_adds_agent_id_env() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("duduclaw-tl");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // Start with empty env block — exactly the broken state we're fixing.
        let existing = serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": fake_bin_path().to_string_lossy(),
                    "args": ["mcp-server"],
                    "env": {}
                }
            }
        });
        let path = agent_dir.join(".mcp.json");
        write_json(&path, &existing);

        let changed = ensure_duduclaw_absolute_path(&agent_dir).unwrap();
        assert!(changed, "migration must report a change");

        let got = read_mcp_json(&path);
        let env = &got["mcpServers"]["duduclaw"]["env"];
        assert_eq!(
            env["DUDUCLAW_AGENT_ID"].as_str(),
            Some("duduclaw-tl"),
            "env block must contain the agent-directory name"
        );
    }

    #[test]
    fn mcp_json_migration_drops_other_env_vars() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("duduclaw-eng-agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let existing = serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": fake_bin_path().to_string_lossy(),
                    "args": ["mcp-server"],
                    "env": { "FOO": "bar", "BAZ": "qux" }
                }
            }
        });
        let path = agent_dir.join(".mcp.json");
        write_json(&path, &existing);

        ensure_duduclaw_absolute_path(&agent_dir).unwrap();

        let got = read_mcp_json(&path);
        let env = &got["mcpServers"]["duduclaw"]["env"];
        // F4 (2026-10-06): regenerated whole — was "must survive".
        assert!(env.get("FOO").is_none(), "FOO must be dropped: {env}");
        assert!(env.get("BAZ").is_none(), "BAZ must be dropped: {env}");
        assert_eq!(
            env["DUDUCLAW_AGENT_ID"].as_str(),
            Some("duduclaw-eng-agent"),
        );
    }

    /// `.mcp.json` platform fix: the duduclaw entry's env is regenerated to
    /// what this module writes, and a second run with nothing to change does
    /// not rewrite the file.
    #[test]
    fn mcp_json_rewrite_drops_unmanaged_duduclaw_env_and_is_idempotent() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("duduclaw-norm");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let path = agent_dir.join(".mcp.json");
        write_json(&path, &serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": fake_bin_path().to_string_lossy(),
                    "args": ["mcp-server"],
                    "env": {
                        "PATH": "/usr/bin",
                        "DUDUCLAW_TURN_ID": "",
                        "DUDUCLAW_SESSION_ID": "forged",
                        "DUDUCLAW_UPSTREAM_UNKNOWN": "0"
                    }
                }
            }
        }));
        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        let env = read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"].clone();
        for k in ["DUDUCLAW_TURN_ID", "DUDUCLAW_SESSION_ID", "DUDUCLAW_UPSTREAM_UNKNOWN"] {
            assert!(env.get(k).is_none(), "{k} must be removed: {env}");
        }
        // F4: a non-`DUDUCLAW_` key is dropped too (PATH, LD_PRELOAD, …).
        assert!(env.get("PATH").is_none(), "{env}");
        assert_eq!(env["DUDUCLAW_AGENT_ID"].as_str(), Some("duduclaw-norm"));

        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn refresh_for_spawn_only_touches_canonical_agent_dirs() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        use crate::mcp_spawn_gate::McpGateOutcome;
        assert!(matches!(refresh_for_spawn(&project).unwrap(), McpGateOutcome::NotApplicable(_)));
        assert!(!project.join(".mcp.json").exists());
        let eph = tmp.path().join("agents/.ephemeral/eph-1");
        std::fs::create_dir_all(&eph).unwrap();
        // F6: ephemeral employees and team role members are repaired too.
        assert_eq!(refresh_for_spawn(&eph).unwrap(), McpGateOutcome::Confirmed);
        assert!(eph.join(".mcp.json").is_file());
        let trash = tmp.path().join("agents/_trash");
        std::fs::create_dir_all(&trash).unwrap();
        assert!(matches!(refresh_for_spawn(&trash).unwrap(), McpGateOutcome::NotApplicable(_)));
        let agent = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(&agent).unwrap();
        assert_eq!(refresh_for_spawn(&agent).unwrap(), McpGateOutcome::Confirmed);
        assert_eq!(refresh_for_spawn(&agent).unwrap(), McpGateOutcome::Confirmed);
        // N4: an employee-shaped directory that does not exist is not passed
        // on unchecked, and neither is a directory outside <home>/agents that
        // holds an agent.toml.
        assert!(refresh_for_spawn(&tmp.path().join("agents/ghost")).is_err());
        let stray = tmp.path().join("elsewhere/agnes");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("agent.toml"), "[agent]\nname = \"agnes\"\n").unwrap();
        assert!(refresh_for_spawn(&stray).is_err());
    }

    /// N4: a duduclaw binary path that is not absolute makes the file
    /// unconfirmable (it used to return "nothing to do" and let the spawn go
    /// ahead with whatever the file said).
    #[test]
    fn a_relative_binary_path_refuses_instead_of_skipping() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(std::path::Path::new("duduclaw"));
        let tmp = TempDir::new().unwrap();
        let agent = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(&agent).unwrap();
        assert!(ensure_duduclaw_absolute_path(&agent).is_err());
        let e = prepare_mcp_config_for_spawn(&agent).unwrap_err();
        assert!(crate::mcp_spawn_gate::is_spawn_gate_error(&e), "{e}");
    }

    /// N1: a directory or file the employee creates under the old lock and
    /// temp-file names in its own directory cannot block the repair: the lock
    /// lives under `<home>/locks/` and the temp file name is unpredictable.
    #[test]
    fn decoys_in_the_employee_directory_do_not_block_the_repair() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let agent = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(agent.join(".mcp.json.lock")).unwrap();
        std::fs::create_dir_all(agent.join(".mcp.json.tmp")).unwrap();
        std::fs::write(agent.join("x.lock"), "decoy").unwrap();
        assert_eq!(
            prepare_mcp_config_for_spawn(&agent).unwrap(),
            crate::mcp_spawn_gate::McpGateOutcome::Confirmed
        );
        assert!(agent.join(".mcp.json").is_file());
        assert!(tmp.path().join("locks").is_dir(), "lock under <home>/locks");
        let leftovers: Vec<_> = std::fs::read_dir(&agent)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp") && n != ".mcp.json.tmp")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// F4: `args` and other fields of the duduclaw entry are reset, and a
    /// managed key this process has no value for (`DUDUCLAW_MCP_ALLOW_
    /// UNAUTHENTICATED`) is not kept; another entry with fields this module
    /// does not model (an HTTP server) is copied through untouched.
    #[test]
    fn mcp_json_entry_is_regenerated_whole_and_other_entries_kept_verbatim() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let path = agent_dir.join(".mcp.json");
        let http = serde_json::json!({ "type": "http", "url": "https://example.com/mcp" });
        write_json(&path, &serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": fake_bin_path().to_string_lossy(),
                    "args": ["mcp-server", "--extra"],
                    "cwd": "/tmp",
                    "env": { "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED": "1" }
                },
                "remote": http.clone()
            }
        }));
        // SAFETY: the bin-env lock serialises the env-mutating tests.
        unsafe { std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED") };
        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        let got = read_mcp_json(&path);
        let entry = &got["mcpServers"]["duduclaw"];
        assert_eq!(entry["args"], serde_json::json!(["mcp-server"]));
        assert!(entry.get("cwd").is_none(), "{entry}");
        assert!(entry["env"].get("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED").is_none(), "{entry}");
        assert_eq!(got["mcpServers"]["remote"], http);
    }

    /// F3: a `.mcp.json` that cannot be confirmed (a directory, invalid JSON)
    /// fails the repair, and the spawn gate audits it and refuses.
    #[test]
    fn an_unconfirmable_mcp_json_refuses_the_spawn_and_is_audited() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let bad_json = home.join("agents/agnes");
        std::fs::create_dir_all(&bad_json).unwrap();
        std::fs::write(bad_json.join(".mcp.json"), "{not json").unwrap();
        let as_dir = home.join("agents/bob");
        std::fs::create_dir_all(as_dir.join(".mcp.json")).unwrap();
        for dir in [&bad_json, &as_dir] {
            assert!(ensure_duduclaw_absolute_path(dir).is_err());
            let e = prepare_mcp_config_for_spawn(dir).unwrap_err();
            assert!(e.contains("無法確認"), "{e}");
        }
        assert_eq!(std::fs::read_to_string(bad_json.join(".mcp.json")).unwrap(), "{not json");
        let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
        assert_eq!(audit.matches(AUDIT_MCP_CONFIG_UNVERIFIED).count(), 2, "{audit}");
    }

    /// An entry carrying the MCP API key is never rewritten by a process that
    /// has no key: the repair refuses (nothing written) instead of dropping it.
    #[test]
    fn a_process_without_the_api_key_does_not_strip_it() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let path = agent_dir.join(".mcp.json");
        {
            let _key = ApiKeyEnvOverride::set("ddc_prod_33333333333333333333333333333333");
            assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        }
        let before = std::fs::read(&path).unwrap();
        if duduclaw_core::mcp_forward_env_vars()
            .iter()
            .any(|(k, _)| k == duduclaw_core::ENV_MCP_API_KEY)
        {
            // An internal key is provisioned in this test process; the case
            // under test cannot be set up here.
            return;
        }
        let e = ensure_duduclaw_absolute_path(&agent_dir).unwrap_err();
        assert!(e.contains(duduclaw_core::ENV_MCP_API_KEY), "{e}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(prepare_mcp_config_for_spawn(&agent_dir).is_err());
    }

    /// F9: the approved-install writer goes through the shared lock and keeps
    /// the duduclaw entry; the repair afterwards keeps the new server.
    #[test]
    fn add_server_then_repair_keeps_both_entries() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agents/agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();
        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        let def = McpServerDef {
            command: "npx".into(),
            args: vec!["-y".into(), "@playwright/mcp".into()],
            env: Default::default(),
        };
        add_server_to_config(&agent_dir, "playwright", &def).unwrap();
        ensure_duduclaw_absolute_path(&agent_dir).unwrap();
        let got = read_mcp_json(&agent_dir.join(".mcp.json"));
        assert!(got["mcpServers"].get("playwright").is_some());
        assert!(got["mcpServers"].get("duduclaw").is_some());
    }

    #[test]
    fn mcp_json_migration_preserves_other_mcp_servers() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("duduclaw-qa");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // Playwright must remain untouched — only `duduclaw` is migrated.
        let existing = serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": fake_bin_path().to_string_lossy(),
                    "args": ["mcp-server"],
                    "env": {}
                },
                "playwright": {
                    "command": "npx",
                    "args": ["-y", "@playwright/mcp", "--headless"],
                    "env": {}
                }
            }
        });
        let path = agent_dir.join(".mcp.json");
        write_json(&path, &existing);

        ensure_duduclaw_absolute_path(&agent_dir).unwrap();

        let got = read_mcp_json(&path);
        assert_eq!(
            got["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"].as_str(),
            Some("duduclaw-qa"),
        );
        // Playwright entry preserved byte-for-byte.
        assert_eq!(
            got["mcpServers"]["playwright"]["command"].as_str(),
            Some("npx")
        );
        assert_eq!(
            got["mcpServers"]["playwright"]["args"][1].as_str(),
            Some("@playwright/mcp")
        );
    }

    #[test]
    fn mcp_json_migration_creates_file_when_absent() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let changed = ensure_duduclaw_absolute_path(&agent_dir).unwrap();
        assert!(changed, "absent .mcp.json must be created");

        let got = read_mcp_json(&agent_dir.join(".mcp.json"));
        assert_eq!(
            got["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"].as_str(),
            Some("agnes"),
        );
    }

    #[test]
    fn mcp_json_migration_is_idempotent_once_migrated() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // First call creates + migrates.
        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        // Second call must be a no-op.
        assert!(
            !ensure_duduclaw_absolute_path(&agent_dir).unwrap(),
            "second call must not rewrite the file"
        );
    }

    /// WP21 debt ⑧ — with an `identity.key` under the home that owns this
    /// agent dir, the written env block gains a `DUDUCLAW_AGENT_TOKEN` that
    /// actually verifies for *this* agent id, and an already-migrated file
    /// missing the token is detected and rewritten.
    #[test]
    fn mcp_json_carries_a_verifiable_identity_token_when_key_exists() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents").join("sales-rep");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let key = duduclaw_core::ensure_identity_key(home).unwrap();

        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        let path = agent_dir.join(".mcp.json");
        let env = read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"].clone();
        assert_eq!(env["DUDUCLAW_AGENT_ID"].as_str(), Some("sales-rep"));
        let token = env["DUDUCLAW_AGENT_TOKEN"].as_str().expect("token written");
        assert!(duduclaw_core::verify_identity_token(&key, "sales-rep", token));
        // The token is bound to this id — it cannot be lifted into another
        // agent's config to impersonate them.
        assert!(!duduclaw_core::verify_identity_token(&key, "ceo", token));

        // Idempotent once written.
        assert!(!ensure_duduclaw_absolute_path(&agent_dir).unwrap());

        // A file that predates the feature (id but no token) is repaired.
        let mut stale = read_mcp_json(&path);
        stale["mcpServers"]["duduclaw"]["env"]
            .as_object_mut()
            .unwrap()
            .remove("DUDUCLAW_AGENT_TOKEN");
        write_json(&path, &stale);
        assert!(
            ensure_duduclaw_absolute_path(&agent_dir).unwrap(),
            "a missing token must be detected as needing an update"
        );
        assert_eq!(
            read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_TOKEN"].as_str(),
            Some(token)
        );
    }

    /// The gateway rotates its internal MCP key before the authenticator's
    /// 30-day hard expiry (`duduclaw-gateway::mcp_internal_key`), and this
    /// boot fixup is what carries the rotated value into every agent's
    /// `.mcp.json`. A *stale* `DUDUCLAW_MCP_API_KEY` already present in the
    /// file must therefore be overwritten, not left alone — otherwise the
    /// rotation never reaches the CLI-spawned MCP children. Everything else
    /// in the env block (identity token, operator-set vars) must survive.
    #[test]
    fn mcp_json_migration_overwrites_stale_forward_api_key() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents").join("sales-rep");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let identity_key = duduclaw_core::ensure_identity_key(home).unwrap();
        let path = agent_dir.join(".mcp.json");

        // Boot 1: the pre-rotation key lands in .mcp.json.
        let stale_key = "ddc_prod_11111111111111111111111111111111";
        let old_env = ApiKeyEnvOverride::set(stale_key);
        assert!(ensure_duduclaw_absolute_path(&agent_dir).unwrap());
        let token = read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"]
            ["DUDUCLAW_AGENT_TOKEN"]
            .as_str()
            .expect("identity token written")
            .to_string();

        // An operator-set var the fixup has no business touching.
        let mut seeded = read_mcp_json(&path);
        seeded["mcpServers"]["duduclaw"]["env"]
            .as_object_mut()
            .unwrap()
            .insert("FOO".into(), serde_json::Value::String("bar".into()));
        write_json(&path, &seeded);

        // Boot 2: gateway rotated the internal key.
        drop(old_env);
        let fresh_key = "ddc_prod_22222222222222222222222222222222";
        let _new_env = ApiKeyEnvOverride::set(fresh_key);

        assert!(
            ensure_duduclaw_absolute_path(&agent_dir).unwrap(),
            "a stale forward key must be detected as needing an update"
        );

        let env = read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"].clone();
        assert_eq!(
            env["DUDUCLAW_MCP_API_KEY"].as_str(),
            Some(fresh_key),
            "the rotated key must overwrite the stale one"
        );
        assert_eq!(
            env["DUDUCLAW_AGENT_TOKEN"].as_str(),
            Some(token.as_str()),
            "identity token preserved"
        );
        assert!(duduclaw_core::verify_identity_token(
            &identity_key,
            "sales-rep",
            &token
        ));
        assert_eq!(env["DUDUCLAW_AGENT_ID"].as_str(), Some("sales-rep"));
        // F4 (2026-10-06): the entry is regenerated whole, so a foreign var
        // does not survive (it could be LD_PRELOAD as easily as FOO).
        assert!(env.get("FOO").is_none(), "foreign env var dropped: {env}");

        // Idempotent once the fresh key is in place.
        assert!(!ensure_duduclaw_absolute_path(&agent_dir).unwrap());
    }

    /// An ephemeral / team-role-member scaffold (`<home>/agents/.ephemeral/<id>`)
    /// gets a `.mcp.json` created from scratch, and that file is its ONLY route
    /// to the duduclaw MCP server: the boot sweep walks `<home>/agents/*` and
    /// never descends into `.ephemeral/`, and `spawn_env`'s allowlist strips
    /// `DUDUCLAW_HOME` / `DUDUCLAW_PORT` from the spawned CLI's environment, so
    /// the env block written here is the only place the child can learn them.
    #[test]
    fn mcp_json_created_for_ephemeral_layout_carries_home_and_eph_identity() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("isolated-home");
        std::fs::create_dir_all(&home).unwrap();
        let _home_env = HomeEnvOverride::set(&tmp.path().join("source-home"));
        let eph_id = "eph-agnes-r1-planner-9d9044";
        let agent_dir = home.join("agents").join(".ephemeral").join(eph_id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        let key = duduclaw_core::ensure_identity_key(&home).unwrap();

        assert!(
            ensure_duduclaw_absolute_path(&agent_dir).unwrap(),
            "a scaffold with no .mcp.json must get one created"
        );

        let path = agent_dir.join(".mcp.json");
        assert!(path.is_file(), ".mcp.json must exist on disk");
        let env = read_mcp_json(&path)["mcpServers"]["duduclaw"]["env"].clone();
        assert_eq!(
            env["DUDUCLAW_AGENT_ID"].as_str(),
            Some(eph_id),
            "the member must self-identify, not fall back to default_agent"
        );
        assert_eq!(
            env["DUDUCLAW_HOME"].as_str(),
            Some(home.to_string_lossy().as_ref()),
            "the member's derived home must override an inherited source home"
        );
        // The `.ephemeral` segment must not shift the derived key root to
        // `<home>/agents` — otherwise every minted token fails verification.
        let token = env["DUDUCLAW_AGENT_TOKEN"].as_str().expect("token written");
        assert!(duduclaw_core::verify_identity_token(&key, eph_id, token));
    }

    /// ...and with no key, the env block is byte-identical to pre-WP21.
    #[test]
    fn mcp_json_has_no_token_when_feature_is_disabled() {
        let _guard = lock_bin_env();
        let _bin = BinEnvOverride::new(&fake_bin_path());

        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agents").join("sales-rep");
        std::fs::create_dir_all(&agent_dir).unwrap();

        ensure_duduclaw_absolute_path(&agent_dir).unwrap();
        let env = read_mcp_json(&agent_dir.join(".mcp.json"))["mcpServers"]["duduclaw"]["env"]
            .clone();
        assert_eq!(env["DUDUCLAW_AGENT_ID"].as_str(), Some("sales-rep"));
        assert!(env.get("DUDUCLAW_AGENT_TOKEN").is_none());
    }
}
