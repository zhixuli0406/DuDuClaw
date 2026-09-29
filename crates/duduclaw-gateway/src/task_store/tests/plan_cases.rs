//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — plan cases.

use super::*;

/// 2026-08-14: the structured panel verdict is persisted verbatim, a
/// stall re-dispatch bumps `dispatch_count` while keeping the original
/// `dispatched_at`, and the visit-graph signal lands on the row.
#[tokio::test]
async fn iteration_rows_carry_verdict_json_and_dispatch_signal() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store
        .record_iteration_dispatch_with_state(
            "g1",
            1,
            "2026-08-14T10:00:00Z",
            Some("hash-a"),
            Some(1),
        )
        .await
        .unwrap();
    // Stall re-dispatch of the same round: count bumps, timestamp stays,
    // refreshed streak wins.
    store
        .record_iteration_dispatch_with_state(
            "g1",
            1,
            "2026-08-14T10:30:00Z",
            Some("hash-a"),
            Some(2),
        )
        .await
        .unwrap();
    let aspects = r#"[{"name":"correctness","pass":true,"reason":""},{"name":"safety","pass":false,"reason":"deleted prod"}]"#;
    store
        .reject_review_with_verdict("g1", "[safety] deleted prod", 3, Some(aspects))
        .await
        .unwrap();

    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    let it = &iters[0];
    assert_eq!(it.dispatched_at, "2026-08-14T10:00:00Z");
    assert_eq!(it.dispatch_count, 2);
    assert_eq!(it.state_hash.as_deref(), Some("hash-a"));
    assert_eq!(it.repeat_streak, Some(2));
    let parsed: serde_json::Value =
        serde_json::from_str(it.verdict_json.as_deref().unwrap()).unwrap();
    assert_eq!(parsed[1]["name"], "safety");
    assert_eq!(parsed[1]["pass"], false);
    // Legacy wrapper still writes rows without a panel.
    store.insert_task(&goal_review_task("g2")).await.unwrap();
    store
        .record_iteration_dispatch("g2", 1, "2026-08-14T11:00:00Z")
        .await
        .unwrap();
    store.reject_review("g2", "nope", 3).await.unwrap();
    let g2 = store.list_iterations("g2").await.unwrap();
    assert!(g2[0].verdict_json.is_none());
    assert_eq!(g2[0].dispatch_count, 1);
}

#[tokio::test]
async fn revising_task_is_claimable_for_next_round() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store.reject_review("g1", "again", 3).await.unwrap();

    // atomic_claim accepts `revising` exactly like `pending`: round+1 work
    // moves it back to in_progress under the claimer.
    let out = store
        .atomic_claim(
            "g1",
            "alice",
            "2026-07-25T10:00:00Z",
            "2026-07-25T10:05:00Z",
        )
        .await
        .unwrap();
    assert!(out.is_claimed());
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(t.status, "in_progress");
    assert_eq!(t.claimed_by.as_deref(), Some("alice"));
}

#[tokio::test]
async fn soft_cap_raises_diminishing_flag() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.revision_round = 2; // next reject → round 3 == soft cap
    store.insert_task(&t).await.unwrap();

    store.reject_review("g1", "still wrong", 3).await.unwrap();
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.revision_round, 3);
    assert!(
        got.diminishing,
        "reaching soft cap 3 raises the diminishing flag"
    );
    assert_eq!(got.status, "revising", "soft cap flags but never blocks");
}

#[tokio::test]
async fn reject_review_escalates_when_retry_budget_spent() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.max_retries = 1;
    t.retry_count = 1; // budget already spent
    store.insert_task(&t).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z")
        .await
        .unwrap();

    let status = store.reject_review("g1", "give up", 3).await.unwrap();
    assert_eq!(status, "needs_human");
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "needs_human"
    );
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters[0].verdict.as_deref(), Some("escalated"));
}

/// WP-4F ③: the round's own `result_summary` is snapshotted into
/// `task_iterations.worker_excerpt` via `duduclaw_core::truncate_bytes`
/// (never a raw byte slice, which panics on a multi-byte CJK boundary),
/// AND the task-level `judge_feedback` is enriched with the composed
/// best-round note when the retry budget is exhausted on a CJK-heavy
/// result. Exercises the real `reject_review` → `iter_verdict_conn` →
/// `goal_budget_best_round` path end-to-end (not just the pure-function
/// unit tests in `goal_loop/state.rs`).
#[tokio::test]
async fn reject_review_escalate_truncates_cjk_excerpt_safely() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let mut t = goal_review_task("g1");
    t.max_retries = 1;
    t.retry_count = 1; // budget already spent ⇒ this rejection escalates
    // Deliberately over the WORKER_EXCERPT_MAX_BYTES (500) budget with a
    // 3-byte-per-char CJK string so a naive `&s[..500]` byte slice would
    // panic (500 does not land on a char boundary for an all-3-byte
    // string — see the constant's own test in goal_loop/state.rs).
    t.result_summary = Some("驗".repeat(400)); // 1200 bytes, all 3-byte chars
    store.insert_task(&t).await.unwrap();
    store
        .record_iteration_dispatch("g1", 1, "2026-08-15T10:00:00Z")
        .await
        .unwrap();

    // Must not panic.
    let status = store
        .reject_review("g1", "見 goal_loop.rs:120 缺邊界檢查", 3)
        .await
        .unwrap();
    assert_eq!(status, "needs_human");

    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    let excerpt = iters[0]
        .worker_excerpt
        .as_deref()
        .expect("a non-empty result_summary must be snapshotted");
    assert!(
        excerpt.len() <= crate::goal_budget_best_round::WORKER_EXCERPT_MAX_BYTES,
        "excerpt must respect the byte budget: {} bytes",
        excerpt.len()
    );
    assert!(!excerpt.is_empty());
    assert!(
        excerpt.chars().all(|c| c == '驗'),
        "truncation must land on a char boundary"
    );

    // The task-level judge_feedback was enriched with the best-round
    // note (this round is the only candidate, so priority 1 picks it via
    // its own verdict_json... actually this round has no verdict_json,
    // so priority 2/3 applies — either way `pick_best_round` finds this
    // sole round and the note is composed).
    let task = store.get_task("g1").await.unwrap().unwrap();
    let fb = task.judge_feedback.expect("judge_feedback must be set");
    assert!(fb.contains("已附上第 1 輪最接近完成的成果"));
    assert!(fb.contains(excerpt));
    assert!(
        fb.contains("goal_loop.rs:120"),
        "extracted gap token must be listed"
    );
}

// ── H11: pause-reason classification ────────────────────────────────

/// `mark_needs_human_with_pause` stamps the class alongside the free-text
/// reason, and the string-only wrapper degrades to the SAFE class rather
/// than guessing one from the text.
#[tokio::test]
async fn mark_needs_human_stamps_the_pause_class() {
    use crate::pause_reason::PauseReason;
    let (store, _dir) = temp_store();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    store.insert_task(&goal_review_task("g2")).await.unwrap();

    store
        .mark_needs_human_with_pause(
            "g1",
            "judge unavailable: connect timeout",
            PauseReason::Infra,
        )
        .await
        .unwrap();
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human");
    assert_eq!(got.pause_reason.as_deref(), Some("infra"));
    assert_eq!(
        PauseReason::from_stored(got.pause_reason.as_deref()),
        PauseReason::Infra
    );

    // The un-classified wrapper must NOT sniff the reason text — it writes
    // `unknown`, which reads as 「需要人工確認」.
    store
        .mark_needs_human("g2", "goal-loop iteration cap")
        .await
        .unwrap();
    let got2 = store.get_task("g2").await.unwrap().unwrap();
    assert_eq!(
        PauseReason::from_stored(got2.pause_reason.as_deref()),
        PauseReason::Unknown
    );
}

/// A judge rejection at the spent retry budget is a hard cap firing, so
/// the escalate branch classifies it `budget_exhausted` — while the
/// `revising` branch (budget left) writes no class at all.
#[tokio::test]
async fn reject_review_classifies_only_the_escalating_branch() {
    use crate::pause_reason::PauseReason;
    let (store, _dir) = temp_store();

    let mut spent = goal_review_task("g1");
    spent.max_retries = 1;
    spent.retry_count = 1;
    store.insert_task(&spent).await.unwrap();
    assert_eq!(
        store.reject_review("g1", "give up", 3).await.unwrap(),
        "needs_human"
    );
    assert_eq!(
        PauseReason::from_stored(
            store
                .get_task("g1")
                .await
                .unwrap()
                .unwrap()
                .pause_reason
                .as_deref()
        ),
        PauseReason::BudgetExhausted
    );

    // Budget remaining ⇒ back around the loop, no pause at all.
    store.insert_task(&goal_review_task("g2")).await.unwrap();
    assert_eq!(
        store.reject_review("g2", "try again", 3).await.unwrap(),
        "revising"
    );
    assert!(
        store
            .get_task("g2")
            .await
            .unwrap()
            .unwrap()
            .pause_reason
            .is_none()
    );
}

/// Every `resolve_needs_human` branch ends the pause, so the class is
/// cleared — a retried task must never re-render the chip of the pause a
/// human just resolved.
#[tokio::test]
async fn resolving_a_pause_clears_the_class() {
    use crate::pause_reason::PauseReason;
    for (decision, expect_status) in [
        ("retry", "pending"),
        ("done", "done"),
        ("abort", "cancelled"),
    ] {
        let (store, _dir) = temp_store();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store
            .mark_needs_human_with_pause("g1", "stuck", PauseReason::NoProgress)
            .await
            .unwrap();
        assert_eq!(
            store
                .get_task("g1")
                .await
                .unwrap()
                .unwrap()
                .pause_reason
                .as_deref(),
            Some("no_progress")
        );

        assert!(store.resolve_needs_human("g1", decision, "").await.unwrap());
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, expect_status);
        assert!(
            got.pause_reason.is_none(),
            "{decision}: the pause is over — the class must be cleared"
        );
    }
}

/// Legacy rows (written before the column existed) and rows that were
/// never escalated both read back as `Unknown` — the safe direction.
#[tokio::test]
async fn pause_reason_round_trips_and_legacy_rows_are_unknown() {
    use crate::pause_reason::PauseReason;
    let (store, _dir) = temp_store();

    let mut t = pending_task("p1");
    t.pause_reason = Some("restart".into());
    store.insert_task(&t).await.unwrap();
    assert_eq!(
        store
            .get_task("p1")
            .await
            .unwrap()
            .unwrap()
            .pause_reason
            .as_deref(),
        Some("restart")
    );

    let plain = pending_task("p2");
    store.insert_task(&plain).await.unwrap();
    let got = store.get_task("p2").await.unwrap().unwrap();
    assert!(got.pause_reason.is_none());
    assert_eq!(
        PauseReason::from_stored(got.pause_reason.as_deref()),
        PauseReason::Unknown
    );
}

/// The `pause_reason` ALTER is idempotent across reopens (the shared
/// migration-loop contract), and rows written before it existed survive.
#[tokio::test]
async fn pause_reason_migration_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&pending_task("old")).await.unwrap();
    }
    for _ in 0..2 {
        let store = TaskStore::open(dir.path()).unwrap();
        let got = store.get_task("old").await.unwrap().unwrap();
        assert!(got.pause_reason.is_none());
    }
}

// ── I-1c "想一想" plan-first: `plan_pending` round-trip + survival ──

/// `plan_pending` round-trips through insert/read like every other
/// column, and is `None` when never set (the overwhelming majority of
/// tasks never go through plan-first).
#[tokio::test]
async fn plan_pending_round_trips_and_defaults_to_none() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("p1");
    t.plan_pending = Some("- 步驟一\n- 步驟二".into());
    store.insert_task(&t).await.unwrap();
    assert_eq!(
        store
            .get_task("p1")
            .await
            .unwrap()
            .unwrap()
            .plan_pending
            .as_deref(),
        Some("- 步驟一\n- 步驟二")
    );

    store.insert_task(&pending_task("p2")).await.unwrap();
    assert!(
        store
            .get_task("p2")
            .await
            .unwrap()
            .unwrap()
            .plan_pending
            .is_none()
    );
}
