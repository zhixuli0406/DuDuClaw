//! ET4: the three controls, honest stop reporting, and "cancelled stays
//! cancelled" across every path that can write a task's status.

use super::super::stop::{stop_status, stop_task};
use super::*;
use crate::approval::ApprovalBroker;
use crate::message_queue::{MessageStatus, QueueMessage};

fn queue_msg(id: &str, task_id: &str) -> QueueMessage {
    QueueMessage {
        id: id.into(),
        sender: "goal-loop-driver".into(),
        target: OWNER.into(),
        payload: format!("[goal-loop task_id={task_id} iter=1] work"),
        status: MessageStatus::Pending,
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
        upstream_unknown: false,
        lane: None,
    }
}

/// Operation rows bound to a task, written straight into the approval store
/// (the P0-B binding machinery is exercised by its own tests; here only the
/// stop report's reading of operation states matters).
fn insert_operation(home: &Path, task_id: &str, state: &str, lease_until: Option<i64>) {
    let conn = rusqlite::Connection::open(home.join("approvals.db")).unwrap();
    conn.execute(
        "INSERT INTO approval_operations(operation_id, run_id, step_key, approval_id, binding_json,
                payload_json, state, lease_owner, lease_until)
         VALUES (?1, ?2, 's', 'a', ?3, '{}', ?4, 'w', ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            serde_json::json!({"task_id": task_id}).to_string(),
            state,
            lease_until
        ],
    )
    .unwrap();
}

async fn child(env: &Env, id: &str, parent: &str, status: &str) {
    let mut t = TaskRow::new(
        id.into(),
        id.into(),
        "sub".into(),
        "medium".into(),
        OWNER.into(),
        "s".into(),
    );
    t.status = status.into();
    t.goal_mode = true;
    t.parent_task_id = Some(parent.into());
    env.store.insert_task(&t).await.unwrap();
}

/// ET4.2: an occurrence with two levels of sub-tasks, one of them mid-turn
/// on a provider that cannot be interrupted, a pending approval, a prepared
/// and an executing action. The stop cancels what it can at once and never
/// reports `stopped` while anything can still change; an executing action
/// whose lease is lost ends `stopped_uncertain`.
#[tokio::test]
async fn stop_reports_honestly_until_nothing_can_change() {
    let env = Env::new();
    let now = t0();
    let resp = create(&env, &input(now), now).await;
    arm_time(&env, &resp, now).await;
    let root = env.wake(3, now).await.created.remove(0);
    child(&env, "c1", &root, "todo").await;
    child(&env, "c2", "c1", "in_progress").await;
    env.queue
        .enqueue(&queue_msg("m-pending", "c1"))
        .await
        .unwrap();
    env.queue
        .enqueue(&queue_msg("m-running", "c2"))
        .await
        .unwrap();
    env.queue.ack("m-running").await.unwrap(); // the turn the dispatcher is awaiting
    let broker = ApprovalBroker::open(env.home()).unwrap();
    let pending = broker
        .request(
            OWNER,
            "goal_kickoff",
            "k",
            serde_json::json!({"task_id": "c1"}),
            600,
        )
        .await
        .unwrap();
    insert_operation(env.home(), "c1", "prepared", None);
    insert_operation(
        env.home(),
        "c2",
        "executing",
        Some(Utc::now().timestamp() - 10),
    );
    let before = task(&env, &root).await;

    let st = stop_task(
        &env.store,
        &env.queue,
        Some(&broker),
        None,
        env.home(),
        &root,
        before.authority_revision,
        "op",
        false,
        now,
    )
    .await
    .unwrap();
    assert_eq!(st.state, "cancel_pending");
    assert_eq!(st.affected_task_ids.len(), 3);
    assert_eq!(st.detail.running_turns, 1);
    assert_eq!(st.detail.operations_executing, 1);
    assert_eq!(st.detail.operations_prepared, 1);
    assert_eq!(st.detail.approvals_invalidated, 1);
    for id in [root.as_str(), "c1", "c2"] {
        assert_eq!(task(&env, id).await.status, "cancelled", "{id}");
    }
    assert!(task(&env, &root).await.authority_revision > before.authority_revision);
    assert_eq!(
        env.queue
            .get_by_id("m-pending")
            .await
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Failed
    );
    assert_eq!(
        broker.get(&pending).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Invalidated
    );
    // Still running ⇒ never "stopped".
    let again = stop_status(&env.store, &env.queue, Some(&broker), None, &root, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.state, "cancel_pending");
    // The executing action loses its lease ⇒ uncertain; the turn ends.
    assert_eq!(broker.recover_operations().await.unwrap(), 1);
    env.queue.complete("m-running", "late").await.unwrap();
    let fin = stop_status(&env.store, &env.queue, Some(&broker), None, &root, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fin.state, "stopped_uncertain");
    assert_eq!(fin.detail.operations_uncertain, 1);
    // The occurrence settles as stopped, which is not an employee failure.
    env.wake(3, now + Duration::minutes(1)).await;
    let occ = env
        .store
        .list_occurrences(&resp.responsibility_id)
        .await
        .unwrap();
    assert_eq!(occ[0].outcome.as_deref(), Some("stopped"));
    let r = env
        .store
        .get_responsibility(&resp.responsibility_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((r.state.as_str(), r.consecutive_failures), ("active", 0));
}

/// Without a readable approval store the stop cannot prove no action is
/// executing, so it stays `cancel_pending` (fail closed).
#[tokio::test]
async fn stop_without_approval_store_never_claims_stopped() {
    let env = Env::new();
    let t = goal_task(&env, "nb", "todo").await;
    let st = stop_task(
        &env.store,
        &env.queue,
        None,
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
    assert_eq!(st.state, "cancel_pending");
    assert!(st.detail.approval_store_unavailable);
}

/// Stop refusals: stale revision, finished task.
#[tokio::test]
async fn stop_cas_and_finished_refusals() {
    let env = Env::new();
    let t = goal_task(&env, "cas", "todo").await;
    let e = stop_task(
        &env.store,
        &env.queue,
        None,
        None,
        env.home(),
        &t.id,
        t.authority_revision + 7,
        "op",
        false,
        t0(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "conflict");
    let d = goal_task(&env, "fin", "done").await;
    let e = stop_task(
        &env.store,
        &env.queue,
        None,
        None,
        env.home(),
        &d.id,
        d.authority_revision,
        "op",
        false,
        t0(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "already_finished");
}

/// §10 hot spot 2: after a stop, no writer brings a task back.
#[tokio::test]
async fn cancelled_by_stop_is_never_revived() {
    let env = Env::new();
    let t = goal_task(&env, "rv", "pending").await;
    let mut claimed = TaskRow::new(
        "rv2".into(),
        "z".into(),
        "d".into(),
        "medium".into(),
        OWNER.into(),
        "s".into(),
    );
    claimed.goal_mode = true;
    claimed.status = "pending".into();
    claimed.parent_task_id = Some(t.id.clone());
    env.store.insert_task(&claimed).await.unwrap();
    let broker = ApprovalBroker::open(env.home()).unwrap();
    stop_task(
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
    let st = |id: &'static str| {
        let env = &env;
        async move { task(env, id).await.status }
    };
    // complete_task (incl. the team composer's background submit)
    let _ = env
        .store
        .complete_task("rv", "late result", "team-composer")
        .await;
    assert_eq!(st("rv").await, "cancelled");
    // both needs_human writers
    assert!(
        !env.store
            .mark_needs_human_with_pause("rv", "x", crate::pause_reason::PauseReason::Unknown)
            .await
            .unwrap()
    );
    assert!(
        !env.store
            .mark_needs_human_sealing_round(
                "rv",
                "x",
                crate::pause_reason::PauseReason::Infra,
                None
            )
            .await
            .unwrap()
    );
    assert_eq!(st("rv").await, "cancelled");
    // dashboard / MCP status edits
    assert!(
        env.store
            .update_task("rv", &serde_json::json!({"status": "pending"}))
            .await
            .is_err()
    );
    assert!(
        env.store
            .update_task("rv", &serde_json::json!({"status": "blocked"}))
            .await
            .is_err()
    );
    // judge rejection (needs `review`) and zombie requeue (needs `in_progress`)
    let _ = env
        .store
        .reject_review_with_ledger("rv", "no", 3, None, None)
        .await;
    assert_eq!(st("rv").await, "cancelled");
    assert!(
        env.store
            .reclaim_zombies(&(Utc::now() + Duration::days(1)).to_rfc3339())
            .await
            .unwrap()
            .is_empty()
    );
    // claim, retry, continue
    let soon = Utc::now().to_rfc3339();
    let lease = (Utc::now() + Duration::minutes(5)).to_rfc3339();
    assert!(
        !env.store
            .atomic_claim("rv", "alice", &soon, &lease)
            .await
            .unwrap()
            .is_claimed()
    );
    assert!(
        !env.store
            .resolve_needs_human("rv", "retry", "")
            .await
            .unwrap()
    );
    assert!(
        env.store
            .continue_from_terminal("rv", "again")
            .await
            .is_err()
    );
    assert_eq!(st("rv").await, "cancelled");
    assert_eq!(st("rv2").await, "cancelled");
    // no new sub-task under the stopped run
    let mut late = TaskRow::new(
        "rv3".into(),
        "n".into(),
        "d".into(),
        "medium".into(),
        OWNER.into(),
        "s".into(),
    );
    late.parent_task_id = Some("rv".into());
    assert!(env.store.insert_task(&late).await.is_err());
    // non-status edits of a stopped task are still allowed (e.g. pin)
    assert!(
        env.store
            .update_task("rv", &serde_json::json!({"pinned": true}))
            .await
            .is_ok()
    );
}

/// ET4.3: disable cancels every armed subscription and drops pending facts,
/// leaves the running occurrence alone, and re-enable never replays.
#[tokio::test]
async fn disable_cuts_the_future_and_enable_does_not_replay() {
    let env = Env::new();
    let now = t0();
    let mut inp = input(now);
    inp.event_subscriptions = vec![
        EventSubscription {
            event_name: "task.created".into(),
            filter: None,
            timeout_at: None,
        },
        EventSubscription {
            event_name: "task.updated".into(),
            filter: None,
            timeout_at: None,
        },
    ];
    let resp = create(&env, &inp, now).await;
    arm_time(&env, &resp, now).await;
    let open = env.wake(3, now).await.created.remove(0);
    arm_time(&env, &resp, now + Duration::minutes(10)).await;
    arm_time(&env, &resp, now + Duration::minutes(10)).await;
    env.wake(0, now + Duration::minutes(10)).await;
    assert_eq!(
        env.store
            .pending_fires(&resp.responsibility_id)
            .await
            .unwrap()
            .len(),
        1,
        "same instant ⇒ one fact"
    );
    let r = service::disable(&env.store, &resp.responsibility_id, 1, "op", "off", now)
        .await
        .unwrap();
    let crate::task_store::RespCas::Applied(r) = r else {
        panic!("disable refused")
    };
    assert_eq!((r.state.as_str(), r.control_epoch), ("disabled", 2));
    let ws = env
        .store
        .list_wakeups(&resp.responsibility_id)
        .await
        .unwrap();
    assert!(ws.iter().all(|w| w.state != "armed"), "{ws:?}");
    assert!(
        env.store
            .pending_fires(&resp.responsibility_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        task(&env, &open).await.status,
        "todo",
        "the running occurrence is untouched"
    );
    // A stale-epoch caller cannot re-enable.
    assert!(matches!(
        service::enable(&env.store, &resp.responsibility_id, 1, "op", now)
            .await
            .unwrap(),
        crate::task_store::RespCas::Conflict(_)
    ));
    let later = now + Duration::days(2);
    let r = service::enable(&env.store, &resp.responsibility_id, 2, "op", later)
        .await
        .unwrap();
    let crate::task_store::RespCas::Applied(r) = r else {
        panic!("enable refused")
    };
    assert_eq!(r.control_epoch, 3);
    let armed: Vec<_> = env
        .store
        .list_wakeups(&resp.responsibility_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|w| w.state == "armed")
        .collect();
    assert_eq!(
        armed.len(),
        3,
        "schedule + two event subscriptions, all on the new epoch"
    );
    let time = armed.iter().find(|w| w.kind == "time").unwrap();
    assert!(
        time.due_at.as_deref().unwrap() > crate::task_store::resp_ts(later).as_str(),
        "next slot only"
    );
    env.store
        .update_task(&open, &serde_json::json!({"status": "done"}))
        .await
        .unwrap();
    let rep = env.wake(3, later).await;
    assert!(rep.created.is_empty(), "missed slots are not replayed");
}
