//! C5: the driver's durable path — fixed message ids, persistent iteration
//! count, restart repair, paused freeze, single-occurrence cost cap — and
//! the guarantee that every other goal task is dispatched exactly as before.

use super::*;
use crate::responsibility::test_hooks;
use crate::task_store::IntentBegin;

async fn one_occurrence(env: &Env) -> (ResponsibilityRow, String) {
    let now = t0();
    let resp = create(env, &input(now), now).await;
    arm_time(env, &resp, now).await;
    let created = env.wake(3, now).await.created;
    assert_eq!(created.len(), 1);
    (resp, created[0].clone())
}

/// Simulate the agent finishing a round that the judge rejected.
async fn round_rejected(env: &Env, task_id: &str) {
    for m in env.queue.goal_messages_for_task(task_id).await.unwrap() {
        // What the dispatcher does before starting a durable round (M3-1).
        env.store.mark_round_started(&m.id, t0()).await.unwrap();
        env.queue.complete(&m.id, "ok").await.unwrap();
    }
    env.store
        .update_task(task_id, &serde_json::json!({"status": "revising"}))
        .await
        .unwrap();
}

/// A plain goal task (no responsibility, no steering) keeps the random
/// message id rail and writes nothing durable.
#[tokio::test]
async fn ordinary_goal_tasks_are_dispatched_unchanged() {
    let env = Env::new();
    goal_task(&env, "plain", "todo").await;
    env.driver().tick_at(t0()).await.unwrap();
    let msgs = env.queue.goal_messages_for_task("plain").await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(
        uuid::Uuid::parse_str(&msgs[0].id).is_ok(),
        "random id: {}",
        msgs[0].id
    );
    assert!(
        msgs[0]
            .payload
            .starts_with("[goal-loop task_id=plain iter=1]")
    );
    let info = env.store.durable_info("plain").await.unwrap();
    assert_eq!(info.persisted_iters, 0);
    assert!(!info.is_durable());
}

/// ET2.5 (iteration half): two rounds, then a rebuilt driver — the third
/// round is iter 3, not 1; at the cap the task goes to a human.
#[tokio::test]
async fn iteration_count_survives_driver_rebuild() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    // Round 3 (E-M1): an occurrence whose spend cannot be measured counts as
    // its full reservation and gets no second round, so this case measures
    // a small spend.
    env.cost.set(&tid, 1);
    let now = t0();
    env.driver().tick_at(now).await.unwrap();
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:1"))
            .await
            .unwrap()
            .is_some()
    );
    round_rejected(&env, &tid).await;
    env.driver()
        .tick_at(now + Duration::seconds(30))
        .await
        .unwrap();
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:2"))
            .await
            .unwrap()
            .is_some()
    );
    round_rejected(&env, &tid).await;
    let mut cfg = loop_cfg();
    cfg.iteration_cap = 3;
    cfg.iteration_cap_simple = 3;
    let rebuilt = GoalLoopDriver::new(env.second_store(), Arc::clone(&env.queue), cfg)
        .with_home_dir(env.home().to_path_buf())
        .with_cost_source(env.cost.clone());
    rebuilt.tick_at(now + Duration::seconds(60)).await.unwrap();
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:3"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:1"))
            .await
            .unwrap()
            .is_some()
    );
    round_rejected(&env, &tid).await;
    // Past `stalled_secs`, so the tracked round is re-evaluated.
    rebuilt.tick_at(now + Duration::minutes(15)).await.unwrap();
    assert_eq!(task(&env, &tid).await.status, "needs_human");
    assert_eq!(
        env.queue.goal_messages_for_task(&tid).await.unwrap().len(),
        3
    );
}

/// ET2.5 (deadline half): sleeping does not move the deadline; a human retry
/// does not either — the next tick parks it again.
#[tokio::test]
async fn deadline_is_absolute_across_retry() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    let driver = env.driver();
    let late = t0() + Duration::hours(5); // occurrence_hours = 4
    driver.tick_at(late).await.unwrap();
    assert_eq!(task(&env, &tid).await.status, "needs_human");
    assert!(
        env.store
            .resolve_needs_human(&tid, "retry", "")
            .await
            .unwrap()
    );
    driver.tick_at(late + Duration::seconds(30)).await.unwrap();
    assert_eq!(task(&env, &tid).await.status, "needs_human");
}

/// ET2.6: measured spend at the cap ⇒ no round, budget-exhausted escalation.
/// Unreadable cost ⇒ no round this tick, no escalation.
#[tokio::test]
async fn occurrence_cost_cap_blocks_the_next_round() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    env.cost.set_fail(true);
    env.driver().tick_at(t0()).await.unwrap();
    assert!(
        env.queue
            .goal_messages_for_task(&tid)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(task(&env, &tid).await.status, "todo");
    env.cost.set_fail(false);
    env.cost.set(&tid, 100); // cap = 100
    env.driver().tick_at(t0()).await.unwrap();
    assert!(
        env.queue
            .goal_messages_for_task(&tid)
            .await
            .unwrap()
            .is_empty()
    );
    let t = task(&env, &tid).await;
    assert_eq!(t.status, "needs_human");
    assert_eq!(t.pause_reason.as_deref(), Some("budget_exhausted"));
}

/// ET4.1: pausing coordination freezes the occurrence's next round; facts
/// keep accumulating; resuming dispatches it.
#[tokio::test]
async fn pause_freezes_next_round_and_resume_continues() {
    let env = Env::new();
    let (resp, tid) = one_occurrence(&env).await;
    env.store
        .update_task(&tid, &serde_json::json!({"status": "revising"}))
        .await
        .unwrap();
    let paused = service::pause(&env.store, &resp.responsibility_id, 1, "op", "hold", t0())
        .await
        .unwrap();
    assert!(matches!(paused, crate::task_store::RespCas::Applied(_)));
    arm_time(&env, &resp, t0() + Duration::minutes(10)).await;
    let driver = env.driver();
    for i in 0..10 {
        driver
            .tick_at(t0() + Duration::minutes(10) + Duration::seconds(30 * i))
            .await
            .unwrap();
    }
    assert!(
        env.queue
            .goal_messages_for_task(&tid)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.store
            .pending_fires(&resp.responsibility_id)
            .await
            .unwrap()
            .len(),
        1
    );
    service::resume(&env.store, &resp.responsibility_id, 1, "op", t0())
        .await
        .unwrap();
    driver.tick_at(t0() + Duration::minutes(20)).await.unwrap();
    assert_eq!(
        env.queue.goal_messages_for_task(&tid).await.unwrap().len(),
        1
    );
    // The new fact waits for this occurrence to finish (one open at a time).
    assert_eq!(
        env.store
            .pending_fires(&resp.responsibility_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// ET1.5 (second half): the driver died after the intent committed and
/// before the enqueue. The next driver resends the same id exactly once and
/// steering is applied exactly once.
#[tokio::test]
async fn intent_left_intended_is_resent_once_with_the_same_id() {
    let env = Env::new();
    let t = goal_task(&env, "g-int", "todo").await;
    super::super::steering::submit(
        &env.store,
        env.home(),
        &t.id,
        "先處理 A 再處理 B",
        "op",
        "req-1",
        t0(),
    )
    .await
    .unwrap();
    let gate = test_hooks::install("intent_committed", &t.id);
    let driver = Arc::new(env.driver());
    let d2 = Arc::clone(&driver);
    let run = tokio::spawn(async move { d2.tick_at(t0()).await });
    gate.arrived.wait().await;
    run.abort(); // crash between step 1 and step 2
    let _ = run.await;
    let steer = env.store.list_steering(&t.id).await.unwrap();
    assert_eq!(steer[0].state, "delivering");
    assert!(
        env.queue
            .goal_messages_for_task(&t.id)
            .await
            .unwrap()
            .is_empty()
    );

    let fresh = env.driver_on(env.second_store());
    fresh.reconcile_durable_dispatch().await.unwrap();
    fresh.tick_at(t0() + Duration::seconds(30)).await.unwrap();
    fresh.tick_at(t0() + Duration::seconds(60)).await.unwrap();
    let msgs = env.queue.goal_messages_for_task(&t.id).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].id, "goal:g-int:1");
    assert!(msgs[0].payload.contains("<operator_direction seq=\"1\""));
    let steer = env.store.list_steering(&t.id).await.unwrap();
    assert_eq!(steer[0].state, "applied");
    assert_eq!(steer[0].applied_round, Some(1));
    assert_eq!(steer[0].applied_message_id.as_deref(), Some("goal:g-int:1"));
}

/// ET1.5 (third case): the message reached the queue but the intent was
/// never marked — the restart repair marks it, and nothing is sent twice
/// (the round is tracked as waiting for pickup).
#[tokio::test]
async fn enqueued_but_unmarked_intent_is_repaired_not_resent() {
    let env = Env::new();
    let t = goal_task(&env, "g-mark", "todo").await;
    super::super::steering::submit(
        &env.store,
        env.home(),
        &t.id,
        "注意格式",
        "op",
        "req-1",
        t0(),
    )
    .await
    .unwrap();
    let IntentBegin::Ready { intent, .. } = env
        .store
        .begin_dispatch_intent(&t.id, 1, t0())
        .await
        .unwrap()
    else {
        panic!("intent refused");
    };
    let msg = crate::message_queue::QueueMessage {
        id: intent.intent_id.clone(),
        sender: "goal-loop-driver".into(),
        target: OWNER.into(),
        payload: format!("[goal-loop task_id={} iter=1] work", t.id),
        status: crate::message_queue::MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 0,
        origin_agent: None,
        sender_agent: None,
        error: None,
        response: None,
        created_at: Utc::now().to_rfc3339(),
        acked_at: None,
        completed_at: None,
        reply_channel: None,
        turn_id: None,
        session_id: None,
    };
    env.queue.enqueue(&msg).await.unwrap();
    let fresh = env.driver();
    assert!(fresh.reconcile_durable_dispatch().await.unwrap() >= 1);
    let steer = env.store.list_steering(&t.id).await.unwrap();
    assert_eq!(steer[0].state, "applied");
    assert_eq!(
        fresh.inflight_len().await,
        1,
        "tracked as waiting for pickup"
    );
    fresh.tick_at(t0() + Duration::seconds(30)).await.unwrap();
    assert_eq!(
        env.queue.goal_messages_for_task(&t.id).await.unwrap().len(),
        1
    );
    // Even a stall re-dispatch never duplicates a pending fixed-id round.
    fresh.tick_at(t0() + Duration::hours(1)).await.unwrap();
    assert_eq!(
        env.queue.goal_messages_for_task(&t.id).await.unwrap().len(),
        1
    );
}

/// Round 3 E-M1: a round ran but nothing was measured ⇒ the spend counts as
/// the full reservation, so no further round is dispatched.
#[tokio::test]
async fn unmeasured_spend_counts_as_the_reservation() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    env.driver().tick_at(t0()).await.unwrap();
    round_rejected(&env, &tid).await;
    env.driver()
        .tick_at(t0() + Duration::seconds(30))
        .await
        .unwrap();
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:2"))
            .await
            .unwrap()
            .is_none()
    );
    let t = task(&env, &tid).await;
    assert_eq!(t.status, "needs_human");
    assert_eq!(t.pause_reason.as_deref(), Some("budget_exhausted"));
}

/// Round 4 (必查): a runtime that reported no token counts writes a usage row
/// with zero tokens. That row is not a measurement, so the round is still
/// charged its reservation instead of costing nothing.
#[tokio::test]
async fn a_usage_row_without_token_counts_counts_as_the_reservation() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    env.driver().tick_at(t0()).await.unwrap();
    round_rejected(&env, &tid).await;
    env.cost.set_unmeasured(&tid);
    env.driver()
        .tick_at(t0() + Duration::seconds(30))
        .await
        .unwrap();
    let t = task(&env, &tid).await;
    assert_eq!(t.status, "needs_human");
    assert_eq!(t.pause_reason.as_deref(), Some("budget_exhausted"));
}
