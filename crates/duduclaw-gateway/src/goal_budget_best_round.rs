//! WP-4F: attach the closest-to-done deliverable when a goal task's budget
//! (iteration cap / wall clock / per-task deadline / judge retry budget)
//! runs out, instead of an empty-handed `needs_human` escalation.
//!
//! ## Problem this replaces
//!
//! Before this module, every `PauseReason::BudgetExhausted` escalation wrote
//! a bare trigger string ("goal-loop iteration cap", "goal-loop deadline",
//! or the last round's judge feedback) into `judge_feedback` — the only
//! field both the channel notification (`goal_notify::needs_human_body`) and
//! the dashboard task-detail view read. A human opening that card saw "I
//! couldn't finish" with no visibility into what the agent actually
//! produced. AutoDesign (arXiv:2608.13560) motivates attaching the
//! best-so-far candidate instead of nothing.
//!
//! ## Selection rule (deterministic, zero LLM)
//!
//! Scans the task's sealed `task_iterations` rows (verdict `rejected` or
//! `escalated` — i.e. every round that did NOT result in acceptance) and
//! picks, in order:
//!
//! 1. **The last round that reached the MAV panel** (`verdict_json` is
//!    `Some`) among rejected/escalated rounds. The two-stage evaluator's own
//!    per-round `candidate_complete`/`continue`/`blocked` verdict is not
//!    persisted anywhere today (`dispatch_engine.rs`'s `PreDecision` is a
//!    request-scoped enum, never written to `task_iterations`), so adding a
//!    literal read of "the evaluator said candidate_complete" would need a
//!    new column purely to mirror a value the panel-reaching itself already
//!    implies in practice: a `Continue` verdict is routed straight back to
//!    `revising` via `reject_review` (no panel call, no `verdict_json`); the
//!    panel only ever runs when the evaluator said `candidate_complete`,
//!    degraded open on its own error/timeout, or two-stage judging is
//!    disabled entirely (in which case every rejected round reaches the
//!    panel, so this tier degrades to "last round" — never wrong, just not
//!    extra-informative). `verdict_json.is_some()` is therefore the
//!    zero-new-column proxy for "this round was taken seriously as a
//!    completion candidate" — see the WP-4F report for the full trade-off.
//! 2. **Fewest extracted gap-fingerprint tokens**
//!    ([`crate::goal_gap_fingerprint::gap_tokens`]) in the round's rejection
//!    feedback, among rounds with at least one extractable token. Fewer
//!    concrete citations/key tokens ⇒ closer to passing. Ties favor the
//!    later round.
//! 3. **The last round**, full stop — the pre-WP-4F fallback content (the
//!    most recent rejection reason), now paired with that round's own
//!    worker excerpt when one was captured.
//!
//! Zero rejected/escalated rounds (the budget ran out before any round was
//! ever judged — e.g. a wall-clock deadline hit before the very first
//! dispatch) ⇒ [`pick_best_round`] returns `None` and the caller keeps the
//! bare pre-WP-4F escalation reason. No round is fabricated.

use crate::task_store::TaskIterationRow;

/// Bytes kept for a round's worker-result excerpt persisted into
/// `task_iterations.worker_excerpt` at verdict time (`reject_review_with_verdict`)
/// — bounded so a multi-KB agent reply never balloons a history row.
/// CJK-safe: callers MUST truncate with `duduclaw_core::truncate_bytes`, never
/// a raw byte slice.
pub const WORKER_EXCERPT_MAX_BYTES: usize = 500;

/// How many gap tokens are actually listed in [`compose_escalation_note`]
/// (a display budget; the underlying extraction can return up to
/// `goal_gap_fingerprint`'s own `MAX_FINGERPRINT_TOKENS`).
const DISPLAYED_GAP_TOKENS: usize = 5;

/// Chars kept from the picked round's own judge feedback when it is used as
/// the fallback "驗收意見" line (no extractable gap tokens at all).
const FALLBACK_FEEDBACK_MAX_CHARS: usize = 200;

/// One "best round" pick — the round attached to a budget-exhausted
/// escalation via [`compose_escalation_note`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BestRoundPick {
    pub round: i64,
    /// The round's own submitted output, when one was captured
    /// (`task_iterations.worker_excerpt`). `None` for rounds sealed before
    /// this column existed, or a round with no result at all.
    pub excerpt: Option<String>,
    /// That round's own rejection feedback (trimmed, unbounded — callers
    /// truncate at display time).
    pub judge_feedback: String,
    /// Extracted gap tokens from `judge_feedback` (display-capped at
    /// [`DISPLAYED_GAP_TOKENS`]), case-preserved, in occurrence order. Empty
    /// when the feedback carried no citation/key token at all.
    pub gaps: Vec<String>,
}

/// Deterministic, zero-LLM selection of the round to attach to a
/// budget-exhausted escalation. See module docs for the three-tier rule.
/// `None` when the task has no rejected/escalated round at all — the caller
/// must then keep its pre-WP-4F empty-handed escalation text, never
/// fabricate a pick.
pub fn pick_best_round(iterations: &[TaskIterationRow]) -> Option<BestRoundPick> {
    let candidates: Vec<&TaskIterationRow> = iterations
        .iter()
        .filter(|it| matches!(it.verdict.as_deref(), Some("rejected") | Some("escalated")))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Priority 1: last round that reached the MAV panel (verdict_json
    // present) among rejected/escalated rounds.
    if let Some(it) = candidates
        .iter()
        .copied()
        .filter(|it| it.verdict_json.is_some())
        .max_by_key(|it| it.round)
    {
        return Some(build_pick(it));
    }

    // Priority 2: fewest gap-fingerprint tokens, among rounds with at least
    // one extractable token. Ties favor the later round.
    let mut best: Option<(&TaskIterationRow, usize)> = None;
    for it in candidates.iter().copied() {
        let fb = it.judge_feedback.as_deref().unwrap_or("");
        let n = crate::goal_gap_fingerprint::gap_tokens(fb).len();
        if n == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((cur, cur_n)) => n < cur_n || (n == cur_n && it.round > cur.round),
        };
        if better {
            best = Some((it, n));
        }
    }
    if let Some((it, _)) = best {
        return Some(build_pick(it));
    }

    // Priority 3: last round, full stop.
    candidates.iter().copied().max_by_key(|it| it.round).map(build_pick)
}

fn build_pick(it: &TaskIterationRow) -> BestRoundPick {
    let feedback = it
        .judge_feedback
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let gaps = crate::goal_gap_fingerprint::gap_tokens(&feedback)
        .into_iter()
        .take(DISPLAYED_GAP_TOKENS)
        .collect();
    BestRoundPick {
        round: it.round,
        excerpt: it
            .worker_excerpt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        judge_feedback: feedback,
        gaps,
    }
}

/// Compose the enriched `judge_feedback` text written on a budget-exhausted
/// escalation. `base_reason` is the escalation trigger's own short text
/// (e.g. `"goal-loop iteration cap"` or the last round's raw judge feedback)
/// — kept verbatim as the first line so any existing consumer reading the
/// reason as a leading prefix (activity feed, logs) is unaffected.
pub fn compose_escalation_note(base_reason: &str, pick: &BestRoundPick) -> String {
    let mut out = format!(
        "{base_reason}\n已附上第 {} 輪最接近完成的成果：",
        pick.round
    );
    match &pick.excerpt {
        Some(excerpt) => out.push_str(excerpt),
        None => out.push_str("（此輪未留下成果摘要）"),
    }
    if !pick.gaps.is_empty() {
        out.push_str("\n驗收時仍缺：");
        out.push_str(&pick.gaps.join("、"));
    } else if !pick.judge_feedback.is_empty() {
        out.push_str("\n驗收意見：");
        out.push_str(&duduclaw_core::truncate_chars(
            &pick.judge_feedback,
            FALLBACK_FEEDBACK_MAX_CHARS,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iter_row(
        round: i64,
        verdict: Option<&str>,
        judge_feedback: Option<&str>,
        verdict_json: Option<&str>,
        worker_excerpt: Option<&str>,
    ) -> TaskIterationRow {
        TaskIterationRow {
            id: round,
            task_id: "g1".to_string(),
            round,
            dispatched_at: "2026-08-15T00:00:00Z".to_string(),
            submitted_at: Some("2026-08-15T00:05:00Z".to_string()),
            judged_at: Some("2026-08-15T00:06:00Z".to_string()),
            verdict: verdict.map(str::to_string),
            judge_feedback: judge_feedback.map(str::to_string),
            feedback_class: None,
            verdict_json: verdict_json.map(str::to_string),
            dispatch_count: 1,
            state_hash: None,
            repeat_streak: None,
            worker_excerpt: worker_excerpt.map(str::to_string),
        }
    }

    // ── 0 輪邊角：維持原行為（None，不硬湊）──────────────────────

    #[test]
    fn no_iterations_returns_none() {
        assert_eq!(pick_best_round(&[]), None);
    }

    #[test]
    fn iterations_with_no_rejected_or_escalated_round_returns_none() {
        // Only an open (never-judged) round and an accepted round — neither
        // is a "budget exhausted while stuck" candidate.
        let rows = vec![
            iter_row(1, None, None, None, None),
            iter_row(2, Some("accepted"), Some("looks good"), Some("[]"), None),
        ];
        assert_eq!(pick_best_round(&rows), None);
    }

    // ── 優先序情境 1：最後一個 candidate_complete（verdict_json 存在）但被駁回的輪 ──

    #[test]
    fn priority1_prefers_last_round_that_reached_the_panel() {
        let rows = vec![
            // Round 1: cheap evaluator said `continue` — never reached the
            // panel (no verdict_json).
            iter_row(1, Some("rejected"), Some("還在進行中，先這樣"), None, Some("draft v1")),
            // Round 2: reached the panel and was rejected (candidate_complete
            // proxy).
            iter_row(
                2,
                Some("rejected"),
                Some("見 goal_loop.rs:120，缺少邊界檢查"),
                Some(r#"[{"name":"correctness","pass":false,"reason":"..."}]"#),
                Some("draft v2 — 已加上大部分邏輯"),
            ),
            // Round 3: evaluator degraded to `continue`-style rejection again
            // (no verdict_json) — must NOT beat round 2 despite being later.
            iter_row(3, Some("rejected"), Some("還缺一步"), None, Some("draft v3")),
        ];
        let pick = pick_best_round(&rows).expect("must pick a round");
        assert_eq!(pick.round, 2, "the last panel-reviewed round wins, not the literal last round");
        assert_eq!(pick.excerpt.as_deref(), Some("draft v2 — 已加上大部分邏輯"));
        assert!(!pick.gaps.is_empty(), "goal_loop.rs:120 citation must be extracted");
    }

    // ── 優先序情境 2：無 verdict_json，選 gap 指紋數最少的輪 ──────

    #[test]
    fn priority2_prefers_fewest_gap_tokens_when_no_round_reached_the_panel() {
        let rows = vec![
            iter_row(
                1,
                Some("rejected"),
                Some("見 a.rs:1 及 b.rs:2，還缺 `foo` 與 `bar` 兩處"),
                None,
                Some("draft v1"),
            ),
            iter_row(
                2,
                Some("rejected"),
                Some("只差 `baz` 一處，見 c.rs:3"),
                None,
                Some("draft v2 — 幾乎完成"),
            ),
        ];
        let pick = pick_best_round(&rows).expect("must pick a round");
        assert_eq!(pick.round, 2, "fewer extractable gaps ⇒ closer to done");
        assert_eq!(pick.excerpt.as_deref(), Some("draft v2 — 幾乎完成"));
    }

    #[test]
    fn priority2_ties_favor_the_later_round() {
        let rows = vec![
            iter_row(1, Some("rejected"), Some("見 a.rs:1"), None, Some("v1")),
            iter_row(2, Some("rejected"), Some("見 b.rs:2"), None, Some("v2")),
        ];
        let pick = pick_best_round(&rows).expect("must pick a round");
        assert_eq!(pick.round, 2, "equal gap counts ⇒ prefer the more recent round");
    }

    // ── 優先序情境 3：都沒有可抽取的 gap（純散文回饋)→ 最後一輪 ──

    #[test]
    fn priority3_falls_back_to_the_last_round_when_nothing_is_extractable() {
        let rows = vec![
            iter_row(1, Some("rejected"), Some("說明太模糊，請補充"), None, Some("v1 摘要")),
            iter_row(2, Some("rejected"), Some("還是不夠清楚"), None, Some("v2 摘要")),
        ];
        let pick = pick_best_round(&rows).expect("must pick a round");
        assert_eq!(pick.round, 2, "no extractable gaps anywhere ⇒ plain last-round fallback");
        assert_eq!(pick.excerpt.as_deref(), Some("v2 摘要"));
        assert!(pick.gaps.is_empty());
    }

    #[test]
    fn escalated_verdict_round_is_eligible_like_rejected() {
        // task_store's own retry-budget-exhausted branch seals the final
        // round with verdict = "escalated", not "rejected" — it must still
        // be considered (it is typically the ONLY sealed round in that
        // path's minimal-repro tests).
        let rows = vec![iter_row(1, Some("escalated"), Some("give up"), None, Some("attempt"))];
        let pick = pick_best_round(&rows).expect("must pick a round");
        assert_eq!(pick.round, 1);
        assert_eq!(pick.excerpt.as_deref(), Some("attempt"));
    }

    // ── compose_escalation_note ───────────────────────────────

    #[test]
    fn compose_note_includes_round_excerpt_and_gaps() {
        let pick = BestRoundPick {
            round: 3,
            excerpt: Some("已完成月報草稿".to_string()),
            judge_feedback: "見 goal_loop.rs:120，缺少邊界檢查".to_string(),
            gaps: vec!["goal_loop.rs:120".to_string()],
        };
        let note = compose_escalation_note("goal-loop iteration cap", &pick);
        assert!(note.starts_with("goal-loop iteration cap\n"));
        assert!(note.contains("第 3 輪"));
        assert!(note.contains("已完成月報草稿"));
        assert!(note.contains("goal_loop.rs:120"));
    }

    #[test]
    fn compose_note_degrades_gracefully_with_no_excerpt_and_no_gaps() {
        let pick = BestRoundPick {
            round: 1,
            excerpt: None,
            judge_feedback: "說明太模糊".to_string(),
            gaps: vec![],
        };
        let note = compose_escalation_note("goal-loop deadline", &pick);
        assert!(note.contains("（此輪未留下成果摘要）"));
        assert!(note.contains("驗收意見"));
        assert!(note.contains("說明太模糊"));
    }

    // ── CJK-safe truncation (③) ────────────────────────────────
    //
    // `WORKER_EXCERPT_MAX_BYTES` truncation itself happens at the
    // `task_store.rs` call site via `duduclaw_core::truncate_bytes` (see
    // `task_store::tests::reject_review_escalate_truncates_cjk_excerpt_safely`
    // for the end-to-end DB round-trip). This test pins the byte budget
    // constant lands on a value that a naive raw byte slice over 3-byte CJK
    // characters would NOT land on cleanly, proving the constant alone can't
    // silently degrade into a panic-prone raw slice elsewhere.
    #[test]
    fn worker_excerpt_budget_is_not_a_multiple_of_a_3_byte_cjk_char() {
        assert_ne!(
            WORKER_EXCERPT_MAX_BYTES % 3,
            0,
            "a budget landing exactly on a 3-byte CJK boundary would hide a raw-slice bug"
        );
        let cjk = "驗".repeat(WORKER_EXCERPT_MAX_BYTES); // way over budget, 3 bytes/char
        let truncated = duduclaw_core::truncate_bytes(&cjk, WORKER_EXCERPT_MAX_BYTES);
        assert!(truncated.len() <= WORKER_EXCERPT_MAX_BYTES);
        // Must still be valid UTF-8 (guaranteed by the type, but assert the
        // char count to prove it backed off to a full character, not a
        // half-eaten one that `truncate_bytes` had to reject entirely).
        assert!(!truncated.is_empty());
        assert!(truncated.chars().all(|c| c == '驗'));
    }
}
