use super::*;
use crate::handlers::push_filter::filter_push_event;

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    handler: MethodHandler,
    db: UserDb,
    manager: UserContext,
    employee: UserContext,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let manager = db
        .create_user(
            "manager@test.invalid",
            "Manager",
            "test-password-only",
            UserRole::Manager,
        )
        .unwrap();
    let employee = db
        .create_user(
            "employee@test.invalid",
            "Employee",
            "test-password-only",
            UserRole::Employee,
        )
        .unwrap();
    let context = |user: &duduclaw_auth::User| {
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = user.id.clone();
        ctx.role = user.role;
        ctx.email = user.email.clone();
        ctx
    };
    let handler = MethodHandler::new(home.clone()).await;
    Fixture {
        _dir: dir,
        home,
        handler,
        manager: context(&manager),
        employee: context(&employee),
        db,
    }
}

fn ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected success: {other:?}"),
    }
}

#[tokio::test]
async fn autonomy_rpc_requires_live_manager_and_operator_binding() {
    let f = fixture().await;
    for method in ["autonomy.pause", "autonomy.resume"] {
        let request = json!({"agent_id": "sales"});
        assert!(!ok(&f
            .handler
            .handle(method, request.clone(), &f.manager)
            .await));
        f.db.bind_agent(&f.manager.user_id, "sales", AccessLevel::Viewer)
            .unwrap();
        assert!(!ok(&f
            .handler
            .handle(method, request.clone(), &f.manager)
            .await));
        f.db.bind_agent(&f.manager.user_id, "sales", AccessLevel::Operator)
            .unwrap();
        assert!(ok(&f
            .handler
            .handle(method, request.clone(), &f.manager)
            .await));
        f.db.bind_agent(&f.employee.user_id, "sales", AccessLevel::Owner)
            .unwrap();
        assert!(!ok(&f
            .handler
            .handle(method, request.clone(), &f.employee)
            .await));
        // Even a cached, previously valid Owner grant cannot survive revocation.
        let mut stale = f.manager.clone();
        stale
            .agent_access
            .insert("sales".into(), AccessLevel::Owner);
        f.db.unbind_agent(&f.manager.user_id, "sales").unwrap();
        assert!(!ok(&f.handler.handle(method, request, &stale).await));
    }
}

#[tokio::test]
async fn autonomy_rpc_global_requires_admin_and_target_is_exclusive() {
    let f = fixture().await;
    let admin = UserContext::admin_fallback();
    for method in ["autonomy.pause", "autonomy.resume"] {
        assert!(!ok(&f
            .handler
            .handle(method, json!({"all":true}), &f.manager)
            .await));
        assert!(ok(&f
            .handler
            .handle(method, json!({"all":true}), &admin)
            .await));
        for invalid in [
            json!({}),
            json!({"all":false}),
            json!({"agent_id":"sales", "all":true}),
            json!({"agent_id":"sales", "all":false}),
            json!({"agent_id":"../sales"}),
            json!({"agent_id":""}),
            json!({"agent_id":null}),
            json!({"all":"true"}),
        ] {
            assert!(
                !ok(&f.handler.handle(method, invalid.clone(), &admin).await),
                "{invalid}"
            );
        }
    }
}

#[tokio::test]
async fn autonomy_rpc_status_and_mutations_do_not_leak_other_agents() {
    let f = fixture().await;
    let admin = UserContext::admin_fallback();
    f.db.bind_agent(&f.manager.user_id, "sales", AccessLevel::Operator)
        .unwrap();
    assert!(ok(&f
        .handler
        .handle(
            "autonomy.pause",
            json!({"agent_id":"private", "reason":"private-reason"}),
            &admin
        )
        .await));
    let changed = payload(
        f.handler
            .handle(
                "autonomy.pause",
                json!({"agent_id":"sales", "reason":"檢查中🐾"}),
                &f.manager,
            )
            .await,
    );
    assert!(!changed.to_string().contains("private-reason"));
    assert_eq!(changed["pause"]["who"], f.manager.user_id);
    assert_eq!(changed["pause"]["reason"], "檢查中🐾");
    let state = payload(
        f.handler
            .handle("autonomy.status", json!({}), &f.manager)
            .await,
    );
    assert!(state["agents"].get("sales").is_some());
    assert!(state["agents"].get("private").is_none());
    let all = payload(f.handler.handle("autonomy.status", json!({}), &admin).await);
    assert!(all["agents"].get("private").is_some());
    f.db.unbind_agent(&f.manager.user_id, "sales").unwrap();
    let state = payload(
        f.handler
            .handle("autonomy.status", json!({}), &f.manager)
            .await,
    );
    assert_eq!(state["agents"], json!({}));
    let employee = payload(
        f.handler
            .handle("autonomy.status", json!({}), &f.employee)
            .await,
    );
    assert_eq!(employee["agents"], json!({}));
}

#[tokio::test]
async fn autonomy_rpc_corruption_reports_error_and_refuses_mutation() {
    let f = fixture().await;
    std::fs::write(f.home.join("autonomy_pause.json"), "{").unwrap();
    let state = payload(
        f.handler
            .handle("autonomy.status", json!({}), &f.employee)
            .await,
    );
    assert!(state["state_error"].as_str().is_some_and(|s| !s.is_empty()));
    for method in ["autonomy.pause", "autonomy.resume"] {
        assert!(!ok(&f
            .handler
            .handle(method, json!({"all":true}), &UserContext::admin_fallback())
            .await));
    }
    assert_eq!(
        std::fs::read_to_string(f.home.join("autonomy_pause.json")).unwrap(),
        "{"
    );
}

#[tokio::test]
async fn autonomy_rpc_push_is_scoped_and_success_is_audited() {
    let f = fixture().await;
    f.db.bind_agent(&f.manager.user_id, "sales", AccessLevel::Viewer)
        .unwrap();
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    f.handler.set_event_tx(tx).await;
    let admin = UserContext::admin_fallback();
    for method in ["autonomy.pause", "autonomy.resume"] {
        assert!(ok(&f
            .handler
            .handle(method, json!({"agent_id":"sales"}), &admin)
            .await));
        let line = rx.try_recv().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["event"],
            "autonomy.changed"
        );
        let event = serde_json::from_str::<Value>(&line).unwrap();
        let event_payload = event["payload"].as_object().unwrap();
        assert_eq!(event_payload.len(), 4);
        for key in ["agent_id", "all", "paused", "at"] {
            assert!(event_payload.contains_key(key));
        }
        for key in ["pause", "reason", "who", "actor", "audit_error"] {
            assert!(!event_payload.contains_key(key));
        }
        assert!(filter_push_event(&f.home, &f.manager, &line).is_some());
        assert!(filter_push_event(&f.home, &f.employee, &line).is_none());
    }
    assert!(ok(&f
        .handler
        .handle("autonomy.pause", json!({"all":true}), &admin)
        .await));
    let line = rx.try_recv().unwrap();
    assert!(filter_push_event(&f.home, &f.employee, &line).is_some());
    let invalid = json!({"event":"autonomy.changed", "payload":{}}).to_string();
    assert!(filter_push_event(&f.home, &f.employee, &invalid).is_none());
    let mut preauth = crate::handlers::push_filter::PushGate::new(&f.home, &admin, true);
    assert!(preauth.filter(&line).is_none());
    let audit = std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("autonomy_paused"));
    assert!(audit.contains("autonomy_resumed"));
}

#[tokio::test]
async fn autonomy_rpc_state_survives_handler_restart() {
    let f = fixture().await;
    let admin = UserContext::admin_fallback();
    assert!(ok(&f
        .handler
        .handle(
            "autonomy.pause",
            json!({"agent_id":"sales", "reason":"restart"}),
            &admin
        )
        .await));
    let restarted = MethodHandler::new(f.home.clone()).await;
    let state = payload(restarted.handle("autonomy.status", json!({}), &admin).await);
    assert_eq!(state["agents"]["sales"]["reason"], "restart");
    assert!(ok(&restarted
        .handle("autonomy.resume", json!({"agent_id":"sales"}), &admin)
        .await));
    assert_eq!(autonomy_gate::status(&f.home).agents.len(), 0);
}

#[tokio::test]
async fn autonomy_rpc_audit_failure_leaves_state_unchanged() {
    let f = fixture().await;
    let admin = UserContext::admin_fallback();
    autonomy_gate::set_pause(&f.home, None, "maintenance", "admin").unwrap();
    let audit = f.home.join("security_audit.jsonl");
    // A directory is deterministically unwritable as the audit log, even as root.
    if audit.exists() {
        std::fs::remove_file(&audit).unwrap();
    }
    std::fs::create_dir(&audit).unwrap();
    for method in ["autonomy.resume", "autonomy.pause"] {
        let before = std::fs::read(f.home.join(autonomy_gate::STATE_FILE)).unwrap();
        assert!(!ok(&f.handler.handle(method, json!({"all": true}), &admin).await));
        assert_eq!(std::fs::read(f.home.join(autonomy_gate::STATE_FILE)).unwrap(), before);
    }
}

#[test]
fn autonomy_change_event_has_only_scope_boolean_and_timestamp() {
    for scope in [Some("alice"), None] {
        let event = change_event(scope, true);
        let object = event.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert_eq!(event["agent_id"], json!(scope));
        assert_eq!(event["all"], scope.is_none());
        assert_eq!(event["paused"], true);
        assert!(chrono::DateTime::parse_from_rfc3339(event["at"].as_str().unwrap()).is_ok());
        for key in ["pause", "reason", "who", "actor"] {
            assert!(object.get(key).is_none());
        }
    }
}

#[tokio::test]
async fn autonomy_requested_audit_distinguishes_employee_named_all_from_global() {
    let f = fixture().await;
    let admin = UserContext::admin_fallback();
    for params in [json!({"agent_id": "all"}), json!({"all": true})] {
        assert!(ok(&f.handler.handle("autonomy.pause", params, &admin).await));
    }
    let audit = std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap();
    let requested: Vec<Value> = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row.to_string().contains("autonomy_change_requested")).collect();
    assert_eq!(requested.len(), 2);
    assert_eq!(requested[0]["details"]["agent_id"], "all");
    assert_eq!(requested[0]["details"]["all"], false);
    assert_eq!(requested[1]["details"]["agent_id"], Value::Null);
    assert_eq!(requested[1]["details"]["all"], true);
}

#[tokio::test]
async fn autonomy_rpc_reports_and_audits_applied_but_durability_unknown() {
    let f = fixture().await;
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    f.handler.set_event_tx(tx.clone()).await;
    let entry = PauseEntry { reason: "maintenance".into(), who: "admin".into(),
        at: chrono::Utc::now().to_rfc3339() };
    let receipt = ChangeReceipt::new(Some("sales"), Some(&entry), "admin").unwrap();
    assert!(matches!(autonomy_gate::mutate_tracked_async(&f.home, Some(entry), receipt).await,
        MutationOutcome::Applied(_)));
    let receipt = autonomy_gate::change_receipts(&f.home).unwrap().remove(0);
    let state = autonomy_gate::status(&f.home);
    let outcome = MutationOutcome::AppliedButDurabilityUnknown(
        state, std::io::Error::other("injected post-rename fsync failure"),
    );
    let reply = payload(finish_change(&f.home, Some(&tx), &receipt, outcome));
    assert_eq!(reply["applied"], true);
    assert_eq!(reply["paused"], true);
    assert_eq!(reply["durability_warning"], "applied_durability_unknown");
    let pushed: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(pushed["payload"]["paused"], true);
    assert!(pushed["payload"].get("pause").is_none());
    let audit = std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap();
    let receipt: Value = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
    assert_eq!(receipt["event_type"], "autonomy_paused");
    assert_eq!(receipt["details"]["scope"]["durability_warning"], "applied_durability_unknown");
}


#[tokio::test]
async fn cancelled_autonomy_rpc_still_completes_audit_and_push() {
    let f = fixture().await;
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    f.handler.set_event_tx(tx).await;
    let admission = autonomy_gate::admit_global(
        &f.home, duduclaw_core::autonomy_gate::AutonomyPath::Background,
    ).await.unwrap();
    let handler = Arc::new(f.handler);
    let request_handler = handler.clone();
    let rpc = tokio::spawn(async move {
        request_handler.handle("autonomy.pause", json!({"all": true}),
            &UserContext::admin_fallback()).await
    });
    // Hold the writer before rename until the connection has gone away.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let audit = std::fs::read_to_string(f.home.join("security_audit.jsonl"))
                .unwrap_or_default();
            if audit.contains("autonomy_change_requested") { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    rpc.abort();
    assert!(rpc.await.unwrap_err().is_cancelled());
    drop(admission);
    let pushed = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await.unwrap().unwrap();
    let pushed: Value = serde_json::from_str(&pushed).unwrap();
    assert_eq!(pushed["event"], "autonomy.changed");
    assert_eq!(pushed["payload"]["paused"], true);
    assert!(autonomy_gate::status(&f.home).global.is_some());
    let audit = duduclaw_security::audit::read_recent_events(&f.home, 100);
    let requested = audit.iter().find(|row| row.event_type == "autonomy_change_requested").unwrap();
    let completed = audit.iter().find(|row| row.event_type == "autonomy_paused").unwrap();
    assert_eq!(requested.details["change_id"], completed.details["change_id"]);
    assert_eq!(completed.details["reconciled"], false);
    assert_eq!(requested.details["request_digest"], completed.details["request_digest"]);
    // Push precedes the final locked acknowledgement; wait for that last write.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let receipts = autonomy_gate::change_receipts(&f.home).unwrap();
            if receipts.len() == 1 && receipts[0].completion_recorded { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
}

async fn applied_receipt(home: &Path, agent: Option<&str>, paused: bool) -> ChangeReceipt {
    let entry = paused.then(|| PauseEntry { reason: "test".into(), who: "admin".into(),
        at: chrono::Utc::now().to_rfc3339() });
    let receipt = ChangeReceipt::new(agent, entry.as_ref(), "admin").unwrap();
    let id = receipt.change_id.clone();
    assert!(matches!(autonomy_gate::mutate_tracked_async(home, entry, receipt).await,
        MutationOutcome::Applied(_)));
    autonomy_gate::change_receipts(home).unwrap().into_iter().find(|r| r.change_id == id).unwrap()
}

#[tokio::test]
async fn autonomy_startup_reconciles_state_receipts_once_even_if_superseded() {
    let f = fixture().await;
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    f.handler.set_event_tx(tx).await;
    // Simulate a crash after rename but before any completion audit/push.
    // No requested audit is needed to reconstruct these applied changes.
    let paused = applied_receipt(&f.home, Some("sales"), true).await;
    let resumed = applied_receipt(&f.home, Some("sales"), false).await;
    f.handler.reconcile_autonomy_changes().await;
    f.handler.reconcile_autonomy_changes().await;
    let audit = duduclaw_security::audit::read_recent_events(&f.home, 100);
    let completions: Vec<_> = audit.iter().filter(|row|
        matches!(row.event_type.as_str(), "autonomy_paused" | "autonomy_resumed")
    ).collect();
    assert_eq!(completions.len(), 2);
    for receipt in [&paused, &resumed] {
        let row = completions.iter().find(|row| row.details["change_id"] == receipt.change_id).unwrap();
        assert_eq!(row.details["reconciled"], true);
        assert_eq!(row.details["actor"], receipt.actor);
        assert_eq!(row.details["request_digest"], receipt.request_digest);
        assert_eq!(row.details["applied_at"], receipt.applied_at);
        assert_eq!(row.details["scope"]["paused"], receipt.method == ChangeMethod::Pause);
    }
    for _ in 0..2 {
        let pushed: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(pushed["payload"]["paused"], false);
    }
    assert!(rx.try_recv().is_err());
    let receipts = autonomy_gate::change_receipts(&f.home).unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|r| r.completion_recorded));
}

#[tokio::test]
async fn autonomy_forged_duplicate_audits_cannot_change_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let receipt = applied_receipt(home, Some("sales"), true).await;
    let forged_id = uuid::Uuid::new_v4().to_string();
    let forged = |id: &str, event: &str| json!({
        "event_type": event, "agent_id": "victim", "details": {
            "change_id": id, "method": "autonomy.resume", "actor": "forged",
            "agent_id": "victim", "all": false, "entry": null
        }
    }).to_string();
    // Duplicate requested rows and a fake completion with the real UUID must
    // neither alter the receipt nor suppress its real completion. An unrelated
    // UUID must never be promoted into an applied change.
    let lines = [forged(&receipt.change_id, "autonomy_change_requested"),
        forged(&receipt.change_id, "autonomy_change_requested"),
        forged(&receipt.change_id, "autonomy_resumed"),
        forged(&forged_id, "autonomy_change_requested")].join("\n") + "\n";
    std::fs::write(home.join("security_audit.jsonl"), &lines).unwrap();
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    reconcile_changes(home, Some(&tx)).unwrap();
    reconcile_changes(home, Some(&tx)).unwrap();
    let content = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    let appended: Vec<Value> = content[lines.len()..].lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0]["event_type"], "autonomy_paused");
    assert_eq!(appended[0]["details"]["actor"], "admin");
    assert_eq!(appended[0]["details"]["scope"]["agent_id"], "sales");
    assert_eq!(appended[0]["details"]["change_id"], receipt.change_id);
    assert_eq!(appended[0]["details"]["request_digest"], receipt.request_digest);
    assert!(autonomy_gate::change_receipts(home).unwrap()[0].completion_recorded);
    let pushed: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(pushed["payload"]["agent_id"], "sales");
    assert_eq!(pushed["payload"]["paused"], true);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn autonomy_completion_failure_keeps_receipts_and_retries_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let first = applied_receipt(home, None, true).await;
    let second = applied_receipt(home, Some("sales"), true).await;
    std::fs::create_dir(home.join("security_audit.jsonl")).unwrap();
    let reply = payload(finish_change(home, None, &first,
        MutationOutcome::Applied(autonomy_gate::status(home))));
    assert_eq!(reply["applied"], true);
    assert!(reply.get("audit_error").is_some());
    // Individual write failures are logged and retained, not a startup failure.
    reconcile_changes(home, None).unwrap();
    assert!(autonomy_gate::change_receipts(home).unwrap().iter().all(|r| !r.completion_recorded));
    std::fs::remove_dir(home.join("security_audit.jsonl")).unwrap();
    reconcile_changes(home, None).unwrap();
    reconcile_changes(home, None).unwrap();
    let receipts = autonomy_gate::change_receipts(home).unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|r| r.completion_recorded));
    let events = duduclaw_security::audit::read_recent_events(home, 10);
    for id in [&first.change_id, &second.change_id] {
        assert_eq!(events.iter().filter(|row| row.details["change_id"] == id.as_str()).count(), 1);
    }
}

#[tokio::test]
async fn autonomy_acknowledgement_failure_reports_applied_and_preserves_pending() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let receipt = applied_receipt(home, None, true).await;
    // Completion audit and push work, but the exclusive acknowledgement cannot
    // open its lock. This failure must not turn an applied mutation into failure.
    let lock = home.join(autonomy_gate::ADMISSION_LOCK_FILE);
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    let reply = payload(finish_change(home, Some(&tx), &receipt,
        MutationOutcome::Applied(autonomy_gate::status(home))));
    assert_eq!(reply["applied"], true);
    assert_eq!(reply["paused"], true);
    assert!(reply.get("audit_error").is_some());
    assert!(rx.try_recv().is_ok());
    assert!(!autonomy_gate::change_receipts(home).unwrap()[0].completion_recorded);
    std::fs::remove_dir(&lock).unwrap();
    reconcile_changes(home, Some(&tx)).unwrap();
    assert!(autonomy_gate::change_receipts(home).unwrap()[0].completion_recorded);
    let events = duduclaw_security::audit::read_recent_events(home, 10);
    // Audit output and the state acknowledgement are separate writes. This
    // crash/failure boundary is at-least-once, with the same receipt ID.
    assert_eq!(events.iter().filter(|row| row.details["change_id"] == receipt.change_id).count(), 2);
}
