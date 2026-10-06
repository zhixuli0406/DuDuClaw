//! P2-A ET1.4: stop vs. dispatch at the three fixed points. The dispatcher
//! runs for real (`poll_and_dispatch_sqlite`); the agent turn itself is the
//! test probe's dry run, so "spawned" is counted without spawning anything.

use super::*;
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use crate::model_call_probe;
use crate::responsibility::test_hooks;
use crate::task_store::{TaskRow, TaskStore};

struct Rig {
    dir: tempfile::TempDir,
    store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    registry: Arc<RwLock<AgentRegistry>>,
}

impl Rig {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\nenabled = true\n\n[goal_loop]\nsteering_enabled = true\n",
        )
        .unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let queue = Arc::new(MessageQueue::open(dir.path()).unwrap());
        let registry = Arc::new(RwLock::new(AgentRegistry::new(dir.path().join("agents"))));
        Self {
            dir,
            store,
            queue,
            registry,
        }
    }

    async fn steered_task(&self, id: &str) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            id.into(),
            "work".into(),
            "medium".into(),
            "alice".into(),
            "s".into(),
        );
        t.status = "todo".into();
        t.goal_mode = true;
        self.store.insert_task(&t).await.unwrap();
        crate::responsibility::steering::submit(
            &self.store,
            self.dir.path(),
            id,
            "先做 A",
            "op",
            "c-1",
            Utc::now(),
        )
        .await
        .unwrap();
        self.store.get_task(id).await.unwrap().unwrap()
    }

    fn driver(&self) -> Arc<crate::goal_loop::GoalLoopDriver> {
        Arc::new(
            crate::goal_loop::GoalLoopDriver::new(
                Arc::clone(&self.store),
                Arc::clone(&self.queue),
                crate::goal_loop::GoalLoopConfig::default(),
            )
            .with_home_dir(self.dir.path().to_path_buf()),
        )
    }

    async fn stop(&self, id: &str) -> crate::responsibility::stop::StopStatus {
        let rev = self
            .store
            .get_task(id)
            .await
            .unwrap()
            .unwrap()
            .authority_revision;
        let broker = crate::approval::ApprovalBroker::open(self.dir.path()).unwrap();
        crate::responsibility::stop::stop_task(
            &self.store,
            &self.queue,
            Some(&broker),
            None,
            self.dir.path(),
            id,
            rev,
            "op",
            false,
            Utc::now(),
        )
        .await
        .unwrap()
    }

    async fn poll(&self) {
        poll_and_dispatch_sqlite(&self.queue, self.dir.path(), &self.registry, None, None)
            .await
            .unwrap();
    }
}

/// Stop at point (a) (intent committed) or (b) (just before the enqueue):
/// the dispatcher never spawns the round.
async fn stop_before_enqueue(point: &str) {
    let rig = Rig::new().await;
    let t = rig.steered_task("fa").await;
    let gate = test_hooks::install(point, &t.id);
    let driver = rig.driver();
    let d = Arc::clone(&driver);
    let tick = tokio::spawn(async move { d.tick_once().await });
    gate.arrived.wait().await;
    rig.stop(&t.id).await;
    gate.release.wait().await;
    tick.await.unwrap().unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::calls(),
        before,
        "{point}: nothing spawned"
    );
    for m in rig.queue.goal_messages_for_task(&t.id).await.unwrap() {
        assert_eq!(m.status, MessageStatus::Failed, "{point}: {m:?}");
    }
    assert_eq!(
        rig.store.get_task(&t.id).await.unwrap().unwrap().status,
        "cancelled"
    );
}

#[tokio::test]
async fn stop_after_intent_commit_spawns_nothing() {
    stop_before_enqueue("intent_committed").await;
}

#[tokio::test]
async fn stop_before_enqueue_spawns_nothing() {
    stop_before_enqueue("before_enqueue").await;
}

/// The queue row is still `pending` when the stop lands (no reconcile ran):
/// the fence alone refuses it.
#[tokio::test]
async fn fence_alone_refuses_a_stopped_round() {
    let rig = Rig::new().await;
    let t = rig.steered_task("fb").await;
    rig.driver().tick_once().await.unwrap();
    let id = format!("goal:{}:1", t.id);
    assert_eq!(
        rig.queue.get_by_id(&id).await.unwrap().unwrap().status,
        MessageStatus::Pending
    );
    rig.store
        .stop_task_tree(&t.id, t.authority_revision, "op", false, Utc::now())
        .await
        .unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before);
    let m = rig.queue.get_by_id(&id).await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        m.error
            .unwrap()
            .starts_with(crate::responsibility::FENCE_ERROR_PREFIX)
    );
}

/// Stop at point (c) (fence passed, turn about to run): one turn runs —
/// the stop says `cancel_pending` meanwhile, `stopped` after the turn ends,
/// and the late `tasks_complete` cannot revive the task.
#[tokio::test]
async fn stop_after_fence_waits_for_the_running_turn() {
    let rig = Rig::new().await;
    let t = rig.steered_task("fc").await;
    rig.driver().tick_once().await.unwrap();
    let id = format!("goal:{}:1", t.id);
    let gate = test_hooks::install("after_fence", &id);
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    let rig = Arc::new(rig);
    let r2 = Arc::clone(&rig);
    let poll = tokio::spawn(async move { r2.poll().await });
    gate.arrived.wait().await;
    let st = rig.stop(&t.id).await;
    assert_eq!(st.state, "cancel_pending");
    assert_eq!(st.detail.running_turns, 1);
    gate.release.wait().await;
    poll.await.unwrap();
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::calls(),
        before + 1,
        "exactly one turn ran"
    );
    let broker = crate::approval::ApprovalBroker::open(rig.dir.path()).unwrap();
    let fin = crate::responsibility::stop::stop_status(
        &rig.store,
        &rig.queue,
        Some(&broker),
        None,
        &t.id,
        Utc::now(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fin.state, "stopped");
    let _ = rig.store.complete_task(&t.id, "late", "alice").await;
    assert_eq!(
        rig.store.get_task(&t.id).await.unwrap().unwrap().status,
        "cancelled"
    );
}

/// Non-`goal:` messages never touch the fence (unchanged dispatcher path).
#[tokio::test]
async fn ordinary_messages_skip_the_fence() {
    let rig = Rig::new().await;
    let mut t = TaskRow::new(
        "plain".into(),
        "p".into(),
        "w".into(),
        "medium".into(),
        "alice".into(),
        "s".into(),
    );
    t.status = "todo".into();
    t.goal_mode = true;
    rig.store.insert_task(&t).await.unwrap();
    rig.driver().tick_once().await.unwrap();
    let msgs = rig.queue.goal_messages_for_task("plain").await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(!msgs[0].id.starts_with("goal:"));
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before + 1);
    assert_eq!(
        rig.queue
            .get_by_id(&msgs[0].id)
            .await
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Done
    );
}

/// Ruling 1: the message is already in the queue while its intent is still
/// `intended` (crash after enqueue, before the mark). The fence accepts
/// `intended`, so the dispatcher runs the round — exactly once, and a later
/// driver tick neither resends nor duplicates it.
#[tokio::test]
async fn queued_message_with_intent_still_intended_runs_exactly_once() {
    let rig = Rig::new().await;
    let t = rig.steered_task("fd").await;
    let crate::task_store::IntentBegin::Ready { intent, .. } = rig
        .store
        .begin_dispatch_intent(&t.id, 1, Utc::now())
        .await
        .unwrap()
    else {
        panic!("intent refused");
    };
    assert_eq!(intent.state, "intended");
    let msg = QueueMessage {
        id: intent.intent_id.clone(),
        sender: "goal-loop-driver".into(),
        target: "alice".into(),
        payload: format!("[goal-loop task_id={} iter=1] work", t.id),
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
    };
    rig.queue.enqueue(&msg).await.unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::calls(),
        before + 1,
        "dispatched exactly once"
    );
    assert_eq!(
        rig.queue
            .get_by_id(&intent.intent_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Done
    );
    rig.driver().reconcile_durable_dispatch().await.unwrap();
    rig.driver().tick_once().await.unwrap();
    let msgs = rig.queue.goal_messages_for_task(&t.id).await.unwrap();
    assert_eq!(
        msgs.iter().filter(|m| m.id == intent.intent_id).count(),
        1,
        "{msgs:?}"
    );
}

fn plain_goal_message(id: &str, task_id: &str) -> QueueMessage {
    QueueMessage {
        id: id.into(),
        sender: "goal-loop-driver".into(),
        target: "alice".into(),
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
    }
}

/// E-H2: a plain goal round (random id, no intent row) of a task stopped
/// after the driver read it is fenced too; an unstopped one still runs.
#[tokio::test]
async fn a_plain_goal_round_of_a_stopped_task_is_fenced() {
    let rig = Rig::new().await;
    let mut t = TaskRow::new(
        "pg".into(),
        "p".into(),
        "w".into(),
        "medium".into(),
        "alice".into(),
        "s".into(),
    );
    t.goal_mode = true;
    rig.store.insert_task(&t).await.unwrap();
    let t = rig.store.get_task("pg").await.unwrap().unwrap();
    rig.queue
        .enqueue(&plain_goal_message("m-plain", &t.id))
        .await
        .unwrap();
    rig.store
        .stop_task_tree(&t.id, t.authority_revision, "op", false, Utc::now())
        .await
        .unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before, "nothing spawned");
    let m = rig.queue.get_by_id("m-plain").await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        m.error
            .unwrap()
            .starts_with(crate::responsibility::FENCE_ERROR_PREFIX)
    );
}

fn set_claim(rig: &Rig, id: &str, lease: chrono::DateTime<Utc>, claimed_at: chrono::DateTime<Utc>) {
    let conn = rusqlite::Connection::open(rig.dir.path().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE tasks SET status='in_progress', claimed_by='alice', claimed_at=?2,
                lease_expires_at=?3, lease_renewed_at=NULL WHERE id=?1",
        rusqlite::params![id, claimed_at.to_rfc3339(), lease.to_rfc3339()],
    )
    .unwrap();
}

async fn plain_goal_task(rig: &Rig, id: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        "p".into(),
        "w".into(),
        "medium".into(),
        "alice".into(),
        "s".into(),
    );
    t.goal_mode = true;
    rig.store.insert_task(&t).await.unwrap();
    rig.store.get_task(id).await.unwrap().unwrap()
}

/// (a) A retried round of a task the employee already claimed (lease still
/// live) is fenced, and the end is quiet: the task stays `in_progress`, is
/// not failed and not sent to a human, and the driver does not count it as
/// a dispatch failure.
#[tokio::test]
async fn a_retried_round_of_a_claimed_task_is_fenced_quietly() {
    let rig = Rig::new().await;
    let t = plain_goal_task(&rig, "retry").await;
    let now = Utc::now();
    set_claim(&rig, &t.id, now + chrono::Duration::minutes(5), now);
    let mut m = plain_goal_message("m-retry", &t.id);
    m.retry_count = 1;
    rig.queue.enqueue(&m).await.unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before);
    let m = rig.queue.get_by_id("m-retry").await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        m.error
            .unwrap()
            .starts_with(crate::responsibility::FENCE_ERROR_PREFIX)
    );
    rig.driver().tick_once().await.unwrap();
    let after = rig.store.get_task(&t.id).await.unwrap().unwrap();
    assert_eq!(after.status, "in_progress");
    assert_eq!(after.pause_reason, None);
    assert_eq!(after.claimed_by.as_deref(), Some("alice"));
}

/// (b) Recovery of an expired claim is zombie reclaim → `pending`; the round
/// the driver then sends is not stopped by the fence.
#[tokio::test]
async fn a_round_after_zombie_reclaim_passes_the_fence() {
    let rig = Rig::new().await;
    let t = plain_goal_task(&rig, "zombie").await;
    let long_ago = Utc::now() - chrono::Duration::hours(2);
    set_claim(&rig, &t.id, long_ago, long_ago);
    let out = rig
        .store
        .reclaim_zombies(&Utc::now().to_rfc3339())
        .await
        .unwrap();
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(
        rig.store.get_task(&t.id).await.unwrap().unwrap().status,
        "pending"
    );
    rig.queue
        .enqueue(&plain_goal_message("m-zombie", &t.id))
        .await
        .unwrap();
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before + 1, "the round ran");
    let m = rig.queue.get_by_id("m-zombie").await.unwrap().unwrap();
    assert!(
        !m.error
            .unwrap_or_default()
            .starts_with(crate::responsibility::FENCE_ERROR_PREFIX)
    );
}

/// Round 4 (M-2): the dispatcher finds the task a heartbeat task-board wake-up
/// is about from the marker the heartbeat writes at the start of the payload.
#[test]
fn heartbeat_marker_names_the_task() {
    assert_eq!(
        extract_heartbeat_task_id("[heartbeat-pull task_id=t-1] 任務看板有一筆待辦"),
        Some("t-1")
    );
    assert_eq!(
        extract_heartbeat_task_id("[heartbeat-stall task_id=t-2] 停滯"),
        Some("t-2")
    );
    // Not at the start, empty, or malformed: not a heartbeat wake-up.
    assert_eq!(
        extract_heartbeat_task_id("hi [heartbeat-pull task_id=t-1]"),
        None
    );
    assert_eq!(extract_heartbeat_task_id("[heartbeat-pull task_id=]"), None);
    assert_eq!(
        extract_heartbeat_task_id("[heartbeat-pull task_id=a b]"),
        None
    );
    assert_eq!(
        extract_heartbeat_task_id("[heartbeat-pull task_id=t-1"),
        None
    );
}

/// M3-3: the goal marker counts only at the very start of the payload.
#[test]
fn goal_marker_is_anchored_at_the_start() {
    assert_eq!(
        extract_goal_loop_task_id_and_round("[goal-loop task_id=t-1 iter=3] work"),
        Some(("t-1", 3))
    );
    let buried = "請看 [goal-loop task_id=t-1 iter=3] 這段";
    assert_eq!(extract_goal_loop_task_id_and_round(buried), None);
    assert_eq!(
        extract_goal_loop_task_id_and_round(" [goal-loop task_id=t-1 iter=3]"),
        None
    );
    // The run history may still link a recorded prompt for display.
    assert_eq!(goal_marker_linkage_for_display(buried), Some(("t-1", 3)));
}

/// M3-3: a message from anyone but the goal-loop driver carries no round,
/// even with a well-formed marker at its start; the driver's own does.
#[tokio::test]
async fn only_the_drivers_goal_marker_gives_a_round() {
    let rig = Rig::new().await;
    let t = plain_goal_task(&rig, "marker").await;
    let mut forged = plain_goal_message("m-forged", &t.id);
    forged.sender = "dashboard".into();
    rig.queue.enqueue(&forged).await.unwrap();
    model_call_probe::set_dry_run(true);
    model_call_probe::set_last_round(Some("unset".into()));
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::last_round(),
        None,
        "no round for a non-driver message"
    );

    rig.queue
        .enqueue(&plain_goal_message("m-driver", &t.id))
        .await
        .unwrap();
    model_call_probe::set_dry_run(true);
    rig.poll().await;
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::last_round().as_deref(),
        Some(t.id.as_str())
    );
}

/// M4-3 (a), through the real dispatcher: the start mark is written after
/// the fence and before the runtime is called; when the mark cannot be
/// written (here: the intent row vanished between the fence and the mark)
/// the runtime is never called.
#[tokio::test]
async fn the_start_mark_comes_before_the_runtime_and_gates_it() {
    let rig = Rig::new().await;
    let t = rig.steered_task("mark-ok").await;
    rig.driver().tick_once().await.unwrap();
    let id = format!("goal:{}:1", t.id);
    let gate = test_hooks::install("after_fence", &id);
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    let rig = Arc::new(rig);
    let r2 = Arc::clone(&rig);
    let poll = tokio::spawn(async move { r2.poll().await });
    gate.arrived.wait().await;
    assert!(
        !rig.store.any_round_started(&t.id).await.unwrap(),
        "not yet marked"
    );
    gate.release.wait().await;
    poll.await.unwrap();
    model_call_probe::set_dry_run(false);
    assert_eq!(model_call_probe::calls(), before + 1, "the round ran");
    assert!(rig.store.any_round_started(&t.id).await.unwrap(), "marked");

    // The mark cannot be written: the round does not start.
    let rig2 = Rig::new().await;
    let t2 = rig2.steered_task("mark-gone").await;
    rig2.driver().tick_once().await.unwrap();
    let id2 = format!("goal:{}:1", t2.id);
    let gate = test_hooks::install("after_fence", &id2);
    model_call_probe::set_dry_run(true);
    let before = model_call_probe::calls();
    let rig2 = Arc::new(rig2);
    let r3 = Arc::clone(&rig2);
    let poll = tokio::spawn(async move { r3.poll().await });
    gate.arrived.wait().await;
    let conn = rusqlite::Connection::open(rig2.dir.path().join("tasks.db")).unwrap();
    conn.execute(
        "DELETE FROM task_dispatch_intents WHERE intent_id = ?1",
        rusqlite::params![id2],
    )
    .unwrap();
    gate.release.wait().await;
    poll.await.unwrap();
    model_call_probe::set_dry_run(false);
    assert_eq!(
        model_call_probe::calls(),
        before,
        "no runtime without the mark"
    );
    let m = rig2.queue.get_by_id(&id2).await.unwrap().unwrap();
    assert_eq!(m.status, MessageStatus::Failed);
    assert!(
        m.error
            .unwrap_or_default()
            .contains("round start could not be recorded"),
        "the failure names why"
    );
}
