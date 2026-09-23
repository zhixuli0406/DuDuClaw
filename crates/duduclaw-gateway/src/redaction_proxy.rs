//! RFC-23 §13.6 (WP-P): make redaction reach the tool surfaces that are
//! **not** DuDuClaw's own MCP server.
//!
//! Two halves, one purpose.
//!
//! 1. **Claude-runtime spawn rewrite** — [`maybe_proxy_mcp_config`]. When an
//!    agent's turn has redaction active, the per-spawn `--mcp-config` no
//!    longer points at the agent's raw `.mcp.json`: every third-party stdio
//!    server in it is rewritten to launch through `duduclaw mcp-proxy`
//!    ([`rewrite_mcp_config_for_proxy`]), which applies the same egress /
//!    result redaction the built-in choke point applies. Redaction inactive ⇒
//!    nothing is written and the spawn is byte-identical to before.
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

use serde_json::{json, Map, Value};
use tracing::{debug, warn};

use duduclaw_llm::{InterceptDecision, ToolInterceptor};
use duduclaw_redaction::{EgressDecision, RedactionManager};

/// Env var carrying the wrapped server's original `.mcp.json` `env` map,
/// JSON-encoded, to the proxy process.
///
/// **Must match `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_VAR`** (the cli
/// crate cannot be depended on from here — see the module docs). A rename on
/// either side is caught by `proxy_env_var_name_is_the_documented_contract`
/// in `duduclaw-cli`.
pub const PROXY_UPSTREAM_ENV_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV";

/// Placeholder substituted for a tool result the pipeline could not redact.
/// Mirrors `duduclaw_cli::mcp_redaction::REDACTION_FAILED_PLACEHOLDER`.
pub const REDACTION_FAILED_PLACEHOLDER: &str = "[redaction failed — value withheld]";

/// Env names copied from the `.mcp.json` `duduclaw` entry onto every
/// rewritten proxy entry, so the proxy process resolves the same home, the
/// same agent identity and the same MCP credential the built-in server does.
///
/// `DUDUCLAW_AGENT_TOKEN` is deliberately NOT carried: it is the MAC proving
/// an agent id was issued by DuDuClaw, used only to authorise MCP tool calls.
/// The proxy makes none — it just redacts a byte stream — so handing it (and,
/// by inheritance, a third-party server) that token would widen the blast
/// radius for nothing.
///
/// `DUDUCLAW_SESSION_ID` is not listed because the Claude CLI passes its own
/// environment down to MCP children and the gateway already sets it on the
/// CLI process (see `channel_reply::spawn_claude_cli_with_env`) — the proxy
/// inherits exactly the same value `duduclaw mcp-server` does.
pub const PROXY_CARRY_ENV: &[&str] = &[
    "DUDUCLAW_HOME",
    "DUDUCLAW_PORT",
    "DUDUCLAW_INSTANCE",
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
];

// ── 1. Spawn-time `.mcp.json` rewrite ───────────────────────────────────────

/// Is this `.mcp.json` entry DuDuClaw's own MCP server?
///
/// `duduclaw` is the default key; `duduclaw-pro` is the legacy name and
/// `duduclaw-<instance>` is the multi-instance form
/// (`duduclaw_core::mcp_server_key`). Never proxied — it already redacts.
fn is_duduclaw_server(name: &str) -> bool {
    name == "duduclaw" || name.starts_with("duduclaw-")
}

/// Rewrite every third-party **stdio** server so it launches through
/// `duduclaw mcp-proxy`.
///
/// Pure: no IO, no env reads. Left untouched are
/// - DuDuClaw's own entry ([`is_duduclaw_server`]),
/// - `type` / `url` entries (HTTP / SSE MCP servers — there is no child
///   process to wrap; a warn is logged so the gap is visible, not silent),
/// - entries with no `command`,
/// - entries already pointing at `mcp-proxy` (idempotent).
///
/// The original `env` map rides along in [`PROXY_UPSTREAM_ENV_VAR`] rather
/// than in argv, so a `PGPASSWORD` never lands in a world-readable
/// `/proc/<pid>/cmdline`.
pub fn rewrite_mcp_config_for_proxy(json: &Value, self_exe: &Path) -> Value {
    let mut out = json.clone();
    let exe = self_exe.to_string_lossy().into_owned();

    let Some(servers) = out.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return out;
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
        if def.get("url").is_some() || def.get("type").is_some() {
            warn!(
                server = %name,
                "RFC-23: HTTP/SSE MCP server is NOT proxied — its tool results reach the model unredacted"
            );
            continue;
        }
        let Some(command) = def.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        if is_already_proxied(&def, &exe) {
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
    out
}

/// The env pairs the proxy needs, lifted off whichever DuDuClaw entry this
/// config carries. Absent entry ⇒ empty (the proxy then relies on the Claude
/// CLI's inherited environment, exactly as a bare `.mcp.json` would).
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
    use crate::redaction_integration::{
        cli_flag_from_env, force_disable_active, BootOutcome,
    };
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
/// `None` ⇒ use the agent's `.mcp.json` unchanged (redaction inactive, no
/// `.mcp.json`, a config we cannot read/parse, or a config with nothing to
/// proxy). `Some(temp)` ⇒ pass `temp`'s path; the returned
/// [`tempfile::TempPath`] deletes the file when dropped, so callers hold it
/// until the child has exited — the same lifetime discipline the
/// `--system-prompt-file` guard uses at the same spawn sites.
pub fn maybe_proxy_mcp_config(home_dir: &Path, mcp_json: &Path) -> Option<tempfile::TempPath> {
    if !redaction_active_for_spawn(home_dir) {
        return None;
    }
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "RFC-23 mcp-proxy: current_exe() failed — external MCP servers stay unproxied");
            return None;
        }
    };
    write_proxy_mcp_config(mcp_json, &self_exe)
}

/// The IO half of [`maybe_proxy_mcp_config`], split out so the rewrite can be
/// exercised against a real file without touching process state.
pub fn write_proxy_mcp_config(mcp_json: &Path, self_exe: &Path) -> Option<tempfile::TempPath> {
    let raw = std::fs::read_to_string(mcp_json)
        .map_err(|e| warn!(path = %mcp_json.display(), error = %e, "RFC-23 mcp-proxy: unreadable .mcp.json"))
        .ok()?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| warn!(path = %mcp_json.display(), error = %e, "RFC-23 mcp-proxy: malformed .mcp.json"))
        .ok()?;

    let rewritten = rewrite_mcp_config_for_proxy(&parsed, self_exe);
    if rewritten == parsed {
        debug!("RFC-23 mcp-proxy: nothing to proxy in this .mcp.json");
        return None;
    }

    let mut file = tempfile::Builder::new()
        .prefix("duduclaw-mcp-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| warn!(error = %e, "RFC-23 mcp-proxy: temp file creation failed"))
        .ok()?;
    {
        use std::io::Write;
        let body = serde_json::to_string_pretty(&rewritten).ok()?;
        file.write_all(body.as_bytes())
            .map_err(|e| warn!(error = %e, "RFC-23 mcp-proxy: temp file write failed"))
            .ok()?;
        file.flush().ok()?;
    }
    let path = file.into_temp_path();
    // The file carries the upstream servers' own credentials, same as the
    // `.mcp.json` it was derived from.
    duduclaw_core::platform::set_owner_only(&path).ok();
    Some(path)
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
        Self { manager, agent_id, session_id }
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
            Ok(EgressDecision::Deny { reason, tokens_seen }) => InterceptDecision::Deny(format!(
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
        let ctx = duduclaw_redaction::ToolContext { tool_name: &name, args: Some(args) };
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
static MANAGER_CACHE: OnceLock<Mutex<Option<(PathBuf, Option<SystemTime>, Arc<RedactionManager>)>>> =
    OnceLock::new();

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
            return Err("redaction became disabled between the gate and the build".to_string())
        }
        crate::redaction_integration::BootOutcome::Poisoned(reason) => {
            return Err(format!("redaction config is poisoned: {reason}"))
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
        assert_eq!(data_file_guard_mode(typo.path()), "on", "typo must fail safe");
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

    fn exe() -> PathBuf {
        PathBuf::from("/opt/duduclaw/bin/duduclaw")
    }

    fn sample() -> Value {
        json!({
            "mcpServers": {
                "duduclaw": {
                    "command": "/opt/duduclaw/bin/duduclaw",
                    "args": ["mcp-server"],
                    "env": {
                        "DUDUCLAW_HOME": "/home/u/.duduclaw",
                        "DUDUCLAW_AGENT_ID": "agnes",
                        "DUDUCLAW_AGENT_TOKEN": "mac-do-not-forward",
                        "DUDUCLAW_MCP_API_KEY": "ddc_dev_0123456789abcdef0123456789abcdef"
                    }
                },
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

    #[test]
    fn duduclaw_entry_is_never_proxied() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe());
        assert_eq!(
            out["mcpServers"]["duduclaw"],
            sample()["mcpServers"]["duduclaw"],
            "the built-in server already redacts — wrapping it would double-tokenise"
        );
    }

    #[test]
    fn instance_scoped_duduclaw_keys_are_also_skipped() {
        let cfg = json!({"mcpServers": {"duduclaw-staging": {"command": "/x", "args": ["mcp-server"]}}});
        assert_eq!(rewrite_mcp_config_for_proxy(&cfg, &exe()), cfg);
    }

    #[test]
    fn stdio_servers_are_rewritten_to_the_proxy() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe());
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
    fn the_original_env_is_preserved_for_the_upstream() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe());
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
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe());
        let env = &out["mcpServers"]["crm_pg"]["env"];
        assert_eq!(env["DUDUCLAW_HOME"], json!("/home/u/.duduclaw"));
        assert_eq!(env["DUDUCLAW_AGENT_ID"], json!("agnes"));
        assert_eq!(
            env["DUDUCLAW_MCP_API_KEY"],
            json!("ddc_dev_0123456789abcdef0123456789abcdef")
        );
        assert!(
            env.get("DUDUCLAW_AGENT_TOKEN").is_none(),
            "the identity MAC has no use in the proxy and must not reach a third-party server"
        );
    }

    #[test]
    fn url_servers_are_left_alone() {
        let out = rewrite_mcp_config_for_proxy(&sample(), &exe());
        assert_eq!(
            out["mcpServers"]["remote_erp"],
            sample()["mcpServers"]["remote_erp"]
        );
    }

    #[test]
    fn rewriting_is_idempotent() {
        let once = rewrite_mcp_config_for_proxy(&sample(), &exe());
        let twice = rewrite_mcp_config_for_proxy(&once, &exe());
        assert_eq!(once, twice);
    }

    #[test]
    fn a_config_without_mcp_servers_is_returned_unchanged() {
        let cfg = json!({"other": 1});
        assert_eq!(rewrite_mcp_config_for_proxy(&cfg, &exe()), cfg);
    }

    #[test]
    fn a_server_without_a_command_is_skipped() {
        let cfg = json!({"mcpServers": {"weird": {"args": ["x"]}}});
        assert_eq!(rewrite_mcp_config_for_proxy(&cfg, &exe()), cfg);
    }

    #[test]
    fn an_all_duduclaw_config_produces_no_temp_file() {
        // Nothing to proxy ⇒ `None`, so the spawn keeps using the original
        // path and stays byte-identical.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            json!({"mcpServers": {"duduclaw": {"command": "/x", "args": ["mcp-server"]}}})
                .to_string(),
        )
        .unwrap();
        assert!(write_proxy_mcp_config(&path, &exe()).is_none());
    }

    #[test]
    fn a_rewritten_config_lands_in_a_temp_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, sample().to_string()).unwrap();

        let temp = write_proxy_mcp_config(&path, &exe()).expect("there is something to proxy");
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(&temp).unwrap()).unwrap();
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
        assert!(write_proxy_mcp_config(&path, &exe()).is_none());
    }

    #[test]
    fn redaction_inactive_when_no_config_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!redaction_active_for_spawn(dir.path()));
        let mcp = dir.path().join(".mcp.json");
        std::fs::write(&mcp, sample().to_string()).unwrap();
        assert!(
            maybe_proxy_mcp_config(dir.path(), &mcp).is_none(),
            "redaction off ⇒ the spawn must be byte-identical to today"
        );
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
        assert_eq!(qualified_tool_name("crm_pg", "pg_select"), "crm_pg.pg_select");
        assert_eq!(qualified_tool_name("", "memory_search"), "memory_search");
    }
}

#[cfg(test)]
mod interceptor_tests {
    use super::*;
    use duduclaw_redaction::{
        ManagerPaths, RedactionConfig, RestoreScope, RuleKind, RuleSpec,
    };

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
        icept.after_call("crm_pg", "pg_select", &json!({"table": "customers"}), &mut result);

        let rendered = result.to_string();
        assert!(!rendered.contains("王小明"), "{rendered}");
        assert!(rendered.contains("<REDACT:DB_FIELD:"), "{rendered}");
        assert!(!rendered.contains("w@example.com"), "general profile: {rendered}");
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
