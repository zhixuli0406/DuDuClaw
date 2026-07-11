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
static FAILOVERS: OnceLock<std::sync::Mutex<HashMap<PathBuf, &'static crate::failover::FailoverManager>>> =
    OnceLock::new();

fn failover(home_dir: &Path) -> &'static crate::failover::FailoverManager {
    let map = FAILOVERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    if let Some(mgr) = guard.get(home_dir) {
        return mgr;
    }
    let built: &'static crate::failover::FailoverManager =
        Box::leak(Box::new(crate::failover::FailoverManager::new(FAILOVER_COOLDOWN_SECS)));
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
    let built: &'static RuntimeRegistry =
        Box::leak(Box::new(RuntimeRegistry::new(home_dir).await));
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
    };

    // RFC-25 R1: route through the FailoverManager so provider health is tracked
    // (3 consecutive failures → cooldown) and a failing primary auto-falls back to
    // the configured fallback (defaulting to the always-available Claude core)
    // instead of the old one-shot `select().execute()` with no health memory.
    let resp = failover(req.home_dir)
        .execute_with_failover(reg, &provider, fallback.as_ref(), req.prompt, &ctx)
        .await?;
    // OTel: record the provider that actually answered (post-failover) and
    // usage on the `chat` span (see `crate::otel`). `gen_ai.request.model`
    // stays the *requested* model per semconv; `resp.model_used` may differ
    // on failover and is already visible via cost telemetry.
    {
        let span = tracing::Span::current();
        span.record(crate::otel::attrs::SYSTEM, resp.runtime_name.as_str());
        span.record(crate::otel::attrs::PROVIDER_NAME, resp.runtime_name.as_str());
        span.record(crate::otel::attrs::USAGE_INPUT_TOKENS, resp.input_tokens);
        span.record(crate::otel::attrs::USAGE_OUTPUT_TOKENS, resp.output_tokens);
    }
    // RFC-25 A3: record token usage so non-Claude (Codex/Gemini/OpenAI) calls are
    // visible to CostTelemetry / 200K price-cliff warnings / adaptive routing.
    // Best-effort and detached: telemetry is a SQLite write under a mutex, so it
    // must not add latency to the reply that's already in hand. Skip agent-less
    // utility calls (empty agent_id) to avoid empty-string attribution.
    if !req.agent_id.is_empty() {
        let home = req.home_dir.to_path_buf();
        let agent_id = req.agent_id.to_string();
        let request_type = req.request_type;
        let model = resp.model_used.clone();
        let usage = TokenUsage {
            input_tokens: resp.input_tokens,
            cache_read_tokens: resp.cache_read_tokens,
            cache_creation_tokens: 0,
            output_tokens: resp.output_tokens,
        };
        tokio::spawn(async move {
            record_usage(home, agent_id, request_type, model, usage).await;
        });
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
    telemetry
        .record(&agent_id, request_type, &model, &usage)
        .await;
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
    let spec = crate::runtime_config::resolve_utility(home_dir, agent_dir);
    if spec.provider == RuntimeType::Claude {
        crate::channel_reply::call_claude_cli_public(prompt, &spec.model, system_prompt, home_dir)
            .await
    } else {
        run_agent_prompt_text(AgentPrompt {
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
        })
        .await
    }
}
