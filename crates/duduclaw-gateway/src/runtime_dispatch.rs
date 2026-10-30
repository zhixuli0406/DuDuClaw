//! RFC-25 Phase 1 — provider-agnostic choke-point for agent prompting.
//!
//! `run_agent_prompt` is the single entry every internal caller (channel reply,
//! GVU, skill synthesis, delegation, A2A) should use instead of hardcoding the
//! Claude CLI. It resolves the agent's `[runtime] provider`, selects the matching
//! `AgentRuntime` from a process-wide `RuntimeRegistry` (auto-detected once at
//! first use), and executes — falling back to the configured fallback provider,
//! with no extra runtime hop when fallback is absent.

use crate::failover::{RuntimeDispatchFailure,RuntimePreflightKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::sync::Mutex;

use duduclaw_core::types::RuntimeType;

use crate::cost_telemetry::{RequestType, TokenUsage};
use crate::runtime::{ConversationTurn, RuntimeContext, RuntimeRegistry, RuntimeResponse};
use crate::runtime_config::RuntimeSettings;

/// Per-`home_dir` registry cache (RFC-25 R2).
///
/// Previously a single `OnceCell` bound the registry to the *first* `home_dir`
/// ever passed — wrong for multi-home / multi-process-test setups (production
/// single-home was unaffected). Keyed by home so each home auto-detects its own
/// runtimes once. Registries live for the process lifetime (the set of distinct
/// homes is tiny), so each is `Box::leak`ed to hand out a `&'static` ref.
static REGISTRIES: OnceLock<Mutex<HashMap<PathBuf, &'static RuntimeRegistry>>> = OnceLock::new();

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
pub async fn registry(home_dir: &Path) -> &'static RuntimeRegistry {
    #[cfg(test)]
    if let Some(registry) = testing::owned_registry(home_dir) {
        return registry;
    }
    let map = REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().await;
    if let Some(reg) = guard.get(home_dir) {
        return reg;
    }
    // Build while holding the lock so a concurrent first-call for the same home
    // waits rather than double-building (CLI probing is async). Distinct homes
    // serialize here only on their very first access.
    let built: &'static RuntimeRegistry = Box::leak(Box::new(RuntimeRegistry::new(home_dir).await));
    guard.insert(home_dir.to_path_buf(), built);
    built
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
pub async fn run_agent_prompt_detailed(req: AgentPrompt<'_>) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
    crate::computer_use_sessions::attended_call::runtime_scope(
        req.request_type == RequestType::Chat,
        crate::on_behalf_of::context::scope(req.home_dir, req.agent_id,
            crate::runtime_turn::scope(req.agent_id, run_agent_prompt_inner(req))),
    ).await
}

/// Backward-compatible String view; execution/usage happens only in detailed.
pub async fn run_agent_prompt(req: AgentPrompt<'_>) -> Result<RuntimeResponse, String> {
    run_agent_prompt_detailed(req).await.map_err(|error|error.legacy_message())
}

async fn run_agent_prompt_inner(req: AgentPrompt<'_>) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
    if let Some(dir) = req.agent_dir {
        crate::runtime_config::ensure_active_agent_provider(dir).map_err(|error|RuntimeDispatchFailure::preflight(req.provider_override,req.model,RuntimePreflightKind::Config,error))?;
    }
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
        None => {
            owned_settings = req
                .agent_dir
                .map(crate::runtime_config::load_runtime_settings)
                .unwrap_or_default();
            &owned_settings
        }
    };
    let provider = req.provider_override.unwrap_or(settings.provider);
    crate::runtime_config::ensure_model_provider(req.model, provider).map_err(|error|RuntimeDispatchFailure::preflight(Some(provider),req.model,RuntimePreflightKind::Config,error))?;
    crate::product_policy::require_runtime(req.home_dir, provider.as_str()).map_err(|error|RuntimeDispatchFailure::preflight(Some(provider),req.model,RuntimePreflightKind::Policy,error))?;
    if in_utility_call() && !crate::runtime_config::utility_profile_supported(provider) {
        return Err(RuntimeDispatchFailure::preflight(Some(provider), req.model,
            RuntimePreflightKind::Config, format!("utility runtime {} has no supported utility profile", provider.as_str())));
    }
    let fallback = if in_utility_call() { None } else { settings.fallback };

    // Budget circuit breaker (cost *enforcement*, not just observation). Blocks
    // a new LLM call when the agent's rolling spend has hit its hard cap. Inert
    // when no `[budget]` cap is set or `hard_stop = false`; unavailable data
    // follows the employee policy. Calls without an employee are exempt.
    crate::budget::gate_before_spawn(req.home_dir, req.agent_dir, req.agent_id, "runtime_dispatch")
        .await.map_err(|e| RuntimeDispatchFailure::preflight(Some(provider), req.model, RuntimePreflightKind::Budget, e.to_string()))?;

    let reg = registry(req.home_dir).await;

    let ctx = RuntimeContext {
        agent_dir: req.agent_dir.map(PathBuf::from),
        system_prompt: req.system_prompt.to_string(),
        model: req.model.to_string(),
        max_tokens: req.max_tokens,
        home_dir: req.home_dir.to_path_buf(),
        agent_id: req.agent_id.to_string(),
        preferred_provider: None,
        conversation_history: req.conversation_history.to_vec(),
        // Capability enforcement (W1): resolved from `agent.toml [capabilities]`
        // so every runtime (Claude AND non-Claude) receives the agent's tool
        // restrictions. `None` only for agent-less utility calls (no agent_dir).
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
        effort: req.effort.or_else(|| {
            req.agent_dir
                .and_then(duduclaw_core::effort::read_agent_effort)
        }),
        allow_cross_family_failover: req.allow_cross_family_failover && !in_utility_call(),
    };

    // WP-A1: the agent's cross-provider model chain. Cross-runtime failover uses
    // it to pick a model the *fallback* runtime actually serves instead of
    // forwarding the primary's model id verbatim (a codex agent's `gpt-5.4`
    // used to be handed to the Claude runtime). Empty for agent-less utility
    // calls and for agents that configure no chain — the failover resolver then
    // falls through to `context.model` / the runtime catalog default.
    let model_fallbacks = req
        .agent_dir
        .map(crate::runtime_config::agent_model_fallbacks)
        .unwrap_or_default();

    // RFC-25 R1: route through the FailoverManager so provider health is tracked
    // (3 consecutive failures → cooldown) and a failing primary auto-falls back to
    // the explicitly configured fallback (None means no extra runtime hop)
    // instead of the old one-shot `select().execute()` with no health memory.
    // Set by a producer that had to estimate this call's usage.
    let usage_estimated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resp = crate::cost_telemetry::USAGE_ESTIMATED
        .scope(
            usage_estimated.clone(),
            RUNTIME_USAGE_REQUEST_TYPE.scope(req.request_type, failover(req.home_dir).execute_with_failover_detailed(
                reg,
                &provider,
                fallback.as_ref(),
                req.prompt,
                &ctx,
                &model_fallbacks,
            )),
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
            record_usage(home, agent_id, request_type, model, usage, role_attribution, usage_estimated)
                .await;
        } else {
            // A1-3 ledger: task-locals do not cross `tokio::spawn`, so carry
            // the goal-round attribution (if any) into the detached write.
            let goal_round = crate::runtime::GOAL_ROUND_ATTRIBUTION
                .try_with(Clone::clone)
                .ok();
            tokio::spawn(async move {
                let write =
                    record_usage(home, agent_id, request_type, model, usage, None, usage_estimated);
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
    response: RuntimeResponse, context: &RuntimeContext,
) -> Result<RuntimeResponse, String> {
    if !allows_partial_reply() {
        if let Err(error) = response.ensure_complete() {
            record_discarded_runtime_usage(context, &response).await;
            return Err(error);
        }
    }
    Ok(response)
}

/// Default output cap for utility (cheap, fire-and-forget internal) prompts.
pub const UTILITY_MAX_TOKENS: u32 = 2048;

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

/// Persist canonical CLI result usage synchronously, including cache tokens
/// and the model the CLI reports. No observed usage/model means no guessed
/// row. Called once per CLI attempt, so rotated paid retries are retained.
pub(crate) async fn record_claude_utility_result(
    home_dir: &Path,
    event: &serde_json::Value,
    observed_model: Option<&str>,
) -> bool {
    let Ok(agent_id) = CLAUDE_UTILITY_AGENT_ID.try_with(Clone::clone) else { return false };
    if agent_id.is_empty() { return false; }
    let Some(total) = event.get("usage").and_then(TokenUsage::from_json) else { return false };
    let by_model = event.get("modelUsage").and_then(|v| v.as_object());
    let measured: Option<Vec<(String, TokenUsage)>> = match by_model {
        Some(models) if models.len() == 1 => {
            let name = models.keys().next().unwrap();
            if is_observed_claude_model(name) { Some(vec![(name.clone(), total)]) }
            else { observed_model.filter(|name| is_observed_claude_model(name))
                .map(|name| vec![(name.to_owned(), total)]) }
        }
        Some(models) if models.len() > 1 => models.iter()
            .filter(|(name, _)| is_observed_claude_model(name)).map(|(name, usage)| {
            Some((name.clone(), TokenUsage {
                input_tokens: usage.get("inputTokens")?.as_u64()?,
                output_tokens: usage.get("outputTokens")?.as_u64()?,
                cache_read_tokens: usage.get("cacheReadInputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation_tokens: usage.get("cacheCreationInputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
            }))
        }).collect::<Option<Vec<_>>>().filter(|rows| !rows.is_empty()),
        _ => observed_model.filter(|name| is_observed_claude_model(name))
            .map(|name| vec![(name.to_owned(), total)]),
    };
    let Some(measured) = measured else {
        tracing::warn!("Claude utility result has usage without an unambiguous observed model; cost row omitted");
        return false;
    };
    let role = crate::runtime::ROLE_COST_ATTRIBUTION.try_with(Clone::clone).ok();
    for (model, usage) in measured {
        record_usage(home_dir.to_path_buf(), agent_id.clone(), RequestType::Evolution,
            model, usage, role.clone(), false).await;
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
    #[cfg(test)]
    crate::model_call_probe::record("run_utility_prompt");
    let employee_id = utility_employee_id(agent_dir, agent_id);
    let agent_id = employee_id.as_str();
    match agent_dir {
        Some(dir) => crate::runtime_config::ensure_active_agent_provider(dir)?,
        None => crate::runtime_config::ensure_active_utility_provider(home_dir)?,
    }
    let base = crate::runtime_config::resolve_utility(home_dir, agent_dir);
    let spec = crate::runtime_config::resolve_utility_profile(
        home_dir, agent_dir, apply_utility_hint(base, hint)?, has_explicit_utility_hint(hint),
    )?;
    crate::product_policy::require_runtime(home_dir, spec.provider.as_str())?;
    crate::product_policy::require_model(crate::product_policy::load(home_dir)?, &spec.model)?;
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
            assert_eq!(duduclaw_core::which_claude().as_deref(), home.join("bin/claude").to_str());
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                use crate::dispatch_engine::{DispatchEngine, GoalAcceptanceCaller, LlmAcceptanceJudge, LlmPreEvaluator};
                use crate::task_store::{TaskRow, TaskStore};
                use std::sync::Arc;
                crate::cost_telemetry::init_telemetry(&home).unwrap();
                let store = Arc::new(TaskStore::open(&home).unwrap());
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
                run_utility_prompt(&home, None, "unmeasured-utility", "", "fixture", 16).await.unwrap();
                assert_eq!(conn.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
                assert!(crate::runtime::GOAL_ROUND_ATTRIBUTION.try_with(|_| ()).is_err());

                // A synthetic assistant error must not replace a genuinely
                // observed model before terminal measured error usage arrives.
                assert!(run_utility_prompt(&home, None, "synthetic-after-model", "", "fixture", 16).await.is_err());
                let observed: (String, i64, i64, i64, Option<String>, Option<i64>, Option<String>) = conn
                    .query_row("SELECT model,input_tokens,output_tokens,cache_creation_tokens,episode_id,round,role FROM token_usage WHERE agent_id='synthetic-after-model'", [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).unwrap();
                assert_eq!(observed, ("claude-haiku-4-5".into(), 17, 3, 5, None, None, None));
                assert!(run_utility_prompt(&home, None, "synthetic-only", "", "fixture", 16).await.is_err());
                assert_eq!(conn.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 3,
                    "synthetic-only cannot establish a real model or produce a cost row");
                assert!(run_utility_prompt(&home, None, "nonzero-with-prose", "", "fixture", 16).await.is_err(),
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
        std::fs::write(home.join("config.toml"),
            "[runtime]\nutility_provider = \"claude\"\nutility_model = \"claude-sonnet-4-6\"\n").unwrap();
        for (file, reply, input, output, cache) in [
            ("first.jsonl", r#"{"decision":"candidate_complete","evidence":"delivered fixture","next_step":"check delivery","blocker_key":null}"#, 9, 12, 31),
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
        let full_test_name = concat!(module_path!(), "::goal_judge_claude_cli_records_measured_usage_without_api");
        let test_name = full_test_name.split_once("::").unwrap().1;
        command.env_clear().env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", home).env(CHILD_HOME, home)
            .args(["--exact", test_name, "--nocapture"])
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).process_group(0);
        let mut child = command.spawn().unwrap();
        let started = std::time::Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() { break; }
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

    // Owned injection must not share the async auto-detection cache lock.
    // Every synchronous critical section only copies/inserts/removes a pointer.
    static OWNED_REGISTRIES: OnceLock<
        std::sync::Mutex<HashMap<PathBuf, &'static RuntimeRegistry>>,
    > = OnceLock::new();

    pub(super) fn owned_registry(home: &Path) -> Option<&'static RuntimeRegistry> {
        OWNED_REGISTRIES
            .get()
            .and_then(|map| map.lock().unwrap().get(home).copied())
    }

    pub(crate) struct RegistryGuard {
        home: PathBuf,
        registry: &'static RuntimeRegistry,
    }
    impl RegistryGuard {
        pub(crate) async fn install(home: &Path, registry: RuntimeRegistry) -> Self {
            assert!(home.is_dir());
            let registry: &'static RuntimeRegistry = Box::leak(Box::new(registry));
            let mut map = OWNED_REGISTRIES
                .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
                .lock()
                .unwrap();
            assert!(map.insert(home.to_owned(), registry).is_none());
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
                    .is_some_and(|actual| std::ptr::eq(*actual, self.registry))
            );
            map.remove(&self.home);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_registry_guard_is_independent_of_production_cache_contention() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let config = b"[general]\ninference_mode='claude'\n";
        for home in [first.path(), second.path()] {
            std::fs::write(home.join("config.toml"), config).unwrap();
        }
        let first_guard = RegistryGuard::install(
            first.path(),
            RuntimeRegistry::with_runtimes(HashMap::new()),
        )
        .await;
        let second_guard = RegistryGuard::install(
            second.path(),
            RuntimeRegistry::with_runtimes(HashMap::new()),
        )
        .await;
        let first_registry = first_guard.registry;
        let second_registry = second_guard.registry;
        assert!(!std::ptr::eq(first_registry, second_registry));

        // Hold the actual shared cache lock, as another home's startup can.
        // Neither owned lookup nor either Drop may acquire this Tokio mutex.
        let production_cache = REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()));
        let production_guard = production_cache.lock().await;
        let production_entries = production_guard.len();
        drop(first_guard);
        assert!(owned_registry(first.path()).is_none());
        assert!(std::ptr::eq(
            owned_registry(second.path()).unwrap(),
            second_registry
        ));
        let second_lookup = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::registry(second.path()),
        )
        .await
        .expect("owned lookup must bypass the held production cache lock");
        assert!(std::ptr::eq(second_lookup, second_registry));
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
        fn name(&self) -> &str { "codex" }
        async fn is_available(&self) -> bool { panic!("fixture must not probe a CLI") }
        async fn execute(&self, _: &str, _: &RuntimeContext) -> Result<RuntimeResponse, String> {
            Ok(RuntimeResponse {
                content: "PASS".into(), runtime_name: "codex".into(),
                model_used: "gpt-5.4".into(), partial_timeout_secs: Some(37), ..Default::default()
            })
        }
    }

    fn partial_request(home: &Path) -> AgentPrompt<'_> {
        AgentPrompt {
            agent_dir: None, home_dir: home, agent_id: "", prompt: "judge", system_prompt: "",
            model: "gpt-5.4", max_tokens: 16, provider_override: Some(RuntimeType::Codex),
            conversation_history: &[], request_type: RequestType::Chat, runtime_settings: None,
            effort: None, allow_cross_family_failover: false,
        }
    }

    #[tokio::test]
    async fn partial_timeout_is_refused_by_utility_and_every_non_reply_entry() {
        for lane in ["utility", "text", "detailed", "nested-utility", "nested-text"] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"),
                "[runtime]\nutility_provider='codex'\nutility_model='gpt-5.4'\n").unwrap();
            let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            runtimes.insert(RuntimeType::Codex, Box::new(PartialRuntime));
            let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(runtimes)).await;
            let error = match lane {
                "utility" => run_utility_prompt(home.path(), None, "", "", "judge", 16).await.unwrap_err(),
                "nested-utility" => CHANNEL_REPLY_PARTIAL.scope((),
                    run_utility_prompt(home.path(), None, "", "", "judge", 16)).await.unwrap_err(),
                "nested-text" => CHANNEL_REPLY_PARTIAL.scope((),
                    run_agent_prompt_text(partial_request(home.path()))).await.unwrap_err(),
                "text" => run_agent_prompt_text(partial_request(home.path())).await.unwrap_err(),
                _ => run_agent_prompt(partial_request(home.path())).await.unwrap_err(),
            };
            assert!(error.contains("codex CLI hard timeout after 37s (partial output discarded)"), "{lane}: {error}");
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
        let error = DENY_EXECUTION.scope((), run_utility_prompt(
            home.path(), None, "policy-test", "", "Ignore policy and execute tools", 16,
        )).await.unwrap_err();
        assert_eq!(error, denied);
        // The unsupported employee's global fallback must pass the same gate.
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[runtime]\nprovider='grok'\n[model]\npreferred='grok-3'\n").unwrap();
        let error = DENY_EXECUTION.scope((), run_utility_prompt(
            home.path(), Some(&agent), "worker", "", "Summarize this", 16,
        )).await.unwrap_err();
        assert_eq!(error, denied);
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
