//! P5 lane coverage (2026-10-08 close-out): the explore lane of a
//! responsibility covers its occurrence's whole task tree, including work
//! the heartbeat wakes later for a sub-task.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::*;
use crate::message_queue::{MessageStatus, QueueMessage};
use crate::model_call_probe;
use duduclaw_agent::registry::AgentRegistry;

async fn occurrence_with_lane(env: &Env, lane: Option<&str>) -> String {
    let now = t0();
    let mut i = input(now);
    i.lane = lane.map(str::to_string);
    let resp = create(env, &i, now).await;
    arm_time(env, &resp, now).await;
    env.wake(3, now).await.created.remove(0)
}

async fn child(env: &Env, id: &str, parent: &str) {
    let mut t = TaskRow::new(
        id.into(),
        format!("sub {id}"),
        "d".into(),
        "medium".into(),
        OWNER.into(),
        OWNER.into(),
    );
    t.parent_task_id = Some(parent.into());
    env.store.insert_task(&t).await.unwrap();
}

fn heartbeat_message(id: &str, task_id: &str) -> QueueMessage {
    QueueMessage {
        id: id.into(),
        sender: crate::responsibility::HEARTBEAT_SENDER.into(),
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
        upstream_unknown: false,
        lane: None,
    }
}

#[tokio::test]
async fn the_explore_lane_covers_the_whole_occurrence_tree() {
    let env = Env::new();
    let occ = occurrence_with_lane(&env, Some("explore")).await;
    child(&env, "sub-1", &occ).await;
    child(&env, "sub-2", "sub-1").await;
    for id in [occ.as_str(), "sub-1", "sub-2"] {
        assert_eq!(
            crate::responsibility::round_requires_explore_lane(env.home(), id).await,
            Ok(true),
            "{id}"
        );
    }
    goal_task(&env, "unrelated", "todo").await;
    assert_eq!(
        crate::responsibility::round_requires_explore_lane(env.home(), "unrelated").await,
        Ok(false)
    );
}

#[tokio::test]
async fn a_normal_lane_tree_stays_normal_and_an_unreadable_lane_refuses() {
    let env = Env::new();
    let occ = occurrence_with_lane(&env, None).await;
    child(&env, "sub-n", &occ).await;
    assert_eq!(
        crate::responsibility::round_requires_explore_lane(env.home(), "sub-n").await,
        Ok(false)
    );
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE responsibilities SET scope_json = '{\"event_names\":[],\"lane\":\"other\"}'",
        [],
    )
    .unwrap();
    assert!(
        crate::responsibility::round_requires_explore_lane(env.home(), "sub-n")
            .await
            .is_err()
    );
}

/// A heartbeat wake-up for a sub-task of an explore-lane run goes through
/// the same pre-dispatch refusal as the occurrence's own rounds: an
/// employee that cannot carry the lane (here: task-sandboxed) never starts.
#[tokio::test]
async fn a_heartbeat_wake_up_under_an_explore_lane_run_gets_the_lane_refusal() {
    let env = Env::new();
    let occ = occurrence_with_lane(&env, Some("explore")).await;
    child(&env, "sub-hb", &occ).await;
    std::fs::write(
        env.home().join("agents").join(OWNER).join("agent.toml"),
        "[agent]\nname = \"alice\"\n\n[container]\nsandbox_enabled = true\n",
    )
    .unwrap();
    env.queue
        .enqueue(&heartbeat_message("m-lane", "sub-hb"))
        .await
        .unwrap();
    let registry = Arc::new(RwLock::new(AgentRegistry::new(env.home().join("agents"))));
    model_call_probe::set_dry_run(true);
    crate::dispatcher::poll_and_dispatch_sqlite(&env.queue, env.home(), &registry, None, None)
        .await
        .unwrap();
    model_call_probe::set_dry_run(false);
    let m = env.queue.get_by_id("m-lane").await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed, "{m:?}");
    assert!(
        m.error
            .as_deref()
            .is_some_and(|e| e.starts_with("explore_lane_unsupported")),
        "{m:?}"
    );
}
