//! A1 ledger completeness (2026-09-30): additive `task_iterations` columns,
//! their migration, and the escalated-round seal.

use super::*;
use crate::pause_reason::PauseReason;
use crate::task_store::{IterationDispatchLedger, TaskIterationRow};

const NEW_COLUMNS: [&str; 7] = [
    "evaluator_verdict",
    "iter_seq",
    "team_mode",
    "gate_inputs_json",
    "state_block_json",
    "knobs_json",
    "pause_reason",
];

fn iteration_columns(home: &std::path::Path) -> HashSet<String> {
    let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(task_iterations)").unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<HashSet<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn ledger_migration_is_idempotent_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = TaskStore::open(dir.path()).unwrap();
        s.insert_task(&pending_task("g1")).await.unwrap();
    }
    let s2 = TaskStore::open(dir.path()).unwrap();
    let _s3 = TaskStore::open(dir.path()).unwrap();
    let cols = iteration_columns(dir.path());
    for c in NEW_COLUMNS {
        assert!(cols.contains(c), "missing column {c}");
    }
    assert!(s2.list_iterations("g1").await.unwrap().is_empty());
}

#[tokio::test]
async fn ledger_migration_upgrades_a_pre_a1_database() {
    let dir = tempfile::tempdir().unwrap();
    // A tasks.db whose `task_iterations` predates A1 (the 2026-08-14 shape),
    // with one sealed row already in it.
    {
        let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE task_iterations (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id        TEXT NOT NULL,
                 round          INTEGER NOT NULL,
                 dispatched_at  TEXT NOT NULL,
                 submitted_at   TEXT,
                 judged_at      TEXT,
                 verdict        TEXT,
                 judge_feedback TEXT,
                 feedback_class TEXT,
                 verdict_json   TEXT,
                 dispatch_count INTEGER NOT NULL DEFAULT 1,
                 state_hash     TEXT,
                 repeat_streak  INTEGER,
                 worker_excerpt TEXT
             );
             INSERT INTO task_iterations (task_id, round, dispatched_at, verdict, judge_feedback)
             VALUES ('old', 1, '2026-08-01T00:00:00Z', 'rejected', 'fix it');",
        )
        .unwrap();
    }
    let store = TaskStore::open(dir.path()).expect("old db must still open");
    let cols = iteration_columns(dir.path());
    for c in NEW_COLUMNS {
        assert!(cols.contains(c), "missing column {c}");
    }
    let iters = store.list_iterations("old").await.unwrap();
    assert_eq!(iters.len(), 1);
    assert_eq!(iters[0].verdict.as_deref(), Some("rejected"));
    assert!(iters[0].iter_seq.is_none());
    assert!(iters[0].pause_reason.is_none());
    assert!(iters[0].knobs_json.is_none());
}

#[tokio::test]
async fn dispatch_ledger_columns_round_trip_and_survive_a_bare_redispatch() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("g1")).await.unwrap();
    let ledger = IterationDispatchLedger {
        iter_seq: Some(3),
        team_mode: Some("team".into()),
        gate_inputs_json: Some(r#"{"decision":"team"}"#.into()),
        state_block_json: Some(r#"{"v":1,"truncated":false}"#.into()),
    };
    store
        .record_iteration_dispatch_with_ledger(
            "g1",
            1,
            "2026-09-30T10:00:00Z",
            Some("abc"),
            Some(1),
            &ledger,
        )
        .await
        .unwrap();
    // A later dispatch of the same round that carries no ledger (the legacy
    // entry point) must not erase what the first one recorded.
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:05:00Z")
        .await
        .unwrap();
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    let it = &iters[0];
    assert_eq!(it.dispatch_count, 2);
    assert_eq!(it.iter_seq, Some(3));
    assert_eq!(it.team_mode.as_deref(), Some("team"));
    assert_eq!(it.gate_inputs_json.as_deref(), Some(r#"{"decision":"team"}"#));
    assert_eq!(
        it.state_block_json.as_deref(),
        Some(r#"{"v":1,"truncated":false}"#)
    );

    // A stall re-dispatch that does carry a newer ordinal updates it.
    store
        .record_iteration_dispatch_with_ledger(
            "g1",
            1,
            "2026-09-30T10:10:00Z",
            None,
            None,
            &IterationDispatchLedger {
                iter_seq: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.iter_seq, Some(4));
    assert_eq!(it.team_mode.as_deref(), Some("team"));
}

#[tokio::test]
async fn evaluator_verdict_is_written_on_the_open_round() {
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    store
        .record_iteration_evaluator_verdict("g1", "candidate_complete")
        .await
        .unwrap();
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.evaluator_verdict.as_deref(), Some("candidate_complete"));
}

#[tokio::test]
async fn knobs_json_is_sealed_with_the_verdict() {
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    assert!(
        store
            .accept_review_with_ledger("g1", "ok", None, Some(r#"{"iteration_cap":5}"#))
            .await
            .unwrap()
    );
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.knobs_json.as_deref(), Some(r#"{"iteration_cap":5}"#));
    // A1-2: the accepted round keeps its own output excerpt.
    assert_eq!(it.worker_excerpt.as_deref(), Some("attempt"));
}

#[tokio::test]
async fn accepted_round_worker_excerpt_is_bounded_on_a_char_boundary() {
    let (store, _dir) = temp_store();
    let mut t = goal_review_task("g1");
    t.result_summary = Some("字".repeat(400)); // 1200 bytes
    store.insert_task(&t).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    store.accept_review("g1", "ok").await.unwrap();
    let it = &store.list_iterations("g1").await.unwrap()[0];
    let ex = it.worker_excerpt.as_deref().expect("excerpt");
    assert!(!ex.is_empty());
    assert!(ex.len() <= crate::goal_budget_best_round::WORKER_EXCERPT_MAX_BYTES);
    assert!(ex.chars().all(|c| c == '字'));
}

#[tokio::test]
async fn sealing_needs_human_marks_round_escalated_with_pause_reason() {
    for pause in [PauseReason::BlockedNeedsDecision, PauseReason::Infra] {
        let (store, _dir) = temp_store();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store
            .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
            .await
            .unwrap();
        assert!(
            store
                .mark_needs_human_sealing_round("g1", "parked", pause, None)
                .await
                .unwrap()
        );
        let t = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert_eq!(t.judge_feedback.as_deref(), Some("parked"));
        assert_eq!(t.pause_reason.as_deref(), Some(pause.as_str()));
        let it = &store.list_iterations("g1").await.unwrap()[0];
        assert_eq!(it.verdict.as_deref(), Some("escalated"));
        assert_eq!(it.pause_reason.as_deref(), Some(pause.as_str()));
        assert!(it.judge_feedback.is_none(), "no judge ruled");
        assert_eq!(it.worker_excerpt.as_deref(), Some("attempt"));
    }
}

#[tokio::test]
async fn escalated_round_keeps_pause_reason_after_resolve_needs_human() {
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    store
        .mark_needs_human_sealing_round("g1", "blocked", PauseReason::BlockedNeedsDecision, None)
        .await
        .unwrap();
    assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert!(t.pause_reason.is_none(), "task-level class is cleared");
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.pause_reason.as_deref(), Some("blocked_needs_decision"));
}

#[tokio::test]
async fn budget_escalation_on_reject_records_pause_reason() {
    let (store, _dir) = temp_store();
    let mut t = goal_review_task("g1");
    t.max_retries = 0;
    store.insert_task(&t).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    assert_eq!(
        store.reject_review("g1", "still wrong", 3).await.unwrap(),
        "needs_human"
    );
    store.resolve_needs_human("g1", "abort", "").await.unwrap();
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.verdict.as_deref(), Some("escalated"));
    assert_eq!(it.pause_reason.as_deref(), Some("budget_exhausted"));
}

#[tokio::test]
async fn driver_escalation_stamps_pause_on_latest_round_without_touching_verdict() {
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    store.reject_review("g1", "fix it", 3).await.unwrap();
    store
        .stamp_iteration_pause("g1", PauseReason::NoProgress.as_str())
        .await
        .unwrap();
    // A second stamp never overwrites the first class.
    store
        .stamp_iteration_pause("g1", PauseReason::BudgetExhausted.as_str())
        .await
        .unwrap();
    let it = &store.list_iterations("g1").await.unwrap()[0];
    assert_eq!(it.verdict.as_deref(), Some("rejected"));
    assert_eq!(it.pause_reason.as_deref(), Some("no_progress"));
}

/// Undo today's A1-2 seal on a row list: the shape the same history had
/// before `mark_needs_human_sealing_round` existed (an un-judged row with
/// no verdict, no excerpt, no pause class).
fn unseal_pre_a1(rows: &[TaskIterationRow]) -> Vec<TaskIterationRow> {
    rows.iter()
        .cloned()
        .map(|mut it| {
            if it.verdict.as_deref() == Some("escalated") && it.judge_feedback.is_none() {
                it.verdict = None;
                it.judged_at = None;
                it.worker_excerpt = None;
                it.knobs_json = None;
                it.pause_reason = None;
            }
            it
        })
        .collect()
}

/// FX7: a round parked without a judge ruling (evaluator `blocked`), then a
/// human `retry` of the SAME round (`revision_round` does not advance), then
/// a reject, then an accept on the next round.
///
/// Expected `list_iterations` (round ASC, id ASC):
/// 1. round 1 — the parked attempt: `escalated`, NULL `judge_feedback`,
///    `pause_reason = blocked_needs_decision`, excerpt of the first attempt.
///    The retry leaves all dispatch-time fields on this row unchanged.
/// 2. round 1 — the retried attempt: a new row opened by dispatch,
///    `rejected` with the
///    judge's feedback and the second attempt's excerpt.
/// 3. round 2 — `accepted`.
///
/// And the inputs the next dispatch reads from the history
/// (`excluded_from_iterations`, `pick_best_round`) are identical to what the
/// pre-seal history would have produced: the sealed row contributes nothing.
#[tokio::test]
async fn human_retry_of_a_sealed_round_keeps_next_dispatch_inputs_unchanged() {
    use crate::goal_loop::state::{excluded_from_iterations, pick_best_round};

    let (store, _dir) = temp_store();
    let mut t = goal_review_task("g1");
    t.status = "in_progress".into();
    t.result_summary = None;
    store.insert_task(&t).await.unwrap();

    // Round 1, first attempt → evaluator `blocked` → parked + sealed.
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    store
        .complete_task("g1", "first attempt", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .mark_needs_human_sealing_round(
                "g1",
                "evaluator blocked",
                PauseReason::BlockedNeedsDecision,
                None
            )
            .await
            .unwrap()
    );

    // Operator presses retry: same round number is dispatched again.
    assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
    let before_retry = store.list_iterations("g1").await.unwrap()[0].clone();
    let after_retry = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(after_retry.revision_round, 0, "retry does not advance the round");
    store
        .record_iteration_dispatch("g1", after_retry.revision_round + 1, "2026-09-30T11:00:00Z")
        .await
        .unwrap();
    let dispatched = store.list_iterations("g1").await.unwrap();
    assert_eq!(dispatched.len(), 2, "retry dispatch opens its own attempt");
    assert_eq!(dispatched[0].dispatch_count, before_retry.dispatch_count);
    assert_eq!(dispatched[0].dispatched_at, before_retry.dispatched_at);
    assert_eq!(dispatched[1].dispatched_at, "2026-09-30T11:00:00Z");
    let retry_id = dispatched[1].id;
    store
        .complete_task("g1", "second attempt", "alice")
        .await
        .unwrap()
        .unwrap();
    let feedback = "缺少 `src/ledger.rs:42` 的測試";
    assert_eq!(
        store.reject_review("g1", feedback, 3).await.unwrap(),
        "revising"
    );

    let rows = store.list_iterations("g1").await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:#?}");
    let (sealed, retried) = (&rows[0], &rows[1]);
    assert_eq!((sealed.round, retried.round), (1, 1));
    assert_eq!(sealed.verdict.as_deref(), Some("escalated"));
    assert!(sealed.judge_feedback.is_none());
    assert_eq!(sealed.pause_reason.as_deref(), Some("blocked_needs_decision"));
    assert_eq!(sealed.worker_excerpt.as_deref(), Some("first attempt"));
    assert_eq!(sealed.dispatch_count, 1, "the sealed dispatch stays immutable");
    assert_eq!(retried.id, retry_id, "submit must use the dispatched attempt");
    assert_eq!(retried.verdict.as_deref(), Some("rejected"));
    assert_eq!(retried.judge_feedback.as_deref(), Some(feedback));
    assert_eq!(retried.worker_excerpt.as_deref(), Some("second attempt"));
    assert!(retried.submitted_at.is_some() && retried.judged_at.is_some());

    // (b) The next dispatch's inputs are what the pre-seal history yields.
    let pre = unseal_pre_a1(&rows);
    assert_eq!(excluded_from_iterations(&rows), excluded_from_iterations(&pre));
    assert_eq!(excluded_from_iterations(&rows), vec![feedback.to_string()]);
    let pick = pick_best_round(&rows);
    assert_eq!(pick, pick_best_round(&pre));
    let pick = pick.expect("the rejected round is a candidate");
    assert_eq!(pick.round, 1);
    assert_eq!(pick.excerpt.as_deref(), Some("second attempt"));

    // Round 2 accepted.
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(t.revision_round, 1);
    store
        .record_iteration_dispatch("g1", t.revision_round + 1, "2026-09-30T12:00:00Z")
        .await
        .unwrap();
    store
        .complete_task("g1", "third attempt", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(store.accept_review("g1", "looks right").await.unwrap());
    let rows = store.list_iterations("g1").await.unwrap();
    let verdicts: Vec<(i64, Option<&str>)> =
        rows.iter().map(|r| (r.round, r.verdict.as_deref())).collect();
    assert_eq!(
        verdicts,
        vec![
            (1, Some("escalated")),
            (1, Some("rejected")),
            (2, Some("accepted")),
        ]
    );
    let pre = unseal_pre_a1(&rows);
    assert_eq!(excluded_from_iterations(&rows), excluded_from_iterations(&pre));
    assert_eq!(pick_best_round(&rows), pick_best_round(&pre));
}

#[tokio::test]
async fn retry_dispatch_preserves_sealed_ledger_and_redispatches_only_new_attempt() {
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    let first = IterationDispatchLedger {
        iter_seq: Some(1),
        team_mode: Some("solo".into()),
        gate_inputs_json: Some("first gate".into()),
        state_block_json: Some("first state".into()),
    };
    store
        .record_iteration_dispatch_with_ledger(
            "g1", 1, "2026-09-30T10:00:00Z", Some("first hash"), Some(0), &first,
        )
        .await
        .unwrap();
    store
        .mark_needs_human_sealing_round("g1", "blocked", PauseReason::BlockedNeedsDecision, None)
        .await
        .unwrap();
    store.resolve_needs_human("g1", "retry", "").await.unwrap();
    let retry = IterationDispatchLedger {
        iter_seq: Some(2),
        team_mode: Some("team".into()),
        gate_inputs_json: Some("retry gate".into()),
        state_block_json: Some("retry state".into()),
    };
    store
        .record_iteration_dispatch_with_ledger(
            "g1", 1, "2026-09-30T11:00:00Z", Some("retry hash"), Some(1), &retry,
        )
        .await
        .unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T11:05:00Z")
        .await
        .unwrap();
    let rows = store.list_iterations("g1").await.unwrap();
    assert_eq!(rows.len(), 2);
    let (sealed, open) = (&rows[0], &rows[1]);
    assert_eq!(sealed.dispatch_count, 1);
    assert_eq!(sealed.state_hash.as_deref(), Some("first hash"));
    assert_eq!(sealed.repeat_streak, Some(0));
    assert_eq!(sealed.iter_seq, Some(1));
    assert_eq!(sealed.team_mode.as_deref(), Some("solo"));
    assert_eq!(sealed.gate_inputs_json.as_deref(), Some("first gate"));
    assert_eq!(sealed.state_block_json.as_deref(), Some("first state"));
    assert_eq!(open.dispatch_count, 2);
    assert_eq!(open.dispatched_at, "2026-09-30T11:00:00Z");
    assert_eq!(open.state_hash.as_deref(), Some("retry hash"));
    assert_eq!(open.iter_seq, Some(2));
    assert_eq!(open.team_mode.as_deref(), Some("team"));
    assert!(open.submitted_at.is_none() && open.judged_at.is_none());
}

#[tokio::test]
async fn retry_submit_skips_a_sealed_attempt_without_submission() {
    let (store, _dir) = temp_store();
    let mut task = goal_review_task("g1");
    task.status = "in_progress".into();
    task.result_summary = None;
    store.insert_task(&task).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T10:00:00Z")
        .await
        .unwrap();
    // A driver/evaluator can seal an attempt before any worker submission.
    store
        .mark_needs_human_sealing_round("g1", "blocked", PauseReason::BlockedNeedsDecision, None)
        .await
        .unwrap();
    store.resolve_needs_human("g1", "retry", "").await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-09-30T11:00:00Z")
        .await
        .unwrap();
    store.complete_task("g1", "retry result", "alice").await.unwrap().unwrap();
    let rows = store.list_iterations("g1").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].submitted_at.is_none(), "sealed row must not be submitted later");
    assert!(rows[1].submitted_at.is_some());
    store.accept_review("g1", "ok").await.unwrap();
    let rows = store.list_iterations("g1").await.unwrap();
    assert_eq!(rows[0].verdict.as_deref(), Some("escalated"));
    assert_eq!(rows[1].verdict.as_deref(), Some("accepted"));
}

#[tokio::test]
async fn submit_and_verdict_choose_latest_open_attempt_for_a_legacy_duplicate_round() {
    let (store, _dir) = temp_store();
    let mut task = goal_review_task("g1");
    task.status = "in_progress".into();
    task.result_summary = None;
    store.insert_task(&task).await.unwrap();
    // Legacy databases can contain several unjudged rows for one round.
    let latest_id = {
        let conn = store.conn.lock().await;
        conn.execute(
            "INSERT INTO task_iterations (task_id, round, dispatched_at)
             VALUES ('g1', 1, '2026-09-30T10:00:00Z'),
                    ('g1', 1, '2026-09-30T11:00:00Z')",
            [],
        ).unwrap();
        conn.last_insert_rowid()
    };
    store.complete_task("g1", "latest result", "alice").await.unwrap().unwrap();
    store.record_iteration_evaluator_verdict("g1", "candidate_complete").await.unwrap();
    store.accept_review("g1", "ok").await.unwrap();
    let rows = store.list_iterations("g1").await.unwrap();
    assert!(rows[0].submitted_at.is_none() && rows[0].judged_at.is_none());
    assert!(rows[0].evaluator_verdict.is_none());
    assert_eq!(rows[1].id, latest_id);
    assert!(rows[1].submitted_at.is_some() && rows[1].judged_at.is_some());
    assert_eq!(rows[1].evaluator_verdict.as_deref(), Some("candidate_complete"));
    assert_eq!(rows[1].verdict.as_deref(), Some("accepted"));
}
