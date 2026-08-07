//! `(state_hash, action_digest)` visit graph — **A2**, structural loop
//! detection for the goal loop (Graph-Based Exploration, arXiv:2512.24156:
//! explicit exploration bookkeeping makes a repeated attempt structurally
//! visible instead of relying on judge-feedback text similarity — reported
//! third place on the private ARC-AGI-3 leaderboard using this primitive
//! alone, no LLM cost).
//!
//! ## What this replaces
//!
//! The pre-A2 goal loop escalated to `needs_human` when two consecutive
//! judge rejections carried byte-identical `judge_feedback` text
//! (`goal_loop.rs`'s old `normalize_feedback` comparison). That signal is
//! narrow: two rounds that fail for the same underlying reason but get
//! worded slightly differently by the judge never trip it, and it forgets
//! everything except the single most recent rejection. This module gives
//! the driver an explicit, cumulative record of "have we been here before"
//! instead — see `goal_loop.rs::tick_once`'s per-candidate loop for the
//! call sites (`peek_streak` / `commit_dispatch` replace the old two-round
//! text comparison one-for-one; the external contract — the
//! `goal_loop.oscillation` activity event type and `"goal-loop no-progress
//! oscillation"` judge_feedback text — is kept byte-identical, because
//! `topology_evolution.rs` (D5) queries that exact event-type string for
//! its own analytics and is out of scope for this change).
//!
//! ## Persistence: in-memory only, and why
//!
//! Held as a plain in-memory `HashMap` behind a `tokio::Mutex`, matching
//! the existing `GoalLoopDriver::inflight` pattern (iteration counters,
//! kickoff state, notify-retry counters — none of that crosses a gateway
//! restart either). Two options were weighed:
//!
//! 1. **In-memory `HashMap`** (chosen): zero schema/IO cost; a restart
//!    clears the visit history the same way it already clears `InFlight`'s
//!    iteration counters — a tolerated, pre-existing behavior of this
//!    driver, not a new weakness introduced here.
//! 2. **Persist to a task-row column**: durable across restarts, but the
//!    task card is explicit that cross-process sharing is not a
//!    requirement — a single gateway process is the only reader/writer of
//!    this graph, and a restart mid-goal already resets other in-memory
//!    driver state today. Paying for a column + JSON (de)serialization on
//!    every round to survive an event (mid-goal gateway restart) the driver
//!    already tolerates losing state across is cost without behavioral
//!    benefit.
//!
//! (1) is strictly cheaper and consistent with the driver's existing state
//! model, so it is what is implemented. If cross-restart survival becomes a
//! real requirement later, promoting this to a task-row column (mirroring
//! `goal_state.rs`'s `GoalStateSnapshot`, which IS persisted because its
//! content is user/agent-visible and worth keeping) is an isolated
//! follow-up — this module's public API (state_hash in, counts out) would
//! not need to change shape.

use std::collections::HashMap;
use std::path::Path;

use tokio::sync::Mutex;

/// Per-task visit bookkeeping.
#[derive(Debug, Clone, Default)]
struct TaskVisits {
    /// `(state_hash, action_digest) -> times this exact pair has been seen`.
    pair_counts: HashMap<(String, String), u32>,
    /// `state_hash` committed at the most recent actual dispatch.
    last_dispatch_state_hash: Option<String>,
    /// Consecutive actual dispatches whose `state_hash` equalled the
    /// previous one (see `commit_dispatch`).
    unchanged_streak: u32,
}

/// The goal loop's `(state_hash, action)` visit graph. One instance is
/// shared (behind `Arc`) by a `GoalLoopDriver` for the gateway process's
/// lifetime.
#[derive(Debug, Default)]
pub struct GoalVisitGraph {
    tasks: Mutex<HashMap<String, TaskVisits>>,
}

impl GoalVisitGraph {
    pub fn new() -> Self {
        Self { tasks: Mutex::new(HashMap::new()) }
    }

    /// What this task's unchanged-streak WOULD become if it were dispatched
    /// right now with `state_hash` — read-only, does not mutate. Call
    /// BEFORE deciding whether to escalate (mirrors the pre-A2 oscillation
    /// guard's placement: read state before the commit that follows a
    /// successful dispatch decision).
    pub async fn peek_streak(&self, task_id: &str, state_hash: &str) -> u32 {
        let tasks = self.tasks.lock().await;
        match tasks.get(task_id) {
            Some(v) if v.last_dispatch_state_hash.as_deref() == Some(state_hash) => {
                v.unchanged_streak + 1
            }
            _ => 1,
        }
    }

    /// Commit an actual dispatch: record `state_hash` as this task's latest
    /// dispatched state, updating the unchanged-streak accordingly. Call
    /// exactly once per real dispatch, after the decision to dispatch this
    /// tick is final (not on a tick that merely considered and deferred).
    /// Returns the new streak value.
    pub async fn commit_dispatch(&self, task_id: &str, state_hash: &str) -> u32 {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.entry(task_id.to_string()).or_default();
        entry.unchanged_streak = if entry.last_dispatch_state_hash.as_deref() == Some(state_hash) {
            entry.unchanged_streak + 1
        } else {
            1
        };
        entry.last_dispatch_state_hash = Some(state_hash.to_string());
        entry.unchanged_streak
    }

    /// Whether `state_hash` already has SOME recorded action repeated ≥2
    /// times for this task — used to annotate the dispatch prompt's
    /// excluded-approaches section ("this exact thing was already tried
    /// from this exact state and failed at least twice; don't repeat it").
    pub async fn has_repeated_action(&self, task_id: &str, state_hash: &str) -> bool {
        let tasks = self.tasks.lock().await;
        tasks
            .get(task_id)
            .map(|v| {
                v.pair_counts
                    .iter()
                    .any(|((sh, _), count)| sh == state_hash && *count >= 2)
            })
            .unwrap_or(false)
    }

    /// Record one completed round's `(state_hash, action_digest)` pair,
    /// returning the pair's new visit count. Call once per round, after the
    /// round's outcome (the agent's completion text / tool activity) is
    /// known — see `goal_loop.rs::GoalLoopDriver::capture_round_state`.
    pub async fn record_round(&self, task_id: &str, state_hash: &str, action_digest: &str) -> u32 {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.entry(task_id.to_string()).or_default();
        let key = (state_hash.to_string(), action_digest.to_string());
        let count = entry.pair_counts.entry(key).or_insert(0);
        *count += 1;
        *count
    }

    /// Drop all tracking for `task_id` — call when the task reaches a
    /// terminal state (A2 lifecycle requirement: clean up at task end so
    /// the in-memory map does not grow unbounded across a long-running
    /// gateway's lifetime).
    pub async fn clear_task(&self, task_id: &str) {
        self.tasks.lock().await.remove(task_id);
    }

    #[cfg(test)]
    async fn task_count(&self) -> usize {
        self.tasks.lock().await.len()
    }
}

/// Action digest for one round: the set of distinct tool categories used
/// (from the shared `tool_calls.jsonl` audit trail) plus a normalized hash
/// of the round's final reply text head. Combined with [`crate::goal_state::state_hash`]
/// this forms the A2 visit-graph key.
///
/// Read-only best-effort scan of `tool_calls.jsonl` via the shared
/// `crate::tool_activity::read_tool_activity_records` helper (WP-A3
/// extracted this out of `dispatch_engine.rs` into its own `pub(crate)`
/// module — this function was written before that extraction landed and
/// now reuses it instead of keeping its own duplicate reader).
pub fn action_digest(
    home_dir: &Path,
    agent_id: &str,
    since: &str,
    until: &str,
    result_text: &str,
) -> String {
    let tools = tool_categories(home_dir, agent_id, since, until);
    let head = duduclaw_core::truncate_chars(result_text.trim(), 200);
    let combined = format!("{}\u{1}{head}", tools.join(","));
    crate::goal_state::short_hash(&combined)
}

/// Distinct tool names invoked by `agent_id` in `[since, until]`, sorted.
/// Missing/unreadable/unparseable audit file ⇒ empty (never fails the
/// caller over an observability gap — inherited from
/// `crate::tool_activity::read_tool_activity_records`'s fail-open
/// contract).
fn tool_categories(home_dir: &Path, agent_id: &str, since: &str, until: &str) -> Vec<String> {
    let records = crate::tool_activity::read_tool_activity_records(home_dir, agent_id, since, until);
    let set: std::collections::BTreeSet<String> =
        records.into_iter().map(|r| r.tool_name).collect();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── peek_streak / commit_dispatch: unchanged-streak bookkeeping ──

    #[tokio::test]
    async fn peek_streak_starts_at_one_for_unknown_task() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.peek_streak("g1", "hash-a").await, 1);
    }

    #[tokio::test]
    async fn repeated_state_hash_increments_streak_on_commit() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 1);
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2, "peek must not mutate");
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 2);
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 3);
    }

    #[tokio::test]
    async fn changed_state_hash_resets_streak() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g1", "hash-a").await;
        assert_eq!(g.peek_streak("g1", "hash-a").await, 3);
        // A different hash resets the streak, even mid-sequence.
        assert_eq!(g.commit_dispatch("g1", "hash-b").await, 1);
        assert_eq!(g.peek_streak("g1", "hash-b").await, 2);
    }

    #[tokio::test]
    async fn peek_does_not_mutate_state() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        // Repeated peeks must be idempotent (read-only).
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 2, "peeks left the real streak at 1");
    }

    #[tokio::test]
    async fn tasks_are_tracked_independently() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g1", "hash-a").await;
        assert_eq!(g.peek_streak("g2", "hash-a").await, 1, "g2 has no history yet");
    }

    // ── record_round / has_repeated_action: pair bookkeeping ────

    #[tokio::test]
    async fn repeated_action_flag_requires_two_visits() {
        let g = GoalVisitGraph::new();
        assert!(!g.has_repeated_action("g1", "hash-a").await);
        g.record_round("g1", "hash-a", "action-1").await;
        assert!(!g.has_repeated_action("g1", "hash-a").await, "one visit is not yet a repeat");
        g.record_round("g1", "hash-a", "action-1").await;
        assert!(g.has_repeated_action("g1", "hash-a").await, "two visits of the same pair IS a repeat");
    }

    #[tokio::test]
    async fn distinct_actions_from_same_state_do_not_flag_as_repeated() {
        let g = GoalVisitGraph::new();
        g.record_round("g1", "hash-a", "action-1").await;
        g.record_round("g1", "hash-a", "action-2").await;
        assert!(
            !g.has_repeated_action("g1", "hash-a").await,
            "two DIFFERENT actions from the same state is exploration, not a loop"
        );
    }

    #[tokio::test]
    async fn record_round_returns_growing_visit_count() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.record_round("g1", "h", "a").await, 1);
        assert_eq!(g.record_round("g1", "h", "a").await, 2);
        assert_eq!(g.record_round("g1", "h", "a").await, 3);
    }

    // ── clear_task: terminal-state cleanup ───────────────────

    #[tokio::test]
    async fn clear_task_drops_all_tracking() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.record_round("g1", "hash-a", "action-1").await;
        g.record_round("g1", "hash-a", "action-1").await;
        assert_eq!(g.task_count().await, 1);

        g.clear_task("g1").await;
        assert_eq!(g.task_count().await, 0);
        // Post-clear, the task behaves as brand new.
        assert_eq!(g.peek_streak("g1", "hash-a").await, 1);
        assert!(!g.has_repeated_action("g1", "hash-a").await);
    }

    #[tokio::test]
    async fn clear_task_is_a_noop_for_unknown_task() {
        let g = GoalVisitGraph::new();
        g.clear_task("ghost").await; // must not panic
        assert_eq!(g.task_count().await, 0);
    }

    #[tokio::test]
    async fn clear_task_does_not_affect_other_tasks() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g2", "hash-a").await;
        g.clear_task("g1").await;
        assert_eq!(g.task_count().await, 1);
        assert_eq!(g.peek_streak("g2", "hash-a").await, 2, "g2 untouched");
    }

    // ── action_digest ─────────────────────────────────────────

    #[test]
    fn action_digest_is_deterministic_and_missing_file_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let a = action_digest(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "did the thing");
        let b = action_digest(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "did the thing");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn action_digest_differs_on_different_result_text() {
        let dir = tempfile::tempdir().unwrap();
        let a = action_digest(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "result one");
        let b = action_digest(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "result two");
        assert_ne!(a, b);
    }

    #[test]
    fn action_digest_reflects_tool_categories_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "web_fetch", "success": true}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:20:00Z", "tool_name": "bash", "success": true}),
            // Out of window — must not affect the digest.
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T05:00:00Z", "tool_name": "later_tool", "success": true}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();

        let with_tools = action_digest(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "same text");
        let empty_dir = tempfile::tempdir().unwrap();
        let without_tools = action_digest(empty_dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z", "same text");
        assert_ne!(with_tools, without_tools, "tool activity must change the digest even when result text is identical");
    }

    #[test]
    fn tool_categories_filters_by_agent_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "web_fetch", "success": true}),
            serde_json::json!({"agent_id": "bob", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "bash", "success": true}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-02T00:10:00Z", "tool_name": "outside_window", "success": true}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();
        let cats = tool_categories(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z");
        assert_eq!(cats, vec!["web_fetch".to_string()]);
    }
}
