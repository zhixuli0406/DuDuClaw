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
/// same `[model] utility` key and share this default — they are intentionally
/// parallel paths, not duplicated config.
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

/// All per-agent runtime/model settings from a single `agent.toml` read (RFC-25 L7).
///
/// Callers that need more than one field (the choke-point reads provider +
/// fallback; utility dispatch reads provider + utility model) should
/// [`load_runtime_settings`] once instead of calling the per-field accessors
/// repeatedly — each accessor re-reads and re-parses the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
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

/// Load `[runtime] provider` / `[runtime] fallback` / `[model] utility` from the
/// agent's `agent.toml` in a single read. Missing/malformed file ⇒ defaults
/// (Claude / no fallback / [`DEFAULT_UTILITY_MODEL`]).
pub fn load_runtime_settings(agent_dir: &Path) -> RuntimeSettings {
    let s = read_sections(agent_dir);

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
        .unwrap_or_else(|| DEFAULT_UTILITY_MODEL.to_string());

    // Provider↔model sanity check: `preferred = "gpt-5"` with the default
    // Claude provider used to route silently into the Claude CLI and fail at
    // the Anthropic API. Warn once per (agent, model) so the misconfiguration
    // is visible without spamming the hot reply path.
    if let Some(preferred) = s.model.preferred.as_deref() {
        if !model_matches_provider(preferred, provider) {
            warn_mismatch_once(agent_dir, preferred, provider);
        }
    }

    RuntimeSettings {
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
    // Same runtime, obviously fine. Otherwise the two must share a family —
    // which is how `antigravity` accepts `gemini-*` (both declare the `gemini`
    // prefix) without needing a special case.
    spec.id == owner.id
        || spec
            .model_prefixes
            .iter()
            .any(|p| owner.model_prefixes.contains(p))
}

/// The model-id family a model id **confidently** belongs to, expressed as the
/// owning runtime catalog id (`"claude"`, `"gemini"`, `"codex"`, …).
///
/// `None` for an id whose naming prefix no catalog entry claims — never a
/// guess.
pub fn model_family(model: &str) -> Option<&'static str> {
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

fn warn_mismatch_once(agent_dir: &Path, model: &str, provider: RuntimeType) {
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
                "[model] preferred does not match [runtime] provider — requests will \
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

/// Cross-provider direct-API fallback chain from `agent.toml [model] fallbacks`
/// (W3/G1). A list of qualified model ids (`"openai/gpt-5.4"`,
/// `"compat:deepseek/deepseek-v3.2"`, ...) tried in order after the preferred
/// model on the Direct-API path.
///
/// Blanks are dropped and each entry is trimmed. A missing/malformed file, a
/// missing `[model]` table, a missing key, or a non-array value all resolve to
/// an empty vec — which the Direct-API path treats as "no chain" and keeps its
/// existing single-shot behavior byte-identically (fail-safe).
pub fn agent_model_fallbacks(agent_dir: &Path) -> Vec<String> {
    // Trim-and-drop-blanks stays here rather than in the schema: the stored
    // value is what the operator wrote, and the dashboard edit form must be
    // able to echo it back verbatim.
    read_sections(agent_dir)
        .model
        .fallbacks
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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
//   utility_provider = "claude"   # claude | codex | gemini | openai_compat
//   utility_model    = "claude-haiku-4-5"
//
// Both layers fall back to Claude / DEFAULT_UTILITY_MODEL, so an absent or
// malformed file is fail-safe (identical to the previous hardcoded behavior).

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
/// Falls back to [`DEFAULT_UTILITY_MODEL`].
pub fn global_utility_model(home_dir: &Path) -> String {
    read_global_config(home_dir)
        .as_ref()
        .and_then(|v| v.get("runtime"))
        .and_then(|r| r.get("utility_model"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_UTILITY_MODEL.to_string())
}

/// Global utility provider + model in a single `config.toml` read.
/// Used by [`resolve_utility`] for the agent-less path so it doesn't parse the
/// file twice (once per field).
fn global_utility_spec(home_dir: &Path) -> UtilitySpec {
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
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_UTILITY_MODEL.to_string());
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
            "[runtime]\nprovider = \"gemini\"\nfallback = \"claude\"\n",
        );
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Gemini);
        assert_eq!(
            agent_runtime_fallback(dir.path()),
            Some(RuntimeType::Claude)
        );
    }

    /// R1 (2026-10): a deprecated runtime parses exactly as before; the
    /// notice is emitted once per (runtime, source) and never for a live one.
    #[test]
    fn deprecated_runtime_notice_is_first_sighting_only_and_value_unchanged() {
        let mut seen = std::collections::HashSet::new();
        let first = deprecated_runtime_first_notice(
            &mut seen,
            RuntimeType::Gemini,
            DeprecatedRuntimeSource::AgentProvider,
            Some("writer"),
        )
        .expect("first sighting warns");
        assert!(first.contains("(agent `writer`)"), "{first}");
        assert!(first.contains("agent.toml [runtime] provider"), "{first}");
        assert!(first.contains("antigravity"), "{first}");
        assert!(first.contains("v1.71.0"), "{first}");
        assert!(first.contains("docs/guides/deprecations.md"), "{first}");
        assert!(deprecated_runtime_first_notice(
            &mut seen,
            RuntimeType::Gemini,
            DeprecatedRuntimeSource::AgentProvider,
            Some("another-agent"),
        )
        .is_none());
        // Different source kind ⇒ its own one-time notice.
        for src in [
            DeprecatedRuntimeSource::AgentFallback,
            DeprecatedRuntimeSource::UtilityProvider,
            DeprecatedRuntimeSource::JudgeProvider,
            DeprecatedRuntimeSource::TeamRole,
            DeprecatedRuntimeSource::DiscoveryAttempt,
        ] {
            let notice = deprecated_runtime_first_notice(&mut seen, RuntimeType::Gemini, src, None);
            assert!(notice.as_deref().is_some_and(|n| !n.contains("(agent")), "{notice:?}");
            assert!(deprecated_runtime_first_notice(&mut seen, RuntimeType::Gemini, src, None).is_none());
        }
        for rt in [RuntimeType::Antigravity, RuntimeType::Claude, RuntimeType::Codex] {
            assert!(deprecated_runtime_first_notice(
                &mut seen,
                rt,
                DeprecatedRuntimeSource::AgentProvider,
                None,
            )
            .is_none());
        }
        assert!(!warn_once_if_deprecated_runtime(
            RuntimeType::Antigravity,
            DeprecatedRuntimeSource::AgentProvider
        ));

        // Parsing is unchanged: provider, fallback and utility_provider all
        // still resolve to the Gemini CLI.
        let dir = TempDir::new().unwrap();
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"gemini\"\nfallback = \"gemini\"\n",
        );
        let s = load_runtime_settings(dir.path());
        assert_eq!(s.provider, RuntimeType::Gemini);
        assert_eq!(s.fallback, Some(RuntimeType::Gemini));
        let home = TempDir::new().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[runtime]\nutility_provider = \"gemini\"\n",
        )
        .unwrap();
        assert_eq!(global_utility_provider(home.path()), RuntimeType::Gemini);
    }

    #[test]
    fn provider_unknown_falls_back_to_claude() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"nonsense\"\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
    }

    // ── W3/G1: [model] fallbacks chain ──────────────────────────────────

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
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"gemini\"\n");
        assert_eq!(agent_uses_non_claude(dir.path()), Some(RuntimeType::Gemini));
    }

    #[test]
    fn non_claude_provider_method() {
        assert_eq!(RuntimeSettings::default().non_claude_provider(), None);
        let s = RuntimeSettings {
            provider: RuntimeType::Codex,
            fallback: None,
            utility_model: DEFAULT_UTILITY_MODEL.to_string(),
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
    fn global_utility_reads_config() {
        let home = TempDir::new().unwrap();
        write_global_config(
            home.path(),
            "[runtime]\nutility_provider = \"gemini\"\nutility_model = \"gemini-2.5-flash\"\n",
        );
        assert_eq!(global_utility_provider(home.path()), RuntimeType::Gemini);
        assert_eq!(global_utility_model(home.path()), "gemini-2.5-flash");
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
        // Global says gemini; agent says codex — agent_dir present must win.
        write_global_config(home.path(), "[runtime]\nutility_provider = \"gemini\"\n");
        write_agent_toml(
            agent.path(),
            "[runtime]\nprovider = \"codex\"\n[model]\nutility = \"o4-mini\"\n",
        );
        let spec = resolve_utility(home.path(), Some(agent.path()));
        assert_eq!(spec.provider, RuntimeType::Codex);
        assert_eq!(spec.model, "o4-mini");
    }

    #[test]
    fn resolve_utility_without_agent_dir_uses_global() {
        let home = TempDir::new().unwrap();
        write_global_config(
            home.path(),
            "[runtime]\nutility_provider = \"gemini\"\nutility_model = \"gemini-2.5-flash\"\n",
        );
        let spec = resolve_utility(home.path(), None);
        assert_eq!(spec.provider, RuntimeType::Gemini);
        assert_eq!(spec.model, "gemini-2.5-flash");
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
            ("gemini-3.1-pro", Gemini),
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
        assert!(!model_matches_provider("gpt-5.4", Gemini));
        // R4: grok models reject other providers; foreign models reject Grok.
        assert!(!model_matches_provider("grok-build-0.1", Claude));
        assert!(!model_matches_provider("claude-sonnet-4-6", Grok));
        assert!(!model_matches_provider("gpt-5.4", Grok));
        assert!(!model_matches_provider("gemini-3.5-flash", Grok));
        // Correct pairings
        assert!(model_matches_provider("claude-haiku-4-5", Claude));
        assert!(model_matches_provider("gpt-5.4-mini", Codex));
        assert!(model_matches_provider("gemini-3.5-flash", Antigravity));
        assert!(model_matches_provider("grok-build-0.1", Grok));
        assert!(model_matches_provider("grok-4", Grok));
        // Qualified form + unknown families + compat always pass
        assert!(model_matches_provider("anthropic/claude-sonnet-5", Claude));
        assert!(model_matches_provider("deepseek-v3.2", Claude));
        assert!(model_matches_provider("claude-sonnet-4-6", OpenAiCompat));
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
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"gemini\"\n");
        assert_eq!(
            agent_runtime_provider(dir.path()),
            RuntimeType::Gemini,
            "a live agent.toml edit must be visible on the next call"
        );
    }

    /// R1 follow-up: the agent-scoped notices name the agent directory.
    #[test]
    fn agent_provider_notice_names_the_agent() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("sales-helper");
        std::fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "[runtime]\nprovider = \"gemini\"\nfallback = \"gemini\"\n");
        CONSULTED_NOTICES.with(|n| n.borrow_mut().clear());
        let s = load_runtime_settings(&dir);
        assert_eq!(s.provider, RuntimeType::Gemini);
        let consulted = CONSULTED_NOTICES.with(|n| n.borrow().clone());
        assert_eq!(
            consulted,
            vec![
                ("gemini", DeprecatedRuntimeSource::AgentProvider, Some("sales-helper".to_string())),
                ("gemini", DeprecatedRuntimeSource::AgentFallback, Some("sales-helper".to_string())),
            ]
        );
    }
}
