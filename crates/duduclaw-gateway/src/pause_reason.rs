//! H11: structured pause-reason classification for `needs_human` goal tasks.
//!
//! Borrowed from grok-build's eight-state goal state machine
//! (`research/harness-2026-08/grok-build.md` §2.3), **adapted rather than
//! copied**: DuDuClaw deliberately does NOT add a new task `status` value —
//! `needs_human` already has a dozen consumers (dashboard board columns, the
//! inbox model, channel decision cards, `resolve_needs_human`'s fail-closed
//! `WHERE status='needs_human'` guards, MCP task tools), and splitting it into
//! eight statuses would break every one of them. Instead the *reason* a task
//! parked is stored as a closed classification alongside the free-text
//! `judge_feedback` that was already there.
//!
//! ## Why a closed set and not the free text
//!
//! Every escalation path already wrote a human-readable reason into
//! `judge_feedback`, but three of those strings are built from LLM output
//! (the two-stage evaluator's `evidence` / `next_step`) or from an arbitrary
//! transport error (`judge unavailable: {e}`). Classifying by substring at
//! read time would therefore be classifying *model-authored prose* — exactly
//! the kind of unanchored `contains` check coding convention 2 forbids for a
//! routing decision. So the class is stamped **at the call site**, where the
//! trigger is known statically, and the stored string is only ever compared
//! for exact equality.
//!
//! ## Fail-safe direction
//!
//! Unknown, empty, absent (every row written before this column existed), and
//! unrecognised values all resolve to [`PauseReason::Unknown`], which reads as
//! 「需要人工確認」 — i.e. an unclassifiable pause degrades toward *more*
//! human attention, never less.

/// Closed classification of why a goal task is parked `needs_human`.
///
/// Six variants, one per family of trigger that actually exists in the
/// codebase today (see the table in the module tests). Deliberately smaller
/// than grok-build's eight states: `user_paused` has no DuDuClaw trigger (a
/// human takeover keeps the task in `needs_human` via `claim_needs_human`
/// without re-escalating it, and `takeover::is_target_paused` freezes a task
/// *without* parking it), so inventing the state would mean shipping a chip
/// nothing can ever set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// The loop ran but the state stopped changing — A2 oscillation
    /// (identical `state_hash` two rounds running).
    NoProgress,
    /// A hard cap fired: iteration cap, judge retry budget, the global
    /// wall clock, or a per-goal `deadline_at`.
    BudgetExhausted,
    /// Something outside the agent blocks the goal and a person must act —
    /// the two-stage evaluator's `Blocked` verdict, or an upstream
    /// dependency that terminally failed.
    BlockedNeedsDecision,
    /// The platform itself failed, not the work — e.g. the acceptance judge
    /// was unreachable (fail-safe park, never an auto-accept).
    Infra,
    /// The gateway restarted while this task was in flight and
    /// `[goal_loop] resume_on_restart = "pause"` parked it for
    /// re-confirmation instead of silently resuming.
    Restart,
    /// Not classified: rows written before this column existed, or any
    /// stored value this build does not recognise. Reads as "needs a human
    /// to look" — the safe direction.
    Unknown,
}

impl PauseReason {
    /// Stable wire/storage token. Never localise this — it is persisted in
    /// SQLite and shipped over the dashboard RPC as a key.
    pub fn as_str(self) -> &'static str {
        match self {
            PauseReason::NoProgress => "no_progress",
            PauseReason::BudgetExhausted => "budget_exhausted",
            PauseReason::BlockedNeedsDecision => "blocked_needs_decision",
            PauseReason::Infra => "infra",
            PauseReason::Restart => "restart",
            PauseReason::Unknown => "unknown",
        }
    }

    /// Resolve a stored column value back into a class. Exact (trimmed,
    /// ASCII-case-insensitive) token equality only — never substring
    /// matching, per coding convention 2. `None` / empty / unrecognised ⇒
    /// [`PauseReason::Unknown`].
    pub fn from_stored(stored: Option<&str>) -> Self {
        match stored.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "no_progress" => PauseReason::NoProgress,
            "budget_exhausted" => PauseReason::BudgetExhausted,
            "blocked_needs_decision" => PauseReason::BlockedNeedsDecision,
            "infra" => PauseReason::Infra,
            "restart" => PauseReason::Restart,
            _ => PauseReason::Unknown,
        }
    }

    /// One short zh-TW phrase for channel notices — end-user vocabulary, no
    /// internal jargon (no "iteration cap", no "oscillation", no task-store
    /// column names). The dashboard renders its own localised chip from
    /// [`Self::as_str`] instead of reusing this.
    pub fn label_zh(self) -> &'static str {
        match self {
            PauseReason::NoProgress => "卡住沒進展",
            PauseReason::BudgetExhausted => "次數或時限用盡",
            PauseReason::BlockedNeedsDecision => "等你決策",
            PauseReason::Infra => "系統問題",
            PauseReason::Restart => "系統重啟後暫停",
            PauseReason::Unknown => "需要人工確認",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant round-trips through its wire token.
    #[test]
    fn wire_tokens_round_trip() {
        for r in [
            PauseReason::NoProgress,
            PauseReason::BudgetExhausted,
            PauseReason::BlockedNeedsDecision,
            PauseReason::Infra,
            PauseReason::Restart,
            PauseReason::Unknown,
        ] {
            assert_eq!(PauseReason::from_stored(Some(r.as_str())), r);
            assert!(!r.label_zh().is_empty());
        }
    }

    /// Absent / empty / whitespace / legacy / typo'd values all land on
    /// `Unknown` — an unclassifiable pause must read as "a human should
    /// look", never as a confident class.
    #[test]
    fn unknown_and_legacy_values_are_safe() {
        for stored in [
            None,
            Some(""),
            Some("   "),
            Some("no-progress"),      // wrong separator
            Some("NO_PROGRESSS"),     // typo
            Some("goal-loop iteration cap"), // a legacy free-text reason
            Some("blocked"),          // a task status, not a pause class
        ] {
            assert_eq!(
                PauseReason::from_stored(stored),
                PauseReason::Unknown,
                "stored = {stored:?}"
            );
        }
        assert_eq!(PauseReason::Unknown.label_zh(), "需要人工確認");
    }

    /// Case and surrounding whitespace are tolerated (a hand-edited DB row
    /// or a config-driven seed should not silently become `Unknown`), but
    /// only for an otherwise-exact token.
    #[test]
    fn stored_lookup_is_trimmed_and_case_insensitive() {
        assert_eq!(PauseReason::from_stored(Some(" Infra ")), PauseReason::Infra);
        assert_eq!(
            PauseReason::from_stored(Some("BUDGET_EXHAUSTED")),
            PauseReason::BudgetExhausted
        );
        // Not a prefix/substring match: extra content is NOT the class.
        assert_eq!(
            PauseReason::from_stored(Some("infra error")),
            PauseReason::Unknown
        );
    }

    /// The wire tokens are the dashboard's i18n keys — pin them so a rename
    /// here cannot silently desync `web/src/i18n/*.json`.
    #[test]
    fn wire_tokens_are_pinned() {
        assert_eq!(PauseReason::NoProgress.as_str(), "no_progress");
        assert_eq!(PauseReason::BudgetExhausted.as_str(), "budget_exhausted");
        assert_eq!(
            PauseReason::BlockedNeedsDecision.as_str(),
            "blocked_needs_decision"
        );
        assert_eq!(PauseReason::Infra.as_str(), "infra");
        assert_eq!(PauseReason::Restart.as_str(), "restart");
        assert_eq!(PauseReason::Unknown.as_str(), "unknown");
    }
}
