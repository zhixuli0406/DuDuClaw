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

/// Load `[runtime] provider` / `[runtime] fallback` / `[model] utility` from the
/// agent's `agent.toml` in a single read. Missing/malformed file ⇒ defaults
/// (Claude / no fallback / [`DEFAULT_UTILITY_MODEL`]).
pub fn load_runtime_settings(agent_dir: &Path) -> RuntimeSettings {
    let s = read_sections(agent_dir);

    // `RuntimeType::parse` (not serde) stays the string→enum step on purpose:
    // it maps an unrecognised provider to Claude with a warning instead of
    // erroring, so a typo remains a warning rather than losing the agent.
    let provider = s
        .runtime
        .provider
        .as_deref()
        .map(RuntimeType::parse)
        .unwrap_or_default();
    let fallback = s.runtime.fallback.as_deref().map(RuntimeType::parse);
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
/// Emits ONLY keys actually present in `agent.toml` (`provider`, `fallback`,
/// `pty_pool_enabled`, `worker_managed`) so the dashboard can distinguish
/// "unset" from an explicit `false` — the PTY-pool OAuth default-enable logic
/// materializes the toggle only when it was never written. A missing/malformed
/// file or absent `[runtime]` table yields an empty object.
pub fn read_runtime_json(agent_dir: &Path) -> serde_json::Value {
    // Every `RuntimeSection` field is `Option` precisely so this function can
    // still tell "unset" from an explicit `false` — collapsing unset into a
    // materialized default here would silently opt agents out of the PTY-pool
    // default-enable migration, which only fires when the key was never
    // written. The four keys are emitted individually (not via
    // `serde_json::to_value`) to keep that contract explicit and to keep the
    // two PTY timeout keys — which this form does not edit — out of the payload.
    let rt = read_sections(agent_dir).runtime;
    let mut obj = serde_json::Map::new();
    if let Some(s) = rt.provider {
        obj.insert("provider".into(), serde_json::Value::String(s));
    }
    if let Some(s) = rt.fallback {
        obj.insert("fallback".into(), serde_json::Value::String(s));
    }
    if let Some(b) = rt.pty_pool_enabled {
        obj.insert("pty_pool_enabled".into(), serde_json::Value::Bool(b));
    }
    if let Some(b) = rt.worker_managed {
        obj.insert("worker_managed".into(), serde_json::Value::Bool(b));
    }
    serde_json::Value::Object(obj)
}

/// Conservative provider↔model compatibility check by model-id naming family.
/// Only flags *confident* mismatches; unknown families and `openai_compat`
/// (which proxies arbitrary models) always pass.
pub fn model_matches_provider(model: &str, provider: RuntimeType) -> bool {
    let m = model.trim().to_ascii_lowercase();
    // Accept the qualified "provider/model" form — take the model part.
    let m = m.rsplit('/').next().unwrap_or(&m);
    let is_claude = m.starts_with("claude");
    let is_openai = m.starts_with("gpt-") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("codex");
    let is_gemini = m.starts_with("gemini");
    let is_grok = m.starts_with("grok");
    match provider {
        RuntimeType::Claude => !(is_openai || is_gemini || is_grok),
        RuntimeType::Codex => !(is_claude || is_gemini || is_grok),
        RuntimeType::Gemini | RuntimeType::Antigravity => !(is_claude || is_openai || is_grok),
        // Grok serves `grok-*` (e.g. grok-build-0.1, grok-4.x); reject other
        // known families, accept grok + unknowns.
        RuntimeType::Grok => !(is_claude || is_openai || is_gemini),
        RuntimeType::OpenAiCompat => true,
    }
}

/// Best-effort model-family → runtime mapping for the dashboard save-time
/// auto-align (`agents.update`): a confident family match returns that
/// family's CLI runtime when its binary is installed, else `openai_compat`
/// (API mode serves any model) so the aligned config is actually runnable.
/// Claude always maps to Claude (its API path is the Anthropic-native client,
/// not chat/completions). Unknown families return `None` — never guess.
pub fn infer_provider_for_model(model: &str) -> Option<RuntimeType> {
    let lower = model.trim().to_ascii_lowercase();
    let m = lower.rsplit('/').next().unwrap_or(&lower);
    if m.starts_with("claude") {
        return Some(RuntimeType::Claude);
    }
    let (family, cli_present) = if m.starts_with("grok") {
        (RuntimeType::Grok, duduclaw_core::which_grok().is_some())
    } else if m.starts_with("gemini") {
        (RuntimeType::Gemini, duduclaw_core::which_gemini().is_some())
    } else if m.starts_with("gpt-")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("codex")
    {
        (RuntimeType::Codex, duduclaw_core::which_codex().is_some())
    } else {
        return None;
    };
    Some(if cli_present {
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
        .map(RuntimeType::parse)
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
        .map(RuntimeType::parse)
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
        assert_eq!(agent_runtime_fallback(dir.path()), Some(RuntimeType::Claude));
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
        // Unknown families: never guess.
        assert_eq!(infer_provider_for_model("qwen3-8b"), None);
        assert_eq!(infer_provider_for_model(""), None);
    }

    #[test]
    fn read_runtime_json_emits_only_present_keys() {
        let dir = TempDir::new().unwrap();
        // Missing file → empty object.
        assert_eq!(read_runtime_json(dir.path()), serde_json::json!({}));
        // Present [runtime] but only some keys → only those keys emitted;
        // absent keys (worker_managed) must stay absent so the frontend can
        // distinguish "unset" from an explicit false.
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"claude\"\npty_pool_enabled = false\n",
        );
        assert_eq!(
            read_runtime_json(dir.path()),
            serde_json::json!({ "provider": "claude", "pty_pool_enabled": false })
        );
        // All four keys present → all emitted, correct types.
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"codex\"\nfallback = \"claude\"\npty_pool_enabled = true\nworker_managed = true\n",
        );
        assert_eq!(
            read_runtime_json(dir.path()),
            serde_json::json!({
                "provider": "codex",
                "fallback": "claude",
                "pty_pool_enabled": true,
                "worker_managed": true,
            })
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
        // `read_runtime_json` must distinguish "never written" from an
        // explicit `false`: the PTY-pool default-enable migration only fires
        // on the former. Materializing a default here would silently opt
        // agents out of it.
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"codex\"\n");
        let json = read_runtime_json(dir.path());
        assert_eq!(json.get("provider").and_then(|v| v.as_str()), Some("codex"));
        assert!(json.get("pty_pool_enabled").is_none(), "unset must be absent");
        assert!(json.get("worker_managed").is_none(), "unset must be absent");

        write_agent_toml(dir.path(), "[runtime]\npty_pool_enabled = false\n");
        let json = read_runtime_json(dir.path());
        assert_eq!(
            json.get("pty_pool_enabled").and_then(|v| v.as_bool()),
            Some(false),
            "explicit false must be present and false"
        );

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
}
