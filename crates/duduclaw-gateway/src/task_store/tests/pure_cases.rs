//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — pure cases.

use super::*;

/// The whole point of the separate column: `resolve_needs_human`'s
/// `retry` arm overwrites `judge_feedback` (with the human's own,
/// possibly-empty, approval note) but must NEVER touch `plan_pending` —
/// otherwise the approved plan would vanish before the next dispatch
/// ever reads it.
#[tokio::test]
async fn plan_pending_survives_a_needs_human_retry_that_clears_judge_feedback() {
    let (store, _dir) = temp_store();
    let mut t = goal_review_task("g1");
    t.status = "needs_human".into();
    t.judge_feedback = Some("- 步驟一\n- 步驟二".into());
    t.plan_pending = Some("- 步驟一\n- 步驟二".into());
    store.insert_task(&t).await.unwrap();

    // Human approves with no extra note (the common "同意執行" click).
    assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "pending");
    assert!(
        got.judge_feedback.is_none(),
        "an empty approval note clears judge_feedback as usual"
    );
    assert_eq!(
        got.plan_pending.as_deref(),
        Some("- 步驟一\n- 步驟二"),
        "plan_pending must survive the retry write untouched"
    );
}

/// `clear_plan_pending` is the driver's one-time-injection consumer —
/// idempotent, unconditional on status.
#[tokio::test]
async fn clear_plan_pending_nulls_the_column() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("p1");
    t.plan_pending = Some("plan text".into());
    store.insert_task(&t).await.unwrap();

    store.clear_plan_pending("p1").await.unwrap();
    assert!(
        store
            .get_task("p1")
            .await
            .unwrap()
            .unwrap()
            .plan_pending
            .is_none()
    );
    // Idempotent — clearing an already-clear column is a no-op, not an error.
    store.clear_plan_pending("p1").await.unwrap();
}

// ── H22: latest activity timestamp (the progress signal) ────────────

#[tokio::test]
async fn latest_activity_at_returns_the_newest_row_or_none() {
    let (store, _dir) = temp_store();
    assert!(store.latest_activity_at("t1").await.unwrap().is_none());

    for ts in [
        "2026-08-15T10:00:00Z",
        "2026-08-15T10:30:00Z",
        "2026-08-15T10:05:00Z",
    ] {
        store
            .append_activity(&ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "goal_loop.dispatched".into(),
                agent_id: "alice".into(),
                task_id: Some("t1".into()),
                summary: "x".into(),
                timestamp: ts.into(),
                metadata: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        store.latest_activity_at("t1").await.unwrap().as_deref(),
        Some("2026-08-15T10:30:00Z"),
        "newest wins regardless of insertion order"
    );
    // Scoped per task — another task's events are not this task's signal.
    assert!(store.latest_activity_at("t2").await.unwrap().is_none());
}

// ── W1-5: claim_needs_human (take over) ─────────────────────────────

#[tokio::test]
async fn claim_needs_human_stamps_claimed_by_without_changing_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "needs_human".into();
    store.insert_task(&t).await.unwrap();

    let changed = store
        .claim_needs_human("g1", "channel:telegram:555")
        .await
        .unwrap();
    assert!(changed);
    let got = store.get_task("g1").await.unwrap().unwrap();
    // Status stays `needs_human` — GoalLoopDriver's candidate query never
    // reads this status, so the loop is already stopped without a
    // dedicated status transition.
    assert_eq!(got.status, "needs_human");
    assert_eq!(got.claimed_by.as_deref(), Some("channel:telegram:555"));
}

#[tokio::test]
async fn claim_needs_human_is_idempotent_and_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "needs_human".into();
    store.insert_task(&t).await.unwrap();

    assert!(
        store
            .claim_needs_human("g1", "channel:telegram:1")
            .await
            .unwrap()
    );
    // A second (even different) decider re-stamps rather than failing —
    // unlike `resolve_needs_human` there is no terminal state to guard.
    assert!(
        store
            .claim_needs_human("g1", "channel:slack:2")
            .await
            .unwrap()
    );
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.claimed_by.as_deref(), Some("channel:slack:2"));
    assert_eq!(got.status, "needs_human");
}

#[tokio::test]
async fn claim_needs_human_fails_closed_off_needs_human() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "pending".into();
    store.insert_task(&t).await.unwrap();

    let changed = store
        .claim_needs_human("g1", "channel:telegram:1")
        .await
        .unwrap();
    assert!(!changed, "a task not in needs_human must not be claimable");
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert!(got.claimed_by.is_none());
}

#[tokio::test]
async fn claim_needs_human_on_missing_task_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let changed = store
        .claim_needs_human("ghost", "channel:telegram:1")
        .await
        .unwrap();
    assert!(!changed);
}

// ── I-3a: continue_from_terminal ("接著做") ─────────────────────────

#[tokio::test]
async fn continue_from_terminal_reopens_done_failed_and_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    for (id, status) in [
        ("g-done", "done"),
        ("g-failed", "failed"),
        ("g-cancelled", "cancelled"),
    ] {
        let mut t = goal_review_task(id);
        t.status = status.into();
        t.completed_at = Some("2026-08-01T00:00:00Z".into());
        t.claimed_by = Some("worker".into());
        store.insert_task(&t).await.unwrap();

        let changed = store
            .continue_from_terminal(id, "再補一份摘要")
            .await
            .unwrap();
        assert!(changed, "{status} must be reopenable");
        let got = store.get_task(id).await.unwrap().unwrap();
        assert_eq!(got.status, "pending");
        assert!(got.claimed_by.is_none(), "a stale claim must be cleared");
        assert!(
            got.completed_at.is_none(),
            "a stale completion timestamp must be cleared"
        );
        assert!(
            got.judge_feedback
                .as_deref()
                .unwrap()
                .contains("再補一份摘要")
        );
    }
}

#[tokio::test]
async fn continue_from_terminal_preserves_revision_round_for_iteration_continuity() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "failed".into();
    t.revision_round = 4;
    store.insert_task(&t).await.unwrap();

    store
        .continue_from_terminal("g1", "再試一次")
        .await
        .unwrap();
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(
        got.revision_round, 4,
        "the round counter must continue, not reset"
    );
}

#[tokio::test]
async fn continue_from_terminal_rejects_a_blank_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "done".into();
    store.insert_task(&t).await.unwrap();

    let err = store.continue_from_terminal("g1", "   ").await.unwrap_err();
    assert!(err.contains("訊息"), "got: {err}");
    assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
}

#[tokio::test]
async fn continue_from_terminal_fails_closed_on_non_terminal_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "needs_human".into();
    store.insert_task(&t).await.unwrap();

    let changed = store
        .continue_from_terminal("g1", "再多做一點")
        .await
        .unwrap();
    assert!(
        !changed,
        "needs_human already has its own retry/done/abort path"
    );
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "needs_human"
    );
}

#[tokio::test]
async fn continue_from_terminal_fails_closed_on_non_goal_mode_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "done".into();
    t.goal_mode = false;
    store.insert_task(&t).await.unwrap();

    let changed = store
        .continue_from_terminal("g1", "再多做一點")
        .await
        .unwrap();
    assert!(
        !changed,
        "an ordinary board task must not be reopenable via this path"
    );
    assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
}

#[tokio::test]
async fn full_round_records_dispatch_submit_verdict_and_agent_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.status = "pending".into();
    t.result_summary = None;
    store.insert_task(&t).await.unwrap();

    // Round 1: dispatch (driver, in the past), claim, complete → review.
    // A past dispatch stamp makes the agent-seconds delta clearly positive
    // against the real `Utc::now()` complete_task uses.
    store
        .record_iteration_dispatch("g1", 1, "2020-01-01T00:00:00Z")
        .await
        .unwrap();
    store
        .atomic_claim(
            "g1",
            "alice",
            "2026-07-25T10:00:05Z",
            "2026-07-25T10:05:00Z",
        )
        .await
        .unwrap();
    store
        .complete_task("g1", "done round 1", "alice")
        .await
        .unwrap();

    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    assert_eq!(iters[0].round, 1);
    assert!(
        iters[0].submitted_at.is_some(),
        "submit stamped on complete"
    );
    // agent_seconds accumulated on the task (submit − dispatch).
    let after = store.get_task("g1").await.unwrap().unwrap();
    assert!(after.agent_seconds > 0, "agent clock accumulated");
    assert_eq!(after.status, "review");

    // Reject → verdict sealed on round 1, task revising.
    store.reject_review("g1", "fix it", 3).await.unwrap();
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters[0].verdict.as_deref(), Some("rejected"));
}

#[tokio::test]
async fn dispatch_iteration_is_idempotent_per_round() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&pending_task("g1")).await.unwrap();
    // A stall re-dispatch of the same round must not open a duplicate row.
    store
        .record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z")
        .await
        .unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-07-25T10:01:00Z")
        .await
        .unwrap();
    assert_eq!(store.list_iterations("g1").await.unwrap().len(), 1);
}

#[tokio::test]
async fn accept_review_seals_accepted_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z")
        .await
        .unwrap();

    assert!(store.accept_review("g1", "looks good").await.unwrap());
    assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters[0].verdict.as_deref(), Some("accepted"));
}

#[tokio::test]
async fn iteration_migration_idempotent_and_old_rows_default_zero() {
    let dir = tempfile::tempdir().unwrap();
    // Simulate a pre-Iterative-Kanban db: insert a task, close, reopen twice.
    {
        let s = TaskStore::open(dir.path()).unwrap();
        s.insert_task(&pending_task("old1")).await.unwrap();
    }
    let s2 = TaskStore::open(dir.path()).unwrap();
    // Reopening runs the ALTERs again — must not error, and the old row's new
    // columns default to zero.
    let t = s2.get_task("old1").await.unwrap().unwrap();
    assert_eq!(t.revision_round, 0);
    assert!(!t.diminishing);
    assert_eq!(t.agent_seconds, 0);
    // task_iterations table exists and is queryable (empty for the old task).
    assert!(s2.list_iterations("old1").await.unwrap().is_empty());
}

#[tokio::test]
async fn flow_metrics_computes_first_pass_yield_and_review_depth() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();

    // done, first pass (revision_round 0).
    let mut d1 = goal_review_task("d1");
    d1.status = "done".into();
    d1.completed_at = Some(chrono::Utc::now().to_rfc3339());
    d1.revision_round = 0;
    d1.agent_seconds = 10;
    store.insert_task(&d1).await.unwrap();
    // done, second round (revision_round 1 → not first pass).
    let mut d2 = goal_review_task("d2");
    d2.status = "done".into();
    d2.completed_at = Some(chrono::Utc::now().to_rfc3339());
    d2.revision_round = 1;
    d2.agent_seconds = 30;
    store.insert_task(&d2).await.unwrap();
    // one still in review (queue depth).
    store.insert_task(&goal_review_task("r1")).await.unwrap();

    let m = store
        .flow_metrics(&chrono::Utc::now().to_rfc3339())
        .await
        .unwrap();
    assert_eq!(m.review_queue_depth, 1);
    assert_eq!(m.accepts_last_7d, 2);
    let alice = m.agents.iter().find(|a| a.agent_id == "alice").unwrap();
    assert_eq!(alice.finished, 2);
    assert!(
        (alice.first_pass_yield - 0.5).abs() < 1e-9,
        "1 of 2 first-pass"
    );
    assert!(
        (alice.avg_rounds - 1.5).abs() < 1e-9,
        "rounds 1 and 2 → avg 1.5"
    );
    assert_eq!(alice.review_queue_depth, 1);
}
