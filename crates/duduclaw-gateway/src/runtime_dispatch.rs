//! RFC-25 Phase 1 — provider-agnostic choke-point for agent prompting.
//!
//! `run_agent_prompt` is the single entry every internal caller (channel reply,
//! GVU, skill synthesis, delegation, A2A) should use instead of hardcoding the
//! Claude CLI. It resolves the agent's `[runtime] provider`, selects the matching
//! `AgentRuntime` from a process-wide `RuntimeRegistry` (auto-detected once at
//! first use), and executes — falling back to the configured fallback provider,
//! then to Claude, when the primary runtime is unavailable.

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
    /// Model id within the chosen provider (e.g. claude-sonnet-4-6, gemini-2.5-flash).
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
/// Selection order: `provider_override` → `[runtime] provider` → `[runtime] fallback` → Claude.
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
pub async fn run_agent_prompt(req: AgentPrompt<'_>) -> Result<RuntimeResponse, String> {
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
    let fallback = settings
        .fallback
        // Always fall back to Claude (the always-available core) if nothing set.
        .or(Some(RuntimeType::Claude));

    // Budget circuit breaker (cost *enforcement*, not just observation). Blocks
    // a new LLM call when the agent's rolling spend has hit its hard cap. Inert
    // when no `[budget]` cap is set or `hard_stop = false`; fail-open when
    // telemetry is unavailable (a kill switch must not brick work if its own DB
    // hiccups). Utility calls (empty agent_id) are exempt.
    let budget = crate::budget::check_agent_budget(req.home_dir, req.agent_dir, req.agent_id).await;
    if budget.is_denied() {
        return Err(budget.user_message());
    }

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
        allow_cross_family_failover: req.allow_cross_family_failover,
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
    // the configured fallback (defaulting to the always-available Claude core)
    // instead of the old one-shot `select().execute()` with no health memory.
    let resp = failover(req.home_dir)
        .execute_with_failover(
            reg,
            &provider,
            fallback.as_ref(),
            req.prompt,
            &ctx,
            &model_fallbacks,
        )
        .await?;
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
            record_usage(home, agent_id, request_type, model, usage, role_attribution).await;
        } else {
            tokio::spawn(async move {
                record_usage(home, agent_id, request_type, model, usage, None).await;
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
    if let Some(a) = role_attribution {
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
    run_agent_prompt(req).await.map(|r| r.content)
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
/// Claude stays on the existing account-rotated CLI path
/// ([`crate::channel_reply::call_claude_cli_public`]) so its behavior is
/// byte-identical to the previous hardcoded `DEFAULT_UTILITY_MODEL` call; any
/// other provider routes through the registry choke-point.
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
}

/// The caller's required reply schema for this call, if any. `None` outside an
/// [`OUTPUT_SCHEMA`] scope — the universal case.
pub fn output_schema_override() -> Option<serde_json::Value> {
    OUTPUT_SCHEMA.try_with(|schema| (**schema).clone()).ok()
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
    let base = crate::runtime_config::resolve_utility(home_dir, agent_dir);
    let spec = apply_utility_hint(base, hint)?;
    // A hinted utility call is, by construction, the judge/evaluator path — so
    // it refuses a cross-family failover rather than letting
    // `execute_with_failover` substitute the default family's model behind the
    // operator's back (the live-test hole). An un-hinted call keeps failover
    // exactly as before.
    //
    // **Fixed 2026-09-28.** This used to compare the effective provider against
    // the base provider (`spec.provider == base_provider`), i.e. it opted out
    // only when the hint MOVED the provider. That reads the wrong signal: with
    // the global `[runtime] utility_provider` and `[dispatch] judge_provider`
    // both set to `codex` — a completely ordinary configuration, and the one a
    // decorrelated-judge setup converges on once the operator makes codex the
    // default utility too — the hint was a no-move, the flag came back `true`,
    // and the verifier could be silently answered by a substituted Claude model.
    // The operator asked for a specific judge family; whether that family also
    // happens to be the default is irrelevant to whether a substitution is
    // acceptable. The presence of a hint is the signal.
    let allow_cross_family_failover = !hint_opts_out_of_cross_family_failover(hint);
    // Live round 8: the reply-shape preference, honoured only where the CLI can
    // actually constrain output. Reported as a `debug!` on every other runtime
    // so "my judge still replies in prose" is answerable from the log instead
    // of by reading this function.
    let schema = hint.and_then(|h| h.output_schema.clone());
    if schema.is_some() && spec.provider != RuntimeType::Codex {
        tracing::debug!(
            provider = %spec.provider.as_str(),
            model = %spec.model,
            "output_schema requested but this runtime has no structured-output flag — ignored"
        );
    }
    if spec.provider == RuntimeType::Claude {
        // Review P2: Claude is the DEFAULT utility provider, and this branch
        // drops `hint.effort` on the floor — silently, on the most common path,
        // while `output_schema` above at least leaves a `debug!`. Wiring it
        // through needs an `effort` parameter on
        // `channel_reply::call_claude_cli_public` (the rotated helper it wraps
        // already has one), which is a signature change in a file this fix
        // wave does not own. Until then the drop is at least visible instead of
        // invisible: "my judge ignores `effort = high`" is answerable from the
        // log rather than by reading this function.
        if let Some(effort) = hint.and_then(|h| h.effort) {
            tracing::warn!(
                provider = %spec.provider.as_str(),
                model = %spec.model,
                effort = %effort.as_str(),
                "utility hint requested a reasoning effort, but the Claude utility path \
                 does not forward it yet — the call runs at the model's default effort"
            );
        }
        crate::channel_reply::call_claude_cli_public(prompt, &spec.model, system_prompt, home_dir)
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
        match schema {
            Some(schema) => OUTPUT_SCHEMA.scope(std::sync::Arc::new(schema), fut).await,
            None => fut.await,
        }
    }
}

/// Does this utility call opt out of cross-family failover? (Pure helper so
/// the rule in [`run_utility_prompt_with_hint`] is unit-testable without
/// spawning a runtime; production reads it through the same predicate.)
///
/// `true` ⇒ that call runs with `allow_cross_family_failover = false`.
///
/// The rule is "was a hint supplied", not "did the hint move the provider".
/// It replaced `hint_changes_provider` on 2026-09-28: comparing the effective
/// provider against the resolved default silently re-enabled substitution
/// whenever the judge family and the default utility family happened to
/// coincide (both `codex`, say), which is exactly the configuration a
/// decorrelated-judge setup drifts into.
pub fn hint_opts_out_of_cross_family_failover(hint: Option<&UtilityModelHint>) -> bool {
    hint.is_some()
}

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
                provider: Some(RuntimeType::Gemini),
                model: Some("gemini-3-pro-preview".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap();
        assert_eq!(got.provider, RuntimeType::Gemini);
        assert_eq!(got.model, "gemini-3-pro-preview");

        // Provider-only: the resolved model rides along, and is validated
        // against the NEW provider (claude-* on Gemini is a mismatch).
        let err = apply_utility_hint(
            base(),
            Some(&UtilityModelHint {
                provider: Some(RuntimeType::Gemini),
                model: None,
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap_err();
        assert!(err.contains("does not belong to runtime gemini"), "{err}");
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
                model: Some("gemini-3-pro-preview".into()),
                effort: None,
                output_schema: None,
            }),
        )
        .unwrap_err();
        assert!(
            err.starts_with("judge model gemini-3-pro-preview does not belong to runtime claude"),
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
                    provider: Some(RuntimeType::Gemini),
                    model: Some("some-unreleased-id".into()),
                    effort: None,
                    output_schema: None,
                })
            )
            .is_ok()
        );
    }

    #[test]
    fn every_hinted_utility_call_opts_out_of_cross_family_failover() {
        // This is the flag `run_utility_prompt_with_hint` computes: a hinted
        // call is the judge/evaluator path, and it must never be rescued by a
        // different family behind the operator's back (the live-test hole).
        assert!(hint_opts_out_of_cross_family_failover(Some(
            &UtilityModelHint {
                provider: Some(RuntimeType::Gemini),
                model: Some("gemini-3-pro-preview".into()),
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
        assert!(hint_opts_out_of_cross_family_failover(Some(
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
                hint_opts_out_of_cross_family_failover(Some(&hint)),
                "{hint:?} is still a judge-path call"
            );
        }

        // Only an un-hinted utility call keeps failover exactly as before.
        assert!(!hint_opts_out_of_cross_family_failover(None));
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
                provider: Some(RuntimeType::Gemini),
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
