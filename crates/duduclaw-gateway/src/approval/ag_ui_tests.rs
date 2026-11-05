use super::*;

fn broker() -> ApprovalBroker {
    ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()))
}

async fn card(broker: &ApprovalBroker, kind: &str) -> ApprovalRecord {
    let id = scope_turn("session", "turn", broker.request("worker", kind, "test", Value::Null, 600))
        .await.unwrap();
    broker.get(&id).await.unwrap().unwrap()
}

#[tokio::test]
async fn ag_ui_batch_decides_all_and_preserves_external_decider() {
    let broker = broker();
    let first = card(&broker, "mcp_call").await;
    let second = card(&broker, "mcp_install").await;
    let external = card(&broker, "memory_forget_source").await;
    broker.decide(&external.id, true, "dashboard:admin").await.unwrap();
    let external = broker.get(&external.id).await.unwrap().unwrap();
    broker.decide_ag_ui_batch("session", "turn",
        &[(first.clone(), true), (second.clone(), false), (external.clone(), true)], "ag-ui:admin").await.unwrap();
    assert_eq!(broker.get(&first.id).await.unwrap().unwrap().status, ApprovalStatus::Approved);
    assert_eq!(broker.get(&second.id).await.unwrap().unwrap().status, ApprovalStatus::Denied);
    let observed = broker.get(&external.id).await.unwrap().unwrap();
    assert_eq!(observed.decided_by, external.decided_by);
    assert_eq!(observed.decided_at, external.decided_at);
}

#[tokio::test]
async fn ag_ui_batch_conflict_rolls_back_earlier_decisions() {
    let broker = broker();
    let first = card(&broker, "mcp_call").await;
    let second = card(&broker, "mcp_call").await;
    broker.decide(&second.id, false, "dashboard:admin").await.unwrap();
    assert!(broker.decide_ag_ui_batch("session", "turn",
        &[(first.clone(), true), (second.clone(), true)], "ag-ui:admin").await.is_err());
    assert_eq!(broker.get(&first.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    assert_eq!(broker.get(&second.id).await.unwrap().unwrap().status, ApprovalStatus::Denied);
}

#[tokio::test]
async fn ag_ui_batch_rechecks_request_digest_origin_and_deadline() {
    for case in ["payload", "principal", "ttl", "expired", "missing_origin", "half_origin", "cancelled", "extra_card"] {
        let broker = broker();
        let first = card(&broker, "mcp_call").await;
        let mut second = card(&broker, "mcp_call").await;
        if case == "extra_card" {
            card(&broker, "mcp_call").await;
        } else {
            let conn = broker.store.conn.lock().await;
            let query = match case {
                "payload" => "UPDATE approvals SET payload='{\"changed\":true}' WHERE id=?1",
                "principal" => "UPDATE approvals SET on_behalf_of='user:other' WHERE id=?1",
                "ttl" => "UPDATE approvals SET ttl_seconds=300 WHERE id=?1",
                "expired" => "UPDATE approvals SET created_at='2000-01-01T00:00:00+00:00' WHERE id=?1",
                "missing_origin" => "DELETE FROM approval_turn_origins WHERE approval_id=?1",
                "half_origin" => "UPDATE approvals SET origin_turn_id=NULL WHERE id=?1",
                "cancelled" => "INSERT INTO cancelled_approval_turns(session_id,turn_id,cancelled_at)
                    SELECT 'session','turn',datetime('now') FROM approvals WHERE id=?1",
                _ => unreachable!(),
            };
            conn.execute(query, [second.id.as_str()]).unwrap();
        }
        if case == "expired" { second = broker.get(&second.id).await.unwrap().unwrap(); }
        assert!(broker.decide_ag_ui_batch("session", "turn",
            &[(first.clone(), true), (second.clone(), true)], "ag-ui:admin").await.is_err(), "{case}");
        assert_eq!(broker.get(&first.id).await.unwrap().unwrap().status, ApprovalStatus::Pending, "{case}");
    }
}

// Keep cases separate so filtered remote logs identify the failing branch.
async fn assert_ag_ui_batch_rejects(case: &str) {
    let broker = broker();
    let first = card(&broker, "mcp_call").await;
    let mut second = card(&broker, if case == "dashboard_pending" { "memory_forget_source" } else { "mcp_call" }).await;
    if case == "scope" || case == "egress" {
        let key = if case == "scope" { "scope_epoch" } else { "egress_guard" };
        second.payload = serde_json::json!({(key): 1});
        broker.replace_text(&second.id, &second.summary, &second.payload).await
            .unwrap_or_else(|error| panic!("{case}: prepare payload: {error}"));
    } else if case == "expired" {
        // Expire a pending card through the broker; decided statuses are final.
        broker.store.conn.lock().await.execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00+00:00' WHERE id=?1",
            [second.id.as_str()],
        ).unwrap();
        assert_eq!(broker.expire_stale().await.unwrap(), 1);
    } else if case != "dashboard_pending" {
        broker.decide(&second.id, true, "dashboard:admin").await
            .unwrap_or_else(|error| panic!("{case}: prepare external decision: {error}"));
        let conn = broker.store.conn.lock().await;
        let query = match case {
            "invalidated" => Some("UPDATE approvals SET invalidated_reason='changed' WHERE id=?1"),
            "late_decision" => Some("UPDATE approvals SET decided_at='2999-01-01T00:00:00+00:00' WHERE id=?1"),
            "missing_decider" => Some("UPDATE approvals SET decided_by=NULL WHERE id=?1"),
            _ => None,
        };
        if let Some(query) = query {
            conn.execute(query, [second.id.as_str()])
                .unwrap_or_else(|error| panic!("{case}: prepare terminal record: {error}"));
        }
    }
    second = broker.get(&second.id).await.unwrap().unwrap();
    match case {
        "expired" => assert_eq!(second.status, ApprovalStatus::Expired),
        "invalidated" => {
            assert_eq!(second.status, ApprovalStatus::Approved);
            assert_eq!(second.invalidated_reason.as_deref(), Some("changed"));
        }
        "late_decision" => {
            assert_eq!(second.status, ApprovalStatus::Approved);
            assert_eq!(second.decided_at.as_deref(), Some("2999-01-01T00:00:00+00:00"));
            assert_eq!(second.decided_by.as_deref(), Some("dashboard:admin"));
        }
        "missing_decider" => {
            assert_eq!(second.status, ApprovalStatus::Approved);
            assert!(second.decided_by.is_none());
            let decided = DateTime::parse_from_rfc3339(second.decided_at.as_deref().unwrap()).unwrap();
            let created = DateTime::parse_from_rfc3339(&second.created_at).unwrap();
            assert!(created <= decided && decided.with_timezone(&Utc) < second.expires_at().unwrap());
        }
        _ => (),
    }
    let result = broker.decide_ag_ui_batch("session", "turn",
        &[(first.clone(), true), (second, case != "wrong_decision")], "ag-ui:admin").await;
    assert!(result.is_err(), "{case}");
    let expected_error = match case {
        "dashboard_pending" | "scope" | "egress" => "approval cannot be decided by AG-UI",
        "invalidated" => "approval changed before resume",
        "expired" | "late_decision" | "missing_decider" | "wrong_decision" => "approval has no matching legal decision",
        _ => unreachable!(),
    };
    assert_eq!(result.unwrap_err(), expected_error, "{case}");
    assert_eq!(broker.get(&first.id).await.unwrap().unwrap().status, ApprovalStatus::Pending, "{case}");
}

#[tokio::test]
async fn ag_ui_batch_rejects_dashboard_pending() {
    assert_ag_ui_batch_rejects("dashboard_pending").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_scope() {
    assert_ag_ui_batch_rejects("scope").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_egress() {
    assert_ag_ui_batch_rejects("egress").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_expired() {
    assert_ag_ui_batch_rejects("expired").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_invalidated() {
    assert_ag_ui_batch_rejects("invalidated").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_late_decision() {
    assert_ag_ui_batch_rejects("late_decision").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_wrong_decision() {
    assert_ag_ui_batch_rejects("wrong_decision").await;
}

#[tokio::test]
async fn ag_ui_batch_rejects_missing_decider() {
    assert_ag_ui_batch_rejects("missing_decider").await;
}

#[tokio::test]
async fn ag_ui_batch_observes_timely_external_decision_after_card_deadline() {
    let broker = broker();
    let card = card(&broker, "memory_forget_source").await;
    broker.decide(&card.id, true, "dashboard:admin").await.unwrap();
    broker.store.conn.lock().await.execute(
        "UPDATE approvals SET created_at='2000-01-01T00:00:00+00:00',
         decided_at='2000-01-01T00:00:01+00:00' WHERE id=?1", [card.id.as_str()],
    ).unwrap();
    let terminal = broker.get(&card.id).await.unwrap().unwrap();
    broker.decide_ag_ui_batch("session", "turn", &[(terminal, true)], "ag-ui:viewer").await.unwrap();
    assert_eq!(broker.get(&card.id).await.unwrap().unwrap().decided_by.as_deref(), Some("dashboard:admin"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ag_ui_batch_serializes_external_conflict_across_connections() {
    let home = tempfile::tempdir().unwrap();
    let first = ApprovalBroker::open(home.path()).unwrap();
    let other = ApprovalBroker::open(home.path()).unwrap();
    for index in 0..8 {
        let turn = format!("turn-{index}");
        let a = scope_turn("session", &turn, first.request("worker", "mcp_call", "first", Value::Null, 600)).await.unwrap();
        let b = scope_turn("session", &turn, first.request("worker", "mcp_call", "second", Value::Null, 600)).await.unwrap();
        let batch = vec![(first.get(&a).await.unwrap().unwrap(), true), (first.get(&b).await.unwrap().unwrap(), true)];
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let external = other.clone();
        let external_id = b.clone();
        let external_barrier = barrier.clone();
        let conflict = tokio::spawn(async move {
            external_barrier.wait().await;
            external.decide(&external_id, false, "dashboard:admin").await
        });
        barrier.wait().await;
        let result = first.decide_ag_ui_batch("session", &turn, &batch, "ag-ui:admin").await;
        let conflict = conflict.await.unwrap();
        let a = first.get(&a).await.unwrap().unwrap();
        let b = first.get(&b).await.unwrap().unwrap();
        if result.is_ok() {
            assert!(conflict.is_err());
            assert_eq!((a.status, b.status), (ApprovalStatus::Approved, ApprovalStatus::Approved));
        } else {
            assert!(conflict.is_ok());
            assert_eq!((a.status, b.status), (ApprovalStatus::Pending, ApprovalStatus::Denied));
        }
    }
}
