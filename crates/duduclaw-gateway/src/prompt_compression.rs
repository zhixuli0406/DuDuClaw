//! Token-budget enforcement pipeline (#12, 2026-05-12).
//!
//! Before #12, the 200 K cliff was diagnosed *after* the request was sent
//! (via `cost_telemetry::record` + `cost_pressure` event). Operators got a
//! warning but the next request still went through unchanged. Budget
//! enforcement closes this loop: at the request boundary, estimate the
//! total input tokens; if over the configured ceiling, walk a fixed
//! compression pipeline. If the pipeline can't bring it under, refuse
//! the request and emit a `budget_exceeded` event.
//!
//! ## Design
//!
//! - **Pure stage functions** — each stage takes `(system, history, user)`
//!   and either returns a compressed version or `None` (stage can't help
//!   further). Callers iterate stages in order until budget is met.
//! - **Cost-pressure aware** — when an agent has a hot `cost_pressure`
//!   flag (#6.3), early stages become more aggressive (e.g. TurnTrim
//!   threshold drops from 800 → 200 chars).
//! - **Token estimation** uses the 1.5 chars/token CJK-aware heuristic
//!   already used by `prompt_audit`, sharing the helper to keep the
//!   estimate consistent across observability and enforcement.
//!
//! ## Stages
//!
//! Stages are ordered from cheapest / least lossy to most aggressive:
//! 1. `TurnTrim` — per-turn 800/200-char tail trim (existing approach,
//!    just made explicit). Loses no semantic content for short replies.
//! 2. `DropOldestToolEchoes` — strip the full content of tool results
//!    older than the last 3 turns, replace with a `[tool_result.id=N]`
//!    stub. Tool results are recoverable via MCP when needed.
//! 3. `BisectAndSummarize` — last resort. Take the older half of history,
//!    summarize via Haiku async into ~500 chars of bullets. This stage
//!    is currently a stub; full impl belongs to #13.
//!
//! ## What's intentionally NOT here
//!
//! - **LlmLingua-2 bridge**: CLAUDE.md mentions this as available infra
//!   but the Python subprocess startup latency makes it a poor fit for
//!   per-request synchronous compression. Should live in #13's async
//!   summarizer instead.
//! - **Meta-token LTSC**: a separate, opt-in `[compression]` config
//!   knob since it costs decode time on the agent side. Deferred.


use tracing::{info, warn};

// ── WP5: cache-aware compression gate (2607.12161) ─────────────────────
//
// The pipeline above is purely token-budget driven — it has no notion of
// prompt-cache health. That's a problem for agents whose system prompt +
// history are already hitting a healthy cache (Anthropic `cache_control:
// ephemeral`): rewriting even a small tail of the history changes the
// bytes the cache is keyed on, which forces a full cache-prefix rebuild.
// The paper's empirical finding is that cache-rebuild cost dominates
// (~87%) the overhead in that regime, i.e. the tokens compression saves
// are smaller than the cache-miss tax it triggers. `should_skip_for_cache`
// is the deterministic gate `maybe_compress_history` (channel_reply.rs)
// consults before entering the pipeline at all.

/// Compression info threaded from `maybe_compress_history` down to the
/// eventual `cost_telemetry` record call, which happens several async
/// frames away (inside `spawn_claude_cli_with_env` / the PTY variant).
/// Mirrors the existing `CHANNEL_REPLY_AGENT_ID` / `CHANNEL_REPLY_USER_ID`
/// task-locals in `claude_runner.rs` — same problem, same fix shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompressionInfo {
    /// Whether the request that's about to be sent was actually rewritten
    /// by the compression pipeline (false for: budget disabled, cache
    /// guard skipped the pipeline, request was already under budget, or
    /// the pipeline ran but couldn't bring it under budget — in that last
    /// case the caller falls back to the original uncompressed history,
    /// so nothing compressed actually went out).
    pub compressed: bool,
    /// Comma-joined stage names that ran (e.g. `"turn_trim"` or
    /// `"turn_trim,drop_oldest_tool_echoes"`). Empty when `compressed` is
    /// false.
    pub stages: String,
}

tokio::task_local! {
    /// See [`CompressionInfo`]. Scoped by `channel_reply::maybe_compress_history`'s
    /// caller alongside `CHANNEL_REPLY_AGENT_ID`.
    pub static CHANNEL_REPLY_COMPRESSION: CompressionInfo;
}

/// Cache-aware gate thresholds. Defaults match the WP5 design doc:
/// skip compression when the agent's trailing cache efficiency is > 50%
/// and the budget overshoot is < 15%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheGuardConfig {
    pub min_eff: f64,
    pub max_overshoot: f64,
}

/// Default minimum cache efficiency for the guard to engage (50%).
pub const DEFAULT_CACHE_GUARD_MIN_EFF: f64 = 0.5;
/// Default maximum budget overshoot the guard tolerates (15%).
pub const DEFAULT_CACHE_GUARD_MAX_OVERSHOOT: f64 = 0.15;

impl Default for CacheGuardConfig {
    fn default() -> Self {
        Self {
            min_eff: DEFAULT_CACHE_GUARD_MIN_EFF,
            max_overshoot: DEFAULT_CACHE_GUARD_MAX_OVERSHOOT,
        }
    }
}

/// Read `[budget] cache_guard_min_eff` / `cache_guard_max_overshoot` from
/// `agent.toml`. Fail-safe in the same shape as
/// `prompt_audit::read_max_input_tokens`: a missing file, unparseable
/// TOML, or an absent key falls back to the module default (**gate
/// enabled** at the paper's thresholds) — only an explicit
/// `cache_guard_min_eff = 0` disables the gate. This differs from
/// `read_max_input_tokens`'s "missing ⇒ disabled" convention on purpose:
/// the gate is a safety optimization that should protect agents by
/// default, not require explicit opt-in per agent.
pub fn read_cache_guard_config(agent_dir: &std::path::Path) -> CacheGuardConfig {
    let toml_path = agent_dir.join("agent.toml");
    let raw = match std::fs::read_to_string(&toml_path) {
        Ok(r) => r,
        Err(_) => return CacheGuardConfig::default(),
    };
    let value: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(_) => return CacheGuardConfig::default(),
    };
    let budget = value.get("budget");
    let as_f64 = |v: &toml::Value| -> Option<f64> {
        v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
    };
    let min_eff = budget
        .and_then(|b| b.get("cache_guard_min_eff"))
        .and_then(as_f64)
        .unwrap_or(DEFAULT_CACHE_GUARD_MIN_EFF);
    let max_overshoot = budget
        .and_then(|b| b.get("cache_guard_max_overshoot"))
        .and_then(as_f64)
        .unwrap_or(DEFAULT_CACHE_GUARD_MAX_OVERSHOOT);
    CacheGuardConfig { min_eff, max_overshoot }
}

/// How far the estimated prompt is over budget, as a ratio (`0.15` = 15%
/// over). Returns `0.0` for a zero budget (disabled budget enforcement —
/// the gate is never consulted in that case anyway, but this keeps the
/// function total instead of panicking on division by zero).
pub fn overshoot_ratio(estimated_tokens: u64, budget_tokens: u64) -> f64 {
    if budget_tokens == 0 {
        return 0.0;
    }
    (estimated_tokens as f64 / budget_tokens as f64) - 1.0
}

/// Deterministic cache-aware gate decision. `min_eff <= 0.0` means the
/// gate is disabled (config convention: `cache_guard_min_eff = 0`) and
/// always returns `false` (never skip — behave exactly like pre-WP5).
/// Otherwise skips the pipeline when the cache is already healthy
/// (`cache_eff > min_eff`) AND the overshoot is mild
/// (`overshoot < max_overshoot`) — the regime where the paper found
/// cache-rebuild cost exceeds compression's savings.
pub fn should_skip_for_cache(
    cache_eff: f64,
    overshoot: f64,
    min_eff: f64,
    max_overshoot: f64,
) -> bool {
    if min_eff <= 0.0 {
        return false;
    }
    cache_eff > min_eff && overshoot < max_overshoot
}

/// Estimate the number of tokens in a chunk of text using the same
/// 1.5 chars/token heuristic the rest of the gateway uses. CJK-safe via
/// `chars().count()`.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count();
    ((chars as f64) / 1.5).ceil() as u64
}

/// Estimate total tokens across system prompt, conversation history, and
/// the upcoming user message. Each history turn's role tag counts as ~3
/// tokens of overhead; we approximate by adding 4 per turn.
pub fn estimate_request_tokens(
    system_prompt: &str,
    history: &[ChatMessage<'_>],
    user_message: &str,
) -> u64 {
    let mut total = estimate_tokens(system_prompt);
    for msg in history {
        total += estimate_tokens(msg.content);
        total += 4; // role tag + structural overhead per Anthropic API
    }
    total += estimate_tokens(user_message);
    total
}

/// Borrowed view of one chat message — kept generic so the same pipeline
/// can be driven from `channel_reply` and `claude_runner` without
/// allocating an intermediate Vec.
#[derive(Debug, Clone)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

/// Owned variant when a stage needs to rewrite content. Stages return
/// these so the caller can either pass them onward (chained compression)
/// or extract the final result.
#[derive(Debug, Clone)]
pub struct OwnedChatMessage {
    pub role: String,
    pub content: String,
}

impl OwnedChatMessage {
    pub fn as_view(&self) -> ChatMessage<'_> {
        ChatMessage {
            role: &self.role,
            content: &self.content,
        }
    }
}

/// Verdict emitted when the pipeline can't bring the request under
/// budget. Caller logs this and aborts the request rather than sending
/// a known-over-budget call.
#[derive(Debug, Clone)]
pub struct BudgetExceeded {
    pub estimated_tokens: u64,
    pub budget_tokens: u64,
    /// Names of stages that ran; useful for debugging which compression
    /// strategies failed to free enough budget.
    pub stages_tried: Vec<&'static str>,
}

/// Pipeline driver — runs `stages` in order until `estimate <= budget`
/// or all stages exhaust. Returns the final history (possibly compressed)
/// or `BudgetExceeded` if no combination of stages worked.
///
/// `stages` is `&[(&'static str, StageFn)]` where each stage is invoked
/// with the *current* history and may rewrite it. Stage functions are
/// pure: same input → same output. This keeps the pipeline testable
/// without mocking I/O.
///
/// Thin wrapper over [`enforce_budget_traced`] that drops the stage-trace
/// on success — kept byte-identical to preserve the existing call sites
/// and tests (WP5, 2607.12161: the traced sibling exists so
/// `cost_telemetry` can record *which* stages actually ran without
/// forcing every caller to consume that extra info).
#[allow(clippy::type_complexity)]
pub fn enforce_budget(
    system_prompt: &str,
    history: Vec<OwnedChatMessage>,
    user_message: &str,
    budget_tokens: u64,
    stages: &[(&'static str, fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>)],
    cost_pressure: bool,
) -> Result<Vec<OwnedChatMessage>, BudgetExceeded> {
    enforce_budget_traced(system_prompt, history, user_message, budget_tokens, stages, cost_pressure)
        .map(|(messages, _stages_ran)| messages)
}

/// Traced sibling of [`enforce_budget`] — identical behavior, but the
/// success case also returns the names of the stages that actually ran
/// (empty when the fast path applied, i.e. the request was already under
/// budget). `cost_telemetry::record_attributed_with_compression` uses
/// this to persist "was this reply compressed, and by which stages" per
/// request row instead of only inferring it from the cache-efficiency
/// trend after the fact.
#[allow(clippy::type_complexity)]
pub fn enforce_budget_traced(
    system_prompt: &str,
    history: Vec<OwnedChatMessage>,
    user_message: &str,
    budget_tokens: u64,
    stages: &[(&'static str, fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>)],
    cost_pressure: bool,
) -> Result<(Vec<OwnedChatMessage>, Vec<&'static str>), BudgetExceeded> {
    let initial = {
        let views: Vec<ChatMessage<'_>> = history.iter().map(|m| m.as_view()).collect();
        estimate_request_tokens(system_prompt, &views, user_message)
    };
    if initial <= budget_tokens {
        // Fast path — no compression needed.
        return Ok((history, Vec::new()));
    }

    info!(
        initial_tokens = initial,
        budget = budget_tokens,
        cost_pressure,
        "prompt over budget — entering compression pipeline"
    );

    let mut current = history;
    let mut stages_tried: Vec<&'static str> = Vec::new();
    for (name, stage) in stages {
        current = stage(current, cost_pressure);
        stages_tried.push(*name);
        let views: Vec<ChatMessage<'_>> = current.iter().map(|m| m.as_view()).collect();
        let after = estimate_request_tokens(system_prompt, &views, user_message);
        info!(
            stage = name,
            after_tokens = after,
            "compression stage applied"
        );
        if after <= budget_tokens {
            return Ok((current, stages_tried));
        }
    }

    let views: Vec<ChatMessage<'_>> = current.iter().map(|m| m.as_view()).collect();
    let final_estimate = estimate_request_tokens(system_prompt, &views, user_message);
    warn!(
        final_tokens = final_estimate,
        budget = budget_tokens,
        stages = ?stages_tried,
        "compression pipeline failed to bring request under budget"
    );
    Err(BudgetExceeded {
        estimated_tokens: final_estimate,
        budget_tokens,
        stages_tried,
    })
}

// ── Stages ──────────────────────────────────────────────────────────

/// TurnTrim — tail-trim each message to a length threshold. The
/// threshold drops to 200 chars when `cost_pressure` is set; otherwise
/// 800. Loses the *prefix* of long tool outputs (the most informative
/// part is usually the head, but we accept that tradeoff to free budget;
/// crucial messages live in the recent turns which see the soft 200/800
/// limit, not zero).
///
/// Implementation is intentionally simple: chop bytes at a char boundary,
/// add a single-line `[trimmed N chars]` marker so the model knows what
/// happened.
pub fn turn_trim(
    history: Vec<OwnedChatMessage>,
    cost_pressure: bool,
) -> Vec<OwnedChatMessage> {
    let threshold = if cost_pressure { 200 } else { 800 };
    history
        .into_iter()
        .map(|msg| {
            if msg.content.chars().count() <= threshold {
                return msg;
            }
            let take = msg.content.chars().take(threshold).collect::<String>();
            let trimmed = msg.content.chars().count() - threshold;
            OwnedChatMessage {
                role: msg.role,
                content: format!("{take}\n[trimmed {trimmed} chars]"),
            }
        })
        .collect()
}

/// DropOldestToolEchoes — for messages with `role == "tool"` or
/// `role == "function"`, when older than the last 3 turns, replace the
/// content with a stub. Tool results are recoverable via MCP `tool_call_history`.
pub fn drop_oldest_tool_echoes(
    history: Vec<OwnedChatMessage>,
    _cost_pressure: bool,
) -> Vec<OwnedChatMessage> {
    let n = history.len();
    // Keep last 3 turns verbatim; for older tool-role messages, stub.
    let keep_from = n.saturating_sub(3);
    history
        .into_iter()
        .enumerate()
        .map(|(idx, msg)| {
            let is_tool = msg.role == "tool" || msg.role == "function";
            if idx < keep_from && is_tool {
                let original_len = msg.content.len();
                OwnedChatMessage {
                    role: msg.role,
                    content: format!("[tool_echo stripped — original {original_len} bytes, recall via tool_call_history]"),
                }
            } else {
                msg
            }
        })
        .collect()
}

/// BisectAndSummarize — placeholder. Full implementation depends on
/// #13's async summarizer being wired up. For now this stage is a no-op
/// that lets the pipeline gracefully fall through to the budget-exceeded
/// verdict instead of pretending to compress.
pub fn bisect_and_summarize(
    history: Vec<OwnedChatMessage>,
    _cost_pressure: bool,
) -> Vec<OwnedChatMessage> {
    // TODO(#13) — fold older half into a Haiku-generated summary.
    // Until then, no-op so the pipeline either succeeds at earlier
    // stages or honestly reports BudgetExceeded.
    history
}

/// The default ordered pipeline used by the gateway. Callers can build
/// their own arrays for tests or experiments.
#[allow(clippy::type_complexity)]
pub fn default_pipeline() -> &'static [(
    &'static str,
    fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>,
)] {
    &[
        ("turn_trim", turn_trim),
        ("drop_oldest_tool_echoes", drop_oldest_tool_echoes),
        ("bisect_and_summarize", bisect_and_summarize),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> OwnedChatMessage {
        OwnedChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    // ── Token estimation ──

    #[test]
    fn estimate_tokens_ascii() {
        // "hello" is 5 chars → ceil(5/1.5) = 4 tokens.
        assert_eq!(estimate_tokens("hello"), 4);
    }

    #[test]
    fn estimate_tokens_cjk() {
        // 4 CJK chars → ceil(4/1.5) = 3 tokens.
        assert_eq!(estimate_tokens("你好世界"), 3);
    }

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_request_tokens_combines_system_history_user() {
        let history = vec![
            ChatMessage {
                role: "user",
                content: "hi",
            },
            ChatMessage {
                role: "assistant",
                content: "hello",
            },
        ];
        let total = estimate_request_tokens("system prompt", &history, "user msg");
        // 8 + 2 + 4 + 4 + 4 + 6 = 28 (approx, dominated by char counts)
        // Just assert > 0 and reasonable bound.
        assert!(total > 10 && total < 100, "got {total}");
    }

    // ── Fast path ──

    #[test]
    fn enforce_budget_fast_path_when_under_budget() {
        let history = vec![msg("user", "hi"), msg("assistant", "hello")];
        let result = enforce_budget(
            "tiny system",
            history.clone(),
            "tiny user",
            10_000,
            default_pipeline(),
            false,
        );
        let out = result.expect("should succeed under budget");
        // Fast path returns history unchanged.
        assert_eq!(out.len(), history.len());
        assert_eq!(out[0].content, "hi");
    }

    // ── TurnTrim ──

    #[test]
    fn turn_trim_passes_through_short_messages() {
        let history = vec![msg("user", "short")];
        let out = turn_trim(history, false);
        assert_eq!(out[0].content, "short");
    }

    #[test]
    fn turn_trim_clips_long_message_at_800_chars_default() {
        let long = "x".repeat(2000);
        let history = vec![msg("assistant", &long)];
        let out = turn_trim(history, false);
        // 800 chars + trim marker.
        assert!(out[0].content.starts_with("xxxxxxxxxxxxxxxxxxxx"));
        assert!(out[0].content.contains("[trimmed 1200 chars]"));
        assert!(out[0].content.chars().count() < 850);
    }

    #[test]
    fn turn_trim_clips_more_aggressively_under_cost_pressure() {
        let long = "x".repeat(2000);
        let history = vec![msg("assistant", &long)];
        let out = turn_trim(history, /* cost_pressure */ true);
        assert!(out[0].content.contains("[trimmed 1800 chars]"));
        assert!(out[0].content.chars().count() < 250);
    }

    // ── DropOldestToolEchoes ──

    #[test]
    fn drop_old_tool_echoes_keeps_recent_turns_verbatim() {
        let history = vec![
            msg("tool", "old tool result aaaaa"),
            msg("user", "..."),
            msg("assistant", "..."),
            msg("tool", "recent tool result"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        // Recent (last 3) untouched.
        assert_eq!(out[3].content, "recent tool result");
        // Old tool result stubbed.
        assert!(out[0].content.contains("[tool_echo stripped"));
    }

    #[test]
    fn drop_old_tool_echoes_does_not_touch_non_tool_roles() {
        let history = vec![
            msg("user", "old user message — full content preserved"),
            msg("assistant", "old assistant reply"),
            msg("user", "fresh"),
            msg("assistant", "fresh"),
            msg("user", "fresh"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        // First two are old AND non-tool → must NOT be stubbed.
        assert!(out[0].content.contains("old user message"));
        assert!(out[1].content.contains("old assistant reply"));
    }

    // ── Pipeline integration ──

    #[test]
    fn pipeline_compresses_when_over_budget_via_turn_trim() {
        let huge = "x".repeat(50_000);
        let history = vec![msg("assistant", &huge)];
        // System + huge user assistant = way over 1000 tokens. TurnTrim
        // alone should bring it under 1000.
        let result = enforce_budget(
            "system",
            history,
            "ask question",
            1_000,
            default_pipeline(),
            false,
        );
        let out = result.expect("turn_trim should suffice");
        assert!(out[0].content.contains("[trimmed"));
    }

    #[test]
    fn pipeline_reports_exceeded_when_no_stage_helps() {
        // Force a budget so small even an empty body exceeds it (system
        // prompt alone is >5 tokens).
        let history = vec![msg("user", "hi")];
        let result = enforce_budget(
            &"x".repeat(10_000), // huge un-trimmable system prompt
            history,
            "u",
            10,
            default_pipeline(),
            false,
        );
        match result {
            Err(BudgetExceeded {
                estimated_tokens,
                budget_tokens,
                stages_tried,
            }) => {
                assert!(estimated_tokens > budget_tokens);
                assert_eq!(budget_tokens, 10);
                assert!(stages_tried.contains(&"turn_trim"));
            }
            Ok(_) => panic!("expected BudgetExceeded with un-trimmable system prompt"),
        }
    }

    #[test]
    fn pipeline_uses_cost_pressure_to_compress_harder() {
        // Compose a history that's just at the budget edge under normal
        // trim but fits under cost_pressure trim.
        let mid = "x".repeat(1_500); // ~1000 tokens
        let history = vec![msg("assistant", &mid)];

        // Without cost pressure: turn_trim caps at 800 chars → ~533
        // tokens. With pressure: 200 chars → ~133 tokens.
        let normal = enforce_budget(
            "s",
            history.clone(),
            "u",
            600,
            default_pipeline(),
            false,
        );
        let pressured = enforce_budget(
            "s",
            history,
            "u",
            600,
            default_pipeline(),
            true,
        );
        // Both should succeed at this budget, but pressured version
        // produces shorter content.
        let normal_len = normal.unwrap()[0].content.len();
        let pressured_len = pressured.unwrap()[0].content.len();
        assert!(
            pressured_len < normal_len,
            "cost_pressure should produce shorter trim ({normal_len} vs {pressured_len})"
        );
    }

    // ── enforce_budget_traced ──

    #[test]
    fn enforce_budget_traced_fast_path_returns_empty_stages() {
        let history = vec![msg("user", "hi")];
        let (out, stages) = enforce_budget_traced(
            "s", history, "u", 10_000, default_pipeline(), false,
        )
        .expect("under budget");
        assert!(stages.is_empty());
        assert_eq!(out[0].content, "hi");
    }

    #[test]
    fn enforce_budget_traced_reports_stages_that_ran() {
        let huge = "x".repeat(50_000);
        let history = vec![msg("assistant", &huge)];
        let (out, stages) = enforce_budget_traced(
            "system", history, "ask question", 1_000, default_pipeline(), false,
        )
        .expect("turn_trim should suffice");
        assert_eq!(stages, vec!["turn_trim"]);
        assert!(out[0].content.contains("[trimmed"));
    }

    #[test]
    fn enforce_budget_and_traced_agree_on_success_payload() {
        // `enforce_budget` must stay byte-identical to the traced sibling
        // minus the stage list — regression guard for the delegation.
        let huge = "x".repeat(50_000);
        let plain = enforce_budget(
            "system", vec![msg("assistant", &huge)], "ask question", 1_000,
            default_pipeline(), false,
        )
        .unwrap();
        let (traced, _stages) = enforce_budget_traced(
            "system", vec![msg("assistant", &huge)], "ask question", 1_000,
            default_pipeline(), false,
        )
        .unwrap();
        assert_eq!(plain[0].content, traced[0].content);
    }

    // ── WP5: cache-aware compression gate ──

    #[test]
    fn should_skip_for_cache_disabled_when_min_eff_zero() {
        // min_eff=0 is the documented "gate disabled" config value —
        // must never skip regardless of how healthy the cache looks.
        assert!(!should_skip_for_cache(0.99, 0.0, 0.0, 0.15));
    }

    #[test]
    fn should_skip_for_cache_skips_when_hot_and_mild_overshoot() {
        assert!(should_skip_for_cache(0.6, 0.1, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_does_not_skip_when_cache_cold() {
        // Cache efficiency below threshold — compression should still run.
        assert!(!should_skip_for_cache(0.2, 0.1, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_does_not_skip_when_overshoot_large() {
        // Cache is healthy but the request is way over budget — still
        // compress, since the token savings likely outweigh a cache miss.
        assert!(!should_skip_for_cache(0.9, 0.5, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_boundary_is_exclusive() {
        // Exactly at the thresholds should NOT skip (strict `>` / `<`).
        assert!(!should_skip_for_cache(0.5, 0.15, 0.5, 0.15));
    }

    #[test]
    fn overshoot_ratio_computes_fraction_over_budget() {
        // 1150 / 1000 - 1 = 0.15
        assert!((overshoot_ratio(1150, 1000) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn overshoot_ratio_negative_when_under_budget() {
        assert!(overshoot_ratio(500, 1000) < 0.0);
    }

    #[test]
    fn overshoot_ratio_zero_budget_is_total_not_panicking() {
        assert_eq!(overshoot_ratio(500, 0), 0.0);
    }

    // ── read_cache_guard_config (fail-safe) ──

    #[test]
    fn read_cache_guard_config_defaults_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }

    #[test]
    fn read_cache_guard_config_defaults_when_toml_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "not valid toml =====").unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }

    #[test]
    fn read_cache_guard_config_reads_explicit_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\ncache_guard_min_eff = 0.4\ncache_guard_max_overshoot = 0.2\n",
        )
        .unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg.min_eff, 0.4);
        assert_eq!(cfg.max_overshoot, 0.2);
    }

    #[test]
    fn read_cache_guard_config_explicit_zero_disables_gate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\ncache_guard_min_eff = 0\n",
        )
        .unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg.min_eff, 0.0);
        // Downstream gate check confirms this actually disables it.
        assert!(!should_skip_for_cache(0.9, 0.0, cfg.min_eff, cfg.max_overshoot));
    }

    #[test]
    fn read_cache_guard_config_missing_budget_section_uses_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "[other]\nfoo = 1\n").unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }
}
