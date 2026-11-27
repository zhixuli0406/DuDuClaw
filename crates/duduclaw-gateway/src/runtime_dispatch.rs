//! RFC-25 Phase 1 — provider-agnostic choke-point for agent prompting.
//!
//! `run_agent_prompt` is the single entry every internal caller (channel reply,
//! GVU, skill synthesis, delegation, A2A) should use instead of hardcoding the
//! Claude CLI. It resolves the agent's `[runtime] provider`, selects the matching
//! `AgentRuntime` from a process-wide `RuntimeRegistry` (auto-detected once at
//! first use), and executes — falling back to the configured fallback provider,
//! with no extra runtime hop when fallback is absent.

use crate::failover::{RuntimeDispatchFailure, RuntimePreflightKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use duduclaw_core::types::RuntimeType;

use crate::cost_telemetry::{RequestType, TokenUsage};
use crate::runtime::{ConversationTurn, RuntimeContext, RuntimeRegistry, RuntimeResponse};
use crate::runtime_config::RuntimeSettings;

tokio::task_local! {
    // Owned employee snapshot, scoped over one chat or background execution.
    // It survives registry unlock, rotation and runtime fallback, not spawn().
    static CHAT_TURN_CONFIG: std::sync::Arc<crate::entry_gate::ResolvedTurnConfig>;
}

pub(crate) async fn with_chat_turn_config<F: std::future::Future>(
    snapshot: std::sync::Arc<crate::entry_gate::ResolvedTurnConfig>, future: F,
) -> F::Output {
    CHAT_TURN_CONFIG.scope(snapshot, future).await
}

/// None means this is a different employee or a caller without a snapshot.
/// Once Some is returned, absent fields are authoritative: never reread disk.
pub(crate) fn chat_turn_config(
    agent_dir: Option<&Path>,
) -> Option<std::sync::Arc<crate::entry_gate::ResolvedTurnConfig>> {
    if in_utility_call() { return None; }
    CHAT_TURN_CONFIG.try_with(|turn| {
        (agent_dir == Some(turn.registry_agent.dir.as_path())).then(|| turn.clone())
    }).ok().flatten()
}

/// Prompt assembly shares the admitted employee document just like runtime routing.
pub(crate) fn chat_profile(agent_dir: &Path) -> std::io::Result<duduclaw_core::profile_sections::ProfileSections> {
    use duduclaw_core::profile_sections::ProfileSections;
    match chat_turn_config(Some(agent_dir)) {
        Some(turn) => {
            let config = turn.document.as_table().ok_or_else(||
                std::io::Error::new(std::io::ErrorKind::InvalidData, "turn configuration is not a table"))?;
            ProfileSections::load_with_config(agent_dir, config)
        }
        None => ProfileSections::load(agent_dir),
    }
}

/// Preserve read_agent_effort's parser semantics, including absence/invalid
/// values meaning provider default, without falling through to another read.
pub(crate) fn chat_effort(agent_dir: Option<&Path>) -> Option<duduclaw_core::effort::Effort> {
    match chat_turn_config(agent_dir) {
        Some(turn) => {
            let value = turn.document.get("model")?.get("effort")?.as_str()?;
            match value.parse() {
                Ok(effort) => Some(effort),
                Err(()) => {
                    tracing::warn!(agent_dir = %turn.registry_agent.dir.display(), value,
                        "agent.toml [model] effort is not one of low/medium/high/xhigh/max — ignoring");
                    None
                }
            }
        }
        None => agent_dir.and_then(duduclaw_core::effort::read_agent_effort),
    }
}

pub(crate) fn chat_fallback_model(agent_dir: &Path) -> Option<String> {
    match chat_turn_config(Some(agent_dir)) {
        Some(turn) => turn.sections.model.fallback.as_deref()
            .map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        None => crate::runtime_config::agent_fallback_model(agent_dir),
    }
}

/// Per-`home_dir` registry cache (RFC-25 R2).
///
/// Previously a single `OnceCell` bound the registry to the *first* `home_dir`
/// ever passed — wrong for multi-home / multi-process-test setups (production
/// single-home was unaffected). Keyed by home so each home auto-detects its own
/// runtimes.
///
/// RM-181 (v1.84.0): an entry is REPLACEABLE. It used to be leaked once and
/// never refreshed, so a runtime installed after the first dispatch stayed
/// "not registered" until a restart. [`invalidate_all_registries`] (after a
/// successful install) drops the cached entries, [`note_runtime_detection`]
/// (after `runtime.detect`) drops one home's entry when its detection changed;
/// a dispatch that finds a configured runtime missing re-probes at most once
/// per [`REGISTRY_REPROBE_INTERVAL`]. A call keeps the `Arc` it started with.
///
/// M1 (v1.84 security review): the process-wide map is a synchronous mutex
/// held only to look up, insert or drop one home's cell — never across the
/// CLI probes. Each home's build runs inside its own `OnceCell`, outside that
/// lock: concurrent first calls for one home share one build, and a rebuild
/// of one home never delays a dispatch on another. Invalidation drops the
/// cell (a build still running on it completes for its own waiters only).
static REGISTRIES: OnceLock<std::sync::Mutex<HashMap<PathBuf, RegistryCell>>> = OnceLock::new();

type RegistryCell = std::sync::Arc<tokio::sync::OnceCell<std::sync::Arc<RuntimeRegistry>>>;

fn registry_cells() -> std::sync::MutexGuard<'static, HashMap<PathBuf, RegistryCell>> {
    REGISTRIES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The registry currently cached for `home_dir`, without building one.
fn cached_registry(home_dir: &Path) -> Option<std::sync::Arc<RuntimeRegistry>> {
    registry_cells().get(home_dir).and_then(|cell| cell.get().cloned())
}

/// Per-home minimum time between two registry invalidations triggered by
/// `runtime.detect` (M1), whatever the detection result.
pub(crate) const DETECT_INVALIDATION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// When `runtime.detect` last invalidated each home's registry.
static DETECT_INVALIDATIONS: OnceLock<std::sync::Mutex<HashMap<PathBuf, std::time::Instant>>> = OnceLock::new();

/// M1: `runtime.detect` reports what it found; only THIS home's registry is
/// dropped, and only when the detection (installed runtimes and their
/// resolved paths) differs from what the cached registry was built from, at
/// most once per [`DETECT_INVALIDATION_INTERVAL`] per home. Any signed-in
/// caller may run `runtime.detect`, so repeated calls must never defeat the
/// re-probe rate limit or touch other homes. Returns whether it invalidated.
pub(crate) fn note_runtime_detection(
    home_dir: &Path,
    detected: &std::collections::BTreeMap<String, String>,
) -> bool {
    // Nothing cached: the next dispatch builds from a fresh probe anyway.
    let Some(current) = cached_registry(home_dir) else { return false };
    if current.detection() == detected {
        return false;
    }
    let now = std::time::Instant::now();
    {
        let mut last = DETECT_INVALIDATIONS.get_or_init(Default::default).lock()
            .unwrap_or_else(|e| e.into_inner());
        match last.get(home_dir) {
            Some(at) if now.duration_since(*at) < DETECT_INVALIDATION_INTERVAL => return false,
            _ => { last.insert(home_dir.to_path_buf(), now); }
        }
    }
    tracing::info!("runtime.detect found a different set of installed runtimes — the next dispatch re-probes this home");
    drop_registry_cell(home_dir);
    true
}

fn drop_registry_cell(home_dir: &Path) {
    registry_cells().remove(home_dir);
}

/// Rate limit of the "configured runtime not registered" re-probe, per home.
pub(crate) const REGISTRY_REPROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// When each home last re-probed because a configured runtime was missing.
static REGISTRY_REPROBES: OnceLock<std::sync::Mutex<HashMap<PathBuf, std::time::Instant>>> = OnceLock::new();

/// Cooldown for a provider that trips its failure threshold (RFC-25 R1).
const FAILOVER_COOLDOWN_SECS: i64 = 60;

/// Per-`home_dir` failover managers (RFC-25 R1, per-(home,provider) granularity).
///
/// Health is keyed per home, not process-globally: provider availability is
/// partly config-driven (an OpenAI-compat `base_url` / API key lives in a home's
/// `config.toml`), so one home's misconfigured endpoint must NOT trip the same
/// `RuntimeType`'s health for another home. Each manager internally keys by
/// `RuntimeType`. Managers live for the process lifetime (homes are few), so each
/// is `Box::leak`ed for a `&'static` ref.
static FAILOVERS: OnceLock<
    std::sync::Mutex<HashMap<PathBuf, &'static crate::failover::FailoverManager>>,
> = OnceLock::new();

fn failover(home_dir: &Path) -> &'static crate::failover::FailoverManager {
    let map = FAILOVERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    if let Some(mgr) = guard.get(home_dir) {
        return mgr;
    }
    let built: &'static crate::failover::FailoverManager = Box::leak(Box::new(
        crate::failover::FailoverManager::new(FAILOVER_COOLDOWN_SECS),
    ));
    guard.insert(home_dir.to_path_buf(), built);
    built
}

/// Get (or lazily build) the runtime registry for `home_dir`.
pub async fn registry(home_dir: &Path) -> std::sync::Arc<RuntimeRegistry> {
    #[cfg(test)]
    if let Some(registry) = testing::owned_registry(home_dir) {
        return registry;
    }
    // The map lock is released before any probing (the guard is a temporary
    // of this statement); a concurrent first call for the same home waits on
    // the same cell rather than double-building.
    let cell = registry_cells().entry(home_dir.to_path_buf()).or_default().clone();
    cell.get_or_init(|| async { std::sync::Arc::new(build_registry(home_dir).await) })
        .await
        .clone()
}

async fn build_registry(home_dir: &Path) -> RuntimeRegistry {
    #[cfg(test)]
    if let Some(build) = testing::registry_builder(home_dir) {
        return build();
    }
    RuntimeRegistry::new(home_dir).await
}

/// Drop `home_dir`'s cached registry: the next dispatch probes the host
/// again (same probes as the first build). In-flight calls keep theirs.
pub async fn invalidate_registry(home_dir: &Path) {
    drop_registry_cell(home_dir);
}

/// Drop every home's cached registry. An installed or removed CLI is a host
/// fact, not a per-home one, so a successful admin install refreshes all
/// homes. `runtime.detect` does NOT use this (see [`note_runtime_detection`]).
pub async fn invalidate_all_registries() {
    registry_cells().clear();
}

/// A configured runtime is not in `current`: re-probe once, rate limited to
/// one rebuild per [`REGISTRY_REPROBE_INTERVAL`] per home. Returns the
/// registry to use for this call (the fresh one, or `current` when the
/// window has not elapsed).
async fn reprobe_for_missing_runtime(
    home_dir: &Path,
    current: std::sync::Arc<RuntimeRegistry>,
    missing: RuntimeType,
) -> std::sync::Arc<RuntimeRegistry> {
    #[cfg(test)]
    if testing::owned_registry(home_dir).is_some() {
        return current;
    }
    let now = std::time::Instant::now();
    let due = {
        let mut last = REGISTRY_REPROBES.get_or_init(Default::default).lock().unwrap();
        match last.get(home_dir) {
            Some(at) if now.duration_since(*at) < REGISTRY_REPROBE_INTERVAL => false,
            _ => {
                last.insert(home_dir.to_path_buf(), now);
                true
            }
        }
    };
    if !due {
        return current;
    }
    tracing::info!(runtime = missing.as_str(), "configured runtime not registered — re-probing installed runtimes");
    invalidate_registry(home_dir).await;
    registry(home_dir).await
}

/// Why a model runs, stated by every caller of the two spawn choke points
/// ([`run_agent_prompt_detailed`] and the Claude CLI spawn). There is no
/// default: a caller that does not say what it is cannot reach a spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPurpose {
    /// An employee's own work (a channel reply, a dispatch). Refused without
    /// that employee's directory (spawn-gate refusal, no fallback).
    EmployeeTurn,
    /// Agent-less helper work through a restricted lane (the utility lane;
    /// on the Claude CLI also an explicit tool allowlist).
    Utility,
}

/// Parameters for a provider-agnostic agent prompt.
pub struct AgentPrompt<'a> {
    pub agent_dir: Option<&'a Path>,
    pub home_dir: &'a Path,
    pub agent_id: &'a str,
    pub prompt: &'a str,
    pub system_prompt: &'a str,
    /// Model id within the chosen provider (e.g. claude-sonnet-4-6, grok-4.3).
    pub model: &'a str,
    pub max_tokens: u32,
    /// Force a specific provider, bypassing `agent_dir`-based resolution.
    /// Used by utility dispatch where the provider is already resolved (including
    /// the global-config path for agent-less tasks). `None` ⇒ resolve from `agent_dir`.
    pub provider_override: Option<RuntimeType>,
    /// Prior conversation turns (chronological, newest last; excludes the current
    /// prompt). Threaded into non-Claude runtimes so Codex/Gemini/OpenAI agents
    /// keep multi-turn context (RFC-25 A1). `&[]` for single-shot / utility calls.
    pub conversation_history: &'a [ConversationTurn],
    /// Cost-telemetry classification for this call (RFC-25 A3). Recorded against
    /// the resolved provider/model so non-Claude usage is visible to
    /// `CostTelemetry` / 200K warnings / adaptive routing.
    pub request_type: RequestType,
    /// Pre-parsed `agent.toml` runtime settings (RFC-25 L7 followup). When the
    /// caller already loaded these (e.g. to make the non-Claude routing decision),
    /// pass them here so the choke-point doesn't re-read + re-parse the file.
    /// `None` ⇒ the choke-point reads from `agent_dir` itself.
    pub runtime_settings: Option<&'a RuntimeSettings>,
    /// Per-call reasoning effort override (P1/WP-3). `None` ⇒ the choke-point
    /// falls back to the agent's own `agent.toml [model] effort`, so ordinary
    /// dispatch/cron/heartbeat callers pass `None` and still honour the
    /// agent's setting. `Some` is for callers that decide effort themselves —
    /// today the utility/judge path via [`UtilityModelHint::effort`], tomorrow
    /// a team role spec `{runtime, model, effort}`.
    pub effort: Option<duduclaw_core::effort::Effort>,
    /// May this call fail over to a runtime serving a different model family?
    /// (P0/WP-B follow-up — see
    /// [`RuntimeContext::allow_cross_family_failover`].)
    ///
    /// `true` for every ordinary caller (byte-identical failover behavior).
    /// [`run_utility_prompt_with_hint`] passes `false` when an operator hint
    /// moved the provider, so a decorrelated judge can never be silently
    /// answered by the worker's family.
    pub allow_cross_family_failover: bool,
    /// Employee turn or agent-less utility work; see [`SpawnPurpose`].
    pub purpose: SpawnPurpose,
}

/// Execute a prompt through the agent's configured runtime provider.
///
/// Primary: `provider_override` → `[runtime] provider` (default Claude).
/// Runtime fallback is attempted only when explicitly configured.
// OTel GenAI semconv (Development): `chat` span for one model call through
// the multi-runtime choke-point. Attribute names centralized in `crate::otel`.
// Provider / effective model / usage are known only after failover resolution,
// so they are declared Empty and `Span::record`ed below.
#[tracing::instrument(
    name = "chat",
    skip_all,
    fields(
        gen_ai.operation.name = "chat",
        gen_ai.system = tracing::field::Empty,
        gen_ai.provider.name = tracing::field::Empty,
        gen_ai.agent.name = %req.agent_id,
        gen_ai.request.model = %req.model,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
    )
)]
pub async fn run_agent_prompt_detailed(
    req: AgentPrompt<'_>,
) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
    run_agent_prompt_scoped(req, None).await
}

tokio::task_local! {
    /// Applies only to the new Claude continuation; older primary paths keep
    /// their existing environment behavior.
    pub(crate) static CLAUDE_FALLBACK_SPAWN: ();
}

pub(crate) fn in_claude_runtime_fallback() -> bool {
    CLAUDE_FALLBACK_SPAWN.try_with(|_| ()).is_ok()
}

pub(crate) fn apply_claude_fallback_spawn_env(
    cmd: &mut tokio::process::Command, caps: Option<&duduclaw_core::types::CapabilitiesConfig>,
) {
    if in_claude_runtime_fallback() {
        // Use the common environment list without changing the runtime's
        // namespace policy: Codex/Grok native sandboxes may need namespaces.
        cmd.env_clear();
        cmd.envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs_for(caps));
        let granted = duduclaw_core::spawn_env::git_credentials_granted_names(caps);
        if !granted.is_empty() {
            tracing::info!(names = ?granted, "runtime fallback: forwarding granted git credential references");
        }
    }
}

/// Keep Claude primary successes in the same health ledger as its failures.
pub(crate) async fn record_claude_runtime_success(home: &Path) {
    failover(home).record_success(&RuntimeType::Claude).await;
}

/// Continue a failed Claude call through the same dispatch context and spawn
/// gates, without executing the primary again. None preserves caller stop logic.
pub(crate) async fn run_claude_runtime_fallback(
    req: AgentPrompt<'_>, error: &str,
) -> Option<Result<RuntimeResponse, RuntimeDispatchFailure>> {
    let fallback = req.runtime_settings.map(|s| s.fallback)
        .unwrap_or_else(|| req.agent_dir.and_then(crate::runtime_config::agent_runtime_fallback));
    if !crate::failover::claude_runtime_fallback_eligible(error, fallback.as_ref()) {
        return None;
    }
    Some(CLAUDE_FALLBACK_SPAWN.scope((), run_agent_prompt_scoped(req, Some(error))).await)
}

async fn run_agent_prompt_scoped(
    req: AgentPrompt<'_>, primary_error: Option<&str>,
) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
    // Direct dispatch callers also need the marker; Chat preserves the
    // WebChat/shared-member scope (or the absence of one for ordinary channels).
    let dispatch = req.request_type == RequestType::Dispatch;
    // Utility request labels are not employee identities; the host-owned
    // agent directory binds the owner, while agent-less calls use global state.
    let owner = req
        .agent_dir
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let run = Box::pin(crate::process_group::with_class(crate::process_group::request_class(req.request_type),
    crate::autonomous_runtime::request_scope(
        req.home_dir,
        &owner,
        req.request_type != RequestType::Chat
            && !(in_utility_call()
                && crate::autonomous_runtime::is_human_interactive(req.home_dir)),
        crate::computer_use_sessions::attended_call::runtime_scope(
            req.request_type == RequestType::Chat,
            crate::on_behalf_of::context::scope(
                req.home_dir,
                req.agent_id,
                crate::runtime_turn::scope(req.agent_id, run_agent_prompt_inner(req, primary_error)),
            ),
        ),
    )));
    if dispatch {
        crate::on_behalf_of::access::MAIN_TURN_USE.scope(
            crate::on_behalf_of::access::MainUse::Assign, run,
        ).await
    } else {
        run.await
    }
}

/// Backward-compatible String view; execution/usage happens only in detailed.
pub async fn run_agent_prompt(req: AgentPrompt<'_>) -> Result<RuntimeResponse, String> {
    run_agent_prompt_detailed(req)
        .await
        .map_err(|error| error.legacy_message())
}

async fn run_agent_prompt_inner(
    req: AgentPrompt<'_>, primary_error: Option<&str>,
) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
    // Every runtime (Codex, Grok, OpenAI-compatible, generic CLIs, Claude via
    // failover) would otherwise start an employee's work with no employee
    // directory: gateway cwd, no MCP config, no hook. The caller's stated
    // purpose decides; the channel-turn task-local is a second refusal.
    // Preflight + spawn-gate prefix: no fallback.
    if req.agent_dir.is_none()
        && (req.purpose == SpawnPurpose::EmployeeTurn
            || (!in_utility_call() && crate::channel_reply::in_channel_reply_turn()))
    {
        tracing::warn!(purpose = ?req.purpose, "runtime dispatch without an employee directory — refused");
        return Err(RuntimeDispatchFailure::preflight(
            req.provider_override,
            req.model,
            RuntimePreflightKind::Config,
            crate::channel_reply::no_employee_spawn_refusal(),
        ));
    }
    crate::isolation_readiness::check_execution().map_err(|error|
        RuntimeDispatchFailure::preflight(req.provider_override, req.model, RuntimePreflightKind::Config, error))?;
    let snapshot = chat_turn_config(req.agent_dir);
    // Knowledge-base tool calls made during this call learn who the turn is
    // for from the gateway's own record (every runtime, not just Claude).
    // Nothing is registered outside a channel turn; dropped on return.
    let _knowledge_turn = (!req.agent_id.is_empty())
        .then(|| crate::knowledge::turn_requesters::register_current_turn(req.agent_id))
        .flatten();
    let _computer_turn = crate::turn_context::register_current_turn(req.agent_id);
    // Reuse caller-provided settings when present (RFC-25 L7 followup — avoids a
    // second agent.toml read on paths that already parsed it to decide routing);
    // otherwise read once here.
    let owned_settings;
    let settings: &RuntimeSettings = match req.runtime_settings {
        Some(s) => s,
        None => match snapshot.as_ref().map(|turn| &turn.runtime_settings) {
            Some(s) => s,
            None => {
                owned_settings = match req.agent_dir {
                    Some(dir) => crate::runtime_config::load_runtime_settings_strict(dir)
                        .map_err(|error| RuntimeDispatchFailure::preflight(
                            req.provider_override, req.model, RuntimePreflightKind::Config,
                            crate::entry_gate::background_refusal(&crate::entry_gate::EntryRefusal::Config(error)),
                        ))?,
                    None => RuntimeSettings::default(),
                };
                &owned_settings
            }
        },
    };
    // Snapshots already passed this gate. Legacy direct callers still check
    // removed runtime/team selectors, but never recover from an invalid read.
    if let Some(dir) = req.agent_dir.filter(|_| snapshot.is_none()) {
        crate::runtime_config::ensure_active_agent_provider(dir).map_err(|diagnostic|
            RuntimeDispatchFailure::preflight(req.provider_override, req.model, RuntimePreflightKind::Config,
                crate::entry_gate::background_refusal(&crate::entry_gate::EntryRefusal::InactiveRuntime { diagnostic })))?;
    }
    let provider = req.provider_override.unwrap_or(settings.provider);
    // v1.84.0: a bare Claude alias saved on a prefixed non-Claude engine runs
    // with that engine's catalog default (warned once per agent) instead of
    // being refused; every other mismatch keeps the refusal below.
    let runnable_model = crate::runtime_config::runnable_model(req.agent_id, req.model, provider);
    let model: &str = &runnable_model;
    crate::runtime_config::ensure_model_provider(model, provider).map_err(|error|
        RuntimeDispatchFailure::preflight(Some(provider), model, RuntimePreflightKind::Config,
            if req.agent_dir.is_some() {
                crate::entry_gate::background_refusal(&crate::entry_gate::EntryRefusal::ModelProviderMismatch {
                    model: model.to_owned(), provider,
                })
            } else { crate::runtime_config::alignment_refusal_with_fallback(error, settings.fallback) }))?;
    crate::product_policy::require_runtime(req.home_dir, provider.as_str()).map_err(|error|RuntimeDispatchFailure::preflight(Some(provider),model,RuntimePreflightKind::Policy,error))?;
    // F5: only a catalog default DuDuClaw substituted is attributed to its
    // runtime's vendor; every other model keeps the v1.83.0 selector rule
    // (checked again by the failover manager for the model it runs).
    if matches!(runnable_model, std::borrow::Cow::Owned(_)) {
        crate::product_policy::load(req.home_dir)
            .and_then(|policy| crate::product_policy::require_substituted_catalog_model(policy, provider, model))
            .map_err(|error|RuntimeDispatchFailure::preflight(Some(provider),model,RuntimePreflightKind::Policy,error))?;
    }
    if in_utility_call() && !crate::runtime_config::utility_profile_supported(provider) {
        return Err(RuntimeDispatchFailure::preflight(Some(provider), model,
            RuntimePreflightKind::Config, crate::runtime_config::utility_profile_refusal(provider)));
    }
    let fallback = if in_utility_call() { None } else { settings.fallback };

    // Budget circuit breaker (cost *enforcement*, not just observation). Blocks
    // a new LLM call when the agent's rolling spend has hit its hard cap. Inert
    // when no `[budget]` cap is set or `hard_stop = false`; unavailable data
    // follows the employee policy. Calls without an employee are exempt.
    crate::budget::gate_before_spawn(req.home_dir, req.agent_dir, req.agent_id, "runtime_dispatch")
        .await.map_err(|e| RuntimeDispatchFailure::preflight(Some(provider), model, RuntimePreflightKind::Budget, e.to_string()))?;

    let mut reg = registry(req.home_dir).await;
    // RM-181: a runtime this employee is configured to use (primary or
    // fallback) may have been installed after the registry was built.
    let missing = std::iter::once(provider).chain(fallback).find(|rt| reg.get(rt).is_none());
    if let Some(missing) = missing {
        reg = reprobe_for_missing_runtime(req.home_dir, reg, missing).await;
    }

    let ctx = RuntimeContext {
        agent_dir: req.agent_dir.map(PathBuf::from),
        system_prompt: req.system_prompt.to_string(),
        model: model.to_string(),
        max_tokens: req.max_tokens,
        home_dir: req.home_dir.to_path_buf(),
        agent_id: req.agent_id.to_string(),
        preferred_provider: None,
        conversation_history: crate::runtime::cap_conversation_history(req.conversation_history),
        // Capability enforcement (W1): resolved from `agent.toml [capabilities]`
        // so every runtime (Claude AND non-Claude) receives the agent's tool
        // restrictions. `None` only for agent-less utility calls (no agent_dir).
        // TODO(W6): capabilities and account_pool remain raw agent.toml reads pending a scope decision.
        capabilities: req
            .agent_dir
            .and_then(crate::runtime::load_agent_capabilities),
        // G1: the agent's `[model] account_pool`, so a runtime that rotates
        // DuDuClaw-managed accounts (the Claude CLI runtime, incl. failover
        // substitutions) honors the same pool the direct paths do. Empty for
        // agent-less utility calls.
        account_pool: req
            .agent_dir
            .map(crate::runtime::load_agent_account_pool)
            .unwrap_or_default(),
        // P1/WP-3: an explicit per-call effort (utility/judge hint, and later a
        // team role spec) wins; otherwise fall back to the agent's own
        // `agent.toml [model] effort`. Both absent ⇒ `None`, and every runtime
        // builds a byte-identical argv.
        effort: req.effort.or_else(|| chat_effort(req.agent_dir)),
        allow_cross_family_failover: req.allow_cross_family_failover && !in_utility_call(),
    };

    // The singular fallback precedes the cross-provider chain. Runtime failover uses
    // it to pick a model the *fallback* runtime actually serves instead of
    // forwarding the primary's model id verbatim (a codex agent's `gpt-5.4`
    // used to be handed to the Claude runtime). Empty for agent-less utility
    // calls and for agents that configure no candidates — the resolver then
    // falls through to `context.model` / the runtime catalog default.
    let model_fallbacks = match snapshot.as_ref() {
        Some(turn) => turn.sections.model.fallback.iter().chain(&turn.sections.model.fallbacks)
            .map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).collect(),
        None => req.agent_dir.map(crate::runtime_config::agent_model_fallbacks).unwrap_or_default(),
    };

    // RFC-25 R1: route through the FailoverManager so provider health is tracked
    // (3 consecutive failures → cooldown) and a failing primary auto-falls back to
    // the explicitly configured fallback (None means no extra runtime hop)
    // instead of the old one-shot `select().execute()` with no health memory.
    // Set by a producer that had to estimate this call's usage.
    let usage_estimated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resp = crate::cost_telemetry::USAGE_ESTIMATED
        .scope(
            usage_estimated.clone(),
            RUNTIME_USAGE_REQUEST_TYPE.scope(req.request_type, async {
                if let Some(error) = primary_error {
                    failover(req.home_dir).after_claude_failure(&reg, fallback.as_ref(), req.prompt,
                        &ctx, &model_fallbacks, error).await
                } else {
                    failover(req.home_dir).execute_with_failover_detailed(&reg, &provider,
                        fallback.as_ref(), req.prompt, &ctx, &model_fallbacks).await
                }
            }),
        )
        .await?;
    let usage_estimated = usage_estimated.load(std::sync::atomic::Ordering::Relaxed);
    // OTel: record the provider that actually answered (post-failover) and
    // usage on the `chat` span (see `crate::otel`). `gen_ai.request.model`
    // stays the *requested* model per semconv; `resp.model_used` may differ
    // on failover and is already visible via cost telemetry.
    {
        let span = tracing::Span::current();
        span.record(crate::otel::attrs::SYSTEM, resp.runtime_name.as_str());
        span.record(
            crate::otel::attrs::PROVIDER_NAME,
            resp.runtime_name.as_str(),
        );
        span.record(crate::otel::attrs::USAGE_INPUT_TOKENS, resp.input_tokens);
        span.record(crate::otel::attrs::USAGE_OUTPUT_TOKENS, resp.output_tokens);
    }
    // RFC-25 A3: record token usage so non-Claude (Codex/Gemini/OpenAI) calls are
    // visible to CostTelemetry / 200K price-cliff warnings / adaptive routing.
    // Ordinary calls remain best-effort and detached. Team-role calls await
    // the write so a short-lived isolated round does not lose its measured
    // cost when it tears down. Skip agent-less utility calls (empty agent_id)
    // to avoid empty-string attribution.
    if !req.agent_id.is_empty() {
        let home = req.home_dir.to_path_buf();
        let agent_id = req.agent_id.to_string();
        let request_type = req.request_type;
        let model = resp.model_used.clone();
        let role_attribution = crate::runtime::ROLE_COST_ATTRIBUTION
            .try_with(Clone::clone)
            .ok();
        let usage = TokenUsage {
            input_tokens: resp.input_tokens,
            cache_read_tokens: resp.cache_read_tokens,
            cache_creation_tokens: 0,
            output_tokens: resp.output_tokens,
        };
        if role_attribution.is_some() {
            // A team round can finish and tear down its isolated home as soon
            // as this call returns. Persist role usage before that happens so
            // P2b and the per-role ledger cannot lose the measured cost.
            record_usage(
                home,
                agent_id,
                request_type,
                model,
                usage,
                role_attribution,
                usage_estimated,
            )
            .await;
        } else {
            // A1-3 ledger: task-locals do not cross `tokio::spawn`, so carry
            // the goal-round attribution (if any) into the detached write.
            let goal_round = crate::runtime::GOAL_ROUND_ATTRIBUTION
                .try_with(Clone::clone)
                .ok();
            tokio::spawn(async move {
                let write = record_usage(
                    home,
                    agent_id,
                    request_type,
                    model,
                    usage,
                    None,
                    usage_estimated,
                );
                match goal_round {
                    Some(g) => crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(g, write).await,
                    None => write.await,
                }
            });
        }
    }
    Ok(resp)
}

/// Record token usage to the global cost telemetry (RFC-25 A3).
/// Best-effort: silently no-ops if telemetry can't be initialised.
async fn record_usage(
    home_dir: PathBuf,
    agent_id: String,
    request_type: RequestType,
    model: String,
    usage: TokenUsage,
    role_attribution: Option<crate::runtime::RoleCostAttribution>,
    usage_estimated: bool,
) {
    let telemetry = match crate::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            let _ = crate::cost_telemetry::init_telemetry(&home_dir);
            match crate::cost_telemetry::get_telemetry() {
                Some(t) => t,
                None => return,
            }
        }
    };
    if usage_estimated {
        let role = role_attribution
            .as_ref()
            .map(|a| (a.role, a.episode_id.as_str()));
        telemetry
            .record_estimated(&agent_id, request_type, &model, &usage, role)
            .await;
    } else if let Some(a) = role_attribution {
        telemetry
            .record_team_role(
                &agent_id,
                request_type,
                a.role,
                &a.episode_id,
                &model,
                &usage,
            )
            .await;
    } else {
        telemetry
            .record(&agent_id, request_type, &model, &usage)
            .await;
    }
}

/// Convenience wrapper returning just the text content (most internal callers).
pub async fn run_agent_prompt_text(req: AgentPrompt<'_>) -> Result<String, String> {
    // A text-only API cannot carry the timeout notice, even inside a reply scope.
    let response = run_agent_prompt(req).await?;
    response.ensure_complete()?;
    Ok(response.content)
}

tokio::task_local! {
    /// Only the channel reply boundary may opt into displaying partial output.
    pub(crate) static CHANNEL_REPLY_PARTIAL: ();
    /// Preserve the caller's classification when a runtime discards an attempt.
    static RUNTIME_USAGE_REQUEST_TYPE: RequestType;
}

/// Evolution keeps tools where authorized, but never inherits a raised work budget.
/// Fail closed: without a positively identified work request type, the cap applies.
pub(crate) fn identity_background_kind() -> &'static str {
    use duduclaw_core::execution_gate::ExecutionClass;
    match crate::process_group::execution_class(ExecutionClass::EmployeeMain) {
        ExecutionClass::Heartbeat => "heartbeat",
        ExecutionClass::Scheduled => "cron",
        ExecutionClass::Judge => "judge",
        _ if crate::runtime::round_task_env().is_some() => "task",
        _ => RUNTIME_USAGE_REQUEST_TYPE.try_with(|kind| kind.as_str()).unwrap_or("unattributed"),
    }
}

pub(crate) fn allows_employee_step_budget() -> bool {
    !in_utility_call() && RUNTIME_USAGE_REQUEST_TYPE.try_with(|kind| matches!(kind,
        RequestType::Chat | RequestType::Cron | RequestType::Dispatch)).unwrap_or(false)
}

/// Classify a work turn that reaches a runtime without [`run_agent_prompt`]
/// (the direct channel-reply Claude CLI path), so it keeps the employee budget.
pub(crate) async fn with_work_request_type<F: std::future::Future>(kind: RequestType, future: F) -> F::Output {
    RUNTIME_USAGE_REQUEST_TYPE.scope(kind, future).await
}

pub(crate) fn allows_partial_reply() -> bool {
    !in_utility_call() && CHANNEL_REPLY_PARTIAL.try_with(|_| ()).is_ok()
}

/// Persist before returning the error or starting a fallback. Accepted partials
/// are recorded by the ordinary success path instead, so each leg is charged once.
pub(crate) async fn record_discarded_runtime_usage(context: &RuntimeContext, response: &RuntimeResponse) {
    let Ok(request_type) = RUNTIME_USAGE_REQUEST_TYPE.try_with(|kind| *kind) else { return; };
    if context.agent_id.is_empty()
        || (response.input_tokens == 0 && response.output_tokens == 0 && response.cache_read_tokens == 0)
    {
        return;
    }
    let role = crate::runtime::ROLE_COST_ATTRIBUTION.try_with(Clone::clone).ok();
    let estimated = crate::cost_telemetry::USAGE_ESTIMATED
        .try_with(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false);
    record_usage(context.home_dir.clone(), context.agent_id.clone(), request_type,
        response.model_used.clone(), TokenUsage {
            input_tokens: response.input_tokens, output_tokens: response.output_tokens,
            cache_read_tokens: response.cache_read_tokens, cache_creation_tokens: 0,
        }, role, estimated).await;
}

pub(crate) async fn validate_runtime_response(
    mut response: RuntimeResponse, context: &RuntimeContext,
) -> Result<RuntimeResponse, String> {
    // This context belongs to the executed leg, including a resolved fallback.
    // Validate before attribution/display can lose that leg's requested model.
    response.model_used = accepted_reported_model(&response.model_used, &context.model).to_string();
    if !allows_partial_reply() {
        if let Some(seconds) = response.partial_timeout_secs {
            record_discarded_runtime_usage(context, &response).await;
            return Err(crate::work_outcome::WorkOutcome::TimeoutTotal {
                seconds,
                partial: String::new(),
            }.encode());
        }
    }
    Ok(response)
}

/// Default output cap for utility (cheap, fire-and-forget internal) prompts.
pub const UTILITY_MAX_TOKENS: u32 = 2048;

/// Host-selected origin for utility work outside an existing gateway scope.
/// Empty `target` denotes global maintenance, never an inferred human caller.
pub enum UtilityContext<'a> {
    HumanInteractive,
    Autonomous {
        target: &'a str,
        callers: &'a [String],
    },
}

pub fn with_utility_context<'a, F: std::future::Future + 'a>(
    home: &'a Path,
    context: UtilityContext<'a>,
    future: F,
) -> impl std::future::Future<Output = F::Output> + 'a {
    let future = Box::pin(future);
    async move {
        match context {
            UtilityContext::HumanInteractive => {
                crate::autonomous_runtime::human_interactive(home, future).await
            }
            UtilityContext::Autonomous { target, callers } => {
                crate::autonomous_runtime::scope(home, target, callers, future).await
            }
        }
    }
}

/// Run a utility (cheap, internal) prompt through the resolved utility runtime
/// (RFC-25 N2).
///
/// Resolution (see [`crate::runtime_config::resolve_utility`]):
/// - `agent_dir` present → that agent's `[runtime] provider` + `[model] utility`.
/// - `agent_dir` absent  → global `config.toml [runtime] utility_provider` / `utility_model`.
///
/// Claude uses the tool-free lightweight CLI lane. Other supported providers
/// use the mandatory utility profile; unsupported employee providers resolve
/// to a tool-free global spec, while unsupported explicit hints are refused.
pub async fn run_utility_prompt(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
    system_prompt: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<String, String> {
    // Thin wrapper: `None` hint ⇒ `apply_utility_hint` returns the resolved
    // spec untouched, so every pre-existing caller is byte-identical.
    run_utility_prompt_with_hint(
        home_dir,
        agent_dir,
        agent_id,
        system_prompt,
        prompt,
        max_tokens,
        None,
    )
    .await
}

/// An operator's override of the resolved utility `(provider, model)` for ONE
/// call (P0/WP-B, verifier decorrelation — arXiv:2607.13918).
///
/// Both halves are independently optional: a hint may switch only the model
/// (staying on the resolved provider), only the provider, or both. An
/// all-`None`/blank hint is inert ([`UtilityModelHint::is_empty`]) and resolves
/// exactly like no hint at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UtilityModelHint {
    /// Runtime to run the call on. `None` ⇒ keep the resolved provider.
    pub provider: Option<RuntimeType>,
    /// Model id within the effective provider. `None`/blank ⇒ keep the
    /// resolved utility model.
    pub model: Option<String>,
    /// Per-call reasoning effort for this utility/judge call (P1/WP-3).
    /// `None` ⇒ the agent's `agent.toml [model] effort`, or the provider
    /// default when that is unset too.
    ///
    /// Deliberately NOT part of [`UtilityModelHint::is_empty`]: emptiness
    /// there means "resolves to the same `(provider, model)` spec", which is
    /// what [`apply_utility_hint`]'s family validation is about. Effort
    /// changes how hard the model thinks, not which model answers.
    pub effort: Option<duduclaw_core::effort::Effort>,
    /// JSON schema this call's reply must conform to (Team-as-Agent live round
    /// 8). `None` ⇒ every argv is byte-identical to before this field existed.
    ///
    /// Round 8's settle failed not on the work but on the *shape of the
    /// verdict*: with `[dispatch] judge_provider = "codex"` both the two-stage
    /// evaluator and the MAV panel replied in prose, and both parsers —
    /// correctly, fail-closed — refused it ("evaluator reply has no string
    /// `decision` field"). Claude follows "reply with ONLY a JSON object";
    /// codex does not reliably, and it does not have to: `codex exec` takes
    /// `--output-schema <FILE>` and constrains the reply itself.
    ///
    /// Honoured today only by [`RuntimeType::Codex`] (see
    /// `crate::runtime::codex`); every other runtime logs and ignores it, so a
    /// schema is a *preference*, never a precondition — an adjudication must
    /// not fail because a backend cannot constrain its output.
    ///
    /// Like `effort`, deliberately NOT part of [`UtilityModelHint::is_empty`]:
    /// a schema changes the reply's shape, not which model answers.
    pub output_schema: Option<serde_json::Value>,
}

impl UtilityModelHint {
    /// A hint that changes nothing (both halves unset or blank).
    pub fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self
                .model
                .as_deref()
                .map(str::trim)
                .map(str::is_empty)
                .unwrap_or(true)
    }
}

/// Fold an operator hint over a resolved utility spec.
///
/// **Fail-closed** (the Goose #10731 lesson — a judge silently answered by the
/// wrong backend is worse than a judge that does not run): when the effective
/// model does not belong to the effective provider's family, this returns
/// `Err` and the caller must NOT spawn. Validation reuses the single
/// [`crate::runtime_config::model_matches_provider`] yardstick, which passes
/// unknown families and family-less runtimes (`openai_compat` and the
/// multi-vendor shells legitimately proxy arbitrary models).
///
/// Note the asymmetry this implies, and it is deliberate: setting only
/// `model` to an id from *another* family is refused rather than silently
/// inferring a provider — the operator must name the runtime too.
pub fn apply_utility_hint(
    base: crate::runtime_config::UtilitySpec,
    hint: Option<&UtilityModelHint>,
) -> Result<crate::runtime_config::UtilitySpec, String> {
    let Some(hint) = hint.filter(|h| !h.is_empty()) else {
        return Ok(base);
    };
    let provider = hint.provider.unwrap_or(base.provider);
    let model = hint
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(base.model);
    if !crate::runtime_config::model_matches_provider(&model, provider) {
        return Err(format!(
            "judge model {model} does not belong to runtime {}",
            provider.as_str()
        ));
    }
    Ok(crate::runtime_config::UtilitySpec { provider, model })
}

/// Resolve the spec a hinted utility call would actually run on, **degrading**
/// to the un-hinted resolution when the hint cannot be honoured.
///
/// Returns `(effective spec, degrade reason)`. A `Some` reason means the hint
/// was dropped — the returned spec is the ordinary
/// [`crate::runtime_config::resolve_utility`] answer, so a misconfigured or
/// un-installed judge runtime can never stall an adjudication (design rule:
/// never fail the settle because of the hint).
///
/// Two degrade causes:
/// 1. family mismatch ([`apply_utility_hint`] `Err`) — detected before any
///    spawn, costs nothing;
/// 2. the hinted provider is not present in this home's auto-detected
///    [`RuntimeRegistry`] (CLI binary missing / backend not configured).
///
/// The availability probe runs **only** when the hint changes the provider —
/// a model-only hint rides the exact path the default resolution would have
/// taken, so probing it would be a behavior change for no signal.
pub async fn resolve_hinted_utility(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    hint: Option<&UtilityModelHint>,
) -> (crate::runtime_config::UtilitySpec, Option<String>) {
    let base = crate::runtime_config::resolve_utility(home_dir, agent_dir);
    let Some(hint) = hint.filter(|h| !h.is_empty()) else {
        return (base, None);
    };
    let hinted = match apply_utility_hint(base.clone(), Some(hint)) {
        Ok(spec) => spec,
        Err(e) => return (base, Some(e)),
    };
    if hinted.provider != base.provider && registry(home_dir).await.get(&hinted.provider).is_none()
    {
        let reason = format!(
            "runtime {} is not available on this host (binary missing or backend not configured)",
            hinted.provider.as_str()
        );
        return (base, Some(reason));
    }
    (hinted, None)
}

// ── Live round 8: structured output for a non-Claude judge ───────────────
//
// `UtilityModelHint::output_schema` has to reach the runtime that spawns the
// CLI. It travels as a task-local rather than as a field on `AgentPrompt` /
// `RuntimeContext`, for the reason `crate::runtime`'s `SPAWN_OVERRIDE` /
// `RUNTIME_OUTCOME` pair already states in its own doc comment: it is a
// caller-scoped fact that must reach code far down the call chain through
// intermediate signatures this work package must not churn. Task-locals thread
// transparently through `.await` inside one tokio task, so no function between
// here and `AgentRuntime::execute` needs to know it exists, and an absent scope
// is a complete no-op.

tokio::task_local! {
    /// JSON schema the current call's reply must conform to, when the caller
    /// asked for one. Read with [`output_schema_override`]; absent scope ⇒
    /// `None` ⇒ every runtime builds a byte-identical argv.
    pub static OUTPUT_SCHEMA: std::sync::Arc<serde_json::Value>;

    /// Explicit attribution for the rotated Claude utility CLI path. This is
    /// independent of channel replies; absent scope leaves legacy CLI calls
    /// unchanged, and the enclosing goal scope supplies episode/round.
    pub(crate) static CLAUDE_UTILITY_AGENT_ID: String;

    /// Mandatory restricted execution profile for every utility call, including
    /// classifiers, judges and Claude. Runtime entry points enforce it too.
    pub(crate) static UTILITY_CALL: ();
}

/// Is the current call a utility call ([`run_utility_prompt_with_hint`])?
pub fn in_utility_call() -> bool {
    UTILITY_CALL.try_with(|_| ()).is_ok()
}

pub(crate) fn is_claude_utility_call() -> bool {
    CLAUDE_UTILITY_AGENT_ID.try_with(|_| ()).is_ok()
}

pub(crate) fn is_observed_claude_model(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty() && model != "<synthetic>"
}

/// Longest model id accepted from external (CLI / runtime) output.
pub(crate) const MAX_REPORTED_MODEL_BYTES: usize = 128;

/// Is `model` a plausible model id: 1..=128 bytes of `[A-Za-z0-9._:/@+-[]]`?
/// Covers every catalog id (`claude-opus-4-6`, `gpt-5.6-sol`,
/// `kimi-code/kimi-for-coding`), the openai-compat presets
/// (`meta-llama/Llama-3.3-70B-Instruct-Turbo`), Ollama tags (`qwen2.5:7b`),
/// `@`-scoped ids and Claude's `[1m]` context suffix. No whitespace, control,
/// bidi or markup character can pass.
pub(crate) fn is_valid_reported_model_id(model: &str) -> bool {
    (1..=MAX_REPORTED_MODEL_BYTES).contains(&model.len())
        && model.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:/@+-[]".contains(&b))
}

/// The ONE place a model name reported by external output (a CLI's stream
/// `message.model`, a runtime's `model_used`) is accepted for display and
/// metadata (`ProgressEvent::ModelInfo` → `SessionInfo.model`,
/// `assistant_done.model`). A string that is not a valid model id is never
/// used; the model id the gateway itself requested for that call is shown
/// instead. Callers append their own decorations (「（備援）」) afterwards.
pub(crate) fn accepted_reported_model<'a>(reported: &'a str, requested: &'a str) -> &'a str {
    if is_valid_reported_model_id(reported) {
        reported
    } else {
        tracing::warn!(
            reported_bytes = reported.len(),
            requested = %duduclaw_core::truncate_chars(requested, 64),
            "CLI-reported model name rejected; showing the requested model"
        );
        requested
    }
}

#[cfg(test)]
mod reported_model_tests {
    use super::*;

    #[tokio::test]
    async fn invalid_runtime_model_uses_each_executed_legs_requested_model() {
        let home = tempfile::tempdir().unwrap();
        // The failover manager validates each response with that leg's context.
        for requested in ["gpt-5.6-sol", "claude-opus-4-6"] {
            let context = RuntimeContext {
                agent_dir: None, system_prompt: String::new(), model: requested.into(),
                max_tokens: 16, home_dir: home.path().to_owned(), agent_id: "test".into(),
                preferred_provider: None, conversation_history: vec![], capabilities: None,
                account_pool: vec![], effort: None, allow_cross_family_failover: true,
            };
            let response = RuntimeResponse {
                content: "answer".into(), model_used: "<img src=x>\n".into(),
                ..Default::default()
            };
            let accepted = validate_runtime_response(response, &context).await.unwrap();
            assert_eq!(accepted.model_used, requested);
            assert_eq!(accepted.content, "answer");
        }
    }
}

/// Persist canonical CLI result usage synchronously, including cache tokens
/// and the model the CLI reports. No observed usage/model means no guessed
/// row. Called once per CLI attempt, so rotated paid retries are retained.
pub(crate) async fn record_claude_utility_result(
    home_dir: &Path,
    event: &serde_json::Value,
    observed_model: Option<&str>,
) -> bool {
    let Ok(agent_id) = CLAUDE_UTILITY_AGENT_ID.try_with(Clone::clone) else {
        return false;
    };
    if agent_id.is_empty() {
        return false;
    }
    let Some(total) = event.get("usage").and_then(TokenUsage::from_json) else {
        return false;
    };
    let by_model = event.get("modelUsage").and_then(|v| v.as_object());
    let measured: Option<Vec<(String, TokenUsage)>> = match by_model {
        Some(models) if models.len() == 1 => {
            let name = models.keys().next().unwrap();
            if is_observed_claude_model(name) {
                Some(vec![(name.clone(), total)])
            } else {
                observed_model
                    .filter(|name| is_observed_claude_model(name))
                    .map(|name| vec![(name.to_owned(), total)])
            }
        }
        Some(models) if models.len() > 1 => models
            .iter()
            .filter(|(name, _)| is_observed_claude_model(name))
            .map(|(name, usage)| {
                Some((
                    name.clone(),
                    TokenUsage {
                        input_tokens: usage.get("inputTokens")?.as_u64()?,
                        output_tokens: usage.get("outputTokens")?.as_u64()?,
                        cache_read_tokens: usage
                            .get("cacheReadInputTokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        cache_creation_tokens: usage
                            .get("cacheCreationInputTokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    },
                ))
            })
            .collect::<Option<Vec<_>>>()
            .filter(|rows| !rows.is_empty()),
        _ => observed_model
            .filter(|name| is_observed_claude_model(name))
            .map(|name| vec![(name.to_owned(), total)]),
    };
    let Some(measured) = measured else {
        tracing::warn!(
            "Claude utility result has usage without an unambiguous observed model; cost row omitted"
        );
        return false;
    };
    let role = crate::runtime::ROLE_COST_ATTRIBUTION
        .try_with(Clone::clone)
        .ok();
    for (model, usage) in measured {
        record_usage(
            home_dir.to_path_buf(),
            agent_id.clone(),
            RequestType::Evolution,
            model,
            usage,
            role.clone(),
            false,
        )
        .await;
    }
    true
}

/// The caller's required reply schema for this call, if any. `None` outside an
/// [`OUTPUT_SCHEMA`] scope — the universal case.
pub fn output_schema_override() -> Option<serde_json::Value> {
    OUTPUT_SCHEMA.try_with(|schema| (**schema).clone()).ok()
}

// Utility callers historically used operation labels as telemetry ids. When
// an employee directory is supplied, use its registry id for both charging and
// enforcement so labels cannot give an employee a fresh budget.
fn utility_employee_id(agent_dir: Option<&Path>, agent_id: &str) -> String {
    if agent_id.is_empty() { return String::new(); }
    agent_dir.and_then(|dir| duduclaw_core::agent_toml::load(dir).agent)
        .and_then(|agent| agent.name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| agent_id.to_owned())
}

/// [`run_utility_prompt`] with an operator [`UtilityModelHint`].
///
/// A hint that fails [`apply_utility_hint`]'s family check returns `Err`
/// **without spawning anything**. Callers that must not fail because of a bad
/// hint (the goal-loop judge / evaluator) resolve with
/// [`resolve_hinted_utility`] first and retry hint-less on a degrade.
pub async fn run_utility_prompt_with_hint(
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
    system_prompt: &str,
    prompt: &str,
    max_tokens: u32,
    hint: Option<&UtilityModelHint>,
) -> Result<String, String> {
    crate::autonomous_runtime::require_utility_context(home_dir).await?;
    #[cfg(test)]
    crate::model_call_probe::record("run_utility_prompt");
    let employee_id = utility_employee_id(agent_dir, agent_id);
    let agent_id = employee_id.as_str();
    match agent_dir {
        Some(dir) => crate::runtime_config::ensure_active_agent_provider(dir)?,
        None => crate::runtime_config::ensure_active_utility_provider(home_dir)?,
    }
    let base = crate::runtime_config::resolve_utility(home_dir, agent_dir);
    let mut spec = crate::runtime_config::resolve_utility_profile(
        home_dir, agent_dir, apply_utility_hint(base, hint)?, has_explicit_utility_hint(hint),
    )?;
    // v1.84.0: same bare-Claude-alias substitution as the conversational
    // preflight, so the isolated and the in-process lane agree.
    let runnable = crate::runtime_config::runnable_model(agent_id, &spec.model, spec.provider);
    let substituted = matches!(runnable, std::borrow::Cow::Owned(_));
    spec.model = runnable.into_owned();
    crate::product_policy::require_runtime(home_dir, spec.provider.as_str())?;
    // F5: vendor attribution only for the catalog default substituted above.
    let policy = crate::product_policy::load(home_dir)?;
    if substituted {
        crate::product_policy::require_substituted_catalog_model(policy, spec.provider, &spec.model)?;
    } else {
        crate::product_policy::require_model(policy, &spec.model)?;
    }
    #[cfg(test)]
    if utility_policy_tests::DENY_EXECUTION.try_with(|_| ()).is_ok() {
        return Err("test forbids utility execution".into());
    }
    // Never enter a conversational fallback from a utility call. Unsupported
    // employee providers were resolved to the global utility spec above.
    let allow_cross_family_failover = false;
    // Live round 8: the reply-shape preference, honoured only where the CLI can
    // actually constrain output. Reported as a `debug!` on every other runtime
    // so "my judge still replies in prose" is answerable from the log instead
    // of by reading this function.
    let schema = hint.and_then(|h| h.output_schema.clone());
    if schema.is_some() && !matches!(spec.provider, RuntimeType::Codex | RuntimeType::OpenAiCompat) {
        tracing::debug!(
            provider = %spec.provider.as_str(),
            model = %spec.model,
            "output_schema requested but this runtime has no structured-output flag — ignored"
        );
    }
    if crate::isolation_egress::resolved_identity().is_some() {
        // Keep original provider, model, autonomy and budget gates before the
        // host execution handoff. CLI utilities can retain read-capable tools.
        if spec.provider != RuntimeType::Claude {
            crate::runtime_config::ensure_model_provider(&spec.model, spec.provider)?;
        }
        let budget_kind = if spec.provider == RuntimeType::Claude { "claude_utility" } else { "runtime_dispatch" };
        crate::budget::gate_before_spawn(home_dir, agent_dir, agent_id, budget_kind)
            .await.map_err(|e| e.legacy_message())?;
        return crate::isolation_egress::run_utility(crate::isolation_egress::UtilityRequest {
            home: home_dir, agent_dir, agent: agent_id, system_prompt, prompt, max_tokens, hint,
            provider: spec.provider, model: &spec.model,
        }).await.unwrap_or_else(|| Err("isolated utility context unavailable".into()));
    }
    if spec.provider == RuntimeType::Claude {
        crate::budget::gate_before_spawn(home_dir, agent_dir, agent_id, "claude_utility")
            .await.map_err(|e| e.legacy_message())?;
        // Mechanical metadata calls deliberately retain the lightweight
        // lane's fixed medium effort; hints cannot widen it.
        if let Some(effort) = hint.and_then(|h| h.effort) {
            tracing::warn!(
                provider = %spec.provider.as_str(),
                model = %spec.model,
                effort = %effort.as_str(),
                "utility hint requested a reasoning effort, but the Claude utility path \
                 uses the fixed medium effort required by the lightweight lane"
            );
        }
        UTILITY_CALL.scope((), CLAUDE_UTILITY_AGENT_ID.scope(agent_id.to_owned(),
            crate::channel_reply::call_claude_cli_public(prompt, &spec.model, system_prompt, home_dir)))
            .await
    } else {
        let fut = run_agent_prompt_text(AgentPrompt {
            agent_dir,
            home_dir,
            agent_id,
            prompt,
            system_prompt,
            model: &spec.model,
            max_tokens,
            provider_override: Some(spec.provider),
            conversation_history: &[],
            request_type: RequestType::Evolution,
            runtime_settings: None,
            effort: hint.and_then(|h| h.effort),
            allow_cross_family_failover,
            purpose: SpawnPurpose::Utility,
        });
        let fut = UTILITY_CALL.scope((), fut);
        match schema {
            Some(schema) => OUTPUT_SCHEMA.scope(std::sync::Arc::new(schema), fut).await,
            None => fut.await,
        }
    }
}

/// Hints identify operator-selected judge calls. Unsupported hinted providers
/// are refused rather than using the employee-to-global fallback policy.
fn has_explicit_utility_hint(hint: Option<&UtilityModelHint>) -> bool {
    hint.is_some()
}

#[cfg(test)]
#[path = "runtime_dispatch_attended_tests.rs"]
mod attended_entry_tests;

#[cfg(test)]
#[path = "runtime_dispatch_model_substitution_tests.rs"]
mod model_substitution_tests;

#[cfg(test)]
mod hint_tests {
    use super::*;
    use crate::runtime_config::UtilitySpec;

    fn base() -> UtilitySpec {
        UtilitySpec {
            provider: RuntimeType::Claude,
            model: "claude-haiku-4-5".to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn goal_judge_claude_cli_records_measured_usage_without_api() {
        const CHILD_HOME: &str = "DUDU_TEST_JUDGE_CLI_HOME";
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            let home = PathBuf::from(home);
            // The resolver also scans fixed global installations and chooses
            // the newest version. Assert the fixture wins before any prompt
            // can be sent; PATH alone is not a hermetic guarantee.
            assert_eq!(
                duduclaw_core::which_claude().as_deref(),
                home.join("bin/claude").to_str()
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                use crate::dispatch_engine::{DispatchEngine, GoalAcceptanceCaller, LlmAcceptanceJudge, LlmPreEvaluator};
                use crate::task_store::{TaskRow, TaskStore};
                use std::sync::Arc;
                crate::cost_telemetry::init_telemetry(&home).unwrap();
                let store = Arc::new(TaskStore::open(&home).unwrap());
                let worker_dir = home.join("agents/fixture-worker");
                std::fs::create_dir_all(&worker_dir).unwrap();
                std::fs::write(worker_dir.join("agent.toml"),
                    "[agent]\nname = \"fixture-worker\"\nstatus = \"active\"\n").unwrap();
                let mut task = TaskRow::new("fixture-goal".into(), "fixture task".into(),
                    "deliver the work".into(), "medium".into(), "fixture-worker".into(), "system".into());
                task.status = "pending".into();
                task.goal_mode = true;
                task.max_retries = 3;
                task.acceptance_criteria = Some("the deliverable is correct".into());
                store.insert_task(&task).await.unwrap();
                let now = chrono::Utc::now();
                let lease = (now + chrono::Duration::minutes(5)).to_rfc3339();
                assert!(store.atomic_claim(&task.id, "fixture-worker", &now.to_rfc3339(), &lease)
                    .await.unwrap().is_claimed());
                store.complete_task(&task.id, "delivered fixture", "fixture-worker").await.unwrap();
                let engine = DispatchEngine::new(store.clone(), Some(Arc::new(LlmAcceptanceJudge::new(
                    GoalAcceptanceCaller { home_dir: home.clone() }).with_reply_contract_home(home.clone()))))
                    .with_home_dir(home.clone())
                    .with_evaluator(Arc::new(LlmPreEvaluator::new(GoalAcceptanceCaller { home_dir: home.clone() })
                        .with_reply_contract_home(home.clone())));
                engine.tick_once().await.unwrap();
                assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().status, "done");
                let conn = rusqlite::Connection::open(home.join("cost_telemetry.db")).unwrap();
                let rows: Vec<(String, String, String, i64, Option<String>, String, i64, i64, i64)> = conn
                    .prepare("SELECT agent_id,request_type,episode_id,round,role,model,input_tokens,output_tokens,cache_creation_tokens FROM token_usage ORDER BY id")
                    .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)))
                    .unwrap().collect::<Result<_, _>>().unwrap();
                assert_eq!(rows.len(), 2, "both real production CLI stages must persist measured usage; rows={rows:?}; wrapper_calls={:?}",
                    std::fs::read_to_string(home.join("cli-count")));
                for row in &rows {
                    assert_eq!((&*row.0, &*row.1, &*row.2, row.3, &row.4, &*row.5),
                        ("goal-acceptance-judge", "evolution", "fixture-goal", 1, &None, "claude-haiku-4-5"));
                }
                assert_eq!((rows[0].6, rows[0].7, rows[0].8), (9, 12, 31));
                assert_eq!((rows[1].6, rows[1].7, rows[1].8), (13, 7, 23));

                // Successful prose without a usage event cannot become a
                // fabricated zero-cost row, including outside review scope.
                crate::autonomous_runtime::scope(&home, "", &[], run_utility_prompt(&home, None, "unmeasured-utility", "", "fixture", 16)).await.unwrap();
                assert_eq!(conn.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
                assert!(crate::runtime::GOAL_ROUND_ATTRIBUTION.try_with(|_| ()).is_err());

                // A synthetic assistant error must not replace a genuinely
                // observed model before terminal measured error usage arrives.
                assert!(crate::autonomous_runtime::scope(&home, "", &[], run_utility_prompt(&home, None, "synthetic-after-model", "", "fixture", 16)).await.is_err());
                let observed: (String, i64, i64, i64, Option<String>, Option<i64>, Option<String>) = conn
                    .query_row("SELECT model,input_tokens,output_tokens,cache_creation_tokens,episode_id,round,role FROM token_usage WHERE agent_id='synthetic-after-model'", [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).unwrap();
                assert_eq!(observed, ("claude-haiku-4-5".into(), 17, 3, 5, None, None, None));
                assert!(crate::autonomous_runtime::scope(&home, "", &[], run_utility_prompt(&home, None, "synthetic-only", "", "fixture", 16)).await.is_err());
                assert_eq!(conn.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 3,
                    "synthetic-only cannot establish a real model or produce a cost row");
                assert!(crate::autonomous_runtime::scope(&home, "", &[], run_utility_prompt(&home, None, "nonzero-with-prose", "", "fixture", 16)).await.is_err(),
                    "assistant PASS followed by a nonzero exit must never accept a judge verdict");
            });
            return;
        }

        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let bin = home.join("bin");
        std::fs::create_dir(&bin).unwrap();
        // This intentionally requests a different model from the one the
        // result actually reports, so the recorder cannot copy a config name.
        std::fs::write(
            home.join("config.toml"),
            "[runtime]\nutility_provider = \"claude\"\nutility_model = \"claude-sonnet-4-6\"\n",
        )
        .unwrap();
        for (file, reply, input, output, cache) in [
            (
                "first.jsonl",
                r#"{"decision":"candidate_complete","evidence":"delivered fixture","next_step":"check delivery","blocker_key":null}"#,
                9,
                12,
                31,
            ),
            ("second.jsonl", "PASS\nfixture accepted", 13, 7, 23),
        ] {
            let result = serde_json::json!({"type":"result","subtype":"success","is_error":false,
                "result":reply,"usage":{"input_tokens":input,"output_tokens":output,
                    "cache_read_input_tokens":0,"cache_creation_input_tokens":cache},
                "modelUsage":{"claude-haiku-4-5":{"inputTokens":input,"outputTokens":output,
                    "cacheReadInputTokens":0,"cacheCreationInputTokens":cache}}});
            std::fs::write(home.join(file), format!("{result}\n")).unwrap();
        }
        std::fs::write(home.join("unmeasured.jsonl"),
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"PASS\"}\n").unwrap();
        for (file, real_model, input, output, cache) in [
            ("synthetic-after-model.jsonl", true, 17, 3, 5),
            ("synthetic-only.jsonl", false, 21, 4, 6),
        ] {
            let mut lines = String::new();
            if real_model {
                lines.push_str(&format!("{}\n", serde_json::json!({"type":"assistant",
                    "message":{"model":"claude-haiku-4-5","content":[{"type":"text","text":"started fixture"}]}})));
            }
            lines.push_str(&format!("{}\n", serde_json::json!({"type":"assistant",
                "message":{"model":"<synthetic>","content":[{"type":"text","text":"synthetic error fixture"}]}})));
            lines.push_str(&format!("{}\n", serde_json::json!({"type":"result","is_error":true,
                "subtype":"error_during_execution","result":"fixture terminal error",
                "usage":{"input_tokens":input,"output_tokens":output,"cache_read_input_tokens":0,"cache_creation_input_tokens":cache}})));
            std::fs::write(home.join(file), lines).unwrap();
        }
        let wrapper = bin.join("claude");
        crate::test_exec::write_executable(&wrapper, "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '99999.0.0\\n'; exit 0; fi\nif [ \"$1\" = auth ]; then printf '{\"loggedIn\":false}\\n'; exit 0; fi\nn=0\nif [ -f \"$HOME/cli-count\" ]; then n=$(/bin/cat \"$HOME/cli-count\"); fi\nn=$((n+1))\nprintf '%s' \"$n\" > \"$HOME/cli-count\"\ncase \"$n\" in\n1) /bin/cat \"$HOME/first.jsonl\";;\n2) /bin/cat \"$HOME/second.jsonl\";;\n3) /bin/cat \"$HOME/unmeasured.jsonl\";;\n4) /bin/cat \"$HOME/synthetic-after-model.jsonl\";;\n5) /bin/cat \"$HOME/synthetic-only.jsonl\";;\n6) /bin/cat \"$HOME/first.jsonl\"; exit 1;;\n*) exit 2;;\nesac\n", 0o755);
        // Global telemetry/CLI discovery are process singletons. An isolated
        // child test keeps PATH/HOME/auth and parent parallel tests untouched.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        // module_path! includes the crate prefix; libtest names do not.
        let full_test_name = concat!(
            module_path!(),
            "::goal_judge_claude_cli_records_measured_usage_without_api"
        );
        let test_name = full_test_name.split_once("::").unwrap().1;
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", home)
            .env(CHILD_HOME, home)
            .args(["--exact", test_name, "--nocapture"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let started = std::time::Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() > std::time::Duration::from_secs(15) {
                let _ = duduclaw_core::platform::kill_process_group(child.id());
                let _ = child.wait();
                panic!("isolated judge CLI fixture exceeded its deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
            "child must execute the fixture test: {}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "isolated CLI regression: {} {}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert_eq!(std::fs::read_to_string(home.join("cli-count")).unwrap(), "6");
    }

    #[test]
    fn no_hint_is_a_no_op() {
        // The byte-identical contract for the pre-existing `run_utility_prompt`
        // callers: `None` (and an all-blank hint) must return the resolved spec
        // untouched.
        assert_eq!(apply_utility_hint(base(), None).unwrap(), base());
        assert_eq!(
            apply_utility_hint(base(), Some(&UtilityModelHint::default())).unwrap(),
            base()
        );
        assert_eq!(
            apply_utility_hint(
                base(),
                Some(&UtilityModelHint {
                    provider: None,
                    model: Some("   ".into()),
                    effort: None,
                    output_schema: None,
                })
            )
            .unwrap(),
            base()
        );
    }

    #[test]
    fn hint_overrides_model_provider_or_both() {
        // Model-only, staying inside the resolved provider's family.
        let got = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: None,
                model: Some("claude-opus-4-6".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap();
        assert_eq!(got.provider, RuntimeType::Claude);
        assert_eq!(got.model, "claude-opus-4-6");

        // Both halves — the decorrelated judge case this WP exists for.
        let got = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: Some(RuntimeType::Grok),
                model: Some("grok-4.3".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap();
        assert_eq!(got.provider, RuntimeType::Grok);
        assert_eq!(got.model, "grok-4.3");

        // Provider-only: the resolved model rides along, and is validated
        // against the NEW provider (claude-* on Codex is a mismatch).
        let err = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: Some(RuntimeType::Codex),
                model: None,
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap_err();
        assert!(err.contains("does not belong to runtime codex"), "{err}");
    }

    #[test]
    fn family_mismatch_is_refused_not_silently_run() {
        // Goose #10731: a judge answered by the wrong backend is worse than a
        // judge that does not run. Model-only hints across families are
        // refused rather than provider-inferred.
        let err = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: None,
                model: Some("grok-4.3".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap_err();
        assert!(
            err.starts_with("judge model grok-4.3 does not belong to runtime claude"),
            "{err}"
        );

        let err = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: Some(RuntimeType::Codex),
                model: Some("claude-opus-4-6".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap_err();
        assert!(err.contains("does not belong to runtime codex"), "{err}");
    }

    #[test]
    fn family_less_runtimes_accept_any_model() {
        // `openai_compat` declares no `model_prefixes` — it legitimately
        // proxies arbitrary vendors, so validation must not invent a mismatch.
        let got = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: Some(RuntimeType::OpenAiCompat),
                model: Some("deepseek-v3.2".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap();
        assert_eq!(got.provider, RuntimeType::OpenAiCompat);
        assert_eq!(got.model, "deepseek-v3.2");

        // An unknown model id on a family-bearing runtime is also allowed —
        // `model_matches_provider` only flags CONFIDENT mismatches.
        assert!(
            apply_utility_hint(
                base(),
                Some(&UtilityModelHint {
                    provider: Some(RuntimeType::Grok),
                    model: Some("some-unreleased-id".into()),
                    effort: None,
                    output_schema: None,
                })
            )
            .is_ok()
        );
    }

    #[test]
    fn explicit_utility_hints_are_distinct_from_background_calls() {
        // This is the flag `run_utility_prompt_with_hint` computes: a hinted
        // call is the judge/evaluator path, and it must never be rescued by a
        // different family behind the operator's back (the live-test hole).
        assert!(has_explicit_utility_hint(Some(
            &UtilityModelHint {
                provider: Some(RuntimeType::Grok),
                model: Some("grok-4.3".into()),
                effort: None,
                output_schema: None,
            }
        )));

        // Regression (2026-09-28): the rule used to be "did the hint MOVE the
        // provider", so naming the same provider the global
        // `[runtime] utility_provider` already resolves to — `[dispatch]
        // judge_provider = "codex"` on a codex-default install — opted back
        // IN to cross-family failover and let a substituted Claude model
        // answer the verifier. Explicitly naming a family is a request for
        // that family, whether or not it is also the default.
        assert!(has_explicit_utility_hint(Some(
            &UtilityModelHint {
                provider: Some(RuntimeType::Claude), // == base().provider
                model: Some("claude-opus-4-6".into()),
                effort: None,
                output_schema: None,
            }
        )));
        // Same for a model-only hint and a schema/effort-only hint: all three
        // are judge-path calls.
        for hint in [
            UtilityModelHint {
                provider: None,
                model: Some("claude-opus-4-6".into()),
                effort: None,
                output_schema: None,
            },
            UtilityModelHint {
                provider: None,
                model: None,
                effort: None,
                output_schema: Some(serde_json::json!({"type": "object"})),
            },
            UtilityModelHint::default(),
        ] {
            assert!(
                has_explicit_utility_hint(Some(&hint)),
                "{hint:?} is still a judge-path call"
            );
        }

        // Only an un-hinted background call may resolve an unsupported employee
        // runtime to the tool-free global spec before execution.
        assert!(!has_explicit_utility_hint(None));
    }

    #[test]
    fn is_empty_tracks_both_halves() {
        assert!(UtilityModelHint::default().is_empty());
        assert!(
            UtilityModelHint {
                provider: None,
                model: Some("\t\n ".into()),
                effort: None,
                output_schema: None,
            }
            .is_empty()
        );
        assert!(
            !UtilityModelHint {
                provider: Some(RuntimeType::Grok),
                model: None,
                effort: None,
                output_schema: None,
            }
            .is_empty()
        );
        assert!(
            !UtilityModelHint {
                provider: None,
                model: Some("claude-opus-4-6".into()),
                effort: None,
                output_schema: None,
            }
            .is_empty()
        );
    }

    // ── Live round 8: `output_schema` plumbing ──────────────────────────

    /// A schema changes the reply's SHAPE, not which model answers — so, like
    /// `effort`, it must stay out of the emptiness test that drives
    /// `apply_utility_hint`'s routing and family validation.
    #[test]
    fn a_schema_only_hint_is_inert_for_routing() {
        let hint = UtilityModelHint {
            provider: None,
            model: None,
            effort: None,
            output_schema: Some(serde_json::json!({"type": "object"})),
        };
        assert!(
            hint.is_empty(),
            "a schema must not make a hint look like a routing override"
        );
        assert_eq!(apply_utility_hint(base(), Some(&hint)).unwrap(), base());
    }

    /// The task-local is the channel from the hint to whichever runtime can
    /// enforce a schema. Absent scope ⇒ `None` ⇒ every argv is byte-identical
    /// to before this field existed.
    #[tokio::test]
    async fn output_schema_override_is_none_outside_a_scope() {
        assert!(output_schema_override().is_none());
        let schema = serde_json::json!({"type": "object", "required": ["decision"]});
        let seen = OUTPUT_SCHEMA
            .scope(std::sync::Arc::new(schema.clone()), async {
                output_schema_override()
            })
            .await;
        assert_eq!(seen, Some(schema));
        assert!(output_schema_override().is_none());
    }
}

/// Owned registry injection only for future isolated source-declared tests.
/// No host CLI detection is performed when this owned-home entry is present.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    use std::sync::Arc;

    // Owned injection must not share the async auto-detection cache lock.
    // Every synchronous critical section only copies/inserts/removes a pointer.
    static OWNED_REGISTRIES: OnceLock<
        std::sync::Mutex<HashMap<PathBuf, Arc<RuntimeRegistry>>>,
    > = OnceLock::new();

    pub(super) fn owned_registry(home: &Path) -> Option<Arc<RuntimeRegistry>> {
        OWNED_REGISTRIES
            .get()
            .and_then(|map| map.lock().unwrap().get(home).cloned())
    }

    pub(crate) struct RegistryGuard {
        home: PathBuf,
        registry: Arc<RuntimeRegistry>,
    }
    impl RegistryGuard {
        pub(crate) async fn install(home: &Path, registry: RuntimeRegistry) -> Self {
            assert!(home.is_dir());
            let registry = Arc::new(registry);
            let mut map = OWNED_REGISTRIES
                .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
                .lock()
                .unwrap();
            assert!(map.insert(home.to_owned(), registry.clone()).is_none());
            Self {
                home: home.to_owned(),
                registry,
            }
        }
    }
    impl Drop for RegistryGuard {
        fn drop(&mut self) {
            let mut map = OWNED_REGISTRIES.get().unwrap().lock().unwrap();
            assert!(
                map.get(&self.home)
                    .is_some_and(|actual| Arc::ptr_eq(actual, &self.registry))
            );
            map.remove(&self.home);
        }
    }

    pub(crate) type RecordedFallback = Arc<std::sync::Mutex<Vec<RuntimeContext>>>;

    pub(crate) struct RecordingCodex(pub RecordedFallback);
    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for RecordingCodex {
        fn name(&self) -> &str { "codex" }
        async fn is_available(&self) -> bool { true }
        async fn execute(&self, _: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
            self.0.lock().unwrap().push(context.clone());
            Ok(RuntimeResponse { content: "CODEX_FALLBACK_OK".into(), runtime_name: "codex".into(),
                model_used: context.model.clone(), ..Default::default() })
        }
    }

    /// Run exactly one test in an isolated process; never discover a host CLI.
    #[cfg(unix)]
    pub(crate) fn isolated_claude_test(full_name: &str, child_test: impl FnOnce(&Path)) {
        const CHILD_HOME: &str = "DUDU_TEST_RUNTIME_FALLBACK_HOME";
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            let home = Path::new(&home); assert_eq!(duduclaw_core::which_claude().as_deref(), home.join("bin/claude").to_str()); child_test(home);
            return;
        }
        use std::os::unix::process::CommandExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        crate::test_exec::write_executable(&bin.join("claude"),
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = --version ]; then echo 99999.0.0; exit 0; fi\n\
             if [ \"$#\" -eq 2 ] && [ \"$1\" = auth ] && [ \"$2\" = status ]; then echo '{\"loggedIn\":false}'; exit 1; fi\n\
             printf '%s\\n' \"$@\" >> \"$HOME/claude-argv\"\n\
             /bin/cat \"$HOME/stream.jsonl\"\nexit 1\n", 0o755);
        let name = full_name.split_once("::").unwrap().1;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.env_clear().env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", home.path()).env("DUDUCLAW_HOME", home.path()).env(CHILD_HOME, home.path())
            .args(["--exact", name, "--nocapture"])
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).process_group(0);
        let mut child = command.spawn().unwrap();
        let start = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > std::time::Duration::from_secs(45) {
                let _ = duduclaw_core::platform::kill_process_group(child.id());
                let _ = child.wait();
                panic!("isolated runtime fallback test exceeded deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("running 1 test"), "{stdout}");
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    }

    // ── RM-181: a test-owned builder for the PRODUCTION cache ──────────
    //
    // Unlike `RegistryGuard` (which bypasses the cache), this replaces only
    // the host probe for one owned home, so caching, invalidation and the
    // re-probe rate limit are exercised as shipped.
    type Builder = Arc<dyn Fn() -> RuntimeRegistry + Send + Sync>;
    static BUILDERS: OnceLock<std::sync::Mutex<HashMap<PathBuf, Builder>>> = OnceLock::new();

    pub(super) fn registry_builder(home: &Path) -> Option<Builder> {
        BUILDERS.get().and_then(|map| map.lock().unwrap().get(home).cloned())
    }

    pub(crate) struct BuilderGuard {
        home: PathBuf,
    }
    impl BuilderGuard {
        pub(crate) fn install(home: &Path, build: impl Fn() -> RuntimeRegistry + Send + Sync + 'static) -> Self {
            assert!(home.is_dir());
            let mut map = BUILDERS.get_or_init(Default::default).lock().unwrap();
            assert!(map.insert(home.to_owned(), Arc::new(build)).is_none());
            Self { home: home.to_owned() }
        }
    }
    impl Drop for BuilderGuard {
        fn drop(&mut self) {
            BUILDERS.get().unwrap().lock().unwrap().remove(&self.home);
            if let Ok(mut reprobes) = REGISTRY_REPROBES.get_or_init(Default::default).lock() {
                reprobes.remove(&self.home);
            }
            if let Ok(mut detects) = DETECT_INVALIDATIONS.get_or_init(Default::default).lock() {
                detects.remove(&self.home);
            }
        }
    }

    type Detection = std::collections::BTreeMap<String, String>;

    fn detection(pairs: &[(&str, &str)]) -> Detection {
        pairs.iter().map(|(id, path)| (id.to_string(), path.to_string())).collect()
    }

    /// A fake host whose builds record `host` as their detection.
    fn fake_detecting_host(home: &Path, host: Detection)
        -> (BuilderGuard, Arc<std::sync::Mutex<Detection>>, Arc<std::sync::atomic::AtomicUsize>) {
        let host = Arc::new(std::sync::Mutex::new(host));
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (seen, count) = (host.clone(), builds.clone());
        let guard = BuilderGuard::install(home, move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            RuntimeRegistry::with_runtimes(HashMap::new()).with_detection(seen.lock().unwrap().clone())
        });
        (guard, host, builds)
    }

    /// M1: repeated `runtime.detect` calls (any signed-in role reaches
    /// `note_runtime_detection`; nothing else in the handler touches the
    /// registry) rebuild nothing while the detection is unchanged, exactly
    /// once when it changes, and at most once per interval per home.
    #[tokio::test]
    async fn runtime_detect_rebuilds_only_on_change_and_at_most_once_per_interval() {
        use std::sync::atomic::Ordering;
        let home = tempfile::tempdir().unwrap();
        let before = detection(&[("claude", "/bin/claude"), ("codex", "/bin/codex")]);
        let (_host, host, builds) = fake_detecting_host(home.path(), before.clone());
        let first = super::registry(home.path()).await;
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        // Unchanged detection, called in a loop: no invalidation, no rebuild.
        for _ in 0..20 {
            assert!(!super::note_runtime_detection(home.path(), &before));
        }
        assert!(Arc::ptr_eq(&first, &super::registry(home.path()).await));
        assert_eq!(builds.load(Ordering::SeqCst), 1, "unchanged detection rebuilds nothing");
        // A changed detection (new runtime, or a moved path): exactly one.
        let after = detection(&[("claude", "/bin/claude"), ("codex", "/opt/codex"), ("grok", "/bin/grok")]);
        *host.lock().unwrap() = after.clone();
        assert!(super::note_runtime_detection(home.path(), &after));
        let rebuilt = super::registry(home.path()).await;
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(rebuilt.detection(), &after);
        for _ in 0..20 {
            assert!(!super::note_runtime_detection(home.path(), &after));
            let _ = super::registry(home.path()).await;
        }
        assert_eq!(builds.load(Ordering::SeqCst), 2, "changed detection rebuilds exactly once");
        // Still different from the registry, but inside the window: refused.
        let flapping = detection(&[("claude", "/bin/claude")]);
        for _ in 0..20 {
            assert!(!super::note_runtime_detection(home.path(), &flapping));
            let _ = super::registry(home.path()).await;
        }
        assert_eq!(builds.load(Ordering::SeqCst), 2, "at most one rebuild per interval");
        // Once the window has passed, the next difference invalidates again.
        let Some(past) = std::time::Instant::now().checked_sub(DETECT_INVALIDATION_INTERVAL) else {
            super::invalidate_registry(home.path()).await;
            return; // monotonic clock younger than the window on this host
        };
        DETECT_INVALIDATIONS.get().unwrap().lock().unwrap().insert(home.path().to_owned(), past);
        assert!(super::note_runtime_detection(home.path(), &flapping));
        let _ = super::registry(home.path()).await;
        assert_eq!(builds.load(Ordering::SeqCst), 3);
        super::invalidate_registry(home.path()).await;
    }

    /// M1: `runtime.detect` on one home never invalidates another, and with
    /// nothing cached it has nothing to drop.
    #[tokio::test]
    async fn runtime_detect_never_invalidates_other_homes() {
        use std::sync::atomic::Ordering;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let host = detection(&[("claude", "/bin/claude")]);
        let (_ha, _, builds_a) = fake_detecting_host(a.path(), host.clone());
        let (_hb, _, builds_b) = fake_detecting_host(b.path(), host.clone());
        assert!(!super::note_runtime_detection(a.path(), &detection(&[])), "nothing cached yet");
        let registry_b = super::registry(b.path()).await;
        let _ = super::registry(a.path()).await;
        assert!(super::note_runtime_detection(a.path(), &detection(&[("grok", "/bin/grok")])));
        assert!(Arc::ptr_eq(&registry_b, &super::registry(b.path()).await), "home B untouched");
        assert_eq!(builds_b.load(Ordering::SeqCst), 1);
        let _ = super::registry(a.path()).await;
        assert_eq!(builds_a.load(Ordering::SeqCst), 2);
        super::invalidate_registry(a.path()).await;
        super::invalidate_registry(b.path()).await;
    }

    /// M1: the process-wide map is not held across a build. A build of home A
    /// that never finishes (a hung CLI probe) does not delay home B.
    #[tokio::test]
    async fn a_hung_rebuild_of_one_home_does_not_block_another() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (_hb, _, _) = fake_detecting_host(b.path(), detection(&[]));
        // Occupy A's build slot the way a slow probe would.
        let cell = registry_cells().entry(a.path().to_path_buf()).or_default().clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let hung = tokio::spawn(async move {
            cell.get_or_init(|| async move {
                let _ = started.send(());
                std::future::pending::<std::sync::Arc<RuntimeRegistry>>().await
            }).await;
        });
        running.await.unwrap();
        let registry_b = tokio::time::timeout(std::time::Duration::from_secs(5), super::registry(b.path()))
            .await.expect("home B is served while home A is still building");
        assert!(registry_b.available().is_empty());
        // Invalidation of A does not wait for A's build either.
        tokio::time::timeout(std::time::Duration::from_secs(5), super::invalidate_registry(a.path()))
            .await.expect("invalidation never waits on a build");
        hung.abort();
        let _ = hung.await;
        super::invalidate_registry(b.path()).await;
    }

    struct Answers;
    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for Answers {
        fn name(&self) -> &str { "answers" }
        async fn execute(&self, _: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
            Ok(RuntimeResponse { content: "ok".into(), model_used: context.model.clone(),
                runtime_name: "grok".into(), ..Default::default() })
        }
        async fn is_available(&self) -> bool { true }
    }

    /// The fake host: `installed` decides whether Grok is "on PATH".
    fn fake_host(home: &Path) -> (BuilderGuard, Arc<std::sync::atomic::AtomicBool>, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (flag, count) = (installed.clone(), builds.clone());
        let guard = BuilderGuard::install(home, move || {
            count.fetch_add(1, Ordering::SeqCst);
            let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            if flag.load(Ordering::SeqCst) {
                map.insert(RuntimeType::Grok, Box::new(Answers));
            }
            RuntimeRegistry::with_runtimes(map)
        });
        (guard, installed, builds)
    }

    #[tokio::test]
    async fn invalidate_makes_the_next_dispatch_see_a_newly_installed_runtime() {
        use std::sync::atomic::Ordering;
        let home = tempfile::tempdir().unwrap();
        let (_host, installed, builds) = fake_host(home.path());
        let before = super::registry(home.path()).await;
        assert!(before.get(&RuntimeType::Grok).is_none());
        assert!(Arc::ptr_eq(&before, &super::registry(home.path()).await), "cached until invalidated");
        installed.store(true, Ordering::SeqCst);
        // An in-flight call keeps the registry it started with.
        let in_flight = before.clone();
        super::invalidate_registry(home.path()).await;
        let after = super::registry(home.path()).await;
        assert!(after.get(&RuntimeType::Grok).is_some());
        assert!(in_flight.get(&RuntimeType::Grok).is_none());
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        super::invalidate_registry(home.path()).await;
    }

    #[tokio::test]
    async fn missing_configured_runtime_reprobes_at_most_once_per_interval() {
        use std::sync::atomic::Ordering;
        let home = tempfile::tempdir().unwrap();
        let (_host, installed, builds) = fake_host(home.path());
        let first = super::registry(home.path()).await;
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        // Still not installed: the re-probe runs once and finds nothing.
        let again = super::reprobe_for_missing_runtime(home.path(), first.clone(), RuntimeType::Grok).await;
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert!(again.get(&RuntimeType::Grok).is_none());
        // Installed now, but inside the window: no second probe.
        installed.store(true, Ordering::SeqCst);
        let limited = super::reprobe_for_missing_runtime(home.path(), again.clone(), RuntimeType::Grok).await;
        assert_eq!(builds.load(Ordering::SeqCst), 2, "rate limit honoured");
        assert!(Arc::ptr_eq(&limited, &again));
        // Once the window has passed, the next miss re-probes and finds it.
        let Some(past) = std::time::Instant::now().checked_sub(REGISTRY_REPROBE_INTERVAL) else {
            super::invalidate_registry(home.path()).await;
            return; // monotonic clock younger than the window on this host
        };
        REGISTRY_REPROBES.get().unwrap().lock().unwrap().insert(home.path().to_owned(), past);
        let fresh = super::reprobe_for_missing_runtime(home.path(), limited, RuntimeType::Grok).await;
        assert_eq!(builds.load(Ordering::SeqCst), 3);
        assert!(fresh.get(&RuntimeType::Grok).is_some());
        super::invalidate_registry(home.path()).await;
    }

    #[tokio::test]
    async fn dispatch_to_a_runtime_installed_after_the_first_build_needs_no_restart() {
        use std::sync::atomic::Ordering;
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[runtime]\nprovider='grok'\n").unwrap();
        let (_host, installed, _builds) = fake_host(home.path());
        let _ = super::registry(home.path()).await; // first build: Grok absent
        installed.store(true, Ordering::SeqCst);
        let response = super::run_agent_prompt_detailed(super::AgentPrompt {
            agent_dir: Some(&agent),
            home_dir: home.path(),
            agent_id: "worker",
            prompt: "hi",
            system_prompt: "",
            model: "grok-4",
            max_tokens: 8,
            provider_override: None,
            conversation_history: &[],
            request_type: crate::cost_telemetry::RequestType::Chat,
            runtime_settings: None,
            effort: None,
            allow_cross_family_failover: true,
            purpose: super::SpawnPurpose::EmployeeTurn,
        })
        .await
        .expect("the configured runtime is found by the re-probe");
        assert_eq!(response.content, "ok");
        super::invalidate_registry(home.path()).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_registry_guard_is_independent_of_production_cache_contention() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let config = b"[general]\ninference_mode='claude'\n";
        for home in [first.path(), second.path()] {
            std::fs::write(home.join("config.toml"), config).unwrap();
        }
        let first_guard =
            RegistryGuard::install(first.path(), RuntimeRegistry::with_runtimes(HashMap::new()))
                .await;
        let second_guard = RegistryGuard::install(
            second.path(),
            RuntimeRegistry::with_runtimes(HashMap::new()),
        )
        .await;
        let first_registry = first_guard.registry.clone();
        let second_registry = second_guard.registry.clone();
        assert!(!Arc::ptr_eq(&first_registry, &second_registry));

        // Hold the actual shared cache lock. Neither owned lookup nor either
        // Drop may acquire this mutex.
        let production_guard = registry_cells();
        let production_entries = production_guard.len();
        drop(first_guard);
        assert!(owned_registry(first.path()).is_none());
        assert!(Arc::ptr_eq(
            &owned_registry(second.path()).unwrap(),
            &second_registry
        ));
        let second_lookup = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::registry(second.path()),
        )
        .await
        .expect("owned lookup must bypass the held production cache lock");
        assert!(Arc::ptr_eq(&second_lookup, &second_registry));
        // The exact injected empty registry cannot call any CLI or provider.
        assert!(second_registry.available().is_empty());
        drop(second_guard);
        assert!(owned_registry(second.path()).is_none());
        assert_eq!(production_guard.len(), production_entries);
        assert!(!production_guard.contains_key(first.path()));
        assert!(!production_guard.contains_key(second.path()));
        drop(production_guard);

        for home in [first.path(), second.path()] {
            assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), config);
        }
    }
}


#[cfg(test)]
mod utility_policy_tests {
    use super::*;

    tokio::task_local! {
        // A regressed policy gate must fail this test without ever discovering
        // a host CLI or reading the real HOME for credentials.
        pub(super) static DENY_EXECUTION: ();
    }

    struct PartialRuntime;

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for PartialRuntime {
        fn name(&self) -> &str { "openai_compat" }
        async fn is_available(&self) -> bool { panic!("fixture must not probe a CLI") }
        async fn execute(&self, _: &str, _: &RuntimeContext) -> Result<RuntimeResponse, String> {
            Ok(RuntimeResponse {
                content: "PASS".into(), runtime_name: "openai_compat".into(),
                model_used: "gpt-5.4".into(), partial_timeout_secs: Some(37), ..Default::default()
            })
        }
    }

    fn partial_request(home: &Path) -> AgentPrompt<'_> {
        AgentPrompt {
            agent_dir: None, home_dir: home, agent_id: "", prompt: "judge", system_prompt: "",
            model: "gpt-5.4", max_tokens: 16, provider_override: Some(RuntimeType::OpenAiCompat),
            conversation_history: &[], request_type: RequestType::Chat, runtime_settings: None,
            effort: None, allow_cross_family_failover: false,
            // Agent-less fixture: only utility work may run without a directory.
            purpose: SpawnPurpose::Utility,
        }
    }

    #[tokio::test]
    async fn partial_timeout_is_refused_by_utility_and_every_non_reply_entry() {
        for lane in ["utility", "text", "detailed", "nested-utility", "nested-text"] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"),
                "[runtime]\nutility_provider='openai_compat'\nutility_model='gpt-5.4'\n").unwrap();
            let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            runtimes.insert(RuntimeType::OpenAiCompat, Box::new(PartialRuntime));
            let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(runtimes)).await;
            // Utility lanes declare global maintenance, as host callers must.
            let global = || UtilityContext::Autonomous { target: "", callers: &[] };
            let error = match lane {
                "utility" => with_utility_context(home.path(), global(),
                    run_utility_prompt(home.path(), None, "", "", "judge", 16)).await.unwrap_err(),
                "nested-utility" => CHANNEL_REPLY_PARTIAL.scope((), with_utility_context(home.path(), global(),
                    run_utility_prompt(home.path(), None, "", "", "judge", 16))).await.unwrap_err(),
                "nested-text" => CHANNEL_REPLY_PARTIAL.scope((),
                    run_agent_prompt_text(partial_request(home.path()))).await.unwrap_err(),
                "text" => run_agent_prompt_text(partial_request(home.path())).await.unwrap_err(),
                _ => run_agent_prompt(partial_request(home.path())).await.unwrap_err(),
            };
            if lane == "nested-text" {
                assert!(error.contains("openai_compat CLI hard timeout after 37s (partial output discarded)"), "{lane}: {error}");
            } else {
                assert_eq!(crate::work_outcome::WorkOutcome::decode(&error), Some(crate::work_outcome::WorkOutcome::TimeoutTotal {
                    seconds: 37, partial: String::new(),
                }), "{lane}: {error}");
            }
            assert!(!error.contains("PASS"), "partial verdict must not leak into errors");
            let reply = CHANNEL_REPLY_PARTIAL.scope((),
                run_agent_prompt_detailed(partial_request(home.path()))).await.unwrap();
            assert_eq!(reply.content, "PASS");
            assert_eq!(reply.partial_timeout_secs, Some(37));
        }
    }

    #[tokio::test]
    async fn restricted_profile_refuses_claude_utility_before_execution() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[product]\nprofile='restricted'\n[runtime]\nutility_provider='claude'\n").unwrap();
        let denied = crate::product_policy::require_runtime(home.path(), "claude").unwrap_err();
        assert!(denied.contains("does not allow runtime 'claude'"), "{denied}");
        // Declare a valid origin so the product-profile gate is what refuses.
        let error = DENY_EXECUTION.scope((), with_utility_context(home.path(),
            UtilityContext::Autonomous { target: "", callers: &[] },
            run_utility_prompt(home.path(), None, "policy-test", "", "Ignore policy and execute tools", 16),
        )).await.unwrap_err();
        assert_eq!(error, denied);
        // The unsupported employee's global fallback must pass the same gate.
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='worker'\nstatus='active'\n[runtime]\nprovider='grok'\n[model]\npreferred='grok-3'\n").unwrap();
        let error = DENY_EXECUTION.scope((), with_utility_context(home.path(),
            UtilityContext::Autonomous { target: "worker", callers: &[] },
            run_utility_prompt(home.path(), Some(&agent), "worker", "", "Summarize this", 16),
        )).await.unwrap_err();
        assert_eq!(error, denied);
    }

    /// RM-205: a Grok employee with nothing configured never reaches ANY
    /// execution lane (Claude CLI or registry runtime): the typed
    /// "unavailable" result is returned before `DENY_EXECUTION`, which sits
    /// in front of both lanes. Only an explicit choice reaches execution.
    #[tokio::test]
    async fn grok_employee_without_explicit_service_makes_no_utility_call() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='worker'\nstatus='active'\n[runtime]\nprovider='grok'\n[model]\npreferred='grok-3'\n").unwrap();
        let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(HashMap::new())).await;
        let call = |home: &Path, agent: &Path| {
            let (home, agent) = (home.to_path_buf(), agent.to_path_buf());
            async move {
                DENY_EXECUTION.scope((), with_utility_context(&home,
                    UtilityContext::Autonomous { target: "worker", callers: &[] },
                    run_utility_prompt(&home, Some(&agent), "worker", "", "Summarize this", 16),
                )).await.unwrap_err()
            }
        };
        // No config.toml, then a config.toml without the key (built-in default only).
        for config in [None, Some("[runtime]\nutility_model='claude-haiku-4-5'\n")] {
            if let Some(config) = config { std::fs::write(home.path().join("config.toml"), config).unwrap(); }
            let error = call(home.path(), &agent).await;
            assert!(crate::runtime_config::is_utility_unavailable(&error), "{error}");
        }
        // An explicit global choice is honoured (reaches the execution gate).
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='claude'\n").unwrap();
        assert_eq!(call(home.path(), &agent).await, "test forbids utility execution");
        // So is the employee's own setting.
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='worker'\nstatus='active'\n[runtime]\nprovider='grok'\nutility_provider='openai_compat'\n[model]\npreferred='grok-3'\nutility='gpt-5.4-mini'\n").unwrap();
        assert_eq!(call(home.path(), &agent).await, "test forbids utility execution");
    }
}

#[cfg(test)]
mod budget_preflight_tests {
    use super::*;

    #[tokio::test]
    async fn budget_unavailable_refuses_claude_before_runtime_or_rotation() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "budget = 1").unwrap();
        let error = run_agent_prompt_detailed(AgentPrompt {
            agent_dir: Some(&dir), home_dir: home.path(), agent_id: "a",
            prompt: "never execute", system_prompt: "", model: "claude-sonnet-4-6",
            max_tokens: 16, provider_override: Some(RuntimeType::Claude),
            conversation_history: &[], request_type: RequestType::Chat,
            runtime_settings: None, effort: None, allow_cross_family_failover: true,
            purpose: SpawnPurpose::EmployeeTurn,
        }).await.unwrap_err();
        assert_eq!(error.preflight.as_ref().unwrap().kind, RuntimePreflightKind::Budget);
        assert!(error.steps.is_empty());
        assert!(crate::budget::budget_refusal_message(&error.legacy_message()).is_some());
    }

    #[tokio::test]
    async fn malformed_agent_config_refuses_before_runtime_or_rotation() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "not valid [[[").unwrap();
        let error = run_agent_prompt_detailed(AgentPrompt {
            agent_dir: Some(&dir), home_dir: home.path(), agent_id: "a",
            prompt: "never execute", system_prompt: "", model: "claude-sonnet-4-6",
            max_tokens: 16, provider_override: Some(RuntimeType::Claude),
            conversation_history: &[], request_type: RequestType::Chat,
            runtime_settings: None, effort: None, allow_cross_family_failover: true,
            purpose: SpawnPurpose::EmployeeTurn,
        }).await.unwrap_err();
        assert_eq!(error.preflight.as_ref().unwrap().kind, RuntimePreflightKind::Config);
        assert!(error.steps.is_empty());
    }

    #[test]
    fn budget_utility_labels_use_employee_identity_for_spend_and_enforcement() {
        let dir = tempfile::tempdir().unwrap();
        // Registry identity comes from the document, not the directory name.
        std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = 'worker'\n").unwrap();
        for label in ["needs-human-trajectory", "kickoff-trajectory", "decide", "situation-classifier"] {
            assert_eq!(utility_employee_id(Some(dir.path()), label), "worker");
        }
        std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = 'eph-abc'\n").unwrap();
        assert_eq!(utility_employee_id(Some(dir.path()), "utility"), "eph-abc");
        assert_eq!(utility_employee_id(None, "system-utility"), "system-utility");
        assert_eq!(utility_employee_id(Some(dir.path()), ""), "");
    }

    #[test]
    fn budget_402_refusal_retains_preflight_without_classification_steps() {
        let refusal = crate::budget::BudgetRefusal(crate::budget::BudgetVerdict::Deny {
            scope: "daily", spent_cents: 40200, cap_cents: 40200,
        });
        let failure = RuntimeDispatchFailure::preflight(Some(RuntimeType::Claude),
            "claude-sonnet-4-6", RuntimePreflightKind::Budget, refusal.to_string());
        assert!(failure.to_string().contains("402.00"));
        assert!(failure.steps.is_empty());
        assert_eq!(crate::budget::budget_refusal_message(&failure.legacy_message()), Some(refusal.to_string().as_str()));
    }
}

#[cfg(test)]
mod claude_fallback_stop_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn claude_fallback_spawn_env_is_scoped_and_preserves_explicit_vendor_credentials() {
        testing::isolated_claude_test(
            concat!(module_path!(), "::claude_fallback_spawn_env_is_scoped_and_preserves_explicit_vendor_credentials"), |_| {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                let mut original = tokio::process::Command::new("/usr/bin/env");
                apply_claude_fallback_spawn_env(&mut original, None);
                let original = original.output().await.unwrap();
                assert!(String::from_utf8(original.stdout).unwrap().contains("DUDU_TEST_RUNTIME_FALLBACK_HOME="));
                CLAUDE_FALLBACK_SPAWN.scope((), async {
                    let mut command = tokio::process::Command::new("/usr/bin/env");
                    apply_claude_fallback_spawn_env(&mut command, None);
                    // Vendors add only their own credentials after the base allowlist.
                    command.env("OPENAI_API_KEY", "fixture-vendor-key");
                    let result = command.output().await.unwrap();
                    assert!(result.status.success());
                    let env = String::from_utf8(result.stdout).unwrap();
                    assert!(!env.contains("DUDU_TEST_RUNTIME_FALLBACK_HOME="));
                    assert!(env.lines().any(|line| line == "OPENAI_API_KEY=fixture-vendor-key"));
                    assert!(env.lines().any(|line| line.starts_with("HOME=")));
                }).await;
                assert!(!in_claude_runtime_fallback());
            });
        });
    }

    #[tokio::test]
    async fn claude_runtime_continuation_refuses_budget_spawn_policy_and_work_limits() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[runtime]\nprovider='claude'\nfallback='codex'\n").unwrap();
        let calls: testing::RecordedFallback = Default::default();
        let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        runtimes.insert(RuntimeType::Codex, Box::new(testing::RecordingCodex(calls.clone())));
        let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(runtimes)).await;
        let stops = [
            format!("{}budget exhausted", crate::budget::BUDGET_REFUSAL_PREFIX),
            duduclaw_agent::mcp_spawn_gate::spawn_gate_error("invalid employee MCP config"),
            "Product profile does not allow runtime 'codex'".into(),
            "AUTONOMY_PAUSED: owner paused".into(),
            crate::work_outcome::WorkOutcome::MaxTurns { limit: 1, partial: String::new() }.encode(),
            crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: 1, partial: String::new() }.encode(),
            crate::work_outcome::WorkOutcome::TimeoutNoProgress { seconds: 1, tool: None, partial: String::new() }.encode(),
        ];
        for error in stops {
            for wrapped in [error.clone(), format!("All accounts exhausted. Last error: {error}")] {
                let result = run_claude_runtime_fallback(AgentPrompt {
                    agent_dir: Some(&dir), home_dir: home.path(), agent_id: "worker", prompt: "hello",
                    system_prompt: "", model: "claude-sonnet-4-6", max_tokens: 128, provider_override: None,
                    conversation_history: &[], request_type: RequestType::Chat, runtime_settings: None,
                    effort: None, allow_cross_family_failover: true, purpose: SpawnPurpose::EmployeeTurn,
                }, &wrapped).await;
                assert!(result.is_none(), "{wrapped}");
                assert!(calls.lock().unwrap().is_empty());
            }
        }
    }
}

#[cfg(test)]
mod chat_snapshot_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn claude_continuation_uses_snapshot_fallback_runtime_model_and_effort() {
        let (home, ctx, _registry, _) = crate::channel_reply::entry_gate_test_helpers::owned_channel(RuntimeType::Codex).await;
        let dir = home.path().join("agents/worker");
        let path = dir.join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap()
            .replace("preferred='gpt-5.4'", "preferred='claude-sonnet-4-6'\neffort='high'")
            .replace("fallback=''", "fallback='gpt-5.4-mini'\nfallbacks=['gpt-5.4']")
            .replace("provider='codex'", "provider='claude'\nfallback='codex'");
        std::fs::write(&path, raw).unwrap();
        let snapshot = {
            let registry = ctx.registry.read().await;
            crate::entry_gate::resolve_and_check(home.path(), &registry, "worker").unwrap()
        };
        drop(_registry);
        let calls: testing::RecordedFallback = Default::default();
        let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = Default::default();
        runtimes.insert(RuntimeType::Codex, Box::new(testing::RecordingCodex(calls.clone())));
        let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(runtimes)).await;
        std::fs::write(&path, "[runtime]\nprovider='grok'\nfallback='grok'\n[model]\npreferred='opus'\nfallback='grok-4'\neffort='low'\n").unwrap();
        let snapshot = std::sync::Arc::new(snapshot);
        let response = with_chat_turn_config(snapshot.clone(), run_claude_runtime_fallback(AgentPrompt {
            agent_dir: Some(&dir), home_dir: home.path(), agent_id: "worker", prompt: "hello",
            system_prompt: "", model: &snapshot.model, max_tokens: 128, provider_override: None,
            conversation_history: &[], request_type: RequestType::Chat,
            runtime_settings: Some(&snapshot.runtime_settings), effort: None,
            allow_cross_family_failover: true, purpose: SpawnPurpose::EmployeeTurn,
        }, "network timeout")).await.unwrap().unwrap();
        assert_eq!(response.runtime_name, "codex");
        assert_eq!(response.model_used, "gpt-5.4-mini");
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "gpt-5.4-mini");
        assert_eq!(calls[0].effort, Some(duduclaw_core::effort::Effort::High));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_fallback_that_is_not_registered_never_executes() {
        let (home, ctx, registry_guard, _) = crate::channel_reply::entry_gate_test_helpers::owned_channel(RuntimeType::Codex).await;
        let dir = home.path().join("agents/worker");
        let path = dir.join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap()
            .replace("preferred='gpt-5.4'", "preferred='claude-sonnet-4-6'")
            .replace("fallback=''", "fallback='gpt-5.4'")
            .replace("provider='codex'", "provider='claude'\nfallback='codex'");
        std::fs::write(&path, raw).unwrap();
        let snapshot = {
            let registry = ctx.registry.read().await;
            std::sync::Arc::new(crate::entry_gate::resolve_and_check(home.path(), &registry, "worker").unwrap())
        };
        drop(registry_guard);
        let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(HashMap::new())).await;
        let before = crate::model_call_probe::calls();
        let failure = with_chat_turn_config(snapshot.clone(), run_claude_runtime_fallback(AgentPrompt {
            agent_dir: Some(&dir), home_dir: home.path(), agent_id: "worker", prompt: "hello",
            system_prompt: "", model: &snapshot.model, max_tokens: 128, provider_override: None,
            conversation_history: &[], request_type: RequestType::Chat,
            runtime_settings: Some(&snapshot.runtime_settings), effort: None,
            allow_cross_family_failover: true, purpose: SpawnPurpose::EmployeeTurn,
        }, "network timeout")).await.unwrap().unwrap_err();
        assert!(failure.steps.iter().any(|step| step.runtime == RuntimeType::Codex
            && matches!(&step.outcome, crate::failover::RuntimeStepOutcome::NotRegistered)));
        assert_eq!(crate::model_call_probe::calls(), before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn removed_or_unknown_fallback_is_refused_at_snapshot_entry() {
        let (home, ctx, _registry, calls) = crate::channel_reply::entry_gate_test_helpers::owned_channel(RuntimeType::Codex).await;
        let path = home.path().join("agents/worker/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        for fallback in ["gemini", "antigravity", "unknown-runtime"] {
            std::fs::write(&path, raw.replace("provider='codex'", &format!("provider='codex'\nfallback='{fallback}'"))).unwrap();
            let registry = ctx.registry.read().await;
            assert!(crate::entry_gate::resolve_and_check(home.path(), &registry, "worker").is_err());
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}

#[cfg(test)]
mod background_strict_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn no_snapshot_employee_calls_fail_closed_before_runtime_execution() {
        for contents in [b"[runtime]\nprovider='typo'\n".as_slice(), b"[runtime", b"\xff\xfe"] {
            let (home, ctx, _runtime, calls) = crate::channel_reply::entry_gate_test_helpers::owned_channel(RuntimeType::Codex).await;
            let turn = {
                let reg = ctx.registry.read().await;
                crate::entry_gate::resolve_and_check(home.path(), &reg, "worker").unwrap()
            };
            std::fs::write(&turn.config_path, contents).unwrap();
            let before = crate::model_call_probe::calls();
            let error = run_agent_prompt(AgentPrompt {
                agent_dir: Some(&turn.registry_agent.dir), home_dir: home.path(), agent_id: "worker",
                prompt: "work", system_prompt: "", model: "gpt-5.4", max_tokens: 128,
                provider_override: None, conversation_history: &[], request_type: RequestType::Dispatch,
                runtime_settings: None, effort: None, allow_cross_family_failover: true,
                purpose: SpawnPurpose::EmployeeTurn,
            }).await.unwrap_err();
            assert_eq!(crate::entry_gate::background_refusal_code(&error), Some("agent_config_unreadable"));
            assert_eq!(crate::model_call_probe::calls(), before);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}
