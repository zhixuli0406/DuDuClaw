//! Per-agent runtime/model config readers from `agent.toml` (RFC-25).
//!
//! Foundation for multi-runtime unlock:
//! - Phase 0: `[model] utility` — the cheap model for internal tasks (replaces
//!   scattered hardcoded `"claude-haiku-4-5"` literals).
//! - Phase 1+: `[runtime] provider` — which AgentRuntime backend to use.
//!
//! These are deliberately lightweight (parse `agent.toml` as a generic
//! `toml::Value`) so callers that only have an `agent_dir` can resolve config
//! without loading the full `AgentConfig`.

use std::path::Path;

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

fn read_agent_toml(agent_dir: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    text.parse::<toml::Value>().ok()
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
    let Some(v) = read_agent_toml(agent_dir) else {
        return RuntimeSettings::default();
    };
    let runtime = v.get("runtime");
    let provider = runtime
        .and_then(|r| r.get("provider"))
        .and_then(|s| s.as_str())
        .map(RuntimeType::parse)
        .unwrap_or_default();
    let fallback = runtime
        .and_then(|r| r.get("fallback"))
        .and_then(|s| s.as_str())
        .map(RuntimeType::parse);
    let utility_model = v
        .get("model")
        .and_then(|m| m.get("utility"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_UTILITY_MODEL.to_string());

    // Provider↔model sanity check: `preferred = "gpt-5"` with the default
    // Claude provider used to route silently into the Claude CLI and fail at
    // the Anthropic API. Warn once per (agent, model) so the misconfiguration
    // is visible without spamming the hot reply path.
    if let Some(preferred) = v
        .get("model")
        .and_then(|m| m.get("preferred"))
        .and_then(|s| s.as_str())
    {
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
    match provider {
        RuntimeType::Claude => !(is_openai || is_gemini),
        RuntimeType::Codex => !(is_claude || is_gemini),
        RuntimeType::Gemini | RuntimeType::Antigravity => !(is_claude || is_openai),
        RuntimeType::OpenAiCompat => true,
    }
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
    read_agent_toml(agent_dir)
        .and_then(|v| {
            v.get("memory")
                .and_then(|m| m.get("decision_continuity"))
                .and_then(|b| b.as_bool())
        })
        .unwrap_or(false)
}

/// TTL in days after which an unanswered open decision is auto-expired (RFC-24
/// §P3.2). Reads `[memory] decision_ttl_days`; defaults to 7. Non-positive or
/// malformed values fall back to the default (7) — TTL is always enforced so the
/// ledger can't grow unbounded.
pub fn decision_ttl_days(agent_dir: &Path) -> i64 {
    const DEFAULT_TTL_DAYS: i64 = 7;
    read_agent_toml(agent_dir)
        .and_then(|v| {
            v.get("memory")
                .and_then(|m| m.get("decision_ttl_days"))
                .and_then(|n| n.as_integer())
        })
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TTL_DAYS)
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

    #[test]
    fn model_provider_mismatch_detection() {
        use RuntimeType::*;
        // Confident mismatches
        assert!(!model_matches_provider("gpt-5", Claude));
        assert!(!model_matches_provider("gemini-3.1-pro", Claude));
        assert!(!model_matches_provider("claude-sonnet-4-6", Codex));
        assert!(!model_matches_provider("gpt-5.4", Gemini));
        // Correct pairings
        assert!(model_matches_provider("claude-haiku-4-5", Claude));
        assert!(model_matches_provider("gpt-5.4-mini", Codex));
        assert!(model_matches_provider("gemini-3.5-flash", Antigravity));
        // Qualified form + unknown families + compat always pass
        assert!(model_matches_provider("anthropic/claude-sonnet-5", Claude));
        assert!(model_matches_provider("deepseek-v3.2", Claude));
        assert!(model_matches_provider("claude-sonnet-4-6", OpenAiCompat));
    }
}
