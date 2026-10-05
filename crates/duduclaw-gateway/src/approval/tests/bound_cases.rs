use super::*;
use crate::approval::{DecisionContext, ExecutionBinding, OperationState, RequestKind};

fn context() -> DecisionContext {
    DecisionContext {
        channel: "line".into(),
        account_id: "bot-1".into(),
        conversation_id: "group-1".into(),
        principal_id: "human-1".into(),
    }
}
fn fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[capabilities]\ncomputer_use=true\n",
    )
    .unwrap();
    home
}
/// `workflow_v1` operations are fenced on server-owned run authority in
/// `workflow.db`. These ledger cases exercise claim/fence/receipt mechanics,
/// so each binding is backed by a real fixture-trigger run: the only run
/// shape the authority check admits without an activation and grant.
fn seed_fixture_run(home: &Path, binding: &ExecutionBinding) {
    use crate::workflow::{
        CostBreakdown, CostBudget, CreatorGrantSnapshot, RunStatus, Trigger, WorkflowRun,
        WorkflowStore,
    };
    drop(WorkflowStore::open(home).unwrap());
    let trigger = Trigger::Fixture {
        fixture_id: "bound-case".into(),
        request_id: binding.run_id.clone(),
    };
    let run = WorkflowRun {
        run_id: binding.run_id.clone(),
        trigger_key: trigger.key("bound-case", 1).unwrap(),
        trigger,
        workflow_id: "bound-case".into(),
        revision: 1,
        workflow_hash: "bound-case-hash".into(),
        skill_hash: "bound-case-skill".into(),
        actor: binding.actor_principal.clone(),
        creator_grant: CreatorGrantSnapshot {
            actor: binding.actor_principal.clone(),
            allowed_tools: Default::default(),
            policy_revision: binding.policy_revision.clone(),
        },
        audience: vec![binding.decision_context.principal_id.clone()],
        task: None,
        input: Value::Null,
        input_hash: payload_hash(&Value::Null),
        input_observed_at: Utc::now().to_rfc3339(),
        policy_revision: binding.policy_revision.clone(),
        environment_hash: binding.environment_hash.clone(),
        grant: None,
        activation_id: None,
        deadline_at: binding.expires_at.clone(),
        budget: CostBudget {
            per_run_micros: 0,
            monthly_micros: 0,
            max_consecutive_failures: 1,
        },
        status: RunStatus::Running,
        cost: CostBreakdown::default(),
        error_code: None,
        created_at: Utc::now().to_rfc3339(),
        decision_context: Some(binding.decision_context.clone()),
        failure_class: None,
        cancelled_by: None,
    };
    let conn = rusqlite::Connection::open(home.join("workflow.db")).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,'running',?7)",
        rusqlite::params![
            run.run_id,
            run.trigger_key,
            run.workflow_id,
            run.revision,
            run.workflow_hash,
            serde_json::to_string(&run).unwrap(),
            run.created_at
        ],
    )
    .unwrap();
}
fn binding(home: &Path, payload: &Value, handler: &str) -> ExecutionBinding {
    let bound = unseeded_binding(home, payload, handler);
    if handler == "workflow_v1" {
        seed_fixture_run(home, &bound);
    }
    bound
}
fn unseeded_binding(home: &Path, payload: &Value, handler: &str) -> ExecutionBinding {
    ExecutionBinding {
        schema_version: 1,
        run_id: uuid::Uuid::new_v4().to_string(),
        run_origin_kind: "workflow".into(),
        actor_principal: "alice".into(),
        decision_context: context(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(payload),
        policy_revision: policy_revision(home, "alice").unwrap(),
        cwd: None,
        environment_hash: payload_hash(&json!({"runner":"fixture"})),
        file_hashes: Default::default(),
        expires_at: (Utc::now() + chrono::Duration::seconds(300)).to_rfc3339(),
        resume_handler: handler.into(),
        resume_version: 1,
    }
}

#[tokio::test]
async fn explicit_id_question_answer_is_data_and_survives_reopen() {
    let home = fixture();
    let payload = json!({"options":["A","B"]});
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(
            RequestKind::Question,
            "alice",
            "選擇方案",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
        )
        .await
        .unwrap();
    drop(b);
    let b = ApprovalBroker::open(home.path()).unwrap();
    assert!(b.decide(&id, true, "api").await.is_err());
    assert!(b.decide_bound(&id, &context(), true).await.is_err());
    assert!(b.prepare_operation(&id, "tool", None).await.is_err());
    for bad in [Value::Null, json!({"approve":true}), json!(""), json!("C")] {
        assert!(b.answer_question(&id, &context(), bad).await.is_err());
    }
    for field in 0..4 {
        let mut wrong = context();
        match field {
            0 => wrong.channel = "telegram".into(),
            1 => wrong.account_id = "other".into(),
            2 => wrong.conversation_id = "other-chat".into(),
            _ => wrong.principal_id = "other-person".into(),
        }
        assert!(b.answer_question(&id, &wrong, json!("B")).await.is_err());
    }
    assert!(
        crate::decision_notify::route_bound_text(home.path(), &context(), "B")
            .await
            .is_none()
    );
    assert!(
        crate::decision_notify::route_bound_text(home.path(), &context(), &format!("回答 {id} B"))
            .await
            .unwrap()
            .is_ok()
    );
    let row = b.get(&id).await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Answered);
    assert_eq!(row.answer, Some(json!("B")));
    assert!(!row.status.is_granted());
}
#[tokio::test]
async fn identity_dimensions_and_payload_policy_drift_refuse_decision() {
    let home = fixture();
    let p = json!({"tool":"send","args":{"to":"one"}});
    let b = ApprovalBroker::open(home.path()).unwrap();
    let bound = binding(home.path(), &p, "workflow_v1");
    let id = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "send",
            p.clone(),
            bound.clone(),
        )
        .await
        .unwrap();
    for field in 0..4 {
        let mut c = context();
        match field {
            0 => c.channel = "telegram".into(),
            1 => c.account_id = "bot-2".into(),
            2 => c.conversation_id = "group-2".into(),
            _ => c.principal_id = "human-2".into(),
        };
        assert!(b.decide_bound(&id, &c, true).await.is_err());
    }
    assert!(
        crate::decision_notify::route_press(
            home.path(),
            "line",
            "human-1",
            &crate::decision_action::encode(
                crate::decision_action::DecisionSource::Approval,
                crate::decision_action::DecisionAct::Approve,
                id.as_str()
            )
        )
        .await
        .unwrap()
        .is_err()
    );
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[capabilities]\ncomputer_use=false\n",
    )
    .unwrap();
    assert!(b.decide_bound(&id, &context(), true).await.is_err());
    assert_eq!(
        b.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Pending
    );
}
#[tokio::test]
async fn legacy_ttl_cas_denies_even_without_expiry_sweep() {
    let b = broker();
    let id = b
        .request("alice", "mcp_tool", "old", json!({}), 1)
        .await
        .unwrap();
    b.store
        .conn
        .lock()
        .await
        .execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",
            params![id.as_str()],
        )
        .unwrap();
    assert!(b.decide(&id, true, "human").await.is_err());
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Expired);
}
#[tokio::test]
async fn two_connections_only_one_decision_claim_and_fenced_receipt() {
    let home = fixture();
    let p = json!({"send":"one"});
    let bound = binding(home.path(), &p, "workflow_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "send", p, bound.clone())
        .await
        .unwrap();
    let op = b
        .prepare_operation(&id, "send", Some("provider-key-1"))
        .await
        .unwrap();
    let other = ApprovalBroker::open(home.path()).unwrap();
    let ctx = context();
    let (a, z) = tokio::join!(
        b.decide_bound(&id, &ctx, true),
        other.decide_bound(&id, &ctx, false)
    );
    assert_ne!(a.is_ok(), z.is_ok());
    if a.is_err() {
        return;
    } // Both legal decisions race; deny cannot execute.
    let (a, z) = tokio::join!(
        b.claim_operation(&op, &bound, "runner-1", 30),
        other.claim_operation(&op, &bound, "runner-2", 30)
    );
    assert_ne!(a.is_ok(), z.is_ok());
    let first = a.or(z).unwrap();
    b.store
        .conn
        .lock()
        .await
        .execute(
            "UPDATE approval_operations SET lease_until=0 WHERE operation_id=?1",
            params![op],
        )
        .unwrap();
    let fresh = other
        .claim_operation(&op, &bound, "runner-3", 30)
        .await
        .unwrap();
    assert!(fresh.fence > first.fence);
    assert!(b.begin_execution(&first, &bound).await.is_err());
    other.begin_execution(&fresh, &bound).await.unwrap();
    assert!(
        b.settle_operation(
            &first,
            OperationState::Succeeded,
            Some(json!({"receipt":"stale"})),
            None
        )
        .await
        .is_err()
    );
    assert!(
        b.settle_operation(&fresh, OperationState::Succeeded, None, None)
            .await
            .is_err()
    );
    other
        .settle_operation(
            &fresh,
            OperationState::Succeeded,
            Some(json!({"provider":"receipt-1"})),
            None,
        )
        .await
        .unwrap();
    assert!(
        b.claim_operation(&op, &bound, "runner-4", 30)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn live_restart_invalidates_prepared_coordinates_without_replay() {
    let home = fixture();
    let p = json!({"click":[100,100]});
    let bound = binding(home.path(), &p, "computer_reobserve_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "click", p, bound.clone())
        .await
        .unwrap();
    let op = b.prepare_operation(&id, "click", None).await.unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    drop(b);
    let b = ApprovalBroker::open(home.path()).unwrap();
    b.invalidate_live_on_restart().await.unwrap();
    assert!(
        b.claim_operation(&op, &bound, "new-runner", 30)
            .await
            .is_err()
    );
    assert_eq!(
        b.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Invalidated
    );
}
#[tokio::test]
async fn payload_scrub_unknown_handler_and_terminal_receipt_never_grant() {
    let home = fixture();
    let p = json!({"secret":"marker"});
    let b = ApprovalBroker::open(home.path()).unwrap();
    let mut bound = binding(home.path(), &p, "workflow_v1");
    bound.resume_handler = "shell_anything".into();
    assert!(
        b.request_bound(
            RequestKind::Approval,
            "alice",
            "x",
            p.clone(),
            bound.clone()
        )
        .await
        .is_err()
    );
    bound.resume_handler = "workflow_v1".into();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "x", p, bound)
        .await
        .unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    b.replace_text(&id, "removed", &json!({})).await.unwrap();
    assert_eq!(
        b.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Invalidated
    );
}

// Real test subprocess exits at a committed side-effect boundary. The parent
// observes only the on-disk ledger and provider read-back after that crash.
#[test]
fn operation_crash_child() {
    let Ok(home) = std::env::var("DUDUCLAW_OPERATION_CRASH_HOME") else {
        return;
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let home = Path::new(&home);
        let p = json!({"external":"write"});
        let bound = binding(home, &p, "workflow_v1");
        let b = ApprovalBroker::open(home).unwrap();
        let id = b
            .request_bound(RequestKind::Approval, "alice", "write", p, bound.clone())
            .await
            .unwrap();
        let op = b
            .prepare_operation(&id, "write", Some("stable-key"))
            .await
            .unwrap();
        b.decide_bound(&id, &context(), true).await.unwrap();
        let claim = b.claim_operation(&op, &bound, "child", 1).await.unwrap();
        b.begin_execution(&claim, &bound).await.unwrap();
        std::fs::write(
            home.join("provider-receipt.json"),
            json!({"key":"stable-key","calls":1,"operation_id":op}).to_string(),
        )
        .unwrap();
        std::process::exit(91);
    });
}
#[tokio::test]
async fn subprocess_provider_succeeded_receipt_not_committed_stays_uncertain_until_readback() {
    let home = fixture();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "approval::tests::bound_cases::operation_crash_child",
            "--nocapture",
        ])
        .env("DUDUCLAW_OPERATION_CRASH_HOME", home.path())
        .status()
        .unwrap();
    assert_eq!(child.code(), Some(91));
    let b = ApprovalBroker::open(home.path()).unwrap();
    b.store
        .conn
        .lock()
        .await
        .execute("UPDATE approval_operations SET lease_until=0", [])
        .unwrap();
    b.recover_operations().await.unwrap();
    let op = b.list_operations().await.unwrap().pop().unwrap();
    assert_eq!(op.state, OperationState::Uncertain);
    assert!(
        b.claim_operation(&op.operation_id, &op.binding, "restarted", 30)
            .await
            .is_err()
    );
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("provider-receipt.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["calls"], 1);
    b.resolve_uncertain(
        &op.operation_id,
        op.fence,
        true,
        receipt,
        "admin",
        "provider read-back verified",
    )
    .await
    .unwrap();
    assert_eq!(
        b.list_operations().await.unwrap()[0].state,
        OperationState::Succeeded
    );
}
#[tokio::test]
async fn unknown_provider_uncertain_is_never_automatically_retried() {
    let home = fixture();
    let p = json!({"write":1});
    let bound = binding(home.path(), &p, "workflow_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "write", p, bound.clone())
        .await
        .unwrap();
    let op = b.prepare_operation(&id, "write", None).await.unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    let c = b.claim_operation(&op, &bound, "runner", 30).await.unwrap();
    b.begin_execution(&c, &bound).await.unwrap();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst); // Fake provider accepted the write before its connection closed.
    b.settle_operation(
        &c,
        OperationState::Uncertain,
        None,
        Some("connection_closed_after_send"),
    )
    .await
    .unwrap();
    drop(b);
    let b = ApprovalBroker::open(home.path()).unwrap();
    assert!(b.claim_operation(&op, &bound, "again", 30).await.is_err());
    if let Ok(retry) = b.claim_operation(&op, &bound, "retry-provider", 30).await {
        b.begin_execution(&retry, &bound).await.unwrap();
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        b.resolve_uncertain(&op, c.fence, true, Value::Null, "admin", "guess")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn task_deadline_is_rechecked_inside_claim_and_execution_transaction() {
    let home = fixture();
    let store = crate::task_store::TaskStore::open(home.path()).unwrap();
    let mut task = crate::task_store::TaskRow::new(
        "deadline".into(),
        "Deadline task".into(),
        "do work".into(),
        "medium".into(),
        "alice".into(),
        "human".into(),
    );
    task.status = "pending".into();
    task.deadline_at = Some((Utc::now() + chrono::Duration::seconds(2)).to_rfc3339());
    store.insert_task(&task).await.unwrap();
    let snap = store.authority_snapshot("deadline").await.unwrap().unwrap();
    let p = json!({"write":1});
    let mut bound = binding(home.path(), &p, "workflow_v1");
    bound.task_id = Some(snap.task_id);
    bound.task_revision = Some(snap.revision);
    bound.task_snapshot_hash = Some(snap.hash);
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "work", p, bound.clone())
        .await
        .unwrap();
    let op = b.prepare_operation(&id, "work", None).await.unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    let claim = b.claim_operation(&op, &bound, "runner", 30).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert!(
        b.begin_execution(&claim, &bound)
            .await
            .unwrap_err()
            .contains("deadline")
    );
    assert!(
        b.claim_operation(&op, &bound, "other", 30)
            .await
            .unwrap_err()
            .contains("deadline")
    );
}
#[tokio::test]
async fn corrupt_non_null_binding_never_downgrades_to_legacy_authority() {
    let b = broker();
    let id = b
        .request("alice", "mcp_tool", "legacy", json!({}), 300)
        .await
        .unwrap();
    b.store
        .conn
        .lock()
        .await
        .execute(
            "UPDATE approvals SET binding_json='{broken' WHERE id=?1",
            params![id.as_str()],
        )
        .unwrap();
    assert!(b.get(&id).await.is_err());
    assert!(b.decide(&id, true, "human").await.is_err());
    let conn = b.store.conn.lock().await;
    let status: String = conn
        .query_row(
            "SELECT status FROM approvals WHERE id=?1",
            params![id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}
mod store_identity_cases;
mod preset_policy_cases;
mod resume_cases;
mod f1b_cases;
