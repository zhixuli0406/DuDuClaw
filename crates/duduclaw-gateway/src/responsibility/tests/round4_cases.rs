//! Round 4 (appendix E): the second review's findings, each pinned by the
//! behaviour it changed.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::super::stop::{stop_status, stop_task};
use super::*;
use crate::approval::ApprovalBroker;
use crate::message_queue::{MessageStatus, QueueMessage};
use crate::model_call_probe;
use duduclaw_agent::registry::AgentRegistry;

async fn one_occurrence(env: &Env) -> (ResponsibilityRow, String) {
    let now = t0();
    let resp = create(env, &input(now), now).await;
    arm_time(env, &resp, now).await;
    let created = env.wake(3, now).await.created;
    assert_eq!(created.len(), 1);
    (resp, created[0].clone())
}

/// The real dispatcher, with the agent turn as the probe's dry run.
async fn poll(env: &Env) {
    let registry = Arc::new(RwLock::new(AgentRegistry::new(env.home().join("agents"))));
    model_call_probe::set_dry_run(true);
    crate::dispatcher::poll_and_dispatch_sqlite(&env.queue, env.home(), &registry, None, None)
        .await
        .unwrap();
    model_call_probe::set_dry_run(false);
}

fn sql(env: &Env, stmt: &str, args: &[&dyn rusqlite::ToSql]) {
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(stmt, args).unwrap();
}

fn assert_not_budget_escalated(t: &TaskRow) {
    assert_ne!(t.status, "needs_human", "{t:?}");
    assert_ne!(t.pause_reason.as_deref(), Some("budget_exhausted"), "{t:?}");
}

// ── H-1: a round that never ran costs nothing ───────────────────────────

/// (A) pause while round 1 waits in the queue, the dispatcher fences it,
/// resume: no "single-run cap reached", and the next round goes out.
#[tokio::test]
async fn pause_then_resume_does_not_count_a_fenced_round() {
    let env = Env::new();
    let (resp, tid) = one_occurrence(&env).await;
    let driver = env.driver();
    driver.tick_at(t0()).await.unwrap();
    let first = format!("goal:{tid}:1");
    assert!(env.queue.get_by_id(&first).await.unwrap().is_some());
    service::pause(&env.store, &resp.responsibility_id, 1, "op", "hold", t0())
        .await
        .unwrap();
    poll(&env).await;
    let m = env.queue.get_by_id(&first).await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        env.store
            .enqueued_intent_ids(&tid)
            .await
            .unwrap()
            .is_empty(),
        "abandoned"
    );
    service::resume(&env.store, &resp.responsibility_id, 1, "op", t0())
        .await
        .unwrap();
    driver.tick_at(t0() + Duration::minutes(1)).await.unwrap();
    assert_not_budget_escalated(&task(&env, &tid).await);
    assert!(
        env.queue
            .get_by_id(&format!("goal:{tid}:2"))
            .await
            .unwrap()
            .is_some(),
        "the round goes out again"
    );
}

/// (B) the round waits in a busy queue past the stall timeout: no
/// escalation, no second copy.
#[tokio::test]
async fn a_round_stuck_in_the_queue_costs_nothing() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    let driver = env.driver();
    driver.tick_at(t0()).await.unwrap();
    driver.tick_at(t0() + Duration::minutes(20)).await.unwrap();
    assert_not_budget_escalated(&task(&env, &tid).await);
    assert_eq!(
        env.queue.goal_messages_for_task(&tid).await.unwrap().len(),
        1
    );
}

/// (C) a restarted driver re-registers the still-waiting round.
#[tokio::test]
async fn a_restart_with_the_round_still_queued_costs_nothing() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    env.driver().tick_at(t0()).await.unwrap();
    let restarted = env.driver();
    restarted
        .tick_at(t0() + Duration::minutes(20))
        .await
        .unwrap();
    restarted
        .tick_at(t0() + Duration::minutes(21))
        .await
        .unwrap();
    assert_not_budget_escalated(&task(&env, &tid).await);
}

/// (D) round 1 fails before any agent ran it.
#[tokio::test]
async fn a_round_that_failed_before_running_costs_nothing() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    let driver = env.driver();
    driver.tick_at(t0()).await.unwrap();
    let first = format!("goal:{tid}:1");
    env.queue.fail(&first, "delegation denied").await.unwrap();
    driver.tick_at(t0() + Duration::minutes(1)).await.unwrap();
    assert_not_budget_escalated(&task(&env, &tid).await);
    assert!(
        !env.store
            .enqueued_intent_ids(&tid)
            .await
            .unwrap()
            .contains(&first),
        "the unrun round's intent is abandoned"
    );
}

/// M3-1 (E): the dispatcher started the round (durable mark), the runtime
/// then failed before the employee claimed the task. The round ran: it is
/// not abandoned, and with nothing measured it counts as the reservation.
#[tokio::test]
async fn a_started_round_that_failed_before_a_claim_counts() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    let driver = env.driver();
    driver.tick_at(t0()).await.unwrap();
    let first = format!("goal:{tid}:1");
    env.store.mark_round_started(&first, t0()).await.unwrap();
    env.queue.ack(&first).await.unwrap();
    env.queue.fail(&first, "runtime error").await.unwrap();
    driver.tick_at(t0() + Duration::minutes(1)).await.unwrap();
    assert!(
        env.store
            .enqueued_intent_ids(&tid)
            .await
            .unwrap()
            .contains(&first),
        "a started round is not abandoned"
    );
    // The cost side reads the same durable mark (see the crash case below
    // for the escalation it leads to).
    assert!(env.store.any_round_started(&tid).await.unwrap());
}

/// M3-1 (F): the gateway crashed mid-round and the message was reset to
/// pending. The start mark survives the reset, so the round still counts.
#[tokio::test]
async fn a_started_round_reset_after_a_crash_still_counts() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    env.driver().tick_at(t0()).await.unwrap();
    let first = format!("goal:{tid}:1");
    env.store.mark_round_started(&first, t0()).await.unwrap();
    // Still pending in the queue (reset after the crash), driver rebuilt.
    let restarted = env.driver();
    restarted
        .tick_at(t0() + Duration::minutes(20))
        .await
        .unwrap();
    restarted
        .tick_at(t0() + Duration::minutes(21))
        .await
        .unwrap();
    let t = task(&env, &tid).await;
    assert_eq!(t.status, "needs_human", "{t:?}");
    assert_eq!(t.pause_reason.as_deref(), Some("budget_exhausted"));
}

// ── M-1: the occurrence gate does not hide the deadline ─────────────────

#[tokio::test]
async fn a_paused_occurrence_still_reaches_a_human_at_its_deadline() {
    let env = Env::new();
    let (resp, tid) = one_occurrence(&env).await;
    service::pause(&env.store, &resp.responsibility_id, 1, "op", "hold", t0())
        .await
        .unwrap();
    env.driver()
        .tick_at(t0() + Duration::hours(5))
        .await
        .unwrap();
    assert_eq!(task(&env, &tid).await.status, "needs_human");
}

#[tokio::test]
async fn an_occurrence_of_an_expired_responsibility_reaches_a_human() {
    let env = Env::new();
    let (resp, tid) = one_occurrence(&env).await;
    sql(
        &env,
        "UPDATE tasks SET status='revising' WHERE id = ?1",
        &[&tid],
    );
    sql(
        &env,
        "UPDATE responsibilities SET state='expired' WHERE responsibility_id = ?1",
        &[&resp.responsibility_id],
    );
    env.driver()
        .tick_at(t0() + Duration::hours(5))
        .await
        .unwrap();
    assert_eq!(task(&env, &tid).await.status, "needs_human");
}

#[tokio::test]
async fn a_hand_off_leaves_occurrences_with_their_owner() {
    let env = Env::new();
    let (_resp, tid) = one_occurrence(&env).await;
    let plain = goal_task(&env, "plain-work", "todo").await;
    env.store
        .reassign_open_tasks(OWNER, "bob", &crate::task_store::resp_ts(t0()))
        .await
        .unwrap();
    assert_eq!(task(&env, &tid).await.assigned_to, OWNER);
    assert_eq!(task(&env, &plain.id).await.assigned_to, "bob");
}

// ── M-2: the store refuses claims and completions inside a stopped tree ─

#[tokio::test]
async fn a_stopped_tree_member_cannot_be_claimed_or_completed() {
    let env = Env::new();
    let root = goal_task(&env, "st-root", "todo").await;
    let mut c = TaskRow::new(
        "st-child".into(),
        "c".into(),
        "d".into(),
        "medium".into(),
        OWNER.into(),
        OWNER.into(),
    );
    c.parent_task_id = Some(root.id.clone());
    env.store.insert_task(&c).await.unwrap();
    env.store
        .stop_task_tree(&root.id, root.authority_revision, "op", false, t0())
        .await
        .unwrap();
    // A member the batch cancel has not reached yet.
    sql(
        &env,
        "UPDATE tasks SET status='pending' WHERE id = 'st-child'",
        &[],
    );
    let lease = crate::task_store::resp_ts(t0() + Duration::minutes(5));
    let out = env
        .store
        .atomic_claim("st-child", OWNER, &crate::task_store::resp_ts(t0()), &lease)
        .await
        .unwrap();
    assert!(
        matches!(out, crate::task_store::ClaimOutcome::NotClaimable),
        "{out:?}"
    );
    assert!(
        env.store
            .complete_task("st-child", "done", OWNER)
            .await
            .is_err()
    );
}

fn heartbeat_message(id: &str, task_id: &str) -> QueueMessage {
    QueueMessage {
        id: id.into(),
        sender: "heartbeat-scheduler".into(),
        target: OWNER.into(),
        payload: format!("[heartbeat-pull task_id={task_id}] 任務看板有一筆待辦"),
        status: MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 0,
        origin_agent: Some("heartbeat".into()),
        sender_agent: Some("heartbeat".into()),
        error: None,
        response: None,
        created_at: Utc::now().to_rfc3339(),
        acked_at: None,
        completed_at: None,
        reply_channel: None,
        turn_id: None,
        session_id: None,
    }
}

/// The stop reconciliation sees (and fails) a waiting heartbeat wake-up.
#[tokio::test]
async fn a_waiting_heartbeat_wake_up_is_failed_by_the_stop() {
    let env = Env::new();
    let t = goal_task(&env, "hb", "todo").await;
    env.queue
        .enqueue(&heartbeat_message("m-hb", &t.id))
        .await
        .unwrap();
    let broker = ApprovalBroker::open(env.home()).unwrap();
    let st = stop_task(
        &env.store,
        &env.queue,
        Some(&broker),
        None,
        env.home(),
        &t.id,
        t.authority_revision,
        "op",
        false,
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(st.detail.messages_failed, 1, "{st:?}");
    assert_eq!(
        env.queue.get_by_id("m-hb").await.unwrap().unwrap().status,
        MessageStatus::Failed
    );
}

/// A heartbeat wake-up of a stopped task that reaches the dispatcher anyway
/// is fenced before any turn.
#[tokio::test]
async fn the_dispatcher_fences_a_heartbeat_wake_up_of_a_stopped_task() {
    let env = Env::new();
    let t = goal_task(&env, "hb2", "todo").await;
    env.store
        .stop_task_tree(&t.id, t.authority_revision, "op", false, t0())
        .await
        .unwrap();
    env.queue
        .enqueue(&heartbeat_message("m-hb2", &t.id))
        .await
        .unwrap();
    let before = model_call_probe::calls();
    poll(&env).await;
    assert_eq!(model_call_probe::calls(), before);
    let m = env.queue.get_by_id("m-hb2").await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        m.error
            .unwrap()
            .starts_with(crate::responsibility::FENCE_ERROR_PREFIX)
    );
}

// ── M-3: no reminder for a responsibility question ──────────────────────

#[tokio::test]
async fn a_question_gets_no_reminder_push() {
    let env = Env::new();
    let broker = ApprovalBroker::open(env.home()).unwrap();
    let q = broker
        .request(
            OWNER,
            crate::responsibility::DECISION_KIND,
            "q",
            serde_json::json!({}),
            3600,
        )
        .await
        .unwrap();
    let control = broker
        .request(OWNER, "mcp_call", "c", serde_json::json!({}), 3600)
        .await
        .unwrap();
    // Both are past two-thirds of their TTL.
    let conn = rusqlite::Connection::open(env.home().join("approvals.db")).unwrap();
    let old = (Utc::now() - Duration::minutes(50)).to_rfc3339();
    conn.execute("UPDATE approvals SET created_at = ?1", [old])
        .unwrap();
    broker.poll(&q).await.unwrap();
    broker.poll(&control).await.unwrap();
    assert!(broker.get(&q).await.unwrap().unwrap().reminded_at.is_none());
    assert!(
        broker
            .get(&control)
            .await
            .unwrap()
            .unwrap()
            .reminded_at
            .is_some(),
        "the generic reminder still runs for other kinds"
    );
}

// ── M-5: a large stop reconciles with one queue query ───────────────────

#[tokio::test]
async fn reconciling_a_large_tree_with_a_busy_queue_is_bounded() {
    let env = Env::new();
    let root = goal_task(&env, "huge", "todo").await;
    for i in 0..1500 {
        let mut c = TaskRow::new(
            format!("huge-{i}"),
            "c".into(),
            "d".into(),
            "medium".into(),
            OWNER.into(),
            OWNER.into(),
        );
        c.parent_task_id = Some(root.id.clone());
        env.store.insert_task(&c).await.unwrap();
    }
    for i in 0..1500 {
        let mut m = heartbeat_message(&format!("other-{i}"), &format!("unrelated-{i}"));
        m.target = "bob".into();
        env.queue.enqueue(&m).await.unwrap();
    }
    let broker = ApprovalBroker::open(env.home()).unwrap();
    let started = std::time::Instant::now();
    let st = stop_task(
        &env.store,
        &env.queue,
        Some(&broker),
        None,
        env.home(),
        &root.id,
        root.authority_revision,
        "op",
        false,
        t0(),
    )
    .await
    .unwrap();
    // Members are cancelled in batches of STOP_TREE_LIMIT per pass.
    let mut again = st.clone();
    let mut passes = 1;
    while again.state == "cancel_pending" && passes < 6 {
        again = stop_status(&env.store, &env.queue, Some(&broker), None, &root.id, t0())
            .await
            .unwrap()
            .unwrap();
        passes += 1;
    }
    let elapsed = started.elapsed();
    eprintln!("M-5: stop over 1501 members / 1500 queued: {passes} passes, {elapsed:?}");
    assert_eq!(st.detail.messages_failed, 0, "unrelated messages untouched");
    assert_eq!(again.state, "stopped", "{again:?}");
    assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");
}
