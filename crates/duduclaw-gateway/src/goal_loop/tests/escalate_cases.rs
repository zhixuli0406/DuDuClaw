//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — escalate cases.

use super::*;

/// Approving the plan (the dashboard/channel "重試" action on a
/// `needs_human` task — no new button kind) must both start execution AND
/// carry the approved plan into that very first round's prompt; the plan
/// is then consumed so a later round never repeats it.
#[tokio::test]
async fn approving_a_pending_plan_dispatches_it_into_round_one_then_consumes_it() {
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

    // Human clicks "重試" (= approve and start) — the SAME resolution
    // path any other needs_human task uses, with no note.
    assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
    let approved = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(approved.status, "pending");
    assert_eq!(
        approved.plan_pending.as_deref(),
        Some("- 查資料\n- 寫報告"),
        "plan_pending must have survived the approval write"
    );

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let dispatched = queue.pending_messages(10).await.unwrap();
    assert_eq!(dispatched.len(), 1, "round 1 must dispatch once approved");
    assert!(
        dispatched[0].payload.contains("查資料") && dispatched[0].payload.contains("寫報告"),
        "the approved plan must reach round 1's prompt: {}",
        dispatched[0].payload
    );
    assert!(
        dispatched[0].payload.contains("<execution_plan>"),
        "the plan is rendered as its own distinct block, not folded into judge feedback"
    );

    // Consumed — a later round (e.g. a judge rejection re-dispatch) must
    // not keep repeating the same plan block forever.
    assert!(
        store
            .get_task("g1")
            .await
            .unwrap()
            .unwrap()
            .plan_pending
            .is_none(),
        "plan_pending must be cleared after being injected once"
    );
}

#[tokio::test]
async fn concurrency_cap_bounds_new_dispatches_per_tick() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    for i in 0..5 {
        store
            .insert_task(&goal_task(&format!("g{i}"), "alice"))
            .await
            .unwrap();
    }

    let cfg = GoalLoopConfig {
        max_concurrent: 2,
        ..small_cfg()
    };
    let d = driver(store, queue.clone(), cfg);
    d.tick_once().await.unwrap();

    // Only 2 of the 5 goal tasks admitted this tick.
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        2,
        "concurrency cap admits at most 2 new tasks"
    );
}

// ── RFC-27: edition concurrency gate (cross-process in-flight cap) ──

#[tokio::test]
async fn edition_concurrency_gate_defers_new_goals_then_frees_slot_on_release() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    for i in 0..3 {
        store
            .insert_task(&goal_task(&format!("g{i}"), "alice"))
            .await
            .unwrap();
    }

    // In-process guard set high (3) so ONLY the edition gate (cap 1) bites —
    // this isolates the new gate from the pre-existing `max_concurrent` one.
    let cfg = GoalLoopConfig {
        max_concurrent: 3,
        ..small_cfg()
    };
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), cfg)
        .with_home_dir(dir.path().to_path_buf())
        .with_concurrency_limit(Some(1), 1800);

    // Tick 1: edition cap 1 admits exactly one of the three new goals; the
    // other two defer (queue semantics — not dropped).
    d.tick_once().await.unwrap();
    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        1,
        "edition cap 1 admits at most one new goal per tick"
    );
    assert_eq!(
        duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
        1,
        "exactly one cross-process lease is held"
    );

    // The admitted task reaches a terminal state → its lease is released.
    let held: Vec<String> = d.inflight.lock().await.keys().cloned().collect();
    assert_eq!(held.len(), 1);
    store
        .update_task(&held[0], &serde_json::json!({ "status": "done" }))
        .await
        .unwrap();

    // Tick 2: reconcile releases the finished task's lease, then a deferred
    // goal is admitted into the freed slot (still capped at 1).
    d.tick_once().await.unwrap();
    assert_eq!(
        duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
        1,
        "the freed slot is taken by exactly one previously-deferred goal"
    );
    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        2,
        "one more goal dispatched after the first released its slot"
    );
}

#[tokio::test]
async fn edition_concurrency_unlimited_is_byte_identical() {
    // `None` limit ⇒ the gate is a complete no-op: all three admit under the
    // in-process cap of 3, and the lease file is never even created.
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    for i in 0..3 {
        store
            .insert_task(&goal_task(&format!("g{i}"), "alice"))
            .await
            .unwrap();
    }
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_concurrency_limit(None, 1800);
    d.tick_once().await.unwrap();
    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        3,
        "unlimited edition dispatches all three under the in-process cap"
    );
    assert!(
        !dir.path().join("concurrency_leases.json").exists(),
        "the unlimited path must never touch the lease file"
    );
}

#[tokio::test]
async fn rejected_task_is_re_dispatched_with_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // Simulate a task that came back from a judge rejection: pending, with
    // judge_feedback and a prior retry.
    let mut t = goal_task("g1", "alice");
    t.status = "pending".into();
    t.judge_feedback = Some("missing the summary section".into());
    store.insert_task(&t).await.unwrap();

    let d = driver(store, queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].payload.contains("missing the summary section"),
        "retry message must carry the judge feedback"
    );
    assert!(pending[0].payload.contains("上一輪驗收未通過"));
}

/// A2 semantics (M3: re-aligned to the pre-A2 guard's exact timing):
/// escalation fires the moment this round's `<state>` hash (goal +
/// confirmed facts + self-reported hypotheses + LATEST judge feedback —
/// `goal_state::StateBlock::hash_input`) would repeat for a 2nd
/// consecutive dispatch — i.e. two consecutive rejections already
/// carried the identical underlying state, so a 3rd dispatch would be
/// provably useless. This matches the pre-A2 guard byte-for-byte in
/// timing (it escalated the moment two consecutive judge rejections
/// carried identical feedback text, never attempting a 3rd dispatch
/// first) while keeping A2's stronger "state" comparison (goal +
/// confirmed facts + hypotheses + latest rejection, not just the raw
/// feedback string). Same external contract as before (event_type
/// `goal_loop.oscillation`, reason text `"goal-loop no-progress
/// oscillation"`).
#[tokio::test]
async fn identical_feedback_two_rounds_escalates_oscillation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // High iteration cap (both difficulty tiers) so ONLY the A2 guard can
    // escalate here — the test goal text classifies as Simple.
    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100; // don't let reject_review self-escalate
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), cfg);

    // Initial dispatch (iter 1, awaiting pickup) — commits state_hash A
    // (no rejection history yet).
    d.tick_once().await.unwrap();

    // Round 1: agent works, judge rejects "same". Next tick re-dispatches
    // — state_hash changes A→B (∅ → "same reason" as the latest excluded
    // entry), so the streak resets to 1: no escalation yet.
    agent_round_then_reject(&d, &store, "g1", "same reason").await;
    d.tick_once().await.unwrap();
    assert_ne!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "needs_human",
        "first rejection must not escalate"
    );

    // Round 2: identical feedback again ⇒ state_hash stays B for what
    // would be a 2nd consecutive dispatch of that exact state ⇒
    // escalate now, WITHOUT ever attempting a 3rd dispatch.
    agent_round_then_reject(&d, &store, "g1", "same reason").await;
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human");
    assert_eq!(
        got.judge_feedback.as_deref(),
        Some("goal-loop no-progress oscillation"),
        "external judge_feedback text kept byte-identical to the pre-A2 guard"
    );
    // Exactly 2 work messages were ever enqueued (iter 1 and iter 2) — no
    // 3rd dispatch happened before escalation, matching the pre-A2 timing.
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        2,
        "no 3rd dispatch before the A2 guard fires"
    );

    let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
    assert!(
        acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
        "an oscillation activity must be recorded under the same event_type \
             topology_evolution.rs (D5) queries"
    );
}

#[tokio::test]
async fn differing_feedback_keeps_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // High cap on both tiers (the test goal classifies as Simple) so the
    // iteration guard never pre-empts the differing-feedback retry path.
    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100;
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), cfg);

    d.tick_once().await.unwrap();

    agent_round_then_reject(&d, &store, "g1", "first problem").await;
    d.tick_once().await.unwrap();

    // Second rejection has DIFFERENT feedback ⇒ NOT oscillation.
    agent_round_then_reject(&d, &store, "g1", "a completely different problem").await;
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_ne!(
        got.status, "needs_human",
        "differing feedback must keep retrying"
    );
    assert_eq!(got.status, "revising");
    let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
    assert!(
        !acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
        "no oscillation should be recorded for differing feedback"
    );
}

// ── H4: gap fingerprinting integrated into the A2 no-progress guard ──

/// The DoD's "same gap, reworded" case, end to end: two rejections that
/// cite the SAME `path:line` but with completely different prose must
/// now escalate exactly like literally-identical feedback would — this
/// was NOT true before H4 (each reworded rejection produced a distinct
/// `state_hash`, so the guard never fired).
#[tokio::test]
async fn reworded_feedback_citing_the_same_gap_escalates_oscillation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100;
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), cfg);
    d.tick_once().await.unwrap();

    agent_round_then_reject(
        &d,
        &store,
        "g1",
        "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120, please add a check.",
    )
    .await;
    d.tick_once().await.unwrap();
    assert_ne!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "needs_human",
        "first rejection must not escalate"
    );

    // Same underlying gap (same path:line), completely reworded prose.
    agent_round_then_reject(
        &d,
        &store,
        "g1",
        "You forgot proper error handling at crates/duduclaw-gateway/src/goal_loop.rs:120 — add validation.",
    )
    .await;
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(
        got.status, "needs_human",
        "reworded feedback citing the same gap must still escalate"
    );
    let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
    assert!(acts.iter().any(|a| a.event_type == "goal_loop.oscillation"));
}

/// Counterpart: two rejections citing DIFFERENT `path:line` gaps must
/// keep retrying, not escalate — the fingerprint must be sensitive to a
/// genuinely different gap, not just insensitive to rewording.
#[tokio::test]
async fn feedback_citing_different_gaps_keeps_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100;
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), cfg);
    d.tick_once().await.unwrap();

    agent_round_then_reject(
        &d,
        &store,
        "g1",
        "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120.",
    )
    .await;
    d.tick_once().await.unwrap();

    agent_round_then_reject(
        &d,
        &store,
        "g1",
        "Missing error handling in crates/duduclaw-gateway/src/goal_state.rs:42.",
    )
    .await;
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_ne!(
        got.status, "needs_human",
        "citations to different gaps must keep retrying"
    );
}
