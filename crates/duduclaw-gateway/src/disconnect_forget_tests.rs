use super::*;
use crate::memory_provenance::{test_msg, test_seed};

async fn seeded(home: &Path, agent: &str, content: &str) -> String {
    let engine = SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let id = test_seed(&engine, agent, content, test_msg("telegram:1", 1)).await;
    connection(home).unwrap().execute(
        "INSERT INTO memory_connector_stamps(memory_store,memory_id,connector_ref,stamped_at) VALUES('memories',?1,'google:home',?2)",
        params![id,Utc::now().to_rfc3339()],
    ).unwrap();
    id
}

async fn approve(home: &Path, approval: &str) {
    ApprovalBroker::open(home).unwrap().decide(&approval.to_owned().into(), true, "dashboard:admin")
        .await.unwrap();
}

#[tokio::test]
async fn decision_crash_is_recovered_and_receipt_matches_two_employees() {
    let home = tempfile::tempdir().unwrap();
    seeded(home.path(), "alice", "private google one").await;
    seeded(home.path(), "bob", "private google two").await;
    let (id, approval) = create_batch(home.path(), "google:home", "Google account").await.unwrap();
    assert_eq!(ApprovalBroker::open(home.path()).unwrap()
        .list_by_kind(crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE).await.unwrap().len(), 1);
    sweep(home.path()).await.unwrap();
    assert_eq!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().len(), 0);
    approve(home.path(), &approval).await;
    // Simulate termination after approvals.db committed, before enqueue.
    assert_eq!(batch(home.path(), &id).unwrap().status, "planned");
    sweep(home.path()).await.unwrap();
    let receipt = receipts(home.path(), Some(&id)).unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "complete");
    assert_eq!(receipt["receipt"]["approved_by"], "dashboard:admin");
    assert_eq!(receipt["receipt"]["counts"].as_array().unwrap().iter()
        .map(|v| v["memories_deleted"].as_u64().unwrap()).sum::<u64>(), 2);
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    sweep(home.path()).await.unwrap();
    assert_eq!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn denied_batch_is_not_reopened_and_disabled_creates_no_plan() {
    let home = tempfile::tempdir().unwrap();
    seeded(home.path(), "alice", "keep private google").await;
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=false\n").unwrap();
    assert_eq!(after_disconnect(home.path(), "google:home", "Google", true).await["status"], "disabled");
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memory_forget_plans", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    ApprovalBroker::open(home.path()).unwrap().decide(&approval.into(), false, "dashboard:admin").await.unwrap();
    sweep(home.path()).await.unwrap();
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "refused");
    assert_eq!(ApprovalBroker::open(home.path()).unwrap()
        .list_by_kind(crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE).await.unwrap().len(), 1);
    assert!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn scope_growth_gets_a_new_card_without_deleting_anything() {
    let home = tempfile::tempdir().unwrap();
    seeded(home.path(), "alice", "first private google").await;
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    seeded(home.path(), "bob", "new employee private google").await;
    sweep(home.path()).await.unwrap();
    let b = batch(home.path(), &id).unwrap();
    assert_eq!(b.status, "planned");
    assert_ne!(b.approval_id.as_deref(), Some(approval.as_str()));
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_,u64>(0)).unwrap(), 2);
    assert!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn committed_apply_without_receipt_resumes_from_durable_counts() {
    let home = tempfile::tempdir().unwrap();
    seeded(home.path(), "alice", "private google before crash").await;
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    enqueue_approved(home.path(), &id).await.unwrap();
    let engine = engine(home.path()).unwrap();
    let plans = batch_plans(home.path(), &engine, &id, false).await.unwrap();
    connection(home.path()).unwrap().execute("UPDATE disconnect_forget_batch_plans SET apply_plan_id=approved_plan_id WHERE batch_id=?1", [&id]).unwrap();
    crate::memory_forget_apply::apply(home.path(), &engine, &plans[0]).await.unwrap();
    assert!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().is_empty());
    sweep(home.path()).await.unwrap();
    assert_eq!(receipts(home.path(), Some(&id)).unwrap()["receipt"]["counts"][0]["memories_deleted"], 1);
}

#[tokio::test]
async fn refused_card_survives_crash_before_approval_id_was_saved() {
    let home = tempfile::tempdir().unwrap();
    seeded(home.path(), "alice", "private google keep").await;
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    connection(home.path()).unwrap().execute("UPDATE disconnect_forget_batches SET approval_id=NULL WHERE id=?1", [&id]).unwrap();
    ApprovalBroker::open(home.path()).unwrap().decide(&approval.into(), false, "dashboard:admin").await.unwrap();
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "refused");
    assert_eq!(ApprovalBroker::open(home.path()).unwrap()
        .list_by_kind(crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE).await.unwrap().len(), 1);
    assert!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn empty_employee_gets_a_fence_before_first_delayed_memory() {
    let home = tempfile::tempdir().unwrap();
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    for directory in ["agents/alice", "agents/.ephemeral/eph-reader"] {
        std::fs::create_dir_all(home.path().join(directory)).unwrap();
    }
    let old_turn = Utc::now() - Duration::minutes(1);
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    sweep(home.path()).await.unwrap();
    assert_eq!(receipts(home.path(), Some(&id)).unwrap()["receipt"]["counts"].as_array().unwrap().len(), 2);
    for agent in ["alice", "eph-reader"] {
        let entry = duduclaw_core::types::MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(), agent_id: agent.into(),
            content: "late first memory of old google data".into(), timestamp: Utc::now(),
            tags: vec![], embedding: None, layer: duduclaw_core::types::MemoryLayer::Semantic,
            importance: 5.0, access_count: 0, last_accessed: None, source_event: "test".into(),
        };
        let provenance = duduclaw_memory::lineage::Provenance::source(test_msg("telegram:late", 2))
            .with_connector_refs_at(vec!["google:home".into()], old_turn);
        assert!(engine.store_temporal(agent, entry, Default::default(), provenance).await.is_err());
    }
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
}
