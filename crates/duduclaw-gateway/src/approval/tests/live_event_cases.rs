//! Broker-side producers must work without an in-process gateway sender.
use super::*;

#[tokio::test]
async fn separate_brokers_persist_redacted_lifecycle_events() {
    let home = tempfile::tempdir().unwrap();
    let employee = ApprovalBroker::open(home.path()).unwrap();
    let id = employee.request(
        "alice", "goal_kickoff", "secret prompt", json!({"token": "secret token"}), 300,
    ).await.unwrap();
    // A second independently opened broker models a different process. It
    // shares only SQLite, never a broadcast sender or a Rust singleton.
    let decider = ApprovalBroker::open(home.path()).unwrap();
    decider.decide(&id, true, "dashboard:admin").await.unwrap();
    assert!(decider.decide(&id, true, "dashboard:admin").await.is_err());
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let rows = bus.fetch_since(0, 100).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.event.as_str()).collect::<Vec<_>>(),
        vec!["approval.created", "approval.decided"]);
    for row in &rows {
        let payload: Value = serde_json::from_str(&row.payload).unwrap();
        assert_eq!(payload["id"], id.as_str());
        assert_eq!(payload["agent_id"], "alice");
        assert_eq!(payload.as_object().unwrap().len(), 4);
        assert!(!row.payload.contains("secret"));
        assert!(payload.get("payload").is_none());
        assert!(payload.get("summary").is_none());
        assert!(payload.get("decided_by").is_none());
    }
    assert_eq!(serde_json::from_str::<Value>(&rows[1].payload).unwrap()["status"], "approved");
}

#[tokio::test]
async fn every_expiration_entry_point_emits_only_once() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    for path in ["poll", "sweep", "decide", "await"] {
        let mut rec = aged(10, 1, false);
        rec.action_kind = "goal_kickoff".into();
        broker.store.insert(&rec).await.unwrap();
        match path {
            "poll" => { assert_eq!(broker.poll(&rec.id).await.unwrap(), ApprovalStatus::Expired); }
            "sweep" => { assert_eq!(broker.expire_stale().await.unwrap(), 1); }
            "decide" => { assert!(broker.decide(&rec.id, true, "test").await.is_err()); }
            "await" => { assert_eq!(broker.await_decision(&rec.id, Duration::from_millis(1)).await.unwrap(), ApprovalStatus::Expired); }
            _ => unreachable!(),
        }
        assert_eq!(broker.poll(&rec.id).await.unwrap(), ApprovalStatus::Expired);
    }
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let rows = bus.fetch_since(0, 100).await.unwrap();
    assert_eq!(rows.iter().filter(|r| r.event == "approval.expired").count(), 4);
    assert_eq!(rows.iter().filter(|r| r.event == "approval.created").count(), 4);
}

#[test]
fn decision_digest_is_canonical_and_tracks_visible_fields() {
    let mut rec = aged(0, 300, false);
    rec.payload = serde_json::from_str(r#"{"z":1,"a":{"b":2,"a":1}}"#).unwrap();
    let digest = decision_digest(&rec);
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    rec.payload = serde_json::from_str(r#"{"a":{"a":1,"b":2},"z":1}"#).unwrap();
    assert_eq!(decision_digest(&rec), digest);
    rec.notify_chat_id = Some("transport bookkeeping".into());
    assert_eq!(decision_digest(&rec), digest);
    rec.summary.push_str(" changed");
    assert_ne!(decision_digest(&rec), digest);
}

#[tokio::test]
async fn digest_guard_accepts_exact_card_and_rejects_changed_card_without_decision() {
    let broker = broker();
    let id = broker.request("a", "mcp_call", "reviewed", json!({"x": 1}), 300).await.unwrap();
    let digest = decision_digest(&broker.get(&id).await.unwrap().unwrap());
    EXPECTED_DECISION_DIGEST.scope(Some((id.to_string(), digest)), async {
        // Models a concurrent privacy scrub after the RPC's first read.
        broker.replace_text(&id, "changed", &json!({"x": 2})).await.unwrap();
        let error = broker.decide(&id, true, "dashboard:admin").await.unwrap_err();
        assert!(error.contains("digest mismatch"));
    }).await;
    let current = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(current.status, ApprovalStatus::Pending);
    EXPECTED_DECISION_DIGEST.scope(
        Some((id.to_string(), decision_digest(&current))),
        broker.decide(&id, true, "dashboard:admin"),
    ).await.unwrap();
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn duplicate_keyed_inserts_do_not_emit_duplicate_creation() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let rec = aged(0, 300, false);
    assert!(broker.store.insert_if_absent(&rec).await.unwrap());
    assert!(!broker.store.insert_if_absent(&rec).await.unwrap());
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let rows = bus.fetch_since(0, 100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event, "approval.created");
}

#[tokio::test]
async fn activation_pricing_change_invalidates_the_reviewed_digest() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let mut rec = aged(0, 300, false);
    rec.action_kind = WORKFLOW_ACTIVATION_KIND.into();
    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let creator = json!({"actor": "a", "allowed_tools": [], "policy_revision": "p"});
    // A deserializable projection is enough for this digest test. No
    // workflow is activated, and no model or external executor is used.
    let request = json!({
        "activation_id": "activation", "draft_id": "draft", "draft_hash": "hash",
        "revision": {
            "definition": {
                "schema_version": 1, "workflow_id": "wf", "revision": 1,
                "skill_revision_hash": "skill", "input_schema": {"type": "null"},
                "output_schema": {"type": "null"}, "required_capabilities": [], "steps": []
            },
            "revision_hash": "revision", "owner": "a", "creator_grant": creator,
            "audience": [], "fixtures_digest": "fixtures", "acceptance_id": "",
            "accepted_at": "", "expires_at": expires,
        },
        "spec": {
            "schema_version": 1, "workflow_id": "wf", "workflow_revision": 1,
            "revision_hash": "revision", "skill_hash": "skill", "fixtures_digest": "fixtures",
            "activation_id": "activation", "actor": "a",
            "operator_context": {"channel": "dashboard", "account_id": "system", "conversation_id": "approval", "principal_id": "admin"},
            "creator_grant": creator, "audience": [], "templates": {},
            "input_max_age_seconds": 60, "fixtures_expires_at": expires,
            "budget": {"per_run_micros": 100, "monthly_micros": 1000, "max_consecutive_failures": 3},
            "expires_at": expires, "policy_revision": "p"
        },
        "fixture_results": [], "fixture_assertions": [], "cron": null
    });
    let _: crate::workflow::ActivationRequest = serde_json::from_value(request.clone()).unwrap();
    rec.payload = json!({"kind": WORKFLOW_ACTIVATION_KIND, "request": request});
    broker.store.insert(&rec).await.unwrap();
    let facts = activation_facts(home.path(), &rec);
    assert!(facts["pricing"].is_object());
    let digest = decision_digest_at(home.path(), &rec);
    assert_eq!(digest, decision_digest_with_activation(&rec, &facts));
    std::fs::write(home.path().join("config.toml"), "[workflow.unit_cost_micros]\napproval = 17\n").unwrap();
    assert_ne!(decision_digest_at(home.path(), &rec), digest);
    let error = EXPECTED_DECISION_DIGEST.scope(
        Some((rec.id.to_string(), digest)),
        EXPECTED_DECISION_HOME.scope(home.path().to_path_buf(), broker.decide(&rec.id, true, "dashboard:admin")),
    ).await.unwrap_err();
    assert!(error.contains("digest mismatch"));
    assert_eq!(broker.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
}

#[tokio::test]
async fn pending_invalidation_emits_a_single_terminal_event() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let rec = aged(0, 300, false);
    broker.store.insert(&rec).await.unwrap();
    broker.invalidate_request(&rec.id, "task stopped").await.unwrap();
    broker.invalidate_request(&rec.id, "task stopped again").await.unwrap();
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let rows = bus.fetch_since(0, 100).await.unwrap();
    assert_eq!(rows.iter().filter(|row| row.event == "approval.decided").count(), 1);
    let payload: Value = serde_json::from_str(&rows[1].payload).unwrap();
    assert_eq!(payload["status"], "invalidated");
    assert!(payload.get("invalidated_reason").is_none());
}

#[tokio::test]
async fn quarantine_creation_event_commits_once_with_card_and_honors_scrub() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let mut rec = aged(0, 300, false);
    rec.action_kind = crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE.into();
    assert!(broker.store.insert_if_absent(&rec).await.unwrap());
    assert!(!broker.store.insert_if_absent(&rec).await.unwrap());
    let rows = bus.fetch_since(0, 100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event, "approval.created");
    assert_eq!(serde_json::from_str::<Value>(&rows[0].payload).unwrap()["id"], rec.id.as_str());
    assert!(broker.get(&rec.id).await.unwrap().is_some());

    let mut scrubbed = aged(0, 300, false);
    scrubbed.action_kind = rec.action_kind.clone();
    bus.delete_approval_lifecycle(scrubbed.id.as_str()).await.unwrap();
    assert!(broker.store.insert_if_absent(&scrubbed).await.unwrap());
    assert_eq!(bus.fetch_since(0, 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn quarantine_creation_event_failure_rolls_back_card_and_connection_recovers() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    let faults = rusqlite::Connection::open(home.path().join("events.db")).unwrap();
    faults.execute_batch(
        "CREATE TRIGGER reject_created BEFORE INSERT ON events
         WHEN NEW.event = 'approval.created'
         BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
    ).unwrap();
    let mut rec = aged(0, 300, false);
    rec.action_kind = crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE.into();
    let error = broker.store.insert(&rec).await.unwrap_err();
    assert!(error.contains("injected event failure"));
    assert!(broker.get(&rec.id).await.unwrap().is_none(), "card and event roll back together");
    assert!(bus.fetch_since(0, 100).await.unwrap().is_empty());
    faults.execute_batch("DROP TRIGGER reject_created").unwrap();
    broker.store.insert(&rec).await.unwrap();
    assert!(broker.get(&rec.id).await.unwrap().is_some());
    assert_eq!(bus.fetch_since(0, 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn quarantine_cancelled_turn_rolls_back_and_detaches_before_connection_reuse() {
    let home = tempfile::tempdir().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let bus = crate::events_store::EventBusStore::open(home.path()).unwrap();
    broker.withdraw_turn("session", "cancelled").await.unwrap();
    let mut rec = aged(0, 300, false);
    rec.action_kind = crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE.into();
    let error = crate::approval::scope_turn("session", "cancelled", broker.store.insert(&rec))
        .await.unwrap_err();
    assert_eq!(error, "approval turn was cancelled");
    assert!(broker.get(&rec.id).await.unwrap().is_none());
    assert!(bus.fetch_since(0, 100).await.unwrap().is_empty());
    {
        let conn = broker.store.conn.lock().await;
        let attached: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='quarantine_live_events')",
            [], |row| row.get(0),
        ).unwrap();
        assert!(!attached, "failed insertion must detach the event database");
    }
    assert!(crate::approval::scope_turn("session", "live", broker.store.insert_if_absent(&rec)).await.unwrap());
    assert!(!crate::approval::scope_turn("session", "other", broker.store.insert_if_absent(&rec)).await.unwrap());
    let conn = broker.store.conn.lock().await;
    let attached: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='quarantine_live_events')",
        [], |row| row.get(0),
    ).unwrap();
    assert!(!attached, "same-card early return must detach the event database");
    let origins: i64 = conn.query_row(
        "SELECT count(*) FROM approval_turn_origins WHERE approval_id=?1", [rec.id.as_str()], |row| row.get(0),
    ).unwrap();
    assert_eq!(origins, 2);
    drop(conn);
    assert_eq!(bus.fetch_since(0, 100).await.unwrap().len(), 1);
}
