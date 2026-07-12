//! O1: Confidence-aware multi-scale routing for sub-agent delegation.
//!
//! When an agent delegates a sub-task (dispatcher consumes `bus_queue` and
//! spawns a CLI subprocess), the sub-agent historically always ran the
//! delegating agent's full preferred model. Per OI-MAS (arXiv:2601.04861),
//! most delegated sub-tasks are mechanically simple and can run on a cheaper
//! tier at large cost savings — the savings come from the *unambiguously*
//! easy mass, so this router is deliberately conservative: any ambiguity or
//! complexity signal escalates the tier, never downgrades it.
//!
//! Design constraints:
//! - **Pure + deterministic + zero-LLM**: scoring is heuristic only
//!   (adapted from `duduclaw-inference`'s `ConfidenceRouter` philosophy),
//!   CJK-aware token estimation via [`crate::cost_telemetry::estimate_tokens`].
//! - **No hardcoded model ids**: tiers resolve exclusively through the
//!   existing config helpers — Cheap ⇒ `[model] utility`, Standard ⇒
//!   `[model] standard` (else preferred), Preferred ⇒ `[model] preferred`.
//! - **Opt-in, default OFF**: `config.toml [delegation] confidence_routing`
//!   with per-agent `agent.toml [model] delegation_routing` override (agent
//!   wins). When off, [`resolve_delegation_model`] returns the preferred
//!   model unchanged — byte-identical dispatch behavior.
//! - **Fail-safe**: nothing in this module can error; any missing/malformed
//!   config resolves to the current behavior (Preferred), never a spawn
//!   failure.

use std::path::Path;

use duduclaw_core::word_contains_ci;

/// Which model tier a delegated sub-task should run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    /// Mechanical, unambiguous sub-task → the agent's utility model.
    Cheap,
    /// Default tier → `[model] standard` when configured, else preferred.
    Standard,
    /// Complexity signals present → the agent's full preferred model
    /// (current behavior).
    Preferred,
}

impl ModelTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Standard => "standard",
            Self::Preferred => "preferred",
        }
    }
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a tier-scoring decision.
#[derive(Debug, Clone)]
pub struct TierDecision {
    pub tier: ModelTier,
    /// Human-readable reason (for tracing / telemetry log lines).
    pub reason: String,
}

/// Mechanical single-step verbs — English (whole-word matched) plus zh-TW
/// equivalents (`word_contains_ci` boundaries are ASCII-only, so CJK needles
/// match as substrings, which is correct for boundary-less CJK text).
const MECHANICAL_KEYWORDS: &[&str] = &[
    // English
    "translate",
    "format",
    "summarize",
    "summarise",
    "classify",
    "list",
    "rename",
    "convert",
    "extract",
    "sort",
    "count",
    "lookup",
    // zh-TW
    "翻譯",
    "整理",
    "摘要",
    "分類",
    "轉換",
    "列出",
    "改名",
    "重新命名",
    "排序",
];

/// Complexity signals — any hit escalates straight to Preferred.
const COMPLEX_KEYWORDS: &[&str] = &[
    // English
    "architecture",
    "architect",
    "security",
    "debug",
    "refactor",
    "design",
    "analyze",
    "analyse",
    "investigate",
    "diagnose",
    "vulnerability",
    "migrate",
    "migration",
    "optimize",
    "optimise",
    "why",
    "root cause",
    // zh-TW
    "架構",
    "安全",
    "除錯",
    "重構",
    "設計",
    "分析",
    "調查",
    "診斷",
    "為什麼",
    "為何",
    "漏洞",
    "遷移",
    "優化",
    "根因",
];

/// A prompt must be at most this many (estimated) tokens to qualify as Cheap.
const CHEAP_MAX_TOKENS: u64 = 120;

/// Prompts longer than this are treated as complex regardless of keywords.
const LONG_PROMPT_TOKENS: u64 = 700;

/// Numbered-list items beyond this count as a multi-step (complex) task.
const MAX_SIMPLE_STEPS: usize = 3;

/// Count lines that look like numbered list items (`1.`, `2)`, `3、` …).
fn numbered_items(prompt: &str) -> usize {
    prompt
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 || digits > 2 {
                return false;
            }
            // Safe slicing: `digits` chars are all ASCII, so the byte index
            // equals the char count here; still use chars().nth for clarity.
            matches!(t.chars().nth(digits), Some('.') | Some(')') | Some('、'))
        })
        .count()
}

/// Score a delegated task prompt into a [`ModelTier`] — pure, deterministic,
/// zero LLM cost.
///
/// Conservative bias: complexity signals win over mechanical signals, and
/// anything ambiguous lands on Standard (or Preferred), never Cheap.
pub fn score_delegation(prompt: &str) -> TierDecision {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: "empty prompt — fail-safe escalation".to_string(),
        };
    }

    let tokens = crate::cost_telemetry::estimate_tokens(trimmed);

    // 1. Complexity keywords → Preferred, regardless of anything else.
    let complex_hits: Vec<&str> = COMPLEX_KEYWORDS
        .iter()
        .filter(|kw| word_contains_ci(trimmed, kw))
        .copied()
        .collect();
    if !complex_hits.is_empty() {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: format!("complexity keywords: {}", complex_hits.join(", ")),
        };
    }

    // 2. Code blocks / code-shaped content → Preferred.
    if trimmed.contains("```") || trimmed.contains("fn ") || trimmed.contains("def ") {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: "contains code".to_string(),
        };
    }

    // 3. Multi-step task (numbered list with >3 items) → Preferred.
    let steps = numbered_items(trimmed);
    if steps > MAX_SIMPLE_STEPS {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: format!("{steps} numbered steps (multi-step task)"),
        };
    }

    // 4. Long prompt → Preferred.
    if tokens > LONG_PROMPT_TOKENS {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: format!("long prompt ({tokens} est. tokens)"),
        };
    }

    // 5. Many open questions → Preferred (analysis-shaped, not mechanical).
    let questions = trimmed.chars().filter(|c| *c == '?' || *c == '？').count();
    if questions > 2 {
        return TierDecision {
            tier: ModelTier::Preferred,
            reason: format!("{questions} questions (analysis-shaped)"),
        };
    }

    // 6. Mechanical verb + short prompt → Cheap; mechanical but longer →
    //    Standard (ambiguity escalates, never downgrades).
    let mech_hits: Vec<&str> = MECHANICAL_KEYWORDS
        .iter()
        .filter(|kw| word_contains_ci(trimmed, kw))
        .copied()
        .collect();
    if !mech_hits.is_empty() {
        if tokens <= CHEAP_MAX_TOKENS {
            return TierDecision {
                tier: ModelTier::Cheap,
                reason: format!(
                    "mechanical keywords: {} + short prompt ({tokens} est. tokens)",
                    mech_hits.join(", ")
                ),
            };
        }
        return TierDecision {
            tier: ModelTier::Standard,
            reason: format!(
                "mechanical keywords: {} but non-trivial length ({tokens} est. tokens) — conservative",
                mech_hits.join(", ")
            ),
        };
    }

    TierDecision {
        tier: ModelTier::Standard,
        reason: "no strong signal — default standard (ambiguity never downgrades)".to_string(),
    }
}

/// Map a tier to a concrete model through the agent's configured models.
///
/// No model ids are hardcoded here — `preferred` / `standard` / `utility`
/// all come from config. Blank inputs fall back to `preferred` (fail-safe:
/// a misconfigured cheap tier must never produce an empty model string).
pub fn tier_model(
    tier: ModelTier,
    preferred: &str,
    standard: Option<&str>,
    utility: &str,
) -> String {
    let pick = match tier {
        ModelTier::Cheap => {
            let u = utility.trim();
            if u.is_empty() {
                preferred
            } else {
                u
            }
        }
        ModelTier::Standard => standard
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(preferred),
        ModelTier::Preferred => preferred,
    };
    pick.to_string()
}

/// Entry point for the dispatch call sites: gate → score → resolve → log.
///
/// - Routing disabled (the default) ⇒ returns `preferred` unchanged.
/// - `provider_is_claude == false` ⇒ returns `preferred` unchanged. The tier
///   models (`[model] utility` / `[model] standard`) are Claude model ids;
///   injecting them into a Codex / Gemini / OpenAI-compat runtime would break
///   the multi-model doctrine (no cross-runtime model id leakage) — the
///   non-Claude agent keeps its own model untouched.
/// - Enabled ⇒ scores the delegated prompt and maps the tier through the
///   agent's configured models ([`tier_model`]).
/// - Every routing decision is logged via `tracing::info!` (tier, reason,
///   chosen model) — the v1 telemetry trail.
///
/// This function cannot fail: every config read is fail-safe and the scoring
/// is pure, so the worst case is "current behavior" (preferred model).
pub fn resolve_delegation_model(
    home_dir: &Path,
    agent_dir: &Path,
    agent_id: &str,
    prompt: &str,
    preferred: &str,
    utility_model: &str,
    provider_is_claude: bool,
) -> String {
    if !provider_is_claude {
        return preferred.to_string();
    }
    if !crate::runtime_config::delegation_routing_enabled(home_dir, agent_dir) {
        return preferred.to_string();
    }

    let decision = score_delegation(prompt);
    let standard = crate::runtime_config::agent_standard_model(agent_dir);
    let model = tier_model(decision.tier, preferred, standard.as_deref(), utility_model);

    tracing::info!(
        agent = %agent_id,
        tier = decision.tier.as_str(),
        reason = %decision.reason,
        preferred = %preferred,
        chosen = %model,
        "delegation confidence routing (O1)"
    );

    model
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, body: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    // ── Tier scoring: mechanical ⇒ Cheap ────────────────────────────

    #[test]
    fn mechanical_english_short_is_cheap() {
        let d = score_delegation("Translate this sentence to English: bonjour");
        assert_eq!(d.tier, ModelTier::Cheap, "reason: {}", d.reason);
        let d = score_delegation("List the files in the attachment");
        assert_eq!(d.tier, ModelTier::Cheap, "reason: {}", d.reason);
    }

    #[test]
    fn mechanical_zh_tw_short_is_cheap() {
        for prompt in [
            "請翻譯這段文字",
            "幫我摘要這篇短文",
            "把這些名字分類一下",
            "列出所有選項",
        ] {
            let d = score_delegation(prompt);
            assert_eq!(
                d.tier,
                ModelTier::Cheap,
                "prompt: {prompt}, reason: {}",
                d.reason
            );
        }
    }

    // ── Tier scoring: complexity ⇒ Preferred ────────────────────────

    #[test]
    fn architectural_english_is_preferred() {
        let d = score_delegation(
            "Redesign the authentication architecture and explain why the current approach fails",
        );
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
    }

    #[test]
    fn architectural_zh_tw_is_preferred() {
        for prompt in [
            "請分析這個模組的架構問題",
            "為什麼這個服務會當機",
            "檢查系統的安全漏洞",
        ] {
            let d = score_delegation(prompt);
            assert_eq!(
                d.tier,
                ModelTier::Preferred,
                "prompt: {prompt}, reason: {}",
                d.reason
            );
        }
    }

    #[test]
    fn complexity_beats_mechanical() {
        // Mechanical verb present but complexity keyword wins (conservative).
        let d = score_delegation("Translate this and then refactor the module");
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
        let d = score_delegation("翻譯後請重構這段程式");
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
    }

    #[test]
    fn code_block_is_preferred() {
        let d = score_delegation("Fix this:\n```rust\nlet x = 1;\n```");
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
    }

    #[test]
    fn many_numbered_steps_is_preferred() {
        let d = score_delegation(
            "Do the following:\n1. read\n2. parse\n3. transform\n4. write\n5. verify",
        );
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
    }

    #[test]
    fn long_prompt_is_preferred() {
        // Mechanical verb + very long body → complexity by length.
        let long = format!("Summarize the following. {}", "word ".repeat(3000));
        let d = score_delegation(&long);
        assert_eq!(d.tier, ModelTier::Preferred, "reason: {}", d.reason);
    }

    // ── Tier scoring: ambiguity ⇒ never Cheap ───────────────────────

    #[test]
    fn ambiguous_is_not_cheap() {
        for prompt in [
            "幫我處理一下這個",
            "take care of the follow-up",
            "handle it",
        ] {
            let d = score_delegation(prompt);
            assert_ne!(
                d.tier,
                ModelTier::Cheap,
                "prompt: {prompt}, reason: {}",
                d.reason
            );
        }
    }

    #[test]
    fn mechanical_but_medium_length_is_standard_not_cheap() {
        // Mechanical verb present but prompt exceeds the Cheap token budget →
        // conservative escalation to Standard.
        let medium = format!("Summarize this report: {}", "detail ".repeat(120));
        let d = score_delegation(&medium);
        assert_eq!(d.tier, ModelTier::Standard, "reason: {}", d.reason);
    }

    #[test]
    fn empty_prompt_fails_safe_to_preferred() {
        let d = score_delegation("   ");
        assert_eq!(d.tier, ModelTier::Preferred);
    }

    #[test]
    fn mechanical_keyword_requires_word_boundary_in_english() {
        // "list" inside "realistic" must not fire (M39 lesson).
        let d = score_delegation("make it more realistic please, keep the tone");
        assert_ne!(d.tier, ModelTier::Cheap, "reason: {}", d.reason);
    }

    // ── Tier → model mapping through config values ──────────────────

    #[test]
    fn tier_model_maps_through_config_values() {
        assert_eq!(
            tier_model(ModelTier::Cheap, "pref-m", None, "util-m"),
            "util-m"
        );
        assert_eq!(
            tier_model(ModelTier::Standard, "pref-m", Some("std-m"), "util-m"),
            "std-m"
        );
        // No standard model configured → Standard falls back to preferred.
        assert_eq!(
            tier_model(ModelTier::Standard, "pref-m", None, "util-m"),
            "pref-m"
        );
        assert_eq!(
            tier_model(ModelTier::Preferred, "pref-m", Some("std-m"), "util-m"),
            "pref-m"
        );
    }

    #[test]
    fn tier_model_blank_inputs_fall_back_to_preferred() {
        // Misconfigured (blank) utility/standard must never yield an empty model.
        assert_eq!(tier_model(ModelTier::Cheap, "pref-m", None, "  "), "pref-m");
        assert_eq!(
            tier_model(ModelTier::Standard, "pref-m", Some(" "), "util-m"),
            "pref-m"
        );
    }

    // ── resolve_delegation_model: config gating ─────────────────────

    #[test]
    fn routing_off_returns_preferred_untouched() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        // Fully unconfigured (default OFF): even a clearly-Cheap prompt keeps
        // the preferred model byte-identically.
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請翻譯這段文字",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "pref-m");
    }

    #[test]
    fn routing_on_globally_routes_cheap_prompt_to_utility() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        write_file(
            home.path(),
            "config.toml",
            "[delegation]\nconfidence_routing = true\n",
        );
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請翻譯這段文字",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "util-m");
        // Complex prompt keeps preferred even with routing on.
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請分析這個模組的架構問題",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "pref-m");
    }

    #[test]
    fn agent_override_beats_global_both_directions() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        // Global ON + agent OFF → untouched.
        write_file(
            home.path(),
            "config.toml",
            "[delegation]\nconfidence_routing = true\n",
        );
        write_file(
            agent.path(),
            "agent.toml",
            "[model]\ndelegation_routing = false\n",
        );
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請翻譯這段文字",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "pref-m");
        // Global OFF + agent ON → routes.
        write_file(home.path(), "config.toml", "");
        write_file(
            agent.path(),
            "agent.toml",
            "[model]\ndelegation_routing = true\n",
        );
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請翻譯這段文字",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "util-m");
    }

    #[test]
    fn standard_tier_uses_configured_standard_model() {
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        write_file(
            home.path(),
            "config.toml",
            "[delegation]\nconfidence_routing = true\n",
        );
        write_file(
            agent.path(),
            "agent.toml",
            "[model]\nstandard = \"std-m\"\n",
        );
        // Ambiguous prompt → Standard tier → configured [model] standard.
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "幫我處理一下這個",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "std-m");
        // Without [model] standard, Standard falls back to preferred.
        write_file(agent.path(), "agent.toml", "");
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "幫我處理一下這個",
            "pref-m",
            "util-m",
            true,
        );
        assert_eq!(m, "pref-m");
    }

    #[test]
    fn non_claude_provider_keeps_preferred_model_untouched() {
        // MED (2026-07 review): with routing globally ON, a delegated agent
        // whose runtime provider is NOT Claude must keep its own model — tier
        // models are Claude ids and must never leak into a gemini/codex spawn.
        let home = TempDir::new().unwrap();
        let agent = TempDir::new().unwrap();
        write_file(
            home.path(),
            "config.toml",
            "[delegation]\nconfidence_routing = true\n",
        );
        let m = resolve_delegation_model(
            home.path(),
            agent.path(),
            "a1",
            "請翻譯這段文字", // clearly Cheap-tier if routing applied
            "gemini-2.5-pro",
            "util-m",
            false, // resolved runtime provider is not Claude
        );
        assert_eq!(m, "gemini-2.5-pro", "non-Claude runtime keeps its own model");
    }

    #[test]
    fn numbered_items_counts_common_forms() {
        assert_eq!(numbered_items("1. a\n2) b\n3、c\n10. d"), 4);
        assert_eq!(numbered_items("no lists here\njust prose"), 0);
        // 3-digit "numbers" (e.g. years) are not list markers.
        assert_eq!(numbered_items("2026. was a good year"), 0);
    }
}
