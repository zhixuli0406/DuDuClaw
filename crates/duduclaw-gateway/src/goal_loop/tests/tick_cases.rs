//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — tick cases.

use super::*;

// ── G3: resolve_deadline_hit (design §6, market-belief-loop sister
// package) ────────────────────────────────────────────────

#[test]
fn resolve_deadline_hit_neither_deadline_reached_is_none() {
    let created = "2026-08-01T00:00:00Z";
    let now = DateTime::parse_from_rfc3339("2026-08-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // wall clock budget 24h, no task deadline ⇒ 12h in, nothing fires.
    assert_eq!(resolve_deadline_hit(created, None, 24, now), None);
}

#[test]
fn resolve_deadline_hit_wall_clock_only_matches_pre_g3_behavior() {
    let created = "2026-08-01T00:00:00Z";
    let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // No deadline_at set ⇒ byte-identical to the old wall-clock-only check.
    assert_eq!(
        resolve_deadline_hit(created, None, 24, now),
        Some(DeadlineHit::WallClock)
    );
}

#[test]
fn resolve_deadline_hit_task_deadline_earlier_fires_first() {
    let created = "2026-08-01T00:00:00Z";
    // Wall clock budget is 24h (deadline 2026-08-02T00:00Z); the task's own
    // deadline_at is much tighter — 4h from creation.
    let deadline_at = "2026-08-01T04:00:00Z";
    let now = DateTime::parse_from_rfc3339("2026-08-01T05:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        resolve_deadline_hit(created, Some(deadline_at), 24, now),
        Some(DeadlineHit::TaskDeadline),
        "the tighter per-task deadline overrides the looser global wall clock"
    );
}

#[test]
fn resolve_deadline_hit_wall_clock_still_wins_when_task_deadline_is_looser() {
    let created = "2026-08-01T00:00:00Z";
    // Task deadline is LATER than the global wall-clock budget (720h vs
    // 24h) — deadline_at can only tighten, never loosen, so the global
    // budget must still fire at 24h.
    let deadline_at = "2026-08-31T00:00:00Z";
    let now = DateTime::parse_from_rfc3339("2026-08-02T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        resolve_deadline_hit(created, Some(deadline_at), 24, now),
        Some(DeadlineHit::WallClock)
    );
}

#[test]
fn resolve_deadline_hit_unparseable_deadline_at_degrades_to_wall_clock_only() {
    let created = "2026-08-01T00:00:00Z";
    let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // Garbage deadline_at must never panic and must never block the
    // wall-clock half of the check (fail-open on the deadline only).
    assert_eq!(
        resolve_deadline_hit(created, Some("not-a-timestamp"), 24, now),
        Some(DeadlineHit::WallClock)
    );
}

#[test]
fn resolve_deadline_hit_unparseable_created_at_with_valid_task_deadline() {
    let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // created_at itself unparseable (legacy/corrupt row) but deadline_at
    // is valid and past ⇒ the task-level deadline still fires.
    assert_eq!(
        resolve_deadline_hit("garbage", Some("2026-08-02T00:00:00Z"), 24, now),
        Some(DeadlineHit::TaskDeadline)
    );
}

#[tokio::test]
async fn candidate_selection_enqueues_only_assigned_goal_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // (1) assigned goal task → should dispatch.
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    // (2) goal task with no assignee → skipped.
    store.insert_task(&goal_task("g2", "  ")).await.unwrap();
    // (3) non-goal task assigned → skipped (not goal_mode).
    let mut plain = goal_task("g3", "alice");
    plain.goal_mode = false;
    store.insert_task(&plain).await.unwrap();

    let d = driver(store, queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "only the assigned goal task is dispatched"
    );
    assert_eq!(pending[0].target, "alice");
    assert!(pending[0].payload.contains("[goal-loop task_id=g1 iter=1]"));
}

#[tokio::test]
async fn in_flight_dedup_does_not_re_enqueue_while_awaiting_pickup() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store, queue.clone(), small_cfg());
    // Two ticks back-to-back: the task is still `todo` (agent hasn't picked
    // it up) and the stall timeout has not elapsed ⇒ only one enqueue.
    d.tick_once().await.unwrap();
    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "no duplicate enqueue while awaiting pickup"
    );
}

#[tokio::test]
async fn iteration_cap_escalates_to_needs_human() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    // iteration_cap = 2, stall = 0 so every tick re-dispatches the (never
    // picked up) task, counting an iteration each time.
    let cfg = GoalLoopConfig {
        iteration_cap: 2,
        stalled_secs: 0,
        ..small_cfg()
    };
    let d = driver(store.clone(), queue.clone(), cfg);

    d.tick_once().await.unwrap(); // iter 1
    d.tick_once().await.unwrap(); // iter 2 (== cap after this dispatch)
    d.tick_once().await.unwrap(); // current_iter 2 >= cap ⇒ escalate

    let t = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(t.status, "needs_human");
    assert_eq!(t.judge_feedback.as_deref(), Some("goal-loop iteration cap"));
    // Two work messages enqueued (iter 1 and 2); the 3rd tick escalated
    // instead of dispatching.
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 2);
}

#[tokio::test]
async fn deadline_cap_escalates_to_needs_human() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // Task created 48h ago, wall-clock budget 24h ⇒ deadline exceeded.
    let mut t = goal_task("g1", "alice");
    t.created_at = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human");
    assert_eq!(got.judge_feedback.as_deref(), Some("goal-loop deadline"));
    // No work message enqueued — the deadline guard fired before dispatch.
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}

/// G3: a task-level `deadline_at` in the past escalates even though the
/// global wall-clock budget has not been reached, with a distinct
/// escalation reason so a human sees WHY (design §6 G3).
#[tokio::test]
async fn task_deadline_escalates_before_wall_clock_with_distinct_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // Created 1h ago (wall-clock budget is 24h — nowhere near tripping),
    // but the assign form's own deadline already lapsed.
    let mut t = goal_task("g1", "alice");
    t.created_at = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    t.deadline_at = Some((Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human");
    assert_eq!(got.judge_feedback.as_deref(), Some("時限已到未通過驗收"));
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}

/// G3: a task-level `deadline_at` set in the FUTURE (looser than the
/// global wall clock, or simply not yet reached) must not escalate —
/// only actually-past deadlines fire.
#[tokio::test]
async fn task_deadline_in_the_future_does_not_escalate() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let mut t = goal_task("g1", "alice");
    t.deadline_at = Some((Utc::now() + chrono::Duration::hours(10)).to_rfc3339());
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_ne!(got.status, "needs_human");
    assert!(!queue.pending_messages(10).await.unwrap().is_empty());
}

/// G2: every dispatch carries the risk boundary section, programmatically
/// — a task with no explicit `risk_boundary` gets the built-in baseline
/// text (no `config.toml [goal_defaults]` present in the test cwd, so
/// `baseline_boundary` fails open to `DEFAULT_BASELINE_BOUNDARY`), never
/// silently omitted.
#[tokio::test]
async fn dispatch_always_injects_risk_boundary_section() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].payload.contains("## 本目標風險邊界"));
    assert!(pending[0].payload.contains("遵循當地法規"));
    assert!(pending[0].payload.contains("驗收判官退回"));
}

/// G2: an explicit per-task `risk_boundary` overrides the baseline text
/// in the injected section.
#[tokio::test]
async fn dispatch_injects_explicit_task_risk_boundary_over_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut t = goal_task("g1", "alice");
    t.risk_boundary = Some("不得動用生產資料庫寫入權限".to_string());
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].payload.contains("不得動用生產資料庫寫入權限"));
    assert!(
        !pending[0].payload.contains("遵循當地法規"),
        "explicit risk_boundary replaces, not appends to, the baseline"
    );
}

#[tokio::test]
async fn failed_dispatch_frees_the_slot_and_the_lease_on_the_next_tick() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    store.insert_task(&goal_task("g2", "alice")).await.unwrap();

    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_concurrency_limit(Some(1), 1800);

    d.tick_once().await.unwrap();
    assert_eq!(
        d.inflight.lock().await.len(),
        1,
        "cap 1 ⇒ one round in flight"
    );
    assert_eq!(
        duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
        1
    );
    assert_eq!(
        fail_all_pending(&queue, "Local inference engine not available").await,
        1
    );

    d.tick_once().await.unwrap();
    assert_eq!(
        duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
        1,
        "the failed round's lease was released and the OTHER task admitted"
    );
    let inflight = d.inflight.lock().await;
    assert_eq!(inflight.len(), 1);
    assert!(
        !inflight.contains_key("g1") || !inflight.contains_key("g2"),
        "exactly one task in flight"
    );
    drop(inflight);
    // The failed task is in back-off, the other one got the slot.
    assert!(
        d.in_dispatch_backoff("g1", Utc::now()).await
            || d.in_dispatch_backoff("g2", Utc::now()).await
    );
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "todo",
        "one failure does not escalate"
    );
}

#[tokio::test]
async fn three_consecutive_dispatch_failures_park_the_task_needs_human() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = GoalLoopDriver::new(
        store.clone(),
        queue.clone(),
        GoalLoopConfig {
            iteration_cap: 10,
            ..small_cfg()
        },
    )
    .with_home_dir(dir.path().to_path_buf());

    for round in 1..=DISPATCH_FAILURE_LIMIT {
        d.tick_once().await.unwrap();
        assert_eq!(
            fail_all_pending(&queue, "runtime not installed").await,
            1,
            "round {round} dispatched"
        );
        d.tick_once().await.unwrap(); // notices the failure
        // Skip the back-off window so the next tick re-dispatches.
        if let Some(entry) = d.dispatch_failures.lock().await.get_mut("g1") {
            entry.1 = Utc::now() - chrono::Duration::seconds(1);
        }
    }
    let task = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(task.status, "needs_human", "third failure escalates");
    assert!(d.inflight.lock().await.is_empty());
    assert!(
        !d.dispatch_failures.lock().await.contains_key("g1"),
        "escalation clears the streak"
    );
}

#[tokio::test]
async fn dispatch_backoff_skips_the_task_until_its_window_passes() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = driver(store.clone(), queue.clone(), small_cfg());

    d.tick_once().await.unwrap();
    fail_all_pending(&queue, "boom").await;
    d.tick_once().await.unwrap(); // failure noticed → back-off 60 s
    d.tick_once().await.unwrap(); // inside the window: no new dispatch
    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "no re-dispatch inside the back-off window"
    );
    assert_eq!(dispatch_backoff_secs(1), 60);
    assert_eq!(dispatch_backoff_secs(2), 120);
    assert_eq!(dispatch_backoff_secs(3), 240);
    assert_eq!(dispatch_backoff_secs(20), 60 * 64, "capped");
}

#[tokio::test]
async fn stale_goal_leases_are_released_when_the_driver_starts() {
    let dir = tempfile::tempdir().unwrap();
    // Two orphaned leases from a previous process life.
    for _ in 0..2 {
        assert!(matches!(
            duduclaw_core::concurrency_try_acquire(
                dir.path(),
                CONCURRENCY_CLASS_GOAL,
                Some(2),
                1800
            ),
            duduclaw_core::ConcurrencyAcquireOutcome::Admitted(_)
        ));
    }
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_concurrency_limit(Some(2), 1800);
    d.release_stale_goal_leases();
    assert_eq!(
        duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
        0
    );
    d.tick_once().await.unwrap();
    assert_eq!(
        d.inflight.lock().await.len(),
        1,
        "a fresh task is admitted after the orphans are gone"
    );
}

#[tokio::test]
async fn plan_pending_task_is_not_dispatched_before_approval() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store
        .insert_task(&plan_first_pending_task(
            "g1",
            "alice",
            "- 查資料\n- 寫報告",
        ))
        .await
        .unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "a plan awaiting human approval must not execute"
    );
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "needs_human"
    );
}

/// WP-G2: a goal with a criteria ledger carries the `## 驗收帳本` section
/// right after the `<state>` block; `criteria_ledger = "off"` and goals
/// without a ledger dispatch the same payload as before.
#[tokio::test]
async fn dispatch_injects_the_criteria_ledger_only_when_present_and_not_off() {
    use crate::goal_loop::criteria_ledger::{CriteriaLedger, CriteriaLedgerMode};
    async fn payload(mode: Option<&str>, with_ledger: bool) -> String {
        let dir = tempfile::tempdir().unwrap();
        if let Some(m) = mode {
            std::fs::write(
                dir.path().join("config.toml"),
                format!("[goal_loop]\ncriteria_ledger = \"{m}\"\n"),
            )
            .unwrap();
        }
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.acceptance_criteria = Some("產出 hello.txt\n內容含你好".into());
        if with_ledger {
            t.criteria_ledger = CriteriaLedger::new("g1", "產出 hello.txt\n內容含你好", CriteriaLedgerMode::Report)
                .map(|l| l.to_json());
        }
        store.insert_task(&t).await.unwrap();
        let d = driver(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf());
        d.tick_once().await.unwrap();
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        pending[0].payload.clone()
    }
    let with = payload(None, true).await;
    assert!(with.contains("## 驗收帳本"), "{with}");
    assert!(with.contains("[C1] 產出 hello.txt — 尚未回報"));
    assert!(with.contains("[C2] 內容含你好 — 尚未回報"));
    let state_end = with.find("</state>").expect("state block");
    let ledger_at = with.find("## 驗收帳本").unwrap();
    let boundary_at = with.find("## 本目標風險邊界").unwrap();
    assert!(state_end < ledger_at && ledger_at < boundary_at);

    let off = payload(Some("off"), true).await;
    let none = payload(None, false).await;
    assert!(!off.contains("驗收帳本"));
    assert_eq!(off, none, "off with a ledger == no ledger");
}
