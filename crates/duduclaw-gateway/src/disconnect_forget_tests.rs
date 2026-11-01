use super::*;
use crate::memory_provenance::test_msg;

async fn seeded(home: &Path, agent: &str, content: &str) -> String {
    seeded_at(home, agent, content, Utc::now()).await
}

async fn seeded_at(home: &Path, agent: &str, content: &str, observed_at: chrono::DateTime<Utc>) -> String {
    let engine = SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let entry = duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(), agent_id: agent.into(), content: content.into(),
        timestamp: Utc::now(), tags: vec![], embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Semantic, importance: 5.0,
        access_count: 0, last_accessed: None, source_event: "test".into(),
    };
    let source = duduclaw_memory::lineage::SourceRef::channel_message("telegram:1", 1, observed_at, None);
    let provenance = duduclaw_memory::lineage::Provenance::source(source)
        .with_connector_refs_at(vec!["google:home".into()], observed_at);
    engine.store_temporal(agent, entry, Default::default(), provenance).await.unwrap()
}

async fn approve(home: &Path, approval: &str) {
    ApprovalBroker::open(home).unwrap().decide(&approval.to_owned().into(), true, "dashboard:admin")
        .await.unwrap();
    // Advance the due queue without waiting for the polling interval.
    connection(home).unwrap().execute("UPDATE disconnect_forget_batches SET next_attempt_at=0 WHERE approval_id=?1",
        [approval]).unwrap();
}

#[tokio::test]
async fn decision_crash_is_recovered_and_receipt_matches_two_employees() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google one").await;
    seeded(home.path(), "bob", "private google two").await;
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
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
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "keep private google").await;
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=false\n").unwrap();
    assert_eq!(after_disconnect(home.path(), "google:home", "Google", true).await["status"], "disabled");
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memory_forget_plans", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
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
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "first private google").await;
    let observed_before_disconnect = Utc::now() - Duration::minutes(1);
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    // The read happened before disconnect; its memory write arrives after approval.
    seeded_at(home.path(), "bob", "new employee private google", observed_before_disconnect).await;
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
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google before crash").await;
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
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
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google keep").await;
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
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
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    for directory in ["agents/alice", "agents/.ephemeral/eph-reader"] {
        std::fs::create_dir_all(home.path().join(directory)).unwrap();
    }
    let old_turn = Utc::now() - Duration::minutes(1);
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
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


// Added regression tests; not run in this change (builds/tests were prohibited).
#[tokio::test]
async fn poison_jobs_back_off_and_newer_batch_gets_a_turn() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    let _engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let conn = connection(home.path()).unwrap();
    for index in 0..100 {
        let id = format!("poison-{index:03}");
        conn.execute("INSERT INTO disconnect_forget_batches(id,connector_ref,display_name,created_at,expires_at,digest)
            VALUES(?1,'google:home','Poison','2000-01-01',?2,'digest')",
            params![id,(Utc::now()+Duration::days(1)).to_rfc3339()]).unwrap();
        conn.execute("INSERT INTO disconnect_forget_batch_plans(batch_id,agent_id,approved_plan_id) VALUES(?1,'alice','missing-plan')",
            [&id]).unwrap();
    }
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (id, approval) = create_batch(home.path(), "google:home", "New batch").await.unwrap();
    approve(home.path(), &approval).await;
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "planned");
    let (attempts, next): (i64,i64) = conn.query_row("SELECT attempt_count,next_attempt_at FROM disconnect_forget_batches WHERE id='poison-000'",
        [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(attempts, 1);
    assert!(next > Utc::now().timestamp());
    assert_eq!(due_jobs(home.path()).unwrap(), vec![id.clone()]);
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "complete");
}

#[tokio::test]
async fn persistent_failures_quarantine_and_admin_retry_preserves_approval() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google retry").await;
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    enqueue_approved(home.path(), &id).await.unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=false\n").unwrap();
    for attempt in 1..=MAX_ATTEMPTS {
        connection(home.path()).unwrap().execute("UPDATE disconnect_forget_batches SET next_attempt_at=0 WHERE id=?1", [&id]).unwrap();
        sweep(home.path()).await.unwrap();
        let count: i64 = connection(home.path()).unwrap().query_row("SELECT attempt_count FROM disconnect_forget_batches WHERE id=?1", [&id], |r| r.get(0)).unwrap();
        assert_eq!(count, attempt);
    }
    assert_eq!(batch(home.path(), &id).unwrap().status, "quarantined");
    assert!(due_jobs(home.path()).unwrap().is_empty());
    let list = receipts(home.path(), None).unwrap();
    assert_eq!(list["jobs"][0]["last_error"], "forget_source disabled or unreadable");
    assert!(!list.to_string().contains("private google retry"));
    assert!(list["receipts"].as_array().unwrap().is_empty());
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    retry(home.path(), &id).unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().approval_id.as_deref(), Some(approval.as_str()));
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "complete");
    assert!(retry(home.path(), &id).is_err());
}

#[tokio::test]
async fn interrupted_attempts_are_bounded_and_errors_do_not_expose_content() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    let _engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (id, _) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    for _ in 0..MAX_ATTEMPTS { assert!(begin_attempt(home.path(), &id).unwrap()); }
    assert!(!begin_attempt(home.path(), &id).unwrap());
    assert_eq!(batch(home.path(), &id).unwrap().status, "quarantined");
    assert_eq!(safe_failure("SQL error with private memory content"), "disconnect job failed during storage or plan validation");
}

#[tokio::test]
async fn shared_counts_reach_admin_card_and_receipt() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    let id = seeded(home.path(), "alice", "shared google and database memory").await;
    let conn = connection(home.path()).unwrap();
    conn.execute("INSERT INTO memory_connector_stamps(memory_store,memory_id,connector_ref,stamped_at,connector_observed_at,source_session,source_message,role)
        SELECT memory_store,memory_id,'db:live',stamped_at,connector_observed_at,source_session,source_message,role
        FROM memory_connector_stamps WHERE memory_id=?1 AND connector_ref='google:home'", [&id]).unwrap();
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (batch_id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    let card = ApprovalBroker::open(home.path()).unwrap().get(&approval.clone().into()).await.unwrap().unwrap();
    assert_eq!(card.payload["counts_by_agent"][0]["retained_shared"], 1);
    approve(home.path(), &approval).await;
    sweep(home.path()).await.unwrap();
    let receipt = receipts(home.path(), Some(&batch_id)).unwrap();
    assert_eq!(receipt["receipt"]["counts"][0]["retained_shared"], 1);
    assert_eq!(receipt["receipt"]["counts"][0]["memories_deleted"], 0);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM memories WHERE id=?1", [&id], |r| r.get::<_,u64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn retry_rpc_requires_admin_and_keeps_quarantined_state_on_refusal() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google admin retry").await;
    super::engine(home.path()).unwrap().record_connector_disconnect("google:home").await.unwrap();
    let (id, _) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    connection(home.path()).unwrap().execute("UPDATE disconnect_forget_batches SET status='quarantined',retry_status='planned',attempt_count=5 WHERE id=?1",
        [&id]).unwrap();
    let handler = crate::handlers::MethodHandler::new(home.path().to_path_buf()).await;
    let mut manager = duduclaw_auth::UserContext::admin_fallback();
    manager.role = duduclaw_auth::UserRole::Manager;
    let refused = handler.handle("memory.forget_batch_retry", json!({"id":id}), &manager).await;
    assert!(matches!(refused, crate::protocol::WsFrame::Response { ok: false, .. }));
    assert_eq!(batch(home.path(), &id).unwrap().status, "quarantined");
    let admin = duduclaw_auth::UserContext::admin_fallback();
    let queued = handler.handle("memory.forget_batch_retry", json!({"id":id}), &admin).await;
    assert!(matches!(queued, crate::protocol::WsFrame::Response { ok: true, .. }));
    assert_eq!(batch(home.path(), &id).unwrap().status, "planned");
    assert_eq!(connection(home.path()).unwrap().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_,u64>(0)).unwrap(), 1);
}


// Added regression test; not run in this change.
#[tokio::test]
async fn applied_batch_recovers_receipt_after_connector_reconnects() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory]\nforget_source=true\n").unwrap();
    seeded(home.path(), "alice", "private google before reconnect").await;
    let engine = super::engine(home.path()).unwrap();
    engine.record_connector_disconnect("google:home").await.unwrap();
    let (id, approval) = create_batch(home.path(), "google:home", "Google").await.unwrap();
    approve(home.path(), &approval).await;
    enqueue_approved(home.path(), &id).await.unwrap();
    let plans = batch_plans(home.path(), &engine, &id, false).await.unwrap();
    connection(home.path()).unwrap().execute("UPDATE disconnect_forget_batch_plans SET apply_plan_id=approved_plan_id WHERE batch_id=?1", [&id]).unwrap();
    crate::memory_forget_apply::apply(home.path(), &engine, &plans[0]).await.unwrap();
    assert!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().is_empty());
    // Reconnect after deletion committed but before the completion receipt.
    connections::mark_connecting(home.path(), "google:home").unwrap();
    std::fs::write(home.path().join("config.toml"),
        "[memory]\nforget_source=true\n[integrations.google_apps_script]\nurl='https://example.test'\nsecret='test'\n").unwrap();
    assert!(connections::refresh(home.path(), &engine, "google:home").await.is_err());
    sweep(home.path()).await.unwrap();
    assert_eq!(batch(home.path(), &id).unwrap().status, "complete");
    assert_eq!(receipts(home.path(), Some(&id)).unwrap()["receipt"]["counts"][0]["memories_deleted"], 1);
    sweep(home.path()).await.unwrap();
    assert_eq!(receipts(home.path(), None).unwrap()["receipts"].as_array().unwrap().len(), 1);
}
