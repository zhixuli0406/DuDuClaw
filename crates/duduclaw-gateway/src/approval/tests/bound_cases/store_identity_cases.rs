use super::*;

#[cfg(unix)]
#[tokio::test]
async fn existing_database_and_sidecars_are_private_and_symlinks_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = fixture();
    let path = home.path().join("approvals.db");
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let b = ApprovalBroker::open(home.path()).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let p = home.path().join(format!("approvals.db{suffix}"));
        if p.exists() {
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    drop(b);
    let linked = fixture();
    symlink(&path, linked.path().join("approvals.db")).unwrap();
    assert!(ApprovalBroker::open(linked.path()).is_err());
}
#[tokio::test]
async fn legacy_schema_additive_migration_does_not_create_binding_or_execution() {
    let home = fixture();
    {
        let conn = Connection::open(home.path().join("approvals.db")).unwrap();
        conn.execute_batch("CREATE TABLE approvals(id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,
            action_kind TEXT NOT NULL,summary TEXT NOT NULL,payload TEXT NOT NULL DEFAULT '{}',
            status TEXT NOT NULL DEFAULT 'pending',created_at TEXT NOT NULL,decided_at TEXT,decided_by TEXT,
            ttl_seconds INTEGER NOT NULL DEFAULT 3600);
    INSERT INTO approvals VALUES('legacy','alice','mcp_tool','old','{}','approved','2000-01-01T00:00:00Z',NULL,NULL,
            3600);")
            .unwrap();
    }
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = ApprovalId::from("legacy".to_string());
    let row = b.get(&id).await.unwrap().unwrap();
    assert!(row.binding.is_none());
    assert!(b.prepare_operation(&id, "write", None).await.is_err());
    assert!(b.list_operations().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn begin_execution_waiting_for_sqlite_lock_cannot_cross_binding_ttl() {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};
    use crate::approval::workflow_grants::authority_test_clock;

    // SQLite's callback proves BEGIN IMMEDIATE actually encountered the writer.
    // Advance only this run's authority clock there, then release the real lock.
    // No expiry budget is consumed by fixture setup or scheduler delays.
    type BlockedWriter = (Connection, Arc<Mutex<DateTime<Utc>>>, DateTime<Utc>);
    thread_local! {
        static WRITER: RefCell<Option<BlockedWriter>> = const { RefCell::new(None) };
        static RELEASE: RefCell<Option<Result<(), String>>> = const { RefCell::new(None) };
    }
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            WRITER.with(|slot| slot.borrow_mut().take());
            RELEASE.with(|slot| slot.borrow_mut().take());
        }
    }
    fn expire_and_release(_attempt: i32) -> bool {
        let Some((blocker, clock, expired)) = WRITER.with(|slot| slot.borrow_mut().take()) else {
            return false;
        };
        // Never unwind through SQLite's C callback.
        let result = clock.lock().map_err(|e| e.to_string()).and_then(|mut now| {
            *now = expired;
            blocker.execute_batch("ROLLBACK").map_err(|e| e.to_string())
        });
        let retry = result.is_ok();
        RELEASE.with(|slot| *slot.borrow_mut() = Some(result));
        retry
    }

    let _cleanup = Cleanup;
    let home = fixture();
    let payload = json!({"side_effect":"once"});
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let mut bound = binding(home.path(), &payload, "workflow_v1");
    let now = Utc::now();
    let expiry = now + chrono::Duration::hours(1);
    bound.expires_at = expiry.to_rfc3339();
    let clock = authority_test_clock::Guard::at(&bound.run_id, now);
    let id = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "send",
            payload,
            bound.clone(),
        )
        .await
        .unwrap();
    let operation = broker.prepare_operation(&id, "send", None).await.unwrap();
    broker.decide_bound(&id, &context(), true).await.unwrap();
    let claim = broker
        .claim_operation(&operation, &bound, "runner", 300)
        .await
        .unwrap();
    let blocker = Connection::open(home.path().join("approvals.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    WRITER.with(|slot| *slot.borrow_mut() = Some((blocker, clock.clock.clone(), expiry)));
    broker.store.conn.lock().await.busy_handler(Some(expire_and_release)).unwrap();
    let result = broker.begin_execution(&claim, &bound).await;
    broker.store.conn.lock().await.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let released = RELEASE.with(|slot| slot.borrow_mut().take());
    assert_eq!(released, Some(Ok(())), "execution must hit the real SQLite writer lock before expiry: {result:?}");
    assert_eq!(result.unwrap_err(), "human operation authority revoked or expired");
    assert_eq!(
        broker.list_operations().await.unwrap()[0].state,
        OperationState::Prepared
    );
}

#[tokio::test]
async fn dashboard_fallback_cannot_outlive_identity_store_or_admin_revocation() {
    use duduclaw_auth::{UserContext, UserDb, UserRole, UserStatus};
    let home = fixture();
    let b = ApprovalBroker::open(home.path()).unwrap();
    assert!(
        b.require_current_dashboard_role(&UserContext::admin_fallback(), UserRole::Admin)
            .is_ok()
    );
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db
        .create_user(
            "admin@example.test",
            "Admin",
            "test-password",
            UserRole::Admin,
        )
        .unwrap();
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = user.id.clone();
    assert!(
        b.require_current_dashboard_role(&ctx, UserRole::Admin)
            .is_ok()
    );
    db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
    assert!(
        b.require_current_dashboard_role(&ctx, UserRole::Admin)
            .is_err()
    );
    assert!(
        b.require_current_dashboard_role(&UserContext::admin_fallback(), UserRole::Admin)
            .is_err()
    );
    drop(db);
    std::fs::write(home.path().join("users.db"), "corrupt identity store").unwrap();
    assert!(
        b.require_current_dashboard_role(&ctx, UserRole::Admin)
            .is_err()
    );
}

#[tokio::test]
async fn corrupt_identity_store_never_downgrades_bound_channel_to_solo() {
    let home = fixture();
    let b = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"write":"one"});
    let id = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "write",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
        )
        .await
        .unwrap();
    std::fs::write(home.path().join("users.db"), "corrupt database").unwrap();
    let result =
        crate::decision_notify::route_bound_text(home.path(), &context(), &format!("確認 {id}"))
            .await
            .unwrap();
    assert!(result.is_err());
    assert_eq!(
        b.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Pending
    );
    assert!(crate::decision_notify::identity_system_active(home.path()));
    std::fs::remove_file(home.path().join("users.db")).unwrap();
    assert!(
        crate::decision_notify::route_bound_text(home.path(), &context(), &format!("確認 {id}"))
            .await
            .unwrap()
            .is_ok()
    );
}

#[tokio::test]
async fn task_recreation_never_revives_approved_claim_or_execution() {
    for changed_content in [false, true] {
        for after_claim in [false, true] {
            let home = fixture();
            let store = crate::task_store::TaskStore::open(home.path()).unwrap();
            let mut task = crate::task_store::TaskRow::new(
                "aba".into(),
                "Original".into(),
                "work".into(),
                "medium".into(),
                "alice".into(),
                "human".into(),
            );
            task.status = "pending".into();
            store.insert_task(&task).await.unwrap();
            let snapshot = store.authority_snapshot(&task.id).await.unwrap().unwrap();
            let payload = json!({"tool":"send"});
            let mut bound = binding(home.path(), &payload, "workflow_v1");
            bound.task_id = Some(task.id.clone());
            bound.task_revision = Some(snapshot.revision);
            bound.task_snapshot_hash = Some(snapshot.hash);
            let broker = ApprovalBroker::open(home.path()).unwrap();
            let id = broker
                .request_bound(
                    RequestKind::Approval,
                    "alice",
                    "work",
                    payload,
                    bound.clone(),
                )
                .await
                .unwrap();
            let op = broker.prepare_operation(&id, "work", None).await.unwrap();
            broker.decide_bound(&id, &context(), true).await.unwrap();
            let claim = if after_claim {
                Some(
                    broker
                        .claim_operation(&op, &bound, "runner", 30)
                        .await
                        .unwrap(),
                )
            } else {
                None
            };
            assert!(store.remove_task(&task.id).await.unwrap());
            drop(store);
            let store = crate::task_store::TaskStore::open(home.path()).unwrap();
            if changed_content {
                task.title = "Replacement".into();
            }
            store.insert_task(&task).await.unwrap();
            assert!(
                store
                    .authority_snapshot(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .revision
                    > snapshot.revision
            );
            let effects = std::sync::atomic::AtomicUsize::new(0);
            let result = if let Some(claim) = claim {
                broker.begin_execution(&claim, &bound).await
            } else {
                broker
                    .claim_operation(&op, &bound, "runner", 30)
                    .await
                    .map(|_| ())
            };
            if result.is_ok() {
                effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            assert!(result.unwrap_err().contains("task authority"));
            assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                broker.list_operations().await.unwrap()[0].state,
                OperationState::Prepared
            );
        }
    }
}

#[tokio::test]
async fn operation_transaction_checks_full_snapshot_even_with_same_revision() {
    let home = fixture();
    let store = crate::task_store::TaskStore::open(home.path()).unwrap();
    let mut task = crate::task_store::TaskRow::new(
        "projection".into(),
        "Original".into(),
        "work".into(),
        "medium".into(),
        "alice".into(),
        "human".into(),
    );
    task.status = "pending".into();
    store.insert_task(&task).await.unwrap();
    let snapshot = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    let payload = json!({"tool":"send"});
    let mut bound = binding(home.path(), &payload, "workflow_v1");
    bound.task_id = Some(task.id.clone());
    bound.task_revision = Some(snapshot.revision);
    bound.task_snapshot_hash = Some(snapshot.hash);
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let id = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "work",
            payload,
            bound.clone(),
        )
        .await
        .unwrap();
    let op = broker.prepare_operation(&id, "work", None).await.unwrap();
    broker.decide_bound(&id, &context(), true).await.unwrap();
    let claim = broker
        .claim_operation(&op, &bound, "runner", 30)
        .await
        .unwrap();
    // Corrupt a projection without its epoch to prove the transaction reads
    // the complete canonical hash, rather than depending only on triggers.
    let conn = Connection::open(home.path().join("tasks.db")).unwrap();
    conn.execute_batch("DROP TRIGGER task_authority_revision_v1;
        UPDATE tasks SET title='Corrupt projection' WHERE id='projection';")
        .unwrap();
    assert!(
        broker
            .begin_execution(&claim, &bound)
            .await
            .unwrap_err()
            .contains("task authority")
    );
}

#[tokio::test]
async fn operator_resolution_inspection_preserves_actor_reason_time_without_payload() {
    let home = fixture();
    let payload = json!({"private":"secret-action-marker"});
    let bound = binding(home.path(), &payload, "workflow_v1");
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let id = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "work",
            payload,
            bound.clone(),
        )
        .await
        .unwrap();
    let op = broker.prepare_operation(&id, "work", None).await.unwrap();
    broker.decide_bound(&id, &context(), true).await.unwrap();
    let claim = broker
        .claim_operation(&op, &bound, "runner", 30)
        .await
        .unwrap();
    broker.begin_execution(&claim, &bound).await.unwrap();
    broker
        .settle_operation(
            &claim,
            OperationState::Uncertain,
            None,
            Some("transport_lost"),
        )
        .await
        .unwrap();
    let handler = crate::handlers::MethodHandler::new(home.path().to_path_buf()).await;
    let admin = duduclaw_auth::UserContext::admin_fallback();
    let mut employee = admin.clone();
    employee.role = duduclaw_auth::UserRole::Employee;
    let params = json!({
        "operation_id": op,
        "expected_fence": claim.fence,
        "succeeded": true,
        "receipt": {"provider_id":"verified-1"},
        "reason": "provider readback verified"
    });
    assert!(matches!(
        handler
            .handle("approvals.resolve_uncertain", params.clone(), &employee)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        handler
            .handle(
                "approvals.operations",
                json!({"operation_id":op}),
                &employee
            )
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        handler
            .handle("approvals.resolve_uncertain", params.clone(), &admin)
            .await,
        crate::protocol::WsFrame::Response { ok: true, .. }
    ));
    assert!(matches!(
        handler
            .handle("approvals.resolve_uncertain", params, &admin)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    let readback = handler
        .handle("approvals.operations", json!({"operation_id":op}), &admin)
        .await;
    let crate::protocol::WsFrame::Response {
        ok: true,
        payload: Some(readback),
        ..
    } = readback
    else {
        panic!("admin operation inspect failed");
    };
    assert_eq!(
        readback["operation"]["operator_resolution"]["actor"],
        admin.user_id
    );
    assert!(!readback.to_string().contains("secret-action-marker"));
    drop(broker);
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = broker.inspect_operation(&op).await.unwrap().unwrap();
    let resolution = row.operator_resolution.as_ref().unwrap();
    assert_eq!(resolution.actor, admin.user_id);
    assert_eq!(resolution.reason, "provider readback verified");
    DateTime::parse_from_rfc3339(&resolution.at).unwrap();
    assert_eq!(row.state, OperationState::Succeeded);
    let serialized = serde_json::to_string(&row).unwrap();
    assert!(!serialized.contains("secret-action-marker"));
    assert!(!serialized.contains("payload_json"));
    assert!(
        broker
            .claim_operation(&op, &bound, "retry", 30)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bound_decision_refuses_tombstoned_origin_inside_transaction() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"write":"one"});
    let id = crate::approval::scope_turn("session", "turn", broker.request_bound(
        RequestKind::Approval, "alice", "write", payload.clone(),
        binding(home.path(), &payload, "workflow_v1"),
    )).await.unwrap();
    {
        let conn = broker.store.conn.lock().await;
        conn.execute("INSERT INTO cancelled_approval_turns(session_id, turn_id, cancelled_at) VALUES ('session','turn',?1)",
            [Utc::now().to_rfc3339()]).unwrap();
    }
    assert!(broker.decide_bound(&id, &context(), true).await.is_err());
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
}
