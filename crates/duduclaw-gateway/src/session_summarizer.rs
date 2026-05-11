//! Async session summarizer (#13, 2026-05-12) — scaffolding.
//!
//! Goal: move conversation history compression OUT of the request path
//! and onto a background task. Every 10 min, scan sessions that are
//! "long enough to need help" and have a stale summary; replace the
//! older half of history with a Haiku-generated bullet summary stored
//! in the session row.
//!
//! At request time, [`crate::channel_reply`] can then build the prompt
//! as `[ summary_of_prior ] + last_3_turns_verbatim` instead of
//! re-running compression on the hot path.
//!
//! ## Scope (this commit — scaffolding)
//!
//! Lands the **pure decision** layer + types:
//!
//! - `SummaryCandidate` — input shape (session_id, turn_count, last
//!   summarized turn, last summarized at)
//! - `SummarizeDecision` — what the policy decided for one candidate
//!   (`Skip` / `SummarizeAt(turn)`)
//! - `decide_summarization` — pure policy that picks candidates above
//!   threshold + cooldown
//! - `format_summarization_prompt` — builds the prompt sent to Haiku,
//!   pinned in tests to keep the schema stable
//!
//! ## Deferred to follow-up
//!
//! - Actually running the background task (needs Haiku call + session
//!   store mutation)
//! - `summary_of_prior` column migration (touches sessions.db schema —
//!   wants its own migration story)
//! - Wiring summary into channel_reply prompt builder
//! - Cost telemetry: each Haiku call should attribute to the session's
//!   agent_id so per-agent budgets count it
//!
//! The deferred work is intentionally a small, well-bounded follow-up:
//! 1 schema migration + 1 task spawn + 1 prompt-builder branch. The
//! policy + prompt are the parts that benefit most from tests, which
//! is why they land first.

use serde::{Deserialize, Serialize};

/// A session that *might* need summarizing. Lighter than the full
/// session row so this module can stay generic — the caller queries
/// the session store and builds these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryCandidate {
    pub session_id: String,
    pub agent_id: String,
    /// Total number of turns recorded so far.
    pub turn_count: u32,
    /// Up to which turn was the last summary based on. 0 if never
    /// summarized.
    pub summarized_through_turn: u32,
    /// Seconds since last summarization run for this session. `None`
    /// if never summarized.
    pub seconds_since_last_summary: Option<u64>,
}

/// Knobs for the summarization policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeParams {
    /// Don't bother summarizing until there are at least this many turns
    /// since the last summary. Default 10 — anything shorter is cheap
    /// to ship verbatim and would yield ~zero compression.
    pub min_new_turns_to_trigger: u32,
    /// Don't re-summarize a session more often than this. Default 3600s
    /// — Haiku calls are cheap but not free, and conversation evolves
    /// faster than the summary needs to follow.
    pub cooldown_seconds: u64,
    /// Maximum number of sessions to summarize per tick — back-pressure
    /// against a sudden burst of long sessions. Default 16; per-tick at
    /// 10-min interval is 96 sessions/hour, ~2300/day.
    pub max_per_tick: usize,
}

impl Default for SummarizeParams {
    fn default() -> Self {
        Self {
            min_new_turns_to_trigger: 10,
            cooldown_seconds: 3600,
            max_per_tick: 16,
        }
    }
}

/// What the policy decided for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizeDecision {
    Skip { reason: SkipReason },
    /// Summarize all turns up to (and including) this turn number.
    /// The session retains turns AFTER this verbatim in `messages`.
    SummarizeUpTo { turn: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    BelowMinTurns,
    InCooldown,
    OverPerTickQuota,
}

/// Pure decision function. Takes the full candidate list, returns the
/// decision per session. Caller drives the actual Haiku call + session
/// store update; this layer is just policy.
pub fn decide_summarization(
    candidates: &[SummaryCandidate],
    params: &SummarizeParams,
) -> Vec<(String, SummarizeDecision)> {
    let mut taken = 0usize;
    candidates
        .iter()
        .map(|c| {
            let decision = decide_one(c, params, taken);
            if matches!(decision, SummarizeDecision::SummarizeUpTo { .. }) {
                taken += 1;
            }
            (c.session_id.clone(), decision)
        })
        .collect()
}

fn decide_one(
    candidate: &SummaryCandidate,
    params: &SummarizeParams,
    already_taken: usize,
) -> SummarizeDecision {
    let new_turns = candidate
        .turn_count
        .saturating_sub(candidate.summarized_through_turn);
    if new_turns < params.min_new_turns_to_trigger {
        return SummarizeDecision::Skip {
            reason: SkipReason::BelowMinTurns,
        };
    }
    if let Some(s) = candidate.seconds_since_last_summary {
        if s < params.cooldown_seconds {
            return SummarizeDecision::Skip {
                reason: SkipReason::InCooldown,
            };
        }
    }
    if already_taken >= params.max_per_tick {
        return SummarizeDecision::Skip {
            reason: SkipReason::OverPerTickQuota,
        };
    }
    // Summarize through the second-to-last turn so the *last* turn
    // (the actual hot context) stays verbatim. Saturating sub keeps
    // us safe at turn_count==1 (which couldn't pass the threshold
    // anyway, but be defensive).
    SummarizeDecision::SummarizeUpTo {
        turn: candidate.turn_count.saturating_sub(3).max(1),
    }
}

/// Build the prompt sent to Haiku for one summarization. Pinned in
/// tests so we don't drift the contract (downstream needs to assume a
/// stable bullet shape).
///
/// `turns_text` is the rendered transcript of turns 1..=N (whatever
/// the caller chose to fold). Format: "user: ...\nassistant: ...\n"
/// — straightforward, no XML wrapping (Haiku handles plain text fine
/// for this).
pub fn format_summarization_prompt(turns_text: &str) -> String {
    format!(
        "Summarize this conversation history as a compact list of bullets, \
         preserving:\n\
         - Decisions made\n\
         - Facts established\n\
         - Open questions / TODOs\n\
         - User preferences expressed\n\
         \n\
         Do not invent details. Be terse — aim for under 500 characters. \
         Output format: dash-prefixed bullets only, no preamble or epilogue.\n\
         \n\
         === TRANSCRIPT ===\n{turns_text}\n=== END ===\n\
         \n\
         Bullets:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, turn: u32, summarized: u32, secs: Option<u64>) -> SummaryCandidate {
        SummaryCandidate {
            session_id: id.to_string(),
            agent_id: "agent".to_string(),
            turn_count: turn,
            summarized_through_turn: summarized,
            seconds_since_last_summary: secs,
        }
    }

    fn pick(
        out: &[(String, SummarizeDecision)],
        id: &str,
    ) -> SummarizeDecision {
        out.iter()
            .find(|(s, _)| s == id)
            .map(|(_, d)| d.clone())
            .expect("session id not in output")
    }

    #[test]
    fn short_session_is_skipped_below_min_turns() {
        // 5 turns total, never summarized → 5 new turns < default 10.
        let cs = vec![cand("s1", 5, 0, None)];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        assert_eq!(
            pick(&out, "s1"),
            SummarizeDecision::Skip {
                reason: SkipReason::BelowMinTurns
            }
        );
    }

    #[test]
    fn long_session_with_no_history_is_summarized() {
        let cs = vec![cand("s1", 30, 0, None)];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        match pick(&out, "s1") {
            SummarizeDecision::SummarizeUpTo { turn } => {
                // Should be 27 = 30 - 3 (keep last 3 verbatim).
                assert_eq!(turn, 27);
            }
            other => panic!("expected SummarizeUpTo, got {other:?}"),
        }
    }

    #[test]
    fn cooldown_blocks_recent_resummary() {
        // 30 turns total, summarized through 15, summarized 5 min ago →
        // 15 new turns since last summary, but cooldown=3600s.
        let cs = vec![cand("s1", 30, 15, Some(300))];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        assert_eq!(
            pick(&out, "s1"),
            SummarizeDecision::Skip {
                reason: SkipReason::InCooldown
            }
        );
    }

    #[test]
    fn cooldown_clears_after_threshold() {
        let cs = vec![cand("s1", 30, 15, Some(3700))];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        assert!(matches!(
            pick(&out, "s1"),
            SummarizeDecision::SummarizeUpTo { .. }
        ));
    }

    #[test]
    fn max_per_tick_back_pressures() {
        // 20 long sessions, max_per_tick=3 → 3 SummarizeUpTo, 17 OverPerTickQuota.
        let cs: Vec<SummaryCandidate> = (0..20)
            .map(|i| cand(&format!("s{i}"), 100, 0, None))
            .collect();
        let params = SummarizeParams {
            min_new_turns_to_trigger: 10,
            cooldown_seconds: 0,
            max_per_tick: 3,
        };
        let out = decide_summarization(&cs, &params);
        let summarized = out
            .iter()
            .filter(|(_, d)| matches!(d, SummarizeDecision::SummarizeUpTo { .. }))
            .count();
        let quota_skipped = out
            .iter()
            .filter(|(_, d)| {
                matches!(
                    d,
                    SummarizeDecision::Skip {
                        reason: SkipReason::OverPerTickQuota
                    }
                )
            })
            .count();
        assert_eq!(summarized, 3);
        assert_eq!(quota_skipped, 17);
    }

    #[test]
    fn never_summarized_session_passes_cooldown_check() {
        // seconds_since_last_summary = None → cooldown shouldn't apply.
        let cs = vec![cand("never", 30, 0, None)];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        assert!(matches!(
            pick(&out, "never"),
            SummarizeDecision::SummarizeUpTo { .. }
        ));
    }

    #[test]
    fn delta_uses_summarized_through_turn_not_total_count() {
        // 100 total turns, summarized through 95, 1h ago → only 5 new
        // turns. Below threshold (10) → skip.
        let cs = vec![cand("recent-summary", 100, 95, Some(3700))];
        let out = decide_summarization(&cs, &SummarizeParams::default());
        assert_eq!(
            pick(&out, "recent-summary"),
            SummarizeDecision::Skip {
                reason: SkipReason::BelowMinTurns
            }
        );
    }

    #[test]
    fn summarize_up_to_keeps_last_three_turns_verbatim() {
        // turn_count=N, decision should say SummarizeUpTo { turn: N-3 }.
        for n in [10, 25, 100, 1000] {
            let cs = vec![cand("s", n, 0, None)];
            let out = decide_summarization(&cs, &SummarizeParams::default());
            match pick(&out, "s") {
                SummarizeDecision::SummarizeUpTo { turn } => {
                    assert_eq!(turn, n - 3, "for turn_count={n}");
                }
                other => panic!("turn_count={n}: expected SummarizeUpTo, got {other:?}"),
            }
        }
    }

    // ── Prompt format pinning ──

    #[test]
    fn summarization_prompt_includes_required_anchors() {
        let prompt = format_summarization_prompt("user: hi\nassistant: hello");
        assert!(prompt.contains("Bullets:"));
        assert!(prompt.contains("=== TRANSCRIPT ==="));
        assert!(prompt.contains("=== END ==="));
        assert!(prompt.contains("user: hi"));
        assert!(prompt.contains("dash-prefixed bullets only"));
    }

    #[test]
    fn summarization_prompt_caps_under_target_char_budget() {
        // The prompt template itself plus a typical transcript shouldn't
        // explode. Sanity bound — if anyone adds 5KB of guardrails
        // the cost story breaks.
        let prompt = format_summarization_prompt("user: hi");
        assert!(
            prompt.len() < 1_000,
            "summarization prompt template ({} bytes) should stay tight",
            prompt.len()
        );
    }
}
