//! Per-agent runtime/model config readers from `agent.toml` (RFC-25).
//!
//! Foundation for multi-runtime unlock:
//! - Phase 0: `[model] utility` — the cheap model for internal tasks (replaces
//!   scattered hardcoded `"claude-haiku-4-5"` literals).
//! - Phase 1+: `[runtime] provider` — which AgentRuntime backend to use.
//!
//! These are callable by anything holding only an `agent_dir` — they do not
//! need the registry or a fully-loaded `AgentConfig`.
//!
//! **R2 schema unification:** they used to parse `agent.toml` into a generic
//! `toml::Value` and walk it by hand, which made `[runtime]`, `[model]
//! fallbacks`/`standard`/`delegation_routing` and `[memory] decision_*`
//! invisible to the typed `AgentConfig`. They now go through the shared typed
//! parse point [`duduclaw_core::agent_toml`]. Two properties are preserved
//! exactly:
//!
//! - **Read-per-call.** Every accessor still re-reads the file, so a live
//!   `agent.toml` edit takes effect without a registry rescan (there is no
//!   file watcher). Callers needing several fields should still
//!   [`load_runtime_settings`] once.
//! - **Missing-key defaults.** Each function's documented fallback is
//!   unchanged, including the value-level filters (`> 0`, non-empty) that
//!   deliberately treat an out-of-range written value as "unset".
//!
//! `config.toml` readers in this module are untouched — the unification is
//! about `agent.toml` only.

use std::path::Path;

use duduclaw_core::agent_toml::{self, AgentTomlSections};
use duduclaw_core::types::RuntimeType;

/// Default lightweight model when `[model] utility` is unset.
///
/// Re-exported from [`duduclaw_core::types::DEFAULT_UTILITY_MODEL`] so the
/// literal lives in exactly one place (RFC-25 L6). The typed
/// [`duduclaw_core::types::ModelConfig::utility`] field (serde round-trip for
/// full-config load/save, e.g. dashboard editing) and this lightweight
/// `agent.toml` reader (for callers that only have an `agent_dir`) both read the
/// same `[model] utility` key. Claude keeps this default; other providers
/// inherit the main model when the utility override is absent or blank.
pub use duduclaw_core::types::DEFAULT_UTILITY_MODEL;

/// One typed read of the agent's `agent.toml` sections.
///
/// Replaces the former `read_agent_toml -> Option<toml::Value>` helper. It
/// returns sections rather than an `Option` because the typed parse is total:
/// a missing, unreadable or malformed file yields all-defaults, which is what
/// every caller's `Option` arm did anyway.
fn read_sections(agent_dir: &Path) -> AgentTomlSections {
    agent_toml::load(agent_dir)
}

fn config_file_name(path: &Path) -> &str {
    duduclaw_core::truncate_bytes(path.file_name().and_then(|name| name.to_str()).unwrap_or("config.toml"), 200)
}

fn config_refusal(path: &Path, reason: &str, diagnostic: &str) -> String {
    tracing::warn!(
        path = %duduclaw_core::truncate_bytes(&path.display().to_string(), 200),
        detail = %duduclaw_core::truncate_bytes(diagnostic, 200),
        "runtime config refused"
    );
    format!("{}: {reason}", config_file_name(path))
}

fn wrong_config_type(path: &Path, field: &str) -> String {
    let reason = format!("wrong type for {field}");
    config_refusal(path, &reason, &reason)
}

/// Validate only runtime selectors before the deliberately lenient typed read.
pub(crate) fn check_runtime_field_types(doc: &toml::Value, path: &Path) -> Result<(), String> {
    if let Some(runtime) = doc.get("runtime") {
        let runtime = runtime.as_table().ok_or_else(|| wrong_config_type(path, "[runtime]"))?;
        for field in ["provider", "fallback"] {
            if runtime.get(field).is_some_and(|value| !value.is_str()) {
                return Err(wrong_config_type(path, &format!("[runtime] {field}")));
            }
        }
    }
    if let Some(team) = doc.get("team") {
        let team = team.as_table().ok_or_else(|| wrong_config_type(path, "[team]"))?;
        if let Some(roles) = team.get("roles") {
            let roles = roles.as_table().ok_or_else(|| wrong_config_type(path, "[team.roles]"))?;
            for role in duduclaw_core::types::Role::ALL {
                if let Some(spec) = roles.get(role.as_str()) {
                    let field = format!("[team.roles.{}]", role.as_str());
                    let spec = spec.as_table().ok_or_else(|| wrong_config_type(path, &field))?;
                    for key in ["runtime", "model"] {
                        if spec.get(key).is_some_and(|value| !value.is_str()) {
                            return Err(wrong_config_type(path, &format!("{field} {key}")));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn check_removed_agent_runtimes(doc: &toml::Value, path: &Path) -> Result<(), String> {
    for field in ["provider", "fallback"] {
        if let Some(removed) = doc.get("runtime").and_then(|runtime| runtime.get(field))
            .and_then(toml::Value::as_str)
            .and_then(duduclaw_core::runtime_catalog::removed_runtime) {
            return Err(removed.refusal(&format!("{} [runtime] {field}", config_file_name(path))));
        }
    }
    for role in duduclaw_core::types::Role::ALL {
        if let Some(removed) = doc.get("team").and_then(|team| team.get("roles"))
            .and_then(|roles| roles.get(role.as_str())).and_then(|spec| spec.get("runtime"))
            .and_then(toml::Value::as_str)
            .and_then(duduclaw_core::runtime_catalog::removed_runtime) {
            return Err(removed.refusal(&format!("{} [team.roles.{}] runtime", config_file_name(path), role.as_str())));
        }
    }
    Ok(())
}

/// Authority checks must not hide a configured runtime behind parse defaults.
pub(crate) fn read_config_for_gate(path: &Path) -> Result<Option<toml::Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound
                && matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
                return Ok(None);
            }
            let reason = if error.kind() == std::io::ErrorKind::InvalidData { "invalid UTF-8" } else { "unreadable" };
            return Err(config_refusal(path, reason, &error.to_string()));
        }
    };
    let doc = text.parse::<toml::Value>()
        .map_err(|error| config_refusal(path, "invalid TOML", error.message()))?;
    check_runtime_field_types(&doc, path)?;
    Ok(Some(doc))
}

pub(crate) fn read_agent_sections_for_gate(agent_dir: &Path) -> Result<AgentTomlSections, String> {
    let source_path = agent_dir.join("agent.toml");
    let source = read_config_for_gate(&source_path)?;
    if let Some(doc) = &source {
        check_removed_agent_runtimes(doc, &source_path)?;
        strict_team_config(doc, &source_path)?;
    }
    let mut effective = source;
    let mut effective_path = source_path;
    if let Some(home) = duduclaw_core::preset::agent_home_dir(agent_dir) {
        if let Some(id) = agent_dir.file_name().and_then(|id| id.to_str()) {
            let path = duduclaw_core::preset::agent_resolved_path(&home, id);
            if let Some(doc) = read_config_for_gate(&path)? {
                check_removed_agent_runtimes(&doc, &path)?;
                strict_team_config(&doc, &path)?;
                effective = Some(doc);
                effective_path = path;
            }
        }
    }
    effective.map(|doc| doc.try_into::<AgentTomlSections>()
        .map_err(|error| config_refusal(&effective_path, "invalid TOML", error.message())))
        .transpose().map(Option::unwrap_or_default)
}

pub(crate) fn strict_team_config(doc: &toml::Value, path: &Path) -> Result<duduclaw_core::types::TeamConfig, String> {
    doc.get("team").map(|team| team.clone().try_into::<duduclaw_core::types::TeamConfig>()
        .map_err(|error| config_refusal(path, "invalid TOML", error.message())))
        .transpose().map(Option::unwrap_or_default)
}

/// Removed runtime choices must be fixed explicitly before dispatch.
/// Source and resolved selectors are checked before any default can be resolved.
pub fn ensure_active_agent_provider(agent_dir: &Path) -> Result<(), String> {
    let sections = read_agent_sections_for_gate(agent_dir)?;
    let global_team = duduclaw_core::preset::agent_home_dir(agent_dir)
        .map(|home| crate::team_composer::read_global_team_config(&home))
        .transpose()?
        .unwrap_or_default();
    ensure_active_team_providers(&global_team, &sections.team)
}

/// The active-provider gate's pure half, shared with the entry snapshot.
/// The caller supplies the already parsed employee team; no employee re-read.
pub(crate) fn ensure_active_team_providers(
    global_team: &duduclaw_core::types::TeamConfig,
    agent_team: &duduclaw_core::types::TeamConfig,
) -> Result<(), String> {
    let team = duduclaw_core::types::TeamConfig::merge(global_team, agent_team);
    for role in duduclaw_core::types::Role::ALL {
        if let Some(removed) = team.roles.get(*role).runtime.as_deref()
            .and_then(duduclaw_core::runtime_catalog::removed_runtime) {
            return Err(removed.refusal(&format!("config.toml [team.roles.{}] runtime", role.as_str())));
        }
    }
    Ok(())
}

pub fn ensure_active_utility_provider(home_dir: &Path) -> Result<(), String> {
    let config = read_config_for_gate(&home_dir.join("config.toml"))?;
    if let Some(removed) = config.as_ref()
        .and_then(|v| v.get("runtime"))
        .and_then(|v| v.get("utility_provider"))
        .and_then(|v| v.as_str())
        .and_then(duduclaw_core::runtime_catalog::removed_runtime)
    {
        return Err(removed.refusal("config.toml [runtime] utility_provider"));
    }
    Ok(())
}

/// All per-agent runtime/model settings from a single `agent.toml` read (RFC-25 L7).
///
/// Callers that need more than one field (the choke-point reads provider +
/// fallback; utility dispatch reads provider + utility model) should
/// [`load_runtime_settings`] once instead of calling the per-field accessors
/// repeatedly — each accessor re-reads and re-parses the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
    /// Read with the provider so live edits cannot form a stale model/provider pair.
    pub preferred_model: Option<String>,
    pub provider: RuntimeType,
    pub fallback: Option<RuntimeType>,
    /// `[model] utility` — the cheap model for internal tasks.
    pub utility_model: String,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            provider: RuntimeType::Claude,
            fallback: None,
            utility_model: DEFAULT_UTILITY_MODEL.to_string(),
            preferred_model: None,
        }
    }
}

impl RuntimeSettings {
    /// The "Claude vs non-Claude" routing decision (RFC-25 L8), centralized on the
    /// parsed settings so callers that already loaded them don't re-read the file.
    /// `Some(provider)` ⇒ route through the [`crate::runtime_dispatch`] choke-point;
    /// `None` ⇒ Claude (caller keeps its own optimized rotation/PTY path).
    pub fn non_claude_provider(&self) -> Option<RuntimeType> {
        match self.provider {
            RuntimeType::Claude => None,
            other => Some(other),
        }
    }
}

/// The ONE sanctioned "unknown runtime ⇒ default" call site family: reading a
/// *stored config field* where refusing would leave the caller with no runtime
/// at all and brick an otherwise working agent.
///
/// It is deliberately loud (`error!`, naming the bad value and the accepted
/// list) and deliberately NOT available to request-scoped code:
/// `duduclaw_core::types::RuntimeType::parse` returns `Option` precisely so an
/// RPC that acts on a caller-supplied runtime name must refuse instead of
/// silently driving the default one. If you are handling a request, do not
/// reach for this function.
fn parse_provider_or_default(
    agent_dir: &Path,
    field: &str,
    value: &str,
    source: DeprecatedRuntimeSource,
) -> RuntimeType {
    match RuntimeType::parse(value) {
        Some(rt) => {
            warn_once_if_deprecated_runtime_for(rt, source, agent_name(agent_dir).as_deref());
            rt
        }
        None => {
            tracing::error!(
                agent_dir = %agent_dir.display(),
                field,
                value = %value,
                valid = %RuntimeType::valid_values(),
                default = %RuntimeType::default().as_str(),
                "unknown runtime in config — falling back to the default runtime; \
                 fix the config, this agent is NOT running the backend it names"
            );
            RuntimeType::default()
        }
    }
}

// ── Deprecated-runtime read notices (R1, 2026-10) ─────────────────────────

/// Which kind of setting a deprecated runtime was read from. The warn-once
/// key is `(runtime, source)`: an operator who set `gemini` both as an
/// agent's provider and as the judge provider hears about each once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeprecatedRuntimeSource {
    /// `agent.toml [runtime] provider`
    AgentProvider,
    /// `agent.toml [runtime] fallback`
    AgentFallback,
    /// `config.toml [runtime] utility_provider`
    UtilityProvider,
    /// `config.toml [dispatch] judge_provider`
    JudgeProvider,
    /// `[team.roles.<role>] runtime`
    TeamRole,
    /// a Discovery attempt runtime (`[discovery.attempt.runtimes.<id>]`)
    DiscoveryAttempt,
}

impl DeprecatedRuntimeSource {
    /// The setting's name as an operator would search for it.
    pub fn setting(&self) -> &'static str {
        match self {
            Self::AgentProvider => "agent.toml [runtime] provider",
            Self::AgentFallback => "agent.toml [runtime] fallback",
            Self::UtilityProvider => "config.toml [runtime] utility_provider",
            Self::JudgeProvider => "config.toml [dispatch] judge_provider",
            Self::TeamRole => "[team.roles.*] runtime",
            Self::DiscoveryAttempt => "[discovery.attempt.runtimes] runtime",
        }
    }
}

/// The agent a per-agent setting belongs to: its directory name.
fn agent_name(agent_dir: &Path) -> Option<String> {
    agent_dir.file_name().and_then(|n| n.to_str()).map(str::to_string)
}

/// Pure half of [`warn_once_if_deprecated_runtime`]: `Some(message)` the
/// first time a deprecated `(runtime, source)` pair is seen in `seen`,
/// `None` for a non-deprecated runtime or a repeat sighting. `agent` names
/// the agent whose setting triggered the first sighting, when the caller
/// knows it (later agents with the same value are not named — the notice is
/// per value and source, not per agent; `duduclaw doctor` lists them all).
pub(crate) fn deprecated_runtime_first_notice(
    seen: &mut std::collections::HashSet<(&'static str, DeprecatedRuntimeSource)>,
    rt: RuntimeType,
    source: DeprecatedRuntimeSource,
    agent: Option<&str>,
) -> Option<String> {
    let dep = rt.deprecation()?;
    if !seen.insert((rt.as_str(), source)) {
        return None;
    }
    let setting = match agent {
        Some(agent) => format!("{} (agent `{agent}`)", source.setting()),
        None => source.setting().to_string(),
    };
    Some(format!("{setting}: {}", dep.notice(rt.as_str())))
}

#[cfg(test)]
thread_local! {
    /// Test-only: every deprecated-runtime notice consulted on this thread,
    /// whether or not the process-wide de-duplication let it through.
    pub(crate) static CONSULTED_NOTICES: std::cell::RefCell<Vec<(&'static str, DeprecatedRuntimeSource, Option<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Emit a deprecation `warn!` at most once per process per
/// `(runtime, source)`. Behaviour is otherwise untouched: the caller keeps
/// using the value it parsed. Readers on hot paths (`load_runtime_settings`
/// runs per reply) call this unconditionally — the de-duplication is here so
/// the log is not flooded with one line forever.
///
/// Returns `true` iff this call emitted the warning.
pub fn warn_once_if_deprecated_runtime(rt: RuntimeType, source: DeprecatedRuntimeSource) -> bool {
    warn_once_if_deprecated_runtime_for(rt, source, None)
}

/// [`warn_once_if_deprecated_runtime`] for a setting that belongs to one
/// agent: the first warning names it.
pub fn warn_once_if_deprecated_runtime_for(
    rt: RuntimeType,
    source: DeprecatedRuntimeSource,
    agent: Option<&str>,
) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    if !rt.is_deprecated() {
        return false;
    }
    #[cfg(test)]
    CONSULTED_NOTICES.with(|n| n.borrow_mut().push((rt.as_str(), source, agent.map(str::to_string))));
    static SEEN: OnceLock<Mutex<HashSet<(&'static str, DeprecatedRuntimeSource)>>> =
        OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    match deprecated_runtime_first_notice(&mut seen, rt, source, agent) {
        Some(msg) => {
            let dep = rt.deprecation();
            tracing::warn!(
                agent = agent.unwrap_or(""),
                value = rt.as_str(),
                setting = source.setting(),
                replacement = dep.map(|d| d.replacement).unwrap_or(""),
                remove_in = dep.map(|d| d.remove_in).unwrap_or(""),
                "{msg}"
            );
            true
        }
        None => false,
    }
}

/// Historical 300-second baseline. Since v1.84.0 it is only the AUTHORIZATION
/// baseline (who may widen an unset limit, a non-Admin's task ceiling); the
/// run limit default is [`DEFAULT_RUN_TIMEOUT_SECS`].
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// RM-181 (v1.84.0): the one run-limit default for every engine — Claude,
/// Codex, Grok, generic CLIs and OpenAI-compatible requests, including
/// agent-less calls — when neither `[runtime] timeout_secs` nor the global
/// `[runtime] default_timeout_secs` is set. (Non-Claude engines used 300.)
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 1800;
/// Bounds of `[runtime] timeout_secs` and of a task's own `timeout_secs`.
pub const MIN_TIMEOUT_SECS: u64 = 30;
pub const MAX_TIMEOUT_SECS: u64 = 3600;

/// Validated global timeout policy. Invalid configuration restores the old hard defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    pub default_secs: Option<u64>,
    pub max_secs: u64,
}

impl Default for TimeoutPolicy {
    fn default() -> Self { Self { default_secs: None, max_secs: MAX_TIMEOUT_SECS } }
}

impl TimeoutPolicy {
    pub(crate) fn from_table(table: &toml::Table) -> Result<Self, String> {
        let Some(runtime) = table.get("runtime") else { return Ok(Self::default()) };
        let runtime = runtime.as_table().ok_or("[runtime] must be a table")?;
        let integer = |key: &str, min: u64, max: u64| -> Result<Option<u64>, String> {
            runtime.get(key).map(|v| v.as_integer().and_then(|n| u64::try_from(n).ok())
                .filter(|n| (min..=max).contains(n))
                .ok_or_else(|| format!("[runtime] {key} must be an integer between {min} and {max}"))).transpose()
        };
        let max_secs = integer("max_timeout_secs", 60, 14_400)?.unwrap_or(MAX_TIMEOUT_SECS);
        let default_secs = integer("default_timeout_secs", MIN_TIMEOUT_SECS, max_secs)?;
        Ok(Self { default_secs, max_secs })
    }

    pub fn load(home: &Path) -> Self {
        let path = home.join("config.toml");
        let result = match std::fs::read_to_string(&path) {
            Ok(text) => text.parse::<toml::Table>().map_err(|e| e.message().to_string())
                .and_then(|table| Self::from_table(&table)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => Err(e.to_string()),
        };
        result.unwrap_or_else(|error| {
            tracing::warn!(%error, "invalid runtime timeout policy; using previous hard defaults");
            Self::default()
        })
    }
}

fn timeout_policy(agent_dir: &Path) -> TimeoutPolicy {
    let home = duduclaw_core::preset::agent_home_dir(agent_dir)
        .or_else(|| crate::agent_hook_installer::home_of_agent_dir(agent_dir))
        .unwrap_or_else(duduclaw_core::platform::duduclaw_home);
    TimeoutPolicy::load(&home)
}

/// The run limit an unset `[runtime] timeout_secs` resolves to. Since
/// v1.84.0 it is the same for every provider (the parameter is kept so
/// callers keep naming the provider the limit applies to).
pub(crate) fn runtime_default_timeout(_provider: Option<&str>, policy: TimeoutPolicy) -> u64 {
    policy.default_secs.unwrap_or(DEFAULT_RUN_TIMEOUT_SECS).clamp(MIN_TIMEOUT_SECS, policy.max_secs)
}

/// Historical authorization baseline; global defaults may narrow the unset baseline.
/// Keep explicit values unclamped here so a global cap cannot hide a stored increase.
pub(crate) fn timeout_authorization_baseline(raw: Option<i64>, policy: TimeoutPolicy) -> u64 {
    raw.map(|n| n.max(MIN_TIMEOUT_SECS as i64) as u64)
        .unwrap_or(policy.default_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).min(DEFAULT_TIMEOUT_SECS))
}

/// Claude's run limit (default [`DEFAULT_RUN_TIMEOUT_SECS`], as every engine),
/// including utility calls.
pub fn claude_timeout_secs(home: &Path, agent_dir: Option<&Path>) -> u64 {
    let policy = TimeoutPolicy::load(home);
    resolve_timeout(agent_dir, policy, runtime_default_timeout(Some("claude"), policy), true)
}

fn resolve_timeout(agent_dir: Option<&Path>, policy: TimeoutPolicy, fallback: u64, task: bool) -> u64 {
    let scoped = if task { agent_dir.and_then(|dir| dir.file_name()).and_then(|n| n.to_str())
        .and_then(scoped_task_timeout_secs) } else { None };
    let requested = scoped.or_else(|| agent_dir.and_then(raw_agent_timeout_secs).map(|v| {
        if v < MIN_TIMEOUT_SECS as i64 {
            tracing::warn!(value = v, "runtime timeout below minimum; clamping");
        }
        v.max(MIN_TIMEOUT_SECS as i64) as u64
    })).unwrap_or(policy.default_secs.unwrap_or(fallback));
    let effective = requested.clamp(MIN_TIMEOUT_SECS, policy.max_secs);
    if requested != effective {
        tracing::warn!(requested, effective, "runtime timeout exceeds global ceiling; clamping");
    }
    effective
}

tokio::task_local! {
    /// `(agent id, limit)` of the task this call works on
    /// (`tasks.timeout_secs`), scoped by the dispatcher around a goal round or
    /// task wake-up for the task's assignee. It applies only to calls made as
    /// that employee; `None`, no scope, or another employee ⇒ the employee's
    /// own `[runtime] timeout_secs`. Who may set a value above the employee's
    /// own limit is decided when the task is created (only an Admin); this
    /// only applies the stored value.
    pub static TASK_TIMEOUT_SECS: Option<(String, u64)>;
}

/// The scoped task limit when this call is made as `agent_id`.
pub fn scoped_task_timeout_secs(agent_id: &str) -> Option<u64> {
    TASK_TIMEOUT_SECS
        .try_with(|scoped| {
            scoped
                .as_ref()
                .filter(|(id, _)| id == agent_id)
                .map(|(_, secs)| (*secs).max(MIN_TIMEOUT_SECS))
        })
        .ok()
        .flatten()
}

/// The limit for this CLI run / OpenAI-compatible request: the current
/// task's own limit when one is scoped for this employee, otherwise the
/// employee's. An agent-less call never takes a task's limit.
pub fn call_timeout_secs(agent_dir: Option<&Path>) -> u64 {
    let Some(dir) = agent_dir else { return resolve_timeout(None, TimeoutPolicy::load(&duduclaw_core::platform::duduclaw_home()), DEFAULT_RUN_TIMEOUT_SECS, false) };
    resolve_timeout(Some(dir), timeout_policy(dir), DEFAULT_RUN_TIMEOUT_SECS, true)
}

/// The highest per-task limit a non-Admin (a dashboard Operator, an AI
/// employee) may set for work assigned to `agent_id`: that employee's own
/// limit — for an employee with the task sandbox on, the shorter of that and
/// its container timeout (a task's limit replaces both). An invalid id gives
/// the default; a sandboxed employee whose container timeout cannot be read
/// gives the minimum.
pub fn employee_timeout_ceiling(home_dir: &Path, agent_id: &str) -> u64 {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return DEFAULT_TIMEOUT_SECS;
    }
    let dir = home_dir.join("agents").join(agent_id);
    if !dir.join("agent.toml").is_file() { return DEFAULT_TIMEOUT_SECS; }
    // Authorization retains the pre-policy employee limit, independently of
    // Claude's execution default and any larger global default/ceiling.
    let legacy = raw_agent_timeout_secs(&dir)
        .map(|n| n.clamp(MIN_TIMEOUT_SECS as i64, MAX_TIMEOUT_SECS as i64) as u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let runtime = legacy.min(effective_agent_timeout_secs(&dir));
    let container = std::fs::read_to_string(dir.join("agent.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|doc| doc.get("container").cloned());
    let sandboxed = container
        .as_ref()
        .and_then(|c| c.get("sandbox_enabled"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    if !sandboxed {
        return runtime;
    }
    let sandbox_secs = container
        .as_ref()
        .and_then(|c| c.get("timeout_ms"))
        .and_then(toml::Value::as_integer)
        .and_then(|ms| u64::try_from(ms).ok())
        .map(|ms| (ms / 1000).clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS))
        .unwrap_or(MIN_TIMEOUT_SECS);
    runtime.min(sandbox_secs)
}

/// Only an absent or empty stored owner may use the global utility path.
/// Reject filesystem aliases and require the employee's declared identity.
pub(crate) fn session_agent_dir(home_dir: &Path, agent_id: Option<&str>) -> Result<Option<std::path::PathBuf>, String> {
    let Some(id) = agent_id.filter(|id| !id.is_empty()) else { return Ok(None) };
    let invalid = || format!("invalid session owner {id:?}; utility call refused");
    if !duduclaw_core::is_valid_agent_id(id) || duduclaw_core::is_reserved_agent_id(id)
        || duduclaw_core::is_internal_agent_dir(id) {
        return Err(invalid());
    }
    let agents = home_dir.join("agents").canonicalize().map_err(|_| invalid())?;
    let dir = agents.join(id);
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| invalid())?;
    if !meta.is_dir() || meta.file_type().is_symlink() { return Err(invalid()); }
    let dir = dir.canonicalize().map_err(|_| invalid())?;
    if dir.parent() != Some(agents.as_path()) || dir.file_name().and_then(|n| n.to_str()) != Some(id) {
        return Err(invalid());
    }
    let config = dir.join("agent.toml");
    let meta = std::fs::symlink_metadata(&config).map_err(|_| invalid())?;
    if !meta.is_file() || meta.file_type().is_symlink() { return Err(invalid()); }
    // Use the shared typed identity schema, without invoking the registry's
    // load_agent side effects (preset artifacts, skills and optional files).
    let text = std::fs::read_to_string(config).map_err(|_| invalid())?;
    let sections: AgentTomlSections = toml::from_str(&text).map_err(|_| invalid())?;
    if sections.agent.and_then(|agent| agent.name).as_deref() != Some(id) { return Err(invalid()); }
    Ok(Some(dir))
}

/// This authority field is per-agent only. Ignore cached preset artifacts,
/// including artifacts written before the preset sanitizer excluded it.
fn raw_agent_timeout_secs(agent_dir: &Path) -> Option<i64> {
    std::fs::read_to_string(agent_dir.join("agent.toml")).ok()
        .and_then(|text| agent_toml::parse(&text).runtime.timeout_secs)
}

/// Raw `agent.toml [runtime] utility_provider` string, if written.
fn raw_agent_utility_provider(agent_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    let table: toml::Table = text.parse().ok()?;
    table.get("runtime")?.get("utility_provider")?.as_str().map(str::to_string)
}

/// Resolve the typed per-agent timeout, tolerating old or malformed settings.
pub fn agent_timeout_secs(agent_dir: &Path) -> u64 {
    resolve_timeout(Some(agent_dir), timeout_policy(agent_dir), DEFAULT_RUN_TIMEOUT_SECS, false)
}

/// Largest employee setting an owner may set without widening authority.
/// An unset setting keeps the historical 300-second authorization baseline.
pub fn agent_timeout_authorization_ceiling(agent_dir: &Path) -> u64 {
    let policy = timeout_policy(agent_dir);
    timeout_authorization_baseline(raw_agent_timeout_secs(agent_dir), policy).min(policy.max_secs)
}

pub fn effective_agent_timeout_secs(agent_dir: &Path) -> u64 {
    let policy = timeout_policy(agent_dir);
    let settings = load_runtime_settings(agent_dir);
    let fallback = runtime_default_timeout(Some(settings.provider.as_str()), policy);
    resolve_timeout(Some(agent_dir), policy, fallback, false)
}

/// Load `[runtime] provider` / `[runtime] fallback` / `[model] utility` from the
/// agent's `agent.toml` in a single read. Missing/malformed file ⇒ defaults
/// (Claude / no fallback / [`DEFAULT_UTILITY_MODEL`]).
pub fn load_runtime_settings(agent_dir: &Path) -> RuntimeSettings {
    settings_from_sections(agent_dir, read_sections(agent_dir))
}

/// Why [`load_runtime_settings_strict`] could not produce settings. Closed set;
/// the diagnostic detail goes to the gateway log, never to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSettingsError {
    /// No `agent.toml` (and no readable preset-resolved artifact).
    Missing,
    /// The file exists but could not be read (permissions, invalid UTF-8, …).
    Unreadable,
    /// Not valid TOML, or a runtime/model selector has the wrong type.
    Malformed,
    /// `[runtime] provider` names no known runtime.
    UnknownProvider,
    /// No usable preferred model or catalog default for a runtime that requires one.
    MissingModel,
}

impl RuntimeSettingsError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Malformed => "malformed",
            Self::UnknownProvider => "unknown_provider",
            Self::MissingModel => "missing_model",
        }
    }
}

/// Default `[model] preferred` for `provider` when the file does not set one.
///
/// Claude uses [`duduclaw_core::types::DEFAULT_PREFERRED_MODEL`]; other engines
/// use their first catalog `fallback_models` entry. Engines without a catalog
/// default return `None`; Kiro delegates selection to its CLI settings, while
/// OpenAI-compatible requests require an explicitly configured model.
pub fn default_preferred_model(provider: RuntimeType) -> Option<String> {
    if provider == RuntimeType::Claude {
        return Some(duduclaw_core::types::DEFAULT_PREFERRED_MODEL.to_string());
    }
    provider.spec().fallback_models.first().map(|(id, _label)| (*id).to_string())
}

impl RuntimeSettings {
    /// `[model] preferred` from this same parse, or the provider's own default.
    /// A runtime without either reports an error, never another engine's model.
    pub fn preferred_or_default(&self) -> Result<String, RuntimeSettingsError> {
        self.preferred_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
            .or_else(|| default_preferred_model(self.provider))
            // Kiro chooses its model through CLI settings, with no per-call model selector.
            .or_else(|| (self.provider == RuntimeType::Kiro).then(String::new))
            .ok_or(RuntimeSettingsError::MissingModel)
    }
}

/// [`load_runtime_settings`] for decisions that must not run on defaults: the
/// same single read of the same file (the preset-resolved artifact when there
/// is one, else `agent.toml`) and the same typed parse, but a missing,
/// unreadable or malformed file, a wrongly typed selector, or an unknown
/// `[runtime] provider` is reported instead of becoming Claude defaults. A
/// missing preferred model also fails if the runtime has no catalog default.
/// Callers take provider AND model from the one returned value.
pub fn load_runtime_settings_strict(agent_dir: &Path) -> Result<RuntimeSettings, RuntimeSettingsError> {
    // Select the effective artifact before reading. An existing but unreadable
    // resolved profile must never silently fall back to another snapshot.
    let mut path = agent_dir.join("agent.toml");
    if let Some(home) = duduclaw_core::preset::agent_home_dir(agent_dir) {
        if let Some(id) = agent_dir.file_name().and_then(|id| id.to_str()) {
            let resolved = duduclaw_core::preset::agent_resolved_path(&home, id);
            match std::fs::symlink_metadata(&resolved) {
                Ok(_) => path = resolved,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(path = %resolved.display(), %error, "cannot inspect resolved employee settings");
                    return Err(RuntimeSettingsError::Unreadable);
                }
            }
        }
    }
    let log = |kind: RuntimeSettingsError, detail: &str| {
        tracing::warn!(
            path = %duduclaw_core::truncate_bytes(&path.display().to_string(), 200),
            kind = kind.as_str(),
            detail = %duduclaw_core::truncate_bytes(detail, 300),
            "agent runtime settings could not be read; turn refused"
        );
        kind
    };
    let text = std::fs::read_to_string(&path).map_err(|error| {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            RuntimeSettingsError::Missing
        } else {
            RuntimeSettingsError::Unreadable
        };
        log(kind, &error.to_string())
    })?;
    let doc = text
        .parse::<toml::Value>()
        .map_err(|error| log(RuntimeSettingsError::Malformed, error.message()))?;
    check_runtime_field_types(&doc, &path).map_err(|error| log(RuntimeSettingsError::Malformed, &error))?;
    if let Some(model) = doc.get("model") {
        let model = model
            .as_table()
            .ok_or_else(|| log(RuntimeSettingsError::Malformed, "[model] is not a table"))?;
        if model.get("preferred").is_some_and(|value| !value.is_str()) {
            return Err(log(RuntimeSettingsError::Malformed, "wrong type for [model] preferred"));
        }
    }
    let sections = doc
        .try_into::<AgentTomlSections>()
        .map_err(|error| log(RuntimeSettingsError::Malformed, error.message()))?;
    if let Some(value) = sections.runtime.provider.as_deref() {
        if RuntimeType::parse(value).is_none() {
            return Err(log(
                RuntimeSettingsError::UnknownProvider,
                &format!("[runtime] provider {:?}", duduclaw_core::truncate_chars(value, 40)),
            ));
        }
    }
    let settings = settings_from_sections(agent_dir, sections);
    settings.preferred_or_default()
        .map_err(|error| log(error, "[model] preferred is required for this runtime"))?;
    Ok(settings)
}

fn settings_from_sections(agent_dir: &Path, s: AgentTomlSections) -> RuntimeSettings {
    let provider = s
        .runtime
        .provider
        .as_deref()
        .map(|v| {
            parse_provider_or_default(
                agent_dir,
                "[runtime] provider",
                v,
                DeprecatedRuntimeSource::AgentProvider,
            )
        })
        .unwrap_or_default();
    // A malformed FALLBACK is dropped, not defaulted: "no fallback" is a valid
    // state, so there is nothing to lose by refusing it — unlike `provider`,
    // where refusing would leave the agent with no brain at all.
    let fallback = s.runtime.fallback.as_deref().and_then(|v| {
        let parsed = RuntimeType::parse(v);
        if parsed.is_none() {
            tracing::error!(
                agent_dir = %agent_dir.display(),
                value = %v,
                valid = %RuntimeType::valid_values(),
                "[runtime] fallback names an unknown runtime — ignoring it (no fallback)"
            );
        }
        if let Some(rt) = parsed {
            warn_once_if_deprecated_runtime_for(
                rt,
                DeprecatedRuntimeSource::AgentFallback,
                agent_name(agent_dir).as_deref(),
            );
        }
        parsed
    });
    let utility_model = s
        .model
        .utility
        .as_ref()
        .filter(|model| !model.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| {
            if provider == RuntimeType::Claude {
                DEFAULT_UTILITY_MODEL.to_string()
            } else {
                s.model.preferred.clone().filter(|model| !model.trim().is_empty()).unwrap_or_default()
            }
        });

    // Provider↔model sanity check: `preferred = "gpt-5"` with the default
    // Claude provider used to route silently into the Claude CLI and fail at
    // the Anthropic API. Warn once per (agent, model) so the misconfiguration
    // is visible without spamming the hot reply path.
    if let Some(preferred) = s.model.preferred.as_deref() {
        if !model_matches_provider(preferred, provider) {
            warn_mismatch_once(agent_dir, "preferred", preferred, provider);
        }
    }
    if let Some(utility) = s.model.utility.as_deref().filter(|v| !v.trim().is_empty()) {
        if !model_matches_provider(utility, provider) {
            warn_mismatch_once(agent_dir, "utility", utility, provider);
        }
    }

    RuntimeSettings {
        preferred_model: s.model.preferred,
        provider,
        fallback,
        utility_model,
    }
}

/// Read the agent's `[runtime]` section as a JSON object for `agents.inspect`.
///
/// Emits ONLY keys actually present in `agent.toml` (`provider`, `fallback`)
/// so the dashboard can distinguish "unset" from an explicitly written value.
/// A missing/malformed file or absent `[runtime]` table yields an empty
/// object.
pub fn read_runtime_json(agent_dir: &Path) -> serde_json::Value {
    // Every `RuntimeSection` field is `Option` precisely so this function can
    // still tell "unset" from an explicitly written value. Keys are emitted
    // individually (not via `serde_json::to_value`) to keep that contract
    // explicit and to keep fields this form does not edit out of the payload.
    let rt = read_sections(agent_dir).runtime;
    let mut obj = serde_json::Map::new();
    if let Some(s) = rt.provider {
        obj.insert("provider".into(), serde_json::Value::String(s));
    }
    if let Some(s) = rt.fallback {
        obj.insert("fallback".into(), serde_json::Value::String(s));
    }
    // RM-205: the employee's own value as written (per-agent authority field).
    if let Some(s) = raw_agent_utility_provider(agent_dir) {
        obj.insert("utility_provider".into(), serde_json::Value::String(s));
    }
    if let Some(secs) = raw_agent_timeout_secs(agent_dir) {
        obj.insert("timeout_secs".into(), serde_json::Value::from(secs));
    }
    if let Some(secs) = duduclaw_core::agent_toml::load(agent_dir).runtime.idle_timeout_secs {
        obj.insert("idle_timeout_secs".into(), serde_json::Value::from(secs));
    }
    if let Some(turns) = duduclaw_core::turn_limits::raw_agent_max_turns(agent_dir) {
        obj.insert("max_turns".into(), serde_json::Value::from(turns));
    }
    serde_json::Value::Object(obj)
}

/// Conservative provider↔model compatibility check by model-id naming family.
/// Only flags *confident* mismatches; unknown families always pass, and so do
/// runtimes that declare **no** family of their own (`openai_compat`, and the
/// multi-vendor shells Copilot / Cursor / OpenCode) because those legitimately
/// proxy arbitrary models.
///
/// WP-B: the families come from
/// `duduclaw_core::runtime_catalog`'s `model_prefixes` instead of a hand-written
/// `is_claude` / `is_openai` / `is_gemini` / `is_grok` ladder that had to grow a
/// term — in two places — for every new backend.
pub fn model_matches_provider(model: &str, provider: RuntimeType) -> bool {
    // Gemini remains an API model family, but no supported CLI owns it.
    // Do not let removal turn a known foreign model into an unknown match.
    if model_family(model) == Some("gemini") {
        return provider.spec().model_prefixes.is_empty();
    }
    // Which runtime does this model id CONFIDENTLY belong to? `None` ⇒ unknown
    // family ⇒ never a mismatch.
    let Some(owner) = duduclaw_core::runtime_catalog::runtime_for_model(model) else {
        return true;
    };
    let spec = provider.spec();
    // A runtime with no declared family serves anything.
    if spec.model_prefixes.is_empty() {
        return true;
    }
    // Same runtime, or two catalog entries declaring the same model prefix.
    spec.id == owner.id
        || spec
            .model_prefixes
            .iter()
            .any(|p| owner.model_prefixes.contains(p))
}

/// The model-id family a model id **confidently** belongs to, expressed as the
/// owning runtime catalog id, or `"gemini"` for the API-only Google family.
///
/// `None` for an unrecognised naming prefix; no vendor is guessed.
pub fn model_family(model: &str) -> Option<&'static str> {
    let bare = model.trim().rsplit('/').next().unwrap_or(model);
    if bare.to_ascii_lowercase().starts_with("gemini-") {
        return Some("gemini");
    }
    duduclaw_core::runtime_catalog::runtime_for_model(model).map(|s| s.id)
}

/// Do two model ids come from the same vendor family?
///
/// P0/WP-B (verifier decorrelation, arXiv:2607.13918): an acceptance judge
/// drawn from the worker's own family inherits the worker's blind spots, so
/// the goal-loop settle path warns (never rejects) when this returns `true`.
///
/// Deliberately conservative in the *quiet* direction: two ids are "same
/// family" when they are literally the same id, or when both resolve to the
/// same catalog family. An unrecognised id paired with a known one is never
/// reported — a warning an operator cannot act on is worse than silence.
pub fn same_model_family(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    match (model_family(a), model_family(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Best-effort model-family → runtime mapping for the dashboard save-time
/// auto-align (`agents.update`): a confident family match returns that
/// family's CLI runtime when its binary is installed, else `openai_compat`
/// (API mode serves any model) so the aligned config is actually runnable.
/// Claude always maps to Claude (its API path is the Anthropic-native client,
/// not chat/completions). Unknown families return `None` — never guess.
///
/// WP-B: driven by the catalog's `model_prefixes` + the catalog-driven binary
/// probe, so a new runtime's models auto-align the moment its entry lands.
/// Test-only: exact model ids for which [`infer_provider_for_model`] treats
/// the family CLI as installed (handler tests must not depend on which CLIs
/// the machine running them has).
#[cfg(test)]
pub(crate) static FAMILY_CLI_INSTALLED_FOR_MODEL: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn infer_provider_for_model(model: &str) -> Option<RuntimeType> {
    let spec = duduclaw_core::runtime_catalog::runtime_for_model(model)?;
    let family = RuntimeType::from_id(spec.id)?;
    if family == RuntimeType::Claude {
        return Some(RuntimeType::Claude);
    }
    // The family's own CLI when it is installed; otherwise the API-mode
    // backend, which serves any model, so the aligned config is runnable.
    #[cfg(test)]
    if let Some(installed) = testing::availability(model) {
        return Some(if installed { family } else { RuntimeType::OpenAiCompat });
    }
    let installed = duduclaw_core::which_runtime(spec.id).is_some();
    #[cfg(test)]
    let installed = installed
        || FAMILY_CLI_INSTALLED_FOR_MODEL.lock().is_ok_and(|models| models.iter().any(|m| m == model));
    Some(if installed {
        family
    } else {
        RuntimeType::OpenAiCompat
    })
}

fn warn_mismatch_once(agent_dir: &Path, field: &str, model: &str, provider: RuntimeType) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let key = format!("{}|{model}", agent_dir.display());
    let warned = WARNED.get_or_init(Default::default);
    if let Ok(mut set) = warned.lock() {
        if set.insert(key) {
            tracing::warn!(
                agent_dir = %agent_dir.display(),
                model,
                provider = provider.as_str(),
                field,
                "[model] field does not match [runtime] provider — requests will \
                 likely fail at the provider API; set [runtime] provider to the \
                 runtime that serves this model"
            );
        }
    }
}

/// Resolve the agent's "utility" model (cheap internal tasks: compression,
/// key-fact extraction, GVU evolution, summarization, skill synthesis).
///
/// Single-field convenience over [`load_runtime_settings`]; prefer the latter
/// when you also need the provider.
pub fn agent_utility_model(agent_dir: &Path) -> String {
    load_runtime_settings(agent_dir).utility_model
}

/// Resolve the agent's runtime provider (RFC-25 Phase 1).
///
/// Single-field convenience over [`load_runtime_settings`].
pub fn agent_runtime_provider(agent_dir: &Path) -> RuntimeType {
    load_runtime_settings(agent_dir).provider
}

/// Runtime-switch model candidates: the explicit single fallback takes priority.
/// The runtime resolver still rejects unknown or incompatible model families.
pub fn agent_model_fallbacks(agent_dir: &Path) -> Vec<String> {
    let model = read_sections(agent_dir).model;
    model.fallback.into_iter().chain(model.fallbacks)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The single configured fallback, retained separately for source attribution.
pub(crate) fn agent_fallback_model(agent_dir: &Path) -> Option<String> {
    read_sections(agent_dir).model.fallback
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Direct API retains its existing plural-only cross-provider chain.
pub fn agent_direct_api_model_fallbacks(agent_dir: &Path) -> Vec<String> {
    read_sections(agent_dir).model.fallbacks.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Diagnostic wording for a pre-call alignment refusal; never authorizes a retry.
pub(crate) fn alignment_refusal_with_fallback(error: String, fallback: Option<RuntimeType>) -> String {
    if fallback.is_some() {
        format!("{error} A fallback runtime is configured, but it was not used because the model setting itself is incorrect.")
    } else { error }
}

/// Resolve the agent's runtime fallback provider (`[runtime] fallback`).
/// `None` when unset. Single-field convenience over [`load_runtime_settings`].
pub fn agent_runtime_fallback(agent_dir: &Path) -> Option<RuntimeType> {
    load_runtime_settings(agent_dir).fallback
}

/// Whether an agent routes through a non-Claude runtime (RFC-25 L8).
///
/// Convenience for callers that have only an `agent_dir` and don't otherwise need
/// the parsed settings. Callers that already hold a [`RuntimeSettings`] (the hot
/// reply/delegation paths, which load it once for routing + the choke-point)
/// should use [`RuntimeSettings::non_claude_provider`] instead to avoid a second
/// `agent.toml` read.
pub fn agent_uses_non_claude(agent_dir: &Path) -> Option<RuntimeType> {
    load_runtime_settings(agent_dir).non_claude_provider()
}

/// Whether `[memory] decision_continuity` is enabled for this agent (RFC-24).
///
/// Opt-in, default `false`. A missing/malformed `agent.toml`, a missing
/// `[memory]` table, a missing key, or a non-bool value all resolve to `false`
/// (fail-safe — the feature stays off unless explicitly turned on).
pub fn decision_continuity_enabled(agent_dir: &Path) -> bool {
    read_sections(agent_dir)
        .memory
        .decision_continuity
        .unwrap_or(false)
}

/// TTL in days after which an unanswered open decision is auto-expired (RFC-24
/// §P3.2). Reads `[memory] decision_ttl_days`; defaults to 7. Non-positive or
/// malformed values fall back to the default (7) — TTL is always enforced so the
/// ledger can't grow unbounded.
pub fn decision_ttl_days(agent_dir: &Path) -> i64 {
    const DEFAULT_TTL_DAYS: i64 = 7;
    // `> 0` stays a reader-side filter: a written `0` has always meant "use
    // the default", never "never expire" — TTL is unconditional so the ledger
    // cannot grow unbounded.
    read_sections(agent_dir)
        .memory
        .decision_ttl_days
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TTL_DAYS)
}

// ── O1: confidence-aware delegation routing config ──────────────────
//
// Opt-in, default OFF. Global switch in `<home>/config.toml`:
//
//   [delegation]
//   confidence_routing = true
//
// Per-agent override in `agent.toml` (agent wins over global, both ways):
//
//   [model]
//   delegation_routing = true   # or false
//   standard = "..."            # optional mid tier; unset ⇒ preferred
//
// All readers are fail-safe: a missing/malformed file or wrong-typed key
// resolves to the default (routing off / no standard model), never an error.

/// Global `config.toml [delegation] confidence_routing` flag (O1).
/// Default `false` — absent/malformed file or non-bool value keeps routing off.
pub fn global_delegation_routing(home_dir: &Path) -> bool {
    read_global_config(home_dir)
        .as_ref()
        .and_then(|v| v.get("delegation"))
        .and_then(|d| d.get("confidence_routing"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
}

/// Per-agent `agent.toml [model] delegation_routing` override (O1).
/// `None` when unset/malformed — the caller falls back to the global flag.
pub fn agent_delegation_routing(agent_dir: &Path) -> Option<bool> {
    // `Option` all the way down: the agent overrides the global flag in BOTH
    // directions, so "unset" and "explicitly false" must stay distinguishable.
    read_sections(agent_dir).model.delegation_routing
}

/// Optional mid-tier model from `agent.toml [model] standard` (O1).
/// `None` (or a blank value) means the config does not distinguish a mid
/// tier — the Standard tier then resolves to the agent's preferred model.
pub fn agent_standard_model(agent_dir: &Path) -> Option<String> {
    read_sections(agent_dir)
        .model
        .standard
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Effective delegation-routing switch (O1): the per-agent override wins in
/// both directions; when the agent is silent, the global flag decides.
/// Fully unconfigured ⇒ `false` (byte-identical legacy dispatch behavior).
pub fn delegation_routing_enabled(home_dir: &Path, agent_dir: &Path) -> bool {
    match agent_delegation_routing(agent_dir) {
        Some(explicit) => explicit,
        None => global_delegation_routing(home_dir),
    }
}

// ── Global utility config (RFC-25 N2) ───────────────────────────────
//
// Background utility tasks (session summarizer, wiki ingest, forced reflection,
// sub-agent prediction, skill synthesis) run cheap internal prompts. The ones
// that have an `agent_dir` use that agent's `[runtime] provider` / `[model]
// utility`. The ones that are genuinely agent-less (only a `home_dir`) read the
// operator-level default from `<home>/config.toml [runtime]`:
//
//   [runtime]
//   utility_provider = "claude"   # claude | openai_compat
//   utility_model    = "claude-haiku-4-5"
//
// An absent/malformed file keeps Claude / DEFAULT_UTILITY_MODEL. With another
// provider, a missing/blank global model lets the backend choose its default.

fn read_global_config(home_dir: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    text.parse::<toml::Value>().ok()
}

/// Global utility provider from `config.toml [runtime] utility_provider`.
/// Falls back to [`RuntimeType::Claude`] when the file/key is missing or unrecognised.
pub fn global_utility_provider(home_dir: &Path) -> RuntimeType {
    read_global_config(home_dir)
        .as_ref()
        .and_then(|v| v.get("runtime"))
        .and_then(|r| r.get("utility_provider"))
        .and_then(|s| s.as_str())
        .map(|v| {
            parse_provider_or_default(
                home_dir,
                "[runtime] utility_provider",
                v,
                DeprecatedRuntimeSource::UtilityProvider,
            )
        })
        .unwrap_or_default()
}

/// Global utility model from `config.toml [runtime] utility_model`.
/// Missing/blank models use [`DEFAULT_UTILITY_MODEL`] for Claude, otherwise the
/// backend's default (empty string; OpenAI-compatible endpoints require a model).
pub fn global_utility_model(home_dir: &Path) -> String {
    global_utility_spec(home_dir).model
}

/// Global utility provider + model in a single `config.toml` read.
/// Used by [`resolve_utility`] for the agent-less path so it doesn't parse the
/// file twice (once per field).
pub(crate) fn global_utility_spec(home_dir: &Path) -> UtilitySpec {
    let cfg = read_global_config(home_dir);
    let runtime = cfg.as_ref().and_then(|v| v.get("runtime"));
    let provider = runtime
        .and_then(|r| r.get("utility_provider"))
        .and_then(|s| s.as_str())
        .map(|v| {
            parse_provider_or_default(
                home_dir,
                "[runtime] utility_provider",
                v,
                DeprecatedRuntimeSource::UtilityProvider,
            )
        })
        .unwrap_or_default();
    let model = runtime
        .and_then(|r| r.get("utility_model"))
        .and_then(|s| s.as_str())
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if provider == RuntimeType::Claude {
                DEFAULT_UTILITY_MODEL.to_string()
            } else {
                String::new()
            }
        });
    UtilitySpec { provider, model }
}

/// Resolved provider + model for a utility (cheap, internal) task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtilitySpec {
    pub provider: RuntimeType,
    pub model: String,
}

/// Resolve the provider + model for a utility task.
///
/// - `agent_dir` present → per-agent `[runtime] provider` + `[model] utility`.
/// - `agent_dir` absent  → global `config.toml [runtime] utility_provider` / `utility_model`.
///
/// Both layers fall back to [`RuntimeType::Claude`] / [`DEFAULT_UTILITY_MODEL`],
/// so the prior hardcoded-Claude behavior is preserved when nothing is configured.
pub fn resolve_utility(home_dir: &Path, agent_dir: Option<&Path>) -> UtilitySpec {
    match agent_dir {
        Some(dir) => {
            // Single read for both provider and utility model (L7).
            let s = load_runtime_settings(dir);
            UtilitySpec {
                provider: s.provider,
                model: s.utility_model,
            }
        }
        // Single read of config.toml for both fields (avoids parsing twice).
        None => global_utility_spec(home_dir),
    }
}

/// Utility selection is an authority gate: unreadable/malformed configured
/// files and unknown provider names cannot silently select another provider.
fn validate_utility_config(path: &Path, key: &str, optional: bool) -> Result<(), String> {
    let Some(cfg) = read_config_for_gate(path)? else {
        return if optional { Ok(()) } else { Err(config_refusal(path, "unreadable", "required config is absent")) };
    };
    if let Some(runtime) = cfg.get("runtime") {
        if let Some(value) = runtime.get(key) {
            let id = value.as_str().ok_or_else(|| wrong_config_type(path, &format!("[runtime] {key}")))?;
            if let Some(removed) = duduclaw_core::runtime_catalog::removed_runtime(id) {
                return Err(removed.refusal(&format!("{} [runtime] {key}", config_file_name(path))));
            }
            if RuntimeType::parse(id).is_none() {
                return Err(format!("{}: unknown utility runtime", config_file_name(path)));
            }
        }
    }
    Ok(())
}

/// Only these runtimes implement the mandatory utility execution profile.
/// Claude/API are tool-free; Codex checks external MCP configuration before spawn.
/// Unknown/new CLI providers remain denied until a utility lane is verified.
pub(crate) fn utility_profile_supported(provider: RuntimeType) -> bool {
    matches!(provider, RuntimeType::Claude | RuntimeType::Codex | RuntimeType::OpenAiCompat)
}

/// Share the Codex spawn-gate refusal across selection and direct dispatch.
pub(crate) fn utility_profile_refusal(provider: RuntimeType) -> String {
    format!("utility runtime {} has no supported utility profile", provider.as_str())
}

// ── RM-205: background work follows the employee's engine ───────────────
//
// An employee whose engine has no utility profile (Grok, generic CLIs) used
// to send its background work — pinned-instruction extraction, key facts,
// summaries, titles, classifiers — to the GLOBAL utility provider, which
// defaults to Claude: the user's messages reached another vendor than the
// one configured for the employee. The rule now:
//   1. the employee's engine supports utility calls → use it (unchanged);
//   2. otherwise another service only when an administrator chose one: the
//      employee's `agent.toml [runtime] utility_provider`, or the global
//      `config.toml [runtime] utility_provider` when that KEY IS PRESENT
//      (the built-in Claude default does not count);
//   3. otherwise the call is unavailable: a typed, clean skip.

/// Marker at the start of the error a utility call returns when no service
/// may run it ([`is_utility_unavailable`]). Callers skip cleanly: nothing is
/// written, nothing is shown, no health penalty.
pub const UTILITY_UNAVAILABLE_PREFIX: &str = "[utility_unavailable] ";

/// The typed "unavailable" result of a utility call.
pub fn utility_unavailable_error() -> String {
    format!("{UTILITY_UNAVAILABLE_PREFIX}background work skipped: this employee's engine has no utility profile and no other service is configured")
}

/// Anchored check for [`utility_unavailable_error`].
pub fn is_utility_unavailable(error: &str) -> bool {
    error.starts_with(UTILITY_UNAVAILABLE_PREFIX)
}

/// Where an employee's background work runs, for the dashboard and doctor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtilityRouteSource {
    EmployeeEngine,
    EmployeeSetting,
    GlobalSetting,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtilityRoute {
    pub provider: Option<RuntimeType>,
    pub source: UtilityRouteSource,
}

impl UtilityRoute {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider.map(|p| p.as_str()),
            "source": self.source,
        })
    }
}

/// `claude|codex|openai_compat` for messages (the utility-capable runtimes).
pub(crate) fn utility_capable_providers_display() -> String {
    RuntimeType::ALL.iter().copied().filter(|rt| utility_profile_supported(*rt))
        .map(|rt| rt.as_str()).collect::<Vec<_>>().join("|")
}

/// The employee's own `agent.toml [runtime] utility_provider` (per-agent
/// authority field: preset artifacts are ignored). An unknown, removed or
/// utility-incapable value is treated as UNSET, never as "use a default".
pub(crate) fn employee_utility_provider(agent_dir: &Path) -> Option<RuntimeType> {
    let text = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let raw = table.get("runtime")?.get("utility_provider")?;
    let parsed = raw.as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(RuntimeType::from_id)
        .filter(|rt| utility_profile_supported(*rt));
    if parsed.is_none() {
        static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>> = std::sync::OnceLock::new();
        if WARNED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(agent_dir.to_path_buf()) {
            tracing::warn!(agent_dir = %agent_dir.display(),
                "[runtime] utility_provider is not a runtime with a utility profile — ignored (treated as unset)");
        }
    }
    parsed
}

/// The global `config.toml [runtime] utility_provider` ONLY when the key is
/// present (an explicit administrator choice). `None` when absent;
/// `Some(None)` when present but unusable (unknown, removed or without a
/// utility profile), which is treated as unset.
pub(crate) fn explicit_global_utility_provider(home_dir: &Path) -> Option<Option<RuntimeType>> {
    let cfg = read_global_config(home_dir)?;
    let raw = cfg.get("runtime")?.get("utility_provider")?;
    Some(raw.as_str()
        .and_then(RuntimeType::parse)
        .filter(|rt| utility_profile_supported(*rt)))
}

/// Which service runs `agent_dir`'s background work (no call is made).
pub fn utility_route(home_dir: &Path, agent_dir: &Path) -> UtilityRoute {
    let engine = load_runtime_settings(agent_dir).provider;
    if utility_profile_supported(engine) {
        return UtilityRoute { provider: Some(engine), source: UtilityRouteSource::EmployeeEngine };
    }
    if let Some(provider) = employee_utility_provider(agent_dir) {
        return UtilityRoute { provider: Some(provider), source: UtilityRouteSource::EmployeeSetting };
    }
    match explicit_global_utility_provider(home_dir) {
        Some(Some(provider)) => UtilityRoute { provider: Some(provider), source: UtilityRouteSource::GlobalSetting },
        _ => UtilityRoute { provider: None, source: UtilityRouteSource::Unavailable },
    }
}

/// Utility model for an employee-selected utility service: the employee's
/// `[model] utility` when it belongs to that service, else the global
/// utility model when the global setting names the same service, else the
/// service default.
fn employee_utility_model(home_dir: &Path, agent_dir: &Path, provider: RuntimeType) -> String {
    let sections = read_sections(agent_dir);
    if let Some(model) = sections.model.utility.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        let familyless = provider.spec().model_prefixes.is_empty();
        if model_matches_provider(model, provider)
            && (familyless || model_family(model) == Some(provider.spec().id)) {
            return model.to_string();
        }
    }
    let global = global_utility_spec(home_dir);
    if global.provider == provider && !global.model.trim().is_empty() {
        return global.model;
    }
    if provider == RuntimeType::Claude { DEFAULT_UTILITY_MODEL.to_string() } else { String::new() }
}

/// One info line per employee per process about where its background work
/// goes when its own engine cannot run it.
fn note_utility_route_once(agent_dir: &Path, engine: RuntimeType, route: UtilityRoute) {
    static NOTED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>> = std::sync::OnceLock::new();
    if NOTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(agent_dir.to_path_buf()) {
        tracing::info!(agent = %agent_dir.display(), runtime = engine.as_str(),
            utility_runtime = route.provider.map(|p| p.as_str()).unwrap_or("none"),
            source = ?route.source,
            "employee runtime has no utility profile; background work routed by explicit setting or skipped");
    }
}

/// System doctor line naming the employees whose background work is skipped.
pub(crate) fn utility_skipped_doctor_line(home_dir: &Path, employees: &[(String, std::path::PathBuf)]) -> Option<String> {
    let skipped: Vec<&str> = employees.iter()
        .filter(|(_, dir)| utility_route(home_dir, dir).source == UtilityRouteSource::Unavailable)
        .map(|(name, _)| name.as_str())
        .collect();
    (!skipped.is_empty()).then(|| format!(
        "以下 AI 員工的執行引擎不支援背景整理工作（重點事實、摘要、標題），目前已略過：{}。可在員工設定指定要改用的服務。",
        skipped.join("、")
    ))
}

/// Resolve execution, keeping explicit judge hints fail-closed. An
/// employee's unsupported runtime uses another service only when an
/// administrator configured one (RM-205); otherwise the call is unavailable.
pub(crate) fn resolve_utility_profile(
    home: &Path,
    agent: Option<&Path>,
    spec: UtilitySpec,
    explicit_hint: bool,
) -> Result<UtilitySpec, String> {
    match agent {
        Some(dir) => {
            validate_utility_config(&dir.join("agent.toml"), "provider", false)?;
            // load_runtime_settings uses this gateway-owned artifact when it
            // exists. Validate it as well as the source, without silently
            // falling back to raw config on a read or parse failure.
            if let Some(agent_home) = duduclaw_core::preset::agent_home_dir(dir) {
                let id = dir.file_name().and_then(|name| name.to_str())
                    .ok_or("invalid utility agent directory")?;
                let resolved = duduclaw_core::preset::agent_resolved_path(&agent_home, id);
                validate_utility_config(&resolved, "provider", true)?;
            }
        }
        None => validate_utility_config(&home.join("config.toml"), "utility_provider", true)?,
    }
    if utility_profile_supported(spec.provider) { return Ok(spec); }
    let Some(dir) = agent.filter(|_| !explicit_hint) else {
        return Err(utility_profile_refusal(spec.provider));
    };
    // (2a) the employee's own explicit choice.
    if let Some(provider) = employee_utility_provider(dir) {
        let route = UtilityRoute { provider: Some(provider), source: UtilityRouteSource::EmployeeSetting };
        note_utility_route_once(dir, spec.provider, route);
        return Ok(UtilitySpec { provider, model: employee_utility_model(home, dir, provider) });
    }
    // (2b) the global setting, only when the key is present. A malformed
    // config or a removed runtime still refuses (fail closed).
    validate_utility_config(&home.join("config.toml"), "utility_provider", true)?;
    if let Some(Some(provider)) = explicit_global_utility_provider(home) {
        ensure_active_utility_provider(home)?;
        let global = global_utility_spec(home);
        if global.provider == provider {
            note_utility_route_once(dir, spec.provider,
                UtilityRoute { provider: Some(provider), source: UtilityRouteSource::GlobalSetting });
            return Ok(global);
        }
    }
    // (3) nobody chose a service: a clean, typed skip. Never the built-in
    // Claude default — the user's text must not reach another vendor.
    note_utility_route_once(dir, spec.provider,
        UtilityRoute { provider: None, source: UtilityRouteSource::Unavailable });
    Err(utility_unavailable_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_agent_toml(dir: &Path, body: &str) {
        let mut f = std::fs::File::create(dir.join("agent.toml")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn utility_validation_checks_effective_and_raw_config() {
        let home = TempDir::new().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        write_agent_toml(&agent, "[runtime]\nprovider='openai_compat'\n");
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        for invalid in ["[broken", "runtime='bad'", "[runtime]\nprovider='unknown'\n", "[runtime]\nprovider=1\n"] {
            std::fs::write(&resolved, invalid).unwrap();
            let spec = resolve_utility(home.path(), Some(&agent));
            assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_err(), "{invalid}");
        }
        std::fs::write(&resolved, "[runtime]\nprovider='openai_compat'\n").unwrap();
        let spec = resolve_utility(home.path(), Some(&agent));
        assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_ok());
        write_agent_toml(&agent, "[broken");
        let spec = resolve_utility(home.path(), Some(&agent));
        assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_err(), "valid artifact cannot hide invalid source");
        write_agent_toml(&agent, "[runtime]\nprovider='openai_compat'\n");
        std::fs::remove_file(&resolved).unwrap();
        std::fs::create_dir(&resolved).unwrap();
        let spec = resolve_utility(home.path(), Some(&agent));
        assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_err(), "unreadable artifact must not fall back to source");
        std::fs::remove_dir(&resolved).unwrap();
        let spec = resolve_utility(home.path(), Some(&agent));
        assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_ok(), "absent artifact uses valid source");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(home.path().join("missing"), &resolved).unwrap();
            let spec = resolve_utility(home.path(), Some(&agent));
            assert!(resolve_utility_profile(home.path(), Some(&agent), spec, false).is_err(), "dangling artifact is unreadable, not absent");
        }
    }

    /// RM-205: an unsupported engine never falls back to the BUILT-IN
    /// default; only an explicit global key or employee setting routes it.
    #[test]
    fn utility_profiles_fallback_only_to_an_explicit_verified_runtime() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        std::fs::write(agent.path().join("agent.toml"), "").unwrap();
        for provider in RuntimeType::ALL.iter().copied() {
            let spec = UtilitySpec { provider, model: "fixture".into() };
            if utility_profile_supported(provider) {
                assert_eq!(resolve_utility_profile(home.path(), Some(agent.path()), spec.clone(), false).unwrap(), spec);
            } else {
                let error = resolve_utility_profile(home.path(), Some(agent.path()), spec.clone(), false).unwrap_err();
                assert!(is_utility_unavailable(&error), "{provider:?}: {error}");
                assert!(resolve_utility_profile(home.path(), None, spec.clone(), false).is_err());
                let hinted = resolve_utility_profile(home.path(), Some(agent.path()), spec.clone(), true).unwrap_err();
                assert!(!is_utility_unavailable(&hinted), "explicit hints keep their refusal");
            }
        }
        // An explicit global value without a utility profile is unusable ⇒ unavailable.
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='grok'\n").unwrap();
        assert!(is_utility_unavailable(&resolve_utility_profile(home.path(), Some(agent.path()),
            UtilitySpec { provider: RuntimeType::Grok, model: "gemini".into() }, false).unwrap_err()));
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='openai_compat'\nutility_model='fixture-model'\n").unwrap();
        assert_eq!(resolve_utility_profile(home.path(), Some(agent.path()),
            UtilitySpec { provider: RuntimeType::Grok, model: "grok".into() }, false).unwrap(),
            UtilitySpec { provider: RuntimeType::OpenAiCompat, model: "fixture-model".into() });
    }

    /// RM-205 resolution table (the dashboard's `utility_route`).
    #[test]
    fn utility_route_resolution_table() {
        let route = |home: &Path, agent: &Path| utility_route(home, agent);
        let fixture = |agent_toml: &str, config: Option<&str>| {
            let home = TempDir::new().unwrap();
            let agent = home.path().join("agents/worker");
            std::fs::create_dir_all(&agent).unwrap();
            std::fs::write(agent.join("agent.toml"), agent_toml).unwrap();
            if let Some(config) = config { std::fs::write(home.path().join("config.toml"), config).unwrap(); }
            (home, agent)
        };
        // Supported engine → its own engine, whatever else is configured.
        let (home, agent) = fixture("[runtime]\nprovider='codex'\nutility_provider='claude'\n", Some("[runtime]\nutility_provider='claude'\n"));
        assert_eq!(route(home.path(), &agent), UtilityRoute { provider: Some(RuntimeType::Codex), source: UtilityRouteSource::EmployeeEngine });
        // Unsupported + employee setting.
        let (home, agent) = fixture("[runtime]\nprovider='grok'\nutility_provider='openai_compat'\n", None);
        assert_eq!(route(home.path(), &agent), UtilityRoute { provider: Some(RuntimeType::OpenAiCompat), source: UtilityRouteSource::EmployeeSetting });
        let spec = resolve_utility(home.path(), Some(&agent));
        assert_eq!(resolve_utility_profile(home.path(), Some(&agent), spec, false).unwrap().provider, RuntimeType::OpenAiCompat);
        // Unsupported + explicit global.
        let (home, agent) = fixture("[runtime]\nprovider='grok'\n", Some("[runtime]\nutility_provider='claude'\n"));
        assert_eq!(route(home.path(), &agent), UtilityRoute { provider: Some(RuntimeType::Claude), source: UtilityRouteSource::GlobalSetting });
        let spec = resolve_utility(home.path(), Some(&agent));
        assert_eq!(resolve_utility_profile(home.path(), Some(&agent), spec, false).unwrap(),
            UtilitySpec { provider: RuntimeType::Claude, model: DEFAULT_UTILITY_MODEL.into() });
        // Unsupported + only the built-in default (no key, or a config without it) → unavailable.
        for config in [None, Some("[runtime]\nutility_model='claude-haiku-4-5'\n")] {
            let (home, agent) = fixture("[runtime]\nprovider='grok'\n", config);
            assert_eq!(route(home.path(), &agent), UtilityRoute { provider: None, source: UtilityRouteSource::Unavailable });
            let spec = resolve_utility(home.path(), Some(&agent));
            assert!(is_utility_unavailable(&resolve_utility_profile(home.path(), Some(&agent), spec, false).unwrap_err()));
            assert!(utility_skipped_doctor_line(home.path(), &[("worker".into(), agent.clone())]).unwrap().contains("worker"));
        }
        // Invalid / unknown / utility-incapable employee values are ignored (treated as unset).
        for bad in ["'nonsense'", "'grok'", "1", "''"] {
            let (home, agent) = fixture(&format!("[runtime]\nprovider='grok'\nutility_provider={bad}\n"), None);
            assert_eq!(employee_utility_provider(&agent), None, "{bad}");
            assert_eq!(route(home.path(), &agent).source, UtilityRouteSource::Unavailable, "{bad}");
            let (home, agent) = fixture(&format!("[runtime]\nprovider='grok'\nutility_provider={bad}\n"), Some("[runtime]\nutility_provider='claude'\n"));
            assert_eq!(route(home.path(), &agent).source, UtilityRouteSource::GlobalSetting, "{bad}");
        }
        // Presence, not the default: distinguishes an explicit value.
        let home = TempDir::new().unwrap();
        assert_eq!(explicit_global_utility_provider(home.path()), None);
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='claude'\n").unwrap();
        assert_eq!(explicit_global_utility_provider(home.path()), Some(Some(RuntimeType::Claude)));
        assert_eq!(route(home.path(), home.path()).to_json(), serde_json::json!({"provider": "claude", "source": "employee_engine"}));
    }

    #[test]
    fn codex_utility_profile_is_supported_without_provider_substitution() {
        let home = TempDir::new().unwrap();
        let spec = UtilitySpec { provider: RuntimeType::Codex, model: "gpt-5.4-mini".into() };
        assert!(utility_profile_supported(RuntimeType::Codex));
        assert_eq!(resolve_utility_profile(home.path(), None, spec.clone(), true).unwrap(), spec);
    }

    #[test]
    fn session_owner_requires_real_direct_child_and_declared_identity() {
        let home = TempDir::new().unwrap();
        let agents = home.path().join("agents");
        std::fs::create_dir(&agents).unwrap();
        assert!(session_agent_dir(home.path(), None).unwrap().is_none());
        assert!(session_agent_dir(home.path(), Some("")).unwrap().is_none());
        for id in duduclaw_core::SYSTEM_SENDERS.iter()
            .chain(duduclaw_core::RESERVED_QUEUE_SENDERS.iter())
            .copied().chain(["../escape", "/tmp", " ", "missing", "default"]) {
            assert!(session_agent_dir(home.path(), Some(id)).is_err(), "{id}");
        }
        for id in ["_trash", "_defaults", "_x", ".hidden"] {
            let internal = agents.join(id);
            std::fs::create_dir(&internal).unwrap();
            std::fs::write(internal.join("agent.toml"), format!("[agent]\nname='{id}'\n")).unwrap();
            assert!(session_agent_dir(home.path(), Some(id)).is_err(), "{id}");
        }
        let employee = agents.join("worker");
        std::fs::create_dir(&employee).unwrap();
        assert!(session_agent_dir(home.path(), Some("worker")).is_err());
        for config in ["", "[agent]\nname='other'\n", "[agent]\nname=1\n", "invalid toml"] {
            std::fs::write(employee.join("agent.toml"), config).unwrap();
            assert!(session_agent_dir(home.path(), Some("worker")).is_err(), "{config}");
        }
        std::fs::write(employee.join("agent.toml"), "[agent]\nname='worker'\n").unwrap();
        assert_eq!(session_agent_dir(home.path(), Some("worker")).unwrap(), Some(employee.canonicalize().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn session_owner_rejects_directory_and_config_symlinks() {
        use std::os::unix::fs::symlink;
        let home = TempDir::new().unwrap();
        let agents = home.path().join("agents");
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("agent.toml"), "[agent]\nname='worker'\n").unwrap();
        symlink(&outside, agents.join("worker")).unwrap();
        assert!(session_agent_dir(home.path(), Some("worker")).is_err());
        std::fs::remove_file(agents.join("worker")).unwrap();
        std::fs::create_dir(agents.join("worker")).unwrap();
        symlink(outside.join("agent.toml"), agents.join("worker/agent.toml")).unwrap();
        assert!(session_agent_dir(home.path(), Some("worker")).is_err());
        // Even a symlink pointing to another direct child is not an identity.
        symlink(agents.join("worker"), agents.join("alias")).unwrap();
        assert!(session_agent_dir(home.path(), Some("alias")).is_err());
    }

    #[test]
    fn explicit_mismatched_utility_model_is_preserved() {
        let agent = tempfile::tempdir().unwrap();
        std::fs::write(agent.path().join("agent.toml"),
            "[runtime]\nprovider = \"codex\"\n[model]\npreferred = \"gpt-5.4\"\nutility = \"claude-haiku-4-5\"\n",
        ).unwrap();
        for _ in 0..2 {
            let settings = load_runtime_settings(agent.path());
            assert_eq!(settings.provider, RuntimeType::Codex);
            assert_eq!(settings.utility_model, "claude-haiku-4-5");
        }
    }

    #[test]
    fn runtime_timeout_ignores_stale_preset_artifacts() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(resolved, "[runtime]\ntimeout_secs=3600\n").unwrap();
        write_agent_toml(&dir, "[runtime]\ntimeout_secs=900\n");
        assert_eq!(agent_timeout_secs(&dir), 900);
        assert_eq!(read_runtime_json(&dir)["timeout_secs"], serde_json::json!(900));
        write_agent_toml(&dir, "[runtime]\n");
        assert_eq!(agent_timeout_secs(&dir), DEFAULT_RUN_TIMEOUT_SECS, "clearing must use the fixed default");
        assert!(read_runtime_json(&dir).get("timeout_secs").is_none(), "UI must not expose stale preset limits");
    }

    #[tokio::test]
    async fn a_scoped_task_limit_replaces_only_its_own_employee_limit() {
        let home = TempDir::new().unwrap();
        let anna = home.path().join("agents/anna");
        let bob = home.path().join("agents/bob");
        std::fs::create_dir_all(&anna).unwrap();
        std::fs::create_dir_all(&bob).unwrap();
        write_agent_toml(&anna, "[runtime]\ntimeout_secs = 300\n");
        write_agent_toml(&bob, "[runtime]\ntimeout_secs = 120\n");
        assert_eq!(call_timeout_secs(Some(&anna)), 300, "no scope: the employee's own");
        let scope = |secs| Some(("anna".to_string(), secs));
        let (own, other, agentless) = TASK_TIMEOUT_SECS
            .scope(scope(1800), async {
                (call_timeout_secs(Some(&anna)), call_timeout_secs(Some(&bob)), call_timeout_secs(None))
            })
            .await;
        assert_eq!(own, 1800);
        assert_eq!(other, 120, "another employee inside the scope keeps its own");
        assert_eq!(agentless, DEFAULT_RUN_TIMEOUT_SECS, "an agent-less call never takes it");
        let none = TASK_TIMEOUT_SECS.scope(None, async { call_timeout_secs(Some(&anna)) }).await;
        assert_eq!(none, 300, "a task without its own limit keeps the employee's");
        for (stored, expected) in [(5, 30), (99_999, 3600)] {
            let got = TASK_TIMEOUT_SECS
                .scope(scope(stored), async { call_timeout_secs(Some(&anna)) })
                .await;
            assert_eq!(got, expected, "{stored} is clamped to the runtime bounds");
        }
    }

    #[test]
    fn the_non_admin_ceiling_is_the_employee_limit() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\ntimeout_secs = 900\n");
        assert_eq!(employee_timeout_ceiling(home.path(), "anna"), 900);
        assert_eq!(employee_timeout_ceiling(home.path(), "missing"), DEFAULT_TIMEOUT_SECS);
        assert_eq!(employee_timeout_ceiling(home.path(), "../anna"), DEFAULT_TIMEOUT_SECS);
        // A sandboxed employee: the shorter of the runtime and container limits.
        write_agent_toml(&dir, "[runtime]\ntimeout_secs = 900\n[container]\nsandbox_enabled = true\ntimeout_ms = 60000\n");
        assert_eq!(employee_timeout_ceiling(home.path(), "anna"), 60);
        write_agent_toml(&dir, "[runtime]\ntimeout_secs = 900\n[container]\nsandbox_enabled = true\ntimeout_ms = 1800000\n");
        assert_eq!(employee_timeout_ceiling(home.path(), "anna"), 900);
        write_agent_toml(&dir, "[runtime]\ntimeout_secs = 900\n[container]\nsandbox_enabled = true\n");
        assert_eq!(employee_timeout_ceiling(home.path(), "anna"), MIN_TIMEOUT_SECS, "unreadable container limit");
        write_agent_toml(&dir, "[runtime]\ntimeout_secs = 900\n[container]\nsandbox_enabled = false\ntimeout_ms = 60000\n");
        assert_eq!(employee_timeout_ceiling(home.path(), "anna"), 900, "sandbox off: runtime only");
    }

    #[test]
    fn runtime_timeout_defaults_clamps_and_ignores_wrong_types() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(agent_timeout_secs(&dir), 1800);
        for (literal, expected) in [("900", 900), ("30", 30), ("3600", 3600),
            ("-1", 30), ("3601", 3600), ("\"oops\"", 1800), ("1.5", 1800), ("true", 1800)] {
            write_agent_toml(&dir, &format!("[runtime]\ntimeout_secs = {literal}\n"));
            assert_eq!(agent_timeout_secs(&dir), expected, "{literal}");
        }
    }

    #[test]
    fn utility_default_follows_non_claude_main_model() {
        let dir = TempDir::new().unwrap();
        for (provider, preferred, utility, expected) in [
            ("codex", "gpt-5", None, "gpt-5"),
            ("codex", "", None, ""),
            ("codex", "  ", Some("  "), ""),
            ("claude", "claude-sonnet-4-6", None, DEFAULT_UTILITY_MODEL),
            ("claude", "claude-sonnet-4-6", Some("  "), DEFAULT_UTILITY_MODEL),
            ("codex", "gpt-5", Some("o4-mini"), "o4-mini"),
        ] {
            let model = if preferred.is_empty() { String::new() }
                else { format!("preferred = {preferred:?}\n") };
            let utility = utility.map(|v| format!("utility = {v:?}\n")).unwrap_or_default();
            write_agent_toml(dir.path(), &format!("[runtime]\nprovider = {provider:?}\n[model]\n{model}{utility}"));
            assert_eq!(load_runtime_settings(dir.path()).utility_model, expected);
        }
    }

    #[test]
    fn utility_defaults_when_absent() {
        let dir = TempDir::new().unwrap();
        assert_eq!(agent_utility_model(dir.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn utility_reads_config() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\nutility = \"claude-sonnet-4-6\"\n");
        assert_eq!(agent_utility_model(dir.path()), "claude-sonnet-4-6");
    }

    #[test]
    fn utility_defaults_on_malformed() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "this is not valid toml ===");
        assert_eq!(agent_utility_model(dir.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn provider_defaults_to_claude() {
        let dir = TempDir::new().unwrap();
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
        assert_eq!(agent_runtime_fallback(dir.path()), None);
    }

    #[test]
    fn provider_reads_config() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"grok\"\nfallback = \"claude\"\n",
        );
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Grok);
        assert_eq!(
            agent_runtime_fallback(dir.path()),
            Some(RuntimeType::Claude)
        );
    }

    /// Removed provider and fallback settings are refused before dispatch.
    #[test]
    fn removed_runtime_in_config_is_not_selected() {
        let dir = TempDir::new().unwrap();
        for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
            for field in ["provider", "fallback"] {
                for value in [removed.id.to_string(), format!(" {} ", removed.id.to_uppercase())] {
                    write_agent_toml(dir.path(), &format!("[runtime]\n{field} = {value:?}\n"));
                    let error = ensure_active_agent_provider(dir.path()).unwrap_err();
                    assert!(error.contains(removed.removed_in), "{error}");
                    assert!(error.contains(field), "{error}");
                }
            }
            std::fs::write(dir.path().join("config.toml"),
                format!("[runtime]\nutility_provider = {:?}\n", removed.id)).unwrap();
            let error = ensure_active_utility_provider(dir.path()).unwrap_err();
            assert!(error.contains(removed.removed_in), "{error}");
        }
    }

    #[test]
    fn resolved_config_cannot_hide_a_removed_source_provider() {
        let home = TempDir::new().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&resolved, "[runtime]\nprovider = \"codex\"\n").unwrap();
        for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
            write_agent_toml(&agent, &format!("[runtime]\nprovider = {:?}\n", removed.id));
            assert!(ensure_active_agent_provider(&agent).unwrap_err().contains(removed.removed_in));
        }
    }

    #[test]
    fn removed_team_runtime_cannot_degrade_to_a_solo_dispatch() {
        let home = TempDir::new().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        write_agent_toml(&agent, "[runtime]\nprovider = \"codex\"\n");
        for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
            write_global_config(home.path(), &format!(
                "[team.roles.verifier]\nruntime = {:?}\n", removed.id));
            let error = ensure_active_agent_provider(&agent).unwrap_err();
            assert!(error.contains(removed.removed_in), "{error}");
            assert!(error.contains("team.roles.verifier"), "{error}");
        }
        // A supported per-agent override repairs the inherited role.
        write_agent_toml(&agent, "[runtime]\nprovider = \"codex\"\n[team.roles.verifier]\nruntime = \"grok\"\n");
        assert!(ensure_active_agent_provider(&agent).is_ok());
    }

    #[test]
    fn provider_gates_refuse_unreadable_or_malformed_configs() {
        for global in [false, true] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(if global { "config.toml" } else { "agent.toml" });
            let check = || if global {
                ensure_active_utility_provider(dir.path())
            } else {
                ensure_active_agent_provider(dir.path())
            };
            assert!(check().is_ok());
            for bytes in [b"[runtime]\nprovider = \"private-content\"\nbroken = [".as_slice(), &[0xff]] {
                std::fs::write(&path, bytes).unwrap();
                let error = check().unwrap_err();
                assert!(error.starts_with(&format!("{}: ", config_file_name(&path))), "{error}");
                assert!(!error.contains(&dir.path().display().to_string()), "{error}");
                assert!(!error.contains("private-content"), "{error}");
            }
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert_eq!(check().unwrap_err(), format!("{}: unreadable", config_file_name(&path)));
            std::fs::remove_dir(&path).unwrap();
            std::fs::write(&path, "[runtime]\nprovider = \"codex\"\nutility_provider = \"codex\"\n").unwrap();
            assert!(check().is_ok());
        }
    }

    #[test]
    fn provider_gates_strictly_check_only_runtime_selectors() {
        let invalid = [
            ("runtime = 42", "[runtime]"),
            ("[runtime]\nprovider = 42", "[runtime] provider"),
            ("[runtime]\nfallback = false", "[runtime] fallback"),
            ("team = 42", "[team]"),
            ("[team]\nroles = false", "[team.roles]"),
            ("[team.roles]\nverifier = 42", "[team.roles.verifier]"),
            ("[team.roles.verifier]\nruntime = 42", "[team.roles.verifier] runtime"),
            ("[team.roles.verifier]\nmodel = false", "[team.roles.verifier] model"),
        ];
        for name in ["agent.toml", "config.toml"] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(name);
            for (text, field) in invalid {
                std::fs::write(&path, text).unwrap();
                assert_eq!(read_config_for_gate(&path).unwrap_err(), format!("{name}: wrong type for {field}"));
            }
            std::fs::write(&path, "[runtime]\ntimeout_secs = false\n[team]\nenabled = 42\n[team.roles.verifier]\neffort = false\n").unwrap();
            assert!(read_config_for_gate(&path).is_ok(), "unrelated fields keep lenient semantics");
        }
    }

    #[test]
    fn source_and_resolved_team_selectors_are_checked_independently() {
        let home = TempDir::new().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        let source = agent.join("agent.toml");
        for invalid_path in [&source, &resolved] {
            let other = if invalid_path == &source { &resolved } else { &source };
            for invalid in [
                "[team]\nenabled = false\n[team.roles.verifier]\nruntime = \"antigravity\"\n",
                "[team.roles.verifier]\nmodel = 42\n",
                "[runtime]\nprovider = \"agy\"\n",
                "[runtime]\nfallback = \"gemini\"\n",
            ] {
                std::fs::write(other, "[runtime]\nprovider = \"codex\"\n").unwrap();
                std::fs::write(invalid_path, invalid).unwrap();
                for error in [read_agent_sections_for_gate(&agent).unwrap_err(), ensure_active_agent_provider(&agent).unwrap_err()] {
                    assert!(error.starts_with(config_file_name(invalid_path)), "{error}");
                    assert!(!error.contains(&home.path().display().to_string()), "{error}");
                }
            }
        }
        std::fs::remove_file(source).unwrap();
        assert!(ensure_active_agent_provider(&agent).is_err(), "resolved content is checked without a source");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_config_symlinks_are_not_absent() {
        let dir = TempDir::new().unwrap();
        for name in ["agent.toml", "config.toml", "worker.toml"] {
            let path = dir.path().join(name);
            assert!(read_config_for_gate(&path).unwrap().is_none());
            std::os::unix::fs::symlink(dir.path().join("missing"), &path).unwrap();
            assert_eq!(read_config_for_gate(&path).unwrap_err(), format!("{name}: unreadable"));
        }
    }

    #[test]
    fn config_refusals_do_not_echo_parser_keys_or_paths() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");
        let key = "私密內容".repeat(100);
        std::fs::write(&path, format!("{key:?} = 1\n{key:?} = 2\n")).unwrap();
        assert_eq!(read_config_for_gate(&path).unwrap_err(), "agent.toml: invalid TOML");
        std::fs::write(&path, [0xff]).unwrap();
        assert_eq!(read_config_for_gate(&path).unwrap_err(), "agent.toml: invalid UTF-8");
    }

    #[test]
    fn provider_unknown_falls_back_to_claude() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"nonsense\"\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
    }

    // ── W3/G1: [model] fallbacks chain ──────────────────────────────────

    #[test]
    fn runtime_fallback_models_prepend_singular_model() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\nfallback=' grok-4 '\n");
        assert_eq!(agent_model_fallbacks(dir.path()), vec!["grok-4"]);
        write_agent_toml(dir.path(), "[model]\nfallback='grok-4'\nfallbacks=['grok-4.7', 'claude-haiku-4-5']\n");
        assert_eq!(agent_model_fallbacks(dir.path()), vec!["grok-4", "grok-4.7", "claude-haiku-4-5"]);
        write_agent_toml(dir.path(), "[model]\nfallback='  '\nfallbacks=['grok-4.7']\n");
        assert_eq!(agent_model_fallbacks(dir.path()), vec!["grok-4.7"]);
    }

    #[test]
    fn model_fallbacks_empty_when_absent() {
        let dir = TempDir::new().unwrap();
        assert!(agent_model_fallbacks(dir.path()).is_empty());
        // Present [model] table but no `fallbacks` key → still empty.
        write_agent_toml(dir.path(), "[model]\nutility = \"claude-haiku-4-5\"\n");
        assert!(agent_model_fallbacks(dir.path()).is_empty());
    }

    #[test]
    fn model_fallbacks_parsed_trimmed_and_blanks_dropped() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(
            dir.path(),
            "[model]\nfallbacks = [\"openai/gpt-5.4\", \" compat:deepseek/deepseek-v3.2 \", \"\"]\n",
        );
        assert_eq!(
            agent_model_fallbacks(dir.path()),
            vec![
                "openai/gpt-5.4".to_string(),
                "compat:deepseek/deepseek-v3.2".to_string(),
            ]
        );
    }

    #[test]
    fn model_fallbacks_empty_on_malformed_or_wrong_type() {
        let dir = TempDir::new().unwrap();
        // Wrong type (string, not array) → empty.
        write_agent_toml(dir.path(), "[model]\nfallbacks = \"openai/gpt-5.4\"\n");
        assert!(agent_model_fallbacks(dir.path()).is_empty());
        // Malformed toml → empty.
        write_agent_toml(dir.path(), "not valid toml ===");
        assert!(agent_model_fallbacks(dir.path()).is_empty());
    }

    // ── RFC-24: decision_continuity opt-in ──────────────────────────────

    #[test]
    fn decision_continuity_defaults_off_when_absent() {
        let dir = TempDir::new().unwrap();
        assert!(!decision_continuity_enabled(dir.path()));
    }

    #[test]
    fn decision_continuity_reads_true() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[memory]\ndecision_continuity = true\n");
        assert!(decision_continuity_enabled(dir.path()));
    }

    #[test]
    fn decision_continuity_reads_false() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[memory]\ndecision_continuity = false\n");
        assert!(!decision_continuity_enabled(dir.path()));
    }

    #[test]
    fn decision_continuity_off_on_malformed_or_wrong_type() {
        let dir = TempDir::new().unwrap();
        // Non-bool value → fail-safe off.
        write_agent_toml(dir.path(), "[memory]\ndecision_continuity = \"yes\"\n");
        assert!(!decision_continuity_enabled(dir.path()));
        // Malformed toml → fail-safe off.
        write_agent_toml(dir.path(), "not valid toml ===");
        assert!(!decision_continuity_enabled(dir.path()));
    }

    #[test]
    fn decision_ttl_defaults_and_overrides() {
        let dir = TempDir::new().unwrap();
        assert_eq!(decision_ttl_days(dir.path()), 7, "default 7 days");
        write_agent_toml(dir.path(), "[memory]\ndecision_ttl_days = 30\n");
        assert_eq!(decision_ttl_days(dir.path()), 30);
        // Non-positive / malformed → default.
        write_agent_toml(dir.path(), "[memory]\ndecision_ttl_days = 0\n");
        assert_eq!(decision_ttl_days(dir.path()), 7);
        write_agent_toml(dir.path(), "[memory]\ndecision_ttl_days = \"x\"\n");
        assert_eq!(decision_ttl_days(dir.path()), 7);
    }

    #[test]
    fn load_runtime_settings_single_read_all_fields() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"codex\"\nfallback = \"claude\"\n[model]\nutility = \"o4-mini\"\n",
        );
        let s = load_runtime_settings(dir.path());
        assert_eq!(s.provider, RuntimeType::Codex);
        assert_eq!(s.fallback, Some(RuntimeType::Claude));
        assert_eq!(s.utility_model, "o4-mini");
    }

    #[test]
    fn load_runtime_settings_defaults_when_absent() {
        let dir = TempDir::new().unwrap();
        let s = load_runtime_settings(dir.path());
        assert_eq!(s, RuntimeSettings::default());
        assert_eq!(s.provider, RuntimeType::Claude);
        assert_eq!(s.fallback, None);
        assert_eq!(s.utility_model, DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn uses_non_claude_predicate() {
        let dir = TempDir::new().unwrap();
        // No config → Claude → None.
        assert_eq!(agent_uses_non_claude(dir.path()), None);
        // Explicit non-Claude → Some(provider).
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"grok\"\n");
        assert_eq!(
            agent_uses_non_claude(dir.path()),
            Some(RuntimeType::Grok)
        );
    }

    #[test]
    fn live_provider_and_preferred_are_read_from_the_same_file() {
        let dir = TempDir::new().unwrap();
        for (provider, model) in [("codex", "gpt-5.6-sol"), ("grok", "grok-4"), ("codex", "gpt-5.6-sol")] {
            write_agent_toml(dir.path(), &format!("[runtime]\nprovider='{provider}'\n[model]\npreferred='{model}'\n"));
            let settings = load_runtime_settings(dir.path());
            assert_eq!(settings.provider.as_str(), provider);
            assert_eq!(settings.preferred_model.as_deref(), Some(model));
        }
    }

    #[test]
    fn strict_runtime_settings_report_each_failure_instead_of_defaulting() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_runtime_settings_strict(dir.path()), Err(RuntimeSettingsError::Missing));

        std::fs::write(dir.path().join("agent.toml"), [0xff, 0xfe, b'[', 0x80]).unwrap();
        assert_eq!(load_runtime_settings_strict(dir.path()), Err(RuntimeSettingsError::Unreadable));

        for malformed in [
            "[runtime\nprovider = 'codex'\n",
            "[runtime]\nprovider = 5\n",
            "runtime = 'codex'\n",
            "[model]\npreferred = 42\n",
            "model = 'gpt-5.4'\n",
        ] {
            write_agent_toml(dir.path(), malformed);
            assert_eq!(load_runtime_settings_strict(dir.path()), Err(RuntimeSettingsError::Malformed), "{malformed}");
            // The non-strict reader keeps its documented defaults.
            let _ = load_runtime_settings(dir.path());
        }

        for unknown in ["not-a-runtime", "", "claude; rm -rf /"] {
            write_agent_toml(dir.path(), &format!("[runtime]\nprovider = {unknown:?}\n[model]\npreferred = 'gpt-5.4'\n"));
            assert_eq!(load_runtime_settings_strict(dir.path()), Err(RuntimeSettingsError::UnknownProvider), "{unknown}");
        }
    }

    #[test]
    fn strict_runtime_settings_take_provider_and_model_from_one_parse() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = 'codex'\n[model]\npreferred = 'gpt-5.6-sol'\n");
        let settings = load_runtime_settings_strict(dir.path()).unwrap();
        assert_eq!(settings, load_runtime_settings(dir.path()));
        assert_eq!(settings.provider, RuntimeType::Codex);
        assert_eq!(settings.preferred_or_default().unwrap(), "gpt-5.6-sol");

        // No `[model] preferred` is not an error: the provider's own default.
        for (provider, runtime) in [("codex", RuntimeType::Codex), ("grok", RuntimeType::Grok), ("claude", RuntimeType::Claude)] {
            write_agent_toml(dir.path(), &format!("[runtime]\nprovider = '{provider}'\n"));
            let settings = load_runtime_settings_strict(dir.path()).unwrap();
            assert_eq!(settings.provider, runtime);
            assert_eq!(settings.preferred_model, None);
            assert_eq!(settings, load_runtime_settings(dir.path()));
            assert_eq!(settings.preferred_or_default(), load_runtime_settings(dir.path()).preferred_or_default());
            assert!(ensure_model_provider(&settings.preferred_or_default().unwrap(), runtime).is_ok(), "{provider}");
        }
        assert_eq!(default_preferred_model(RuntimeType::Claude).unwrap(), duduclaw_core::types::DEFAULT_PREFERRED_MODEL);

        // No `[runtime]` at all: the documented Claude provider, not an error.
        write_agent_toml(dir.path(), "[model]\npreferred = '  '\n");
        let settings = load_runtime_settings_strict(dir.path()).unwrap();
        assert_eq!(settings.provider, RuntimeType::Claude);
        assert_eq!(settings.preferred_or_default().unwrap(), duduclaw_core::types::DEFAULT_PREFERRED_MODEL);
    }

    #[test]
    fn strict_runtime_settings_require_a_model_only_when_the_runtime_needs_one() {
        let dir = TempDir::new().unwrap();
        for provider in ["openai_compat", "kiro"] {
            let runtime = RuntimeType::parse(provider).unwrap();
            assert_eq!(default_preferred_model(runtime), None);
            write_agent_toml(dir.path(), &format!("[runtime]\nprovider='{provider}'\n[model]\npreferred='custom-model'\n"));
            let settings = load_runtime_settings_strict(dir.path()).unwrap();
            assert_eq!(settings.provider, runtime);
            assert_eq!(settings.preferred_or_default(), Ok("custom-model".to_string()));
            for model in ["", "[model]\npreferred=''\n", "[model]\npreferred='  '\n"] {
                write_agent_toml(dir.path(), &format!("[runtime]\nprovider='{provider}'\n{model}"));
                let expected = if runtime == RuntimeType::Kiro { Ok(String::new()) }
                    else { Err(RuntimeSettingsError::MissingModel) };
                assert_eq!(load_runtime_settings_strict(dir.path()).and_then(|s| s.preferred_or_default()), expected);
                // Even callers of the unchanged lenient loader cannot invent a Claude model.
                let settings = load_runtime_settings(dir.path());
                assert_eq!(settings.provider, runtime);
                assert_eq!(settings.preferred_or_default(), expected);
            }
        }
        assert_eq!(RuntimeSettingsError::MissingModel.as_str(), "missing_model");
    }

    #[test]
    fn strict_runtime_settings_use_only_the_providers_own_default() {
        let dir = TempDir::new().unwrap();
        for (provider, expected) in [
            (RuntimeType::Codex, RuntimeType::Codex.spec().fallback_models[0].0),
            (RuntimeType::Claude, duduclaw_core::types::DEFAULT_PREFERRED_MODEL),
        ] {
            for model in ["", "[model]\npreferred='  '\n"] {
                write_agent_toml(dir.path(), &format!("[runtime]\nprovider='{}'\n{model}", provider.as_str()));
                let settings = load_runtime_settings_strict(dir.path()).unwrap();
                assert_eq!(settings.preferred_or_default(), Ok(expected.to_string()));
            }
        }
    }

    #[test]
    fn strict_runtime_settings_never_fall_back_from_an_unreadable_resolved_profile() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\n[model]\npreferred='opus'\n");
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&resolved, "[runtime]\nprovider='grok'\n[model]\npreferred='grok-4'\n").unwrap();
        let settings = load_runtime_settings_strict(&dir).unwrap();
        assert_eq!(settings.provider, RuntimeType::Grok);
        assert_eq!(settings.preferred_or_default().unwrap(), "grok-4");
        std::fs::write(&resolved, "[runtime").unwrap();
        assert_eq!(load_runtime_settings_strict(&dir), Err(RuntimeSettingsError::Malformed));
        std::fs::write(&resolved, [0xff]).unwrap();
        assert_eq!(load_runtime_settings_strict(&dir), Err(RuntimeSettingsError::Unreadable));
        // The legacy reader deliberately retains its fallback behavior.
        assert_eq!(load_runtime_settings(&dir).provider, RuntimeType::Claude);
    }

    #[test]
    fn strict_reader_accepts_catalog_aliases_and_toml_encodings() {
        let dir = TempDir::new().unwrap();
        for spec in duduclaw_core::runtime_catalog::CATALOG {
            for alias in std::iter::once(spec.id).chain(spec.aliases.iter().copied()) {
                for provider in [alias.to_string(), format!(" {} ", alias.to_uppercase())] {
                    for prefix in ["", "\u{feff}"] {
                        write_agent_toml(dir.path(), &format!("{prefix}[runtime]\r\nprovider={provider:?}\r\n[model]\r\npreferred='custom-model'\r\n[extra]\r\nfuture=true\r\n"));
                        assert_eq!(load_runtime_settings_strict(dir.path()).unwrap(), load_runtime_settings(dir.path()), "{provider}");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn strict_reader_accepts_repository_agent_configs() {
        fn collect(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
            if !dir.is_dir() { return; }
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() { collect(&path, files); }
                else if entry.file_name() == "agent.toml" { files.push(path); }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let mut files = Vec::new();
        for dir in ["templates", "docs", "tests", "crates", "distribution"] {
            collect(&root.join(dir), &mut files);
        }
        if root.join("agent.toml").is_file() { files.push(root.join("agent.toml")); }
        assert!(!files.is_empty());
        let mut accepted = 0;
        for file in files {
            let dir = TempDir::new().unwrap();
            std::fs::copy(&file, dir.path().join("agent.toml")).unwrap();
            let legacy = load_runtime_settings(dir.path());
            // Registry rejection is not a strict-reader compatibility regression.
            if duduclaw_agent::AgentRegistry::load_agent(dir.path()).await.is_err() { continue; }
            accepted += 1;
            assert_eq!(load_runtime_settings_strict(dir.path()).unwrap_or_else(|e| panic!("{}: {e:?}", file.display())), legacy, "{}", file.display());
        }
        assert!(accepted > 0);
    }

    #[test]
    fn non_claude_provider_method() {
        assert_eq!(RuntimeSettings::default().non_claude_provider(), None);
        let s = RuntimeSettings {
            provider: RuntimeType::Codex,
            fallback: None,
            utility_model: DEFAULT_UTILITY_MODEL.to_string(),
            preferred_model: None,
        };
        assert_eq!(s.non_claude_provider(), Some(RuntimeType::Codex));
    }

    // ── Global utility config (N2) ──────────────────────────────────

    fn write_global_config(home: &Path, body: &str) {
        let mut f = std::fs::File::create(home.join("config.toml")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn global_utility_defaults_when_config_absent() {
        let home = TempDir::new().unwrap();
        assert_eq!(global_utility_provider(home.path()), RuntimeType::Claude);
        assert_eq!(global_utility_model(home.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn global_utility_blank_model_defaults_follow_provider() {
        let home = TempDir::new().unwrap();
        for (provider, expected_provider, expected_model) in [
            ("claude", RuntimeType::Claude, DEFAULT_UTILITY_MODEL),
            ("codex", RuntimeType::Codex, ""),
            ("grok", RuntimeType::Grok, ""),
            ("openai_compat", RuntimeType::OpenAiCompat, ""),
            ("nonsense", RuntimeType::Claude, DEFAULT_UTILITY_MODEL),
        ] {
            for model in ["", "utility_model = ''\n", "utility_model = '  '\n"] {
                write_global_config(home.path(), &format!(
                    "[runtime]\nutility_provider = {provider:?}\n{model}"
                ));
                assert_eq!(global_utility_provider(home.path()), expected_provider);
                assert_eq!(global_utility_model(home.path()), expected_model, "{provider}: {model}");
                assert_eq!(resolve_utility(home.path(), None), UtilitySpec {
                    provider: expected_provider,
                    model: expected_model.into(),
                });
            }
        }
    }

    #[test]
    fn global_utility_reads_config() {
        let home = TempDir::new().unwrap();
        write_global_config(
            home.path(),
            "[runtime]\nutility_provider = \"grok\"\nutility_model = \"grok-4.3\"\n",
        );
        assert_eq!(
            global_utility_provider(home.path()),
            RuntimeType::Grok
        );
        assert_eq!(global_utility_model(home.path()), "grok-4.3");
    }

    #[test]
    fn global_utility_unknown_provider_falls_back_to_claude() {
        let home = TempDir::new().unwrap();
        write_global_config(home.path(), "[runtime]\nutility_provider = \"nonsense\"\n");
        assert_eq!(global_utility_provider(home.path()), RuntimeType::Claude);
    }

    #[test]
    fn resolve_utility_with_agent_dir_uses_agent_config() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        // Global says Grok; agent says Codex — agent_dir present must win.
        write_global_config(
            home.path(),
            "[runtime]\nutility_provider = \"grok\"\n",
        );
        write_agent_toml(
            agent.path(),
            "[runtime]\nprovider = \"codex\"\n[model]\nutility = \"o4-mini\"\n",
        );
        let spec = resolve_utility(home.path(), Some(agent.path()));
        assert_eq!(resolve_utility_profile(home.path(), Some(agent.path()), spec.clone(), false).unwrap(), spec);
        assert_eq!(spec.provider, RuntimeType::Codex);
        assert_eq!(spec.model, "o4-mini");
    }

    #[test]
    fn resolve_utility_without_agent_dir_uses_global() {
        let home = TempDir::new().unwrap();
        write_global_config(
            home.path(),
            "[runtime]\nutility_provider = \"grok\"\nutility_model = \"grok-4.3\"\n",
        );
        let spec = resolve_utility(home.path(), None);
        assert_eq!(spec.provider, RuntimeType::Grok);
        assert_eq!(spec.model, "grok-4.3");
    }

    #[test]
    fn resolve_utility_fully_unconfigured_is_claude_default() {
        let home = TempDir::new().unwrap();
        let spec = resolve_utility(home.path(), None);
        assert_eq!(spec.provider, RuntimeType::Claude);
        assert_eq!(spec.model, DEFAULT_UTILITY_MODEL);
    }

    // ── O1: delegation confidence-routing config ────────────────────

    #[test]
    fn delegation_routing_defaults_off_when_unconfigured() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        assert!(!global_delegation_routing(home.path()));
        assert_eq!(agent_delegation_routing(agent.path()), None);
        assert!(!delegation_routing_enabled(home.path(), agent.path()));
    }

    #[test]
    fn delegation_routing_global_flag() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        write_global_config(home.path(), "[delegation]\nconfidence_routing = true\n");
        assert!(global_delegation_routing(home.path()));
        assert!(delegation_routing_enabled(home.path(), agent.path()));
    }

    #[test]
    fn delegation_routing_agent_override_wins_both_ways() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        // Global ON, agent explicitly OFF → off.
        write_global_config(home.path(), "[delegation]\nconfidence_routing = true\n");
        write_agent_toml(agent.path(), "[model]\ndelegation_routing = false\n");
        assert_eq!(agent_delegation_routing(agent.path()), Some(false));
        assert!(!delegation_routing_enabled(home.path(), agent.path()));
        // Global OFF, agent explicitly ON → on.
        write_global_config(home.path(), "");
        write_agent_toml(agent.path(), "[model]\ndelegation_routing = true\n");
        assert!(delegation_routing_enabled(home.path(), agent.path()));
    }

    #[test]
    fn delegation_routing_fail_safe_on_malformed() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        // Wrong-typed values → default off / None.
        write_global_config(home.path(), "[delegation]\nconfidence_routing = \"yes\"\n");
        write_agent_toml(agent.path(), "[model]\ndelegation_routing = \"yes\"\n");
        assert!(!global_delegation_routing(home.path()));
        assert_eq!(agent_delegation_routing(agent.path()), None);
        assert!(!delegation_routing_enabled(home.path(), agent.path()));
        // Malformed toml → same.
        write_global_config(home.path(), "not valid toml ===");
        write_agent_toml(agent.path(), "not valid toml ===");
        assert!(!delegation_routing_enabled(home.path(), agent.path()));
    }

    #[test]
    fn standard_model_reads_and_filters_blank() {
        let agent = TempDir::new().unwrap();
        assert_eq!(agent_standard_model(agent.path()), None);
        write_agent_toml(agent.path(), "[model]\nstandard = \"claude-sonnet-4-6\"\n");
        assert_eq!(
            agent_standard_model(agent.path()),
            Some("claude-sonnet-4-6".to_string())
        );
        // Blank / whitespace value → None (fail-safe to preferred).
        write_agent_toml(agent.path(), "[model]\nstandard = \"  \"\n");
        assert_eq!(agent_standard_model(agent.path()), None);
    }

    #[test]
    fn infer_provider_for_model_families() {
        use RuntimeType::*;
        assert_eq!(infer_provider_for_model("claude-sonnet-4-6"), Some(Claude));
        // Family CLIs may or may not be installed on the test host — the
        // inference must land on the family CLI or the openai_compat API
        // fallback, never a different family and never Claude.
        for (model, family) in [
            ("grok-4.5", Grok),
            ("gpt-5.4", Codex),
            ("xai/grok-4.5", Grok), // qualified provider/model form
        ] {
            let got = infer_provider_for_model(model).expect(model);
            assert!(
                got == family || got == OpenAiCompat,
                "{model} inferred {got:?}"
            );
        }
        // WP-B: families that used to be unknown now have a runtime. `qwen*`
        // reaches Qwen Code (or the API fallback when its CLI isn't
        // installed) instead of returning None and skipping the auto-align.
        for (model, family) in [
            ("qwen3-coder-plus", Qwen),
            ("kimi-for-coding", Kimi),
            ("devstral-small", Vibe),
        ] {
            let got = infer_provider_for_model(model).expect(model);
            assert!(
                got == family || got == OpenAiCompat,
                "{model} inferred {got:?}"
            );
        }
        // Multi-vendor shells claim no family, so a model they happen to serve
        // still infers its OWN vendor's runtime — `gpt-5` must not be hijacked
        // to Copilot/Cursor/OpenCode just because they can run it.
        let gpt = infer_provider_for_model("gpt-5").unwrap();
        assert!(gpt == Codex || gpt == OpenAiCompat, "{gpt:?}");
        // Genuinely unknown families: still never guess.
        assert_eq!(infer_provider_for_model("deepseek-v3.2"), None);
        assert_eq!(infer_provider_for_model("llama-3.2-3b"), None);
        assert_eq!(infer_provider_for_model(""), None);
    }

    /// WP-B: the multi-vendor shells (Copilot / Cursor / OpenCode / Kiro)
    /// declare no model family, so they must never be a confident mismatch —
    /// otherwise every model a user picks for them would be "auto-aligned"
    /// away to another runtime on save.
    #[test]
    fn multi_vendor_shells_never_mismatch() {
        use RuntimeType::*;
        for provider in [Copilot, Cursor, OpenCode, Kiro] {
            for model in [
                "claude-sonnet-4-6",
                "gpt-5.4",
                "gemini-3.5-flash",
                "grok-4",
                "something-unknown",
            ] {
                assert!(
                    model_matches_provider(model, provider),
                    "{provider:?} must accept {model}"
                );
            }
        }
    }

    /// The new single-family runtimes behave like the old ones: they accept
    /// their own family and reject other known families.
    #[test]
    fn new_single_family_runtimes_reject_foreign_families() {
        use RuntimeType::*;
        assert!(model_matches_provider("qwen3-coder-plus", Qwen));
        assert!(!model_matches_provider("claude-sonnet-4-6", Qwen));
        assert!(!model_matches_provider("gpt-5.4", Qwen));
        assert!(model_matches_provider("kimi-for-coding", Kimi));
        assert!(!model_matches_provider("gemini-3.5-flash", Kimi));
        assert!(model_matches_provider("mistral-medium-3.5", Vibe));
        assert!(!model_matches_provider("grok-4", Vibe));
        // …and an unknown family still passes everywhere (never a confident
        // mismatch).
        assert!(model_matches_provider("deepseek-v3.2", Qwen));
    }

    #[test]
    fn read_runtime_json_emits_only_present_keys() {
        let dir = TempDir::new().unwrap();
        // Missing file → empty object.
        assert_eq!(read_runtime_json(dir.path()), serde_json::json!({}));
        // Present [runtime] but only some keys → only those keys emitted;
        // an absent key must stay absent so the frontend can distinguish
        // "unset" from a written value.
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"claude\"\n");
        assert_eq!(
            read_runtime_json(dir.path()),
            serde_json::json!({ "provider": "claude" })
        );
        // Both keys present → both emitted, correct types.
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"codex\"\nfallback = \"claude\"\n",
        );
        assert_eq!(
            read_runtime_json(dir.path()),
            serde_json::json!({ "provider": "codex", "fallback": "claude" })
        );
        // Malformed toml → empty object (fail-safe).
        write_agent_toml(dir.path(), "not valid toml ===");
        assert_eq!(read_runtime_json(dir.path()), serde_json::json!({}));
    }

    #[test]
    fn model_provider_mismatch_detection() {
        use RuntimeType::*;
        // Confident mismatches
        assert!(!model_matches_provider("gpt-5", Claude));
        assert!(!model_matches_provider("gemini-3.1-pro", Claude));
        assert!(!model_matches_provider("claude-sonnet-4-6", Codex));
        // R4: grok models reject other providers; foreign models reject Grok.
        assert!(!model_matches_provider("grok-build-0.1", Claude));
        assert!(!model_matches_provider("claude-sonnet-4-6", Grok));
        assert!(!model_matches_provider("gpt-5.4", Grok));
        assert!(!model_matches_provider("gemini-3.5-flash", Grok));
        // Correct pairings
        assert!(model_matches_provider("claude-haiku-4-5", Claude));
        assert!(model_matches_provider("gpt-5.4-mini", Codex));
        assert!(model_matches_provider("grok-build-0.1", Grok));
        assert!(model_matches_provider("grok-4", Grok));
        // Qualified form + unknown families + compat always pass
        assert!(model_matches_provider("anthropic/claude-sonnet-5", Claude));
        assert!(model_matches_provider("deepseek-v3.2", Claude));
        assert!(model_matches_provider("claude-sonnet-4-6", OpenAiCompat));
    }

    /// v1.84.0: the bare Claude CLI aliases are the Claude family (exact
    /// equality), so they are handled exactly like a `claude-*` id: a
    /// confident mismatch on Codex/Grok, a match on Claude and on familyless
    /// runtimes, and the same preflight refusal / save-time alignment.
    #[test]
    fn bare_claude_aliases_are_the_claude_family_everywhere() {
        use RuntimeType::*;
        for alias in ["opus", "sonnet", "haiku", "fable", "Opus"] {
            assert_eq!(model_family(alias), Some("claude"), "{alias}");
            assert!(model_matches_provider(alias, Claude), "{alias}");
            for foreign in [Codex, Grok, Qwen] {
                assert_eq!(model_matches_provider(alias, foreign), model_matches_provider("claude-opus-4-6", foreign));
                assert!(!model_matches_provider(alias, foreign), "{alias} on {foreign:?}");
                assert!(ensure_model_provider(alias, foreign).is_err());
            }
            assert!(model_matches_provider(alias, OpenAiCompat));
            assert!(model_matches_provider(alias, Kiro));
            assert!(same_model_family(alias, "claude-sonnet-4-6"));
            assert!(!same_model_family(alias, "gpt-5.4"));
            assert_eq!(infer_provider_for_model(alias), Some(Claude));
        }
        // Not prefixes: another vendor's look-alike stays an unknown family.
        for other in ["opus-foo", "my-sonnet", "haiku2"] {
            assert_eq!(model_family(other), None, "{other}");
            assert!(model_matches_provider(other, Codex), "{other}");
            assert!(ensure_model_provider(other, Grok).is_ok());
        }
        // Save-time alignment treats `opus` on Codex exactly like `claude-*`.
        for model in ["opus", "claude-opus-4-6"] {
            let mut table: toml::Table = format!("[model]\npreferred = \"{model}\"\n[runtime]\nprovider = \"codex\"\n").parse().unwrap();
            assert_eq!(align_model_runtime(&mut table).unwrap().aligned_provider(), Some(Claude), "{model}");
        }
    }

    // ── R5 default-direction locks ──────────────────────────────────────
    //
    // These keys moved from hand-written `toml::Value` walks onto the typed
    // `[runtime]` / `[model]` / `[memory]` sections. Each assertion below
    // pins a missing-key direction that is historical behavior, deliberately
    // preserved rather than harmonized. The set is intentionally inconsistent
    // — `provider` absent means "Claude" (a real value) while `fallback`
    // absent means `None` (no value), and `delegation_routing` absent means
    // "defer to the global flag" rather than either boolean. Anyone changing
    // one of these is changing behavior, not tidying a refactor.

    #[test]
    fn default_direction_runtime_provider_absent_is_claude() {
        // Absent ⇒ Claude, i.e. a REAL default, not "unset".
        let dir = TempDir::new().unwrap();
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
        write_agent_toml(dir.path(), "[runtime]\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
        // …and `agent_uses_non_claude` therefore says None (stay on the
        // optimized Claude path) rather than routing through the choke-point.
        assert_eq!(agent_uses_non_claude(dir.path()), None);
    }

    #[test]
    fn default_direction_runtime_fallback_absent_is_none_not_claude() {
        // Contrast with `provider` above: same section, opposite direction.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"codex\"\n");
        assert_eq!(agent_runtime_fallback(dir.path()), None);
    }

    #[test]
    fn default_direction_unknown_provider_warns_and_falls_back_not_errors() {
        // A typo must degrade to Claude, never cost the agent its config.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"claudee\"\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
    }

    #[test]
    fn default_direction_wrong_typed_runtime_key_is_ignored_not_fatal() {
        // Pre-migration this key lived outside the typed schema, so a
        // wrong-typed value was invisible. Typing the section must not turn it
        // into a parse failure that removes the agent from the registry.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = 42\nfallback = true\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
        assert_eq!(agent_runtime_fallback(dir.path()), None);
        assert_eq!(read_runtime_json(dir.path()), serde_json::json!({}));
    }

    #[test]
    fn default_direction_runtime_json_omits_unset_keys() {
        // `read_runtime_json` must distinguish "never written" from a written
        // value; materializing a default here would make the dashboard show a
        // setting the file does not actually carry.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"codex\"\n");
        let json = read_runtime_json(dir.path());
        assert_eq!(json.get("provider").and_then(|v| v.as_str()), Some("codex"));
        assert!(json.get("fallback").is_none(), "unset must be absent");

        // No `[runtime]` at all ⇒ empty object, not a defaults object.
        let empty = TempDir::new().unwrap();
        assert_eq!(read_runtime_json(empty.path()), serde_json::json!({}));
    }

    #[test]
    fn default_direction_utility_model_absent_is_the_shared_constant() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\npreferred = \"claude-sonnet-4-6\"\n");
        assert_eq!(agent_utility_model(dir.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn default_direction_model_fallbacks_absent_is_empty_chain() {
        // Empty ⇒ "no chain" ⇒ the Direct-API path keeps its single-shot
        // behavior byte-identically. Blanks are dropped, not preserved.
        let dir = TempDir::new().unwrap();
        assert!(agent_model_fallbacks(dir.path()).is_empty());
        write_agent_toml(dir.path(), "[model]\n");
        assert!(agent_model_fallbacks(dir.path()).is_empty());
        write_agent_toml(dir.path(), "[model]\nfallbacks = \"not-an-array\"\n");
        assert!(agent_model_fallbacks(dir.path()).is_empty());
        write_agent_toml(
            dir.path(),
            "[model]\nfallbacks = [\"  openai/gpt-5.4 \", \"   \", \"x/y\"]\n",
        );
        assert_eq!(
            agent_model_fallbacks(dir.path()),
            vec!["openai/gpt-5.4".to_string(), "x/y".to_string()]
        );
    }

    #[test]
    fn default_direction_standard_model_blank_counts_as_absent() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\nstandard = \"   \"\n");
        assert_eq!(agent_standard_model(dir.path()), None);
        write_agent_toml(dir.path(), "[model]\nstandard = \" mid \"\n");
        assert_eq!(agent_standard_model(dir.path()), Some("mid".to_string()));
    }

    #[test]
    fn default_direction_delegation_routing_absent_is_none_not_false() {
        // Three-state on purpose: the per-agent key overrides the global flag
        // in BOTH directions, so "unset" must stay distinguishable from an
        // explicit `false` or the override could never turn routing OFF.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\n");
        assert_eq!(agent_delegation_routing(dir.path()), None);
        write_agent_toml(dir.path(), "[model]\ndelegation_routing = false\n");
        assert_eq!(agent_delegation_routing(dir.path()), Some(false));
        write_agent_toml(dir.path(), "[model]\ndelegation_routing = true\n");
        assert_eq!(agent_delegation_routing(dir.path()), Some(true));
    }

    #[test]
    fn default_direction_decision_continuity_absent_is_false() {
        let dir = TempDir::new().unwrap();
        assert!(!decision_continuity_enabled(dir.path()));
        write_agent_toml(dir.path(), "[memory]\n");
        assert!(!decision_continuity_enabled(dir.path()));
        write_agent_toml(dir.path(), "[memory]\ndecision_continuity = true\n");
        assert!(decision_continuity_enabled(dir.path()));
    }

    #[test]
    fn default_direction_decision_ttl_zero_means_default_not_never() {
        // Opposite of `decision_continuity` in the SAME section: the feature
        // switch is opt-in (absent ⇒ off) but the TTL is unconditional
        // (absent OR zero OR negative ⇒ 7 days), because the ledger must not
        // be able to grow unbounded.
        let dir = TempDir::new().unwrap();
        assert_eq!(decision_ttl_days(dir.path()), 7);
        for body in [
            "[memory]\n",
            "[memory]\ndecision_ttl_days = 0\n",
            "[memory]\ndecision_ttl_days = -1\n",
            "[memory]\ndecision_ttl_days = \"30\"\n",
        ] {
            write_agent_toml(dir.path(), body);
            assert_eq!(decision_ttl_days(dir.path()), 7, "for {body:?}");
        }
        write_agent_toml(dir.path(), "[memory]\ndecision_ttl_days = 30\n");
        assert_eq!(decision_ttl_days(dir.path()), 30);
    }

    #[test]
    fn default_direction_missing_and_malformed_file_agree() {
        // Every accessor treats "no file" and "not TOML" identically — none of
        // them ever surfaced an error to the caller.
        let missing = TempDir::new().unwrap();
        let broken = TempDir::new().unwrap();
        write_agent_toml(broken.path(), "[runtime\nprovider = ");
        for dir in [missing.path(), broken.path()] {
            assert_eq!(agent_runtime_provider(dir), RuntimeType::Claude);
            assert_eq!(agent_runtime_fallback(dir), None);
            assert_eq!(agent_utility_model(dir), DEFAULT_UTILITY_MODEL);
            assert!(agent_model_fallbacks(dir).is_empty());
            assert_eq!(agent_standard_model(dir), None);
            assert_eq!(agent_delegation_routing(dir), None);
            assert!(!decision_continuity_enabled(dir));
            assert_eq!(decision_ttl_days(dir), 7);
            assert_eq!(read_runtime_json(dir), serde_json::json!({}));
        }
    }

    #[test]
    fn edits_take_effect_without_a_registry_rescan() {
        // The migrated readers must keep reading the file per call. The
        // registry's `AgentConfig` cache has no mtime invalidation and no
        // watcher, so routing these through it would have turned an immediate
        // read into a stale one — a silent behavior change.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"codex\"\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Codex);
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"grok\"\n");
        assert_eq!(
            agent_runtime_provider(dir.path()),
            RuntimeType::Grok,
            "a live agent.toml edit must be visible on the next call"
        );
    }

    /// A supported provider remains active without a deprecation notice.
    #[test]
    fn grok_provider_is_not_deprecated() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("sales-helper");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(
            &dir,
            "[runtime]\nprovider = \"grok\"\nfallback = \"grok\"\n",
        );
        CONSULTED_NOTICES.with(|n| n.borrow_mut().clear());
        let s = load_runtime_settings(&dir);
        assert_eq!(s.provider, RuntimeType::Grok);
        assert_eq!(s.fallback, Some(RuntimeType::Grok));
        let consulted = CONSULTED_NOTICES.with(|n| n.borrow().clone());
        assert!(consulted.is_empty());
    }

    #[test]
    fn claude_timeout_policy_preserves_defaults_and_clamps_employees() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\n");
        assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 1800);
        assert_eq!(effective_agent_timeout_secs(&dir), 1800);
        for provider in ["Claude", " claude ", "removed-runtime"] {
            write_agent_toml(&dir, &format!("[runtime]\nprovider={provider:?}\n"));
            assert_eq!(load_runtime_settings(&dir).provider, RuntimeType::Claude);
            assert_eq!(runtime_default_timeout(Some(provider), TimeoutPolicy::default()), 1800);
            assert_eq!(effective_agent_timeout_secs(&dir), 1800, "{provider}");
        }
        // RM-181 (v1.84.0): one default for every engine (non-Claude used 300).
        for provider in RuntimeType::ALL.iter().filter(|p| **p != RuntimeType::Claude) {
            write_agent_toml(&dir, &format!("[runtime]\nprovider='{}'\n", provider.as_str()));
            assert_eq!(call_timeout_secs(Some(&dir)), 1800, "{provider:?}");
            assert_eq!(effective_agent_timeout_secs(&dir), 1800, "{provider:?}");
            assert_eq!(runtime_default_timeout(Some(provider.as_str()), TimeoutPolicy::default()), 1800, "{provider:?}");
        }
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\ntimeout_secs=900\n");
        assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 900);
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=600\ndefault_timeout_secs=120\n").unwrap();
        assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 600);
        assert_eq!(call_timeout_secs(Some(&dir)), 600);
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\n");
        assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 120);
        assert_eq!(call_timeout_secs(Some(&dir)), 120);
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=14400\n").unwrap();
        write_agent_toml(&dir, "[runtime]\ntimeout_secs=12000\n");
        assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 12000);
        assert_eq!(call_timeout_secs(Some(&dir)), 12000);
    }

    #[test]
    fn employee_edit_authorization_is_separate_from_effective_timeout() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\n");
        assert_eq!(effective_agent_timeout_secs(&dir), 1800);
        assert_eq!(agent_timeout_authorization_ceiling(&dir), 300);
        write_agent_toml(&dir, "[runtime]\nprovider='claude'\ntimeout_secs=900\n");
        assert_eq!(agent_timeout_authorization_ceiling(&dir), 900);
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=600\n").unwrap();
        assert_eq!(agent_timeout_authorization_ceiling(&dir), 600);
        assert_eq!(effective_agent_timeout_secs(&dir), 600);
    }

    #[test]
    fn ephemeral_codex_role_uses_home_timeout_policy() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/.ephemeral/codex-role");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\nprovider='codex'\ntimeout_secs=1800\n");
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=600\ndefault_timeout_secs=120\n").unwrap();
        assert_eq!(call_timeout_secs(Some(&dir)), 600);
        write_agent_toml(&dir, "[runtime]\nprovider='codex'\n");
        assert_eq!(call_timeout_secs(Some(&dir)), 120);
    }

    #[test]
    fn invalid_timeout_policy_restores_previous_hard_defaults() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        for bad in ["runtime = false", "[runtime]\ndefault_timeout_secs=false", "not toml", "[runtime]\nmax_timeout_secs=0", "[runtime]\nmax_timeout_secs=14401",
            "[runtime]\nmax_timeout_secs='unlimited'", "[runtime]\ndefault_timeout_secs=0",
            "[runtime]\nmax_timeout_secs=60\ndefault_timeout_secs=61"] {
            std::fs::write(home.path().join("config.toml"), bad).unwrap();
            write_agent_toml(&dir, "[runtime]\nprovider='claude'\n");
            assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 1800, "{bad}");
            assert_eq!(call_timeout_secs(Some(&dir)), 1800, "{bad}");
            for provider in RuntimeType::ALL.iter().filter(|p| **p != RuntimeType::Claude) {
                write_agent_toml(&dir, &format!("[runtime]\nprovider='{}'\n", provider.as_str()));
                assert_eq!(effective_agent_timeout_secs(&dir), 1800, "{bad}: {provider:?}");
                assert_eq!(call_timeout_secs(Some(&dir)), 1800, "{bad}: {provider:?}");
            }
            write_agent_toml(&dir, "[runtime]\ntimeout_secs=20000\n");
            assert_eq!(claude_timeout_secs(home.path(), Some(&dir)), 3600);
            assert_eq!(call_timeout_secs(Some(&dir)), 3600);
        }
    }

    #[tokio::test]
    async fn scoped_claude_task_timeout_cannot_exceed_global_ceiling() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=600\n").unwrap();
        let got = TASK_TIMEOUT_SECS.scope(Some(("anna".into(), 1800)), async {
            claude_timeout_secs(home.path(), Some(&dir))
        }).await;
        assert_eq!(got, 600);
        std::fs::write(home.path().join("config.toml"), "[runtime]\nmax_timeout_secs=7200\n").unwrap();
        let got = TASK_TIMEOUT_SECS.scope(Some(("anna".into(), 6000)), async {
            claude_timeout_secs(home.path(), Some(&dir))
        }).await;
        assert_eq!(got, 6000);
    }

}

#[path = "runtime_config/alignment.rs"]
mod alignment;
pub(crate) use alignment::{ModelRuntimeAlignment, align_model_runtime, ensure_model_provider};
pub(crate) use alignment::{model_substitution_notice, model_substitution_notices, runnable_model};
#[cfg(test)]
pub(crate) use alignment::bare_claude_alias_substitute;
#[cfg(test)]
pub(crate) use alignment::testing;

/// Zero disables the event-silence deadline. Invalid reads restore the built-in window.
pub(crate) const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 900;
pub(crate) fn validate_idle_timeout(value: i64, maximum: u64) -> Result<u64, String> {
    u64::try_from(value).ok().filter(|v| *v == 0 || (60..=maximum).contains(v))
        .ok_or_else(|| format!("idle timeout must be 0 or 60..={maximum} seconds"))
}
pub(crate) fn idle_policy_checked(home: &Path) -> Result<u64, String> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DEFAULT_IDLE_TIMEOUT_SECS),
        Err(e) => return Err(e.to_string()),
    };
    idle_policy_from_table(&text.parse::<toml::Table>().map_err(|e| e.to_string())?)
}
pub(crate) fn idle_policy_from_table(table: &toml::Table) -> Result<u64, String> {
    let maximum = TimeoutPolicy::from_table(table)?.max_secs;
    let Some(runtime) = table.get("runtime") else { return Ok(DEFAULT_IDLE_TIMEOUT_SECS.min(maximum)); };
    let runtime = runtime.as_table().ok_or("invalid runtime table")?;
    match runtime.get("default_idle_timeout_secs") {
        Some(v) => validate_idle_timeout(v.as_integer().ok_or("idle timeout must be integer seconds")?, maximum),
        None => Ok(DEFAULT_IDLE_TIMEOUT_SECS.min(maximum)),
    }
}
pub(crate) fn idle_timeout_secs(home: &Path, dir: Option<&Path>) -> u64 {
    let resolve = || -> Result<u64, String> {
        let default = idle_policy_checked(home)?;
        let Some(dir) = dir else { return Ok(default); };
        let text = match duduclaw_core::agent_toml::read_effective_text(dir) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(default),
            Err(e) => return Err(e.to_string()),
        };
        let table = text.parse::<toml::Table>().map_err(|e| e.to_string())?;
        let Some(runtime) = table.get("runtime") else { return Ok(default); };
        let runtime = runtime.as_table().ok_or("invalid runtime table")?;
        match runtime.get("idle_timeout_secs") {
            Some(v) => validate_idle_timeout(v.as_integer().ok_or("idle timeout must be integer seconds")?, TimeoutPolicy::load(home).max_secs),
            None => Ok(default),
        }
    };
    resolve().unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS)
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    #[test]
    fn idle_policy_resolution_validation_and_read_errors() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(idle_timeout_secs(home.path(), Some(&dir)), 900);
        std::fs::write(home.path().join("config.toml"), "[runtime]\ndefault_idle_timeout_secs=1200\n").unwrap();
        assert_eq!(idle_timeout_secs(home.path(), Some(&dir)), 1200);
        for (raw, expected) in [("60",60), ("0",0), ("'bad'",900), ("59",900), ("3601",900)] {
            std::fs::write(dir.join("agent.toml"), format!("[runtime]\nidle_timeout_secs={raw}\n")).unwrap();
            assert_eq!(idle_timeout_secs(home.path(), Some(&dir)), expected);
        }
        std::fs::write(home.path().join("config.toml"), "[broken").unwrap();
        assert!(idle_policy_checked(home.path()).is_err());
        assert_eq!(idle_timeout_secs(home.path(), Some(&dir)), 900);
        for raw in ["59", "-1", "3601", "'bad'"] {
            assert!(idle_policy_from_table(&format!("[runtime]\ndefault_idle_timeout_secs={raw}").parse().unwrap()).is_err());
        }
    }
}

/// RM-182: missing keys preserve first-message extraction; unreadable files,
/// malformed tables/types and unknown values fail closed. Employee settings
/// are intersected with the installation setting and can never enable it.
pub fn pinned_task_enabled(home: &Path, agent_dir: Option<&Path>) -> bool {
    fn read(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else { return false; };
        let Ok(doc) = text.parse::<toml::Value>() else { return false; };
        let value = match doc.get("memory") {
            None => return true,
            Some(memory) => match memory.as_table() {
                None => return false,
                Some(memory) => memory.get("pinned_task"),
            },
        };
        match value {
            None => true,
            Some(value) if value.as_str() == Some("first_message") => true,
            Some(value) if value.as_str() == Some("off") => false,
            Some(_) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| tracing::warn!("Unknown [memory] pinned_task value; treating it as off"));
                false
            }
        }
    }
    let global = read(&home.join("config.toml"));
    let employee = agent_dir.is_some_and(|dir| read(&dir.join("agent.toml")));
    global && employee
}

#[cfg(test)]
mod pinned_task_setting_tests {
    use super::*;

    #[test]
    fn off_and_unknown_fail_closed_and_agent_only_narrows() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agent");
        std::fs::create_dir(&agent).unwrap();
        for (global, employee, enabled) in [
            ("", "", true),
            ("first_message", "first_message", true),
            ("off", "first_message", false),
            ("first_message", "off", false),
            ("unknown", "first_message", false),
            ("first_message", "unknown", false),
        ] {
            for (path, value) in [(home.path().join("config.toml"), global), (agent.join("agent.toml"), employee)] {
                std::fs::write(path, if value.is_empty() { String::new() }
                    else { format!("[memory]\npinned_task = {value:?}\n") }).unwrap();
            }
            assert_eq!(pinned_task_enabled(home.path(), Some(&agent)), enabled);
        }
        std::fs::write(agent.join("agent.toml"), "[memory]\npinned_task = true").unwrap();
        assert!(!pinned_task_enabled(home.path(), Some(&agent)));
        assert!(!pinned_task_enabled(home.path(), None));
        std::fs::write(agent.join("agent.toml"), "invalid = [").unwrap();
        assert!(!pinned_task_enabled(home.path(), Some(&agent)));
        std::fs::remove_file(agent.join("agent.toml")).unwrap();
        assert!(!pinned_task_enabled(home.path(), Some(&agent)));
    }
}
