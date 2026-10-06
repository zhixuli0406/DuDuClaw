//! P2-A ET4.2 against the real operations API: an action prepared and
//! approved for a task can no longer be claimed once the task is stopped,
//! and the stop report counts it as prepared (not executing).

use super::*;

#[tokio::test]
async fn stopped_task_refuses_claim_of_a_prepared_operation() {
    let home = fixture();
    std::fs::write(
        home.path().join("config.toml"),
        "[dispatch]\nenabled = true\n",
    )
    .unwrap();
    let store = crate::task_store::TaskStore::open(home.path()).unwrap();
    let queue = crate::message_queue::MessageQueue::open(home.path()).unwrap();
    let mut task = crate::task_store::TaskRow::new(
        "stop-op".into(),
        "Stop task".into(),
        "do work".into(),
        "medium".into(),
        "alice".into(),
        "human".into(),
    );
    task.status = "pending".into();
    store.insert_task(&task).await.unwrap();
    let snap = store.authority_snapshot("stop-op").await.unwrap().unwrap();
    let p = json!({"write": 1});
    let mut bound = binding(home.path(), &p, "workflow_v1");
    bound.task_id = Some(snap.task_id.clone());
    bound.task_revision = Some(snap.revision);
    bound.task_snapshot_hash = Some(snap.hash);
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "work", p, bound.clone())
        .await
        .unwrap();
    let op = b.prepare_operation(&id, "work", None).await.unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();

    let rev = store
        .get_task("stop-op")
        .await
        .unwrap()
        .unwrap()
        .authority_revision;
    let status = crate::responsibility::stop::stop_task(
        &store,
        &queue,
        Some(&b),
        None,
        home.path(),
        "stop-op",
        rev,
        "operator-1",
        false,
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(status.detail.operations_prepared, 1, "{status:?}");
    assert_eq!(status.detail.operations_executing, 0);
    assert_eq!(status.state, "stopped", "{status:?}");

    let err = b
        .claim_operation(&op, &bound, "runner", 30)
        .await
        .unwrap_err();
    assert_eq!(err, "task authority changed or task closed");
}
