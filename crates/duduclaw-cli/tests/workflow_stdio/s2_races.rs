//! Structural source fixture; real activation/operation broker decisions and real CLI effects.
//! This does not replace the A-service five-fixture producer acceptance journey.
use super::*;
use duduclaw_gateway::approval::{
    AcceptedRevisionGrant, EffectTemplate, GrantSpec, OperationState,
};
use std::os::unix::fs::PermissionsExt;

pub(super) async fn accept_activation(
    home: &Path,
    revision: &mut ApprovedWorkflowRevision,
    run: &WorkflowRun,
    tuning: &GrantTuning,
) -> AcceptedRevisionGrant {
    let scoped_task = match &tuning.scope_task_id {
        Some(id) => Value::String(id.clone()),
        None => run.input["task_id"].clone(),
    };
    let context = run.decision_context.clone().unwrap();
    let spec = GrantSpec {
        schema_version: 1,
        workflow_id: run.workflow_id.clone(),
        workflow_revision: run.revision,
        revision_hash: revision.revision_hash.clone(),
        skill_hash: run.skill_hash.clone(),
        fixtures_digest: revision.fixtures_digest.clone(),
        activation_id: uuid::Uuid::new_v4().to_string(),
        actor: run.actor.clone(),
        operator_context: context.clone(),
        creator_grant: run.creator_grant.clone(),
        audience: run.audience.clone(),
        templates: BTreeMap::from([(
            "fixed".into(),
            EffectTemplate {
                step_id: "effect".into(),
                tool: "tasks_update".into(),
                input_schema: object_schema(&run.input),
                resource_scope: BTreeMap::from([("task_id".into(), scoped_task)]),
                receipt_adapter_version: 1,
            },
        )]),
        input_max_age_seconds: tuning.input_max_age_seconds,
        fixtures_expires_at: revision.expires_at.clone(),
        budget: run.budget.clone(),
        expires_at: revision.expires_at.clone(),
        policy_revision: run.policy_revision.clone(),
    };
    let payload = json!({
        "kind": "workflow_activation",
        "activation_id": spec.activation_id,
        "spec_hash": spec.hash(),
        "revision_hash": spec.revision_hash,
        "fixtures_digest": spec.fixtures_digest
    });
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: uuid::Uuid::new_v4().to_string(),
        run_origin_kind: "workflow".into(),
        actor_principal: run.actor.clone(),
        decision_context: context.clone(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&payload),
        policy_revision: run.policy_revision.clone(),
        cwd: None,
        environment_hash: run.environment_hash.clone(),
        file_hashes: BTreeMap::new(),
        expires_at: run.deadline_at.clone(),
        resume_handler: "workflow_v1".into(),
        resume_version: 1,
    };
    let broker = ApprovalBroker::open(home).unwrap();
    let acceptance = broker
        .request_bound(
            RequestKind::Approval,
            &run.actor,
            "Accept exact race-test revision",
            payload,
            binding,
        )
        .await
        .unwrap();
    // U3 (F1b): an activation is accepted by a current Admin in the dashboard.
    let _ = &context;
    let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let admin = match users.get_user_by_email("s2-admin@test.invalid").unwrap() {
        Some(user) => user,
        None => users
            .create_user(
                "s2-admin@test.invalid",
                "S2 admin",
                "isolated-test-password",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap(),
    };
    let admin_ctx = duduclaw_auth::UserContext {
        user_id: admin.id,
        email: admin.email,
        role: duduclaw_auth::UserRole::Admin,
        agent_access: Default::default(),
        must_change_password: false,
    };
    broker
        .decide_bound_dashboard(&acceptance, &admin_ctx, true)
        .await
        .unwrap();
    revision.acceptance_id = acceptance.to_string();
    AcceptedRevisionGrant {
        activation_id: spec.activation_id.clone(),
        acceptance_id: acceptance.to_string(),
        revision: revision.clone(),
        spec_hash: spec.hash(),
        spec,
    }
}

async fn auto_ticket(home: &Path, session: &Session, prepare: &PrepareTicket) -> ExecuteTicket {
    let store = WorkflowStore::open(home).unwrap();
    let run = store
        .get_run(&prepare.effective.run_id)
        .await
        .unwrap()
        .unwrap();
    let payload =
        json!({"name":prepare.effective.tool,"arguments":prepare.effective.effective_arguments});
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: run.run_id,
        run_origin_kind: "workflow".into(),
        actor_principal: run.actor,
        decision_context: run.decision_context.unwrap(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&payload),
        policy_revision: prepare.effective.policy_revision.clone(),
        cwd: Some(
            std::fs::canonicalize(home.join("agents/alice"))
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        environment_hash: prepare.effective.environment_hash.clone(),
        file_hashes: BTreeMap::new(),
        expires_at: prepare.effective.expires_at.clone(),
        resume_handler: "workflow_v1".into(),
        resume_version: 1,
    };
    let broker = ApprovalBroker::open(home).unwrap();
    let operation_id = broker
        .prepare_granted_operation(
            run.grant.as_ref().unwrap(),
            &prepare.effective.step_key,
            &payload,
            &binding,
            None,
        )
        .await
        .unwrap();
    let mut ticket = ExecuteTicket {
        version: 1,
        session_id: session.id.clone(),
        operation_id,
        binding_digest: workflow_mcp::digest(&binding).unwrap(),
        prepare_digest: workflow_mcp::digest(prepare).unwrap(),
        expires_at: binding.expires_at,
        mac: String::new(),
    };
    ticket.mac = workflow_mcp::sign(
        &session.secret,
        "execute",
        &workflow_mcp::unsigned(&ticket).unwrap(),
    )
    .unwrap();
    ticket
}

pub(super) fn checkpoint(home: &Path, phase: &str, operation: &str) -> std::path::PathBuf {
    let dir = home.join(".workflow-test-checkpoint");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.join("request"), format!("{phase}:{operation}")).unwrap();
    std::fs::set_permissions(dir.join("request"), std::fs::Permissions::from_mode(0o600)).unwrap();
    dir
}
pub(super) async fn reached(dir: &Path, phase: &str, operation: &str) {
    // Waiting bound only: the debug CLI needed >15 s to reach a boundary when
    // the suite ran in parallel. The CLI side still pauses at most 30 s.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if std::fs::read_to_string(dir.join(phase)).is_ok_and(|s| s == operation) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("CLI did not reach requested authority boundary");
}
pub(super) fn denied<T: std::fmt::Debug>(result: Result<T, McpError>, expected: &str) {
    match result {
        Err(McpError::Rpc { code, message }) => {
            assert_eq!(code, -32003);
            assert_eq!(message, expected);
        }
        other => panic!("expected exact authority RPC denial, got {other:?}"),
    }
}
pub(super) fn effects(home: &Path) -> i64 {
    rusqlite::Connection::open(home.join("tasks.db"))
        .unwrap()
        .query_row("SELECT n FROM s2_effect_count", [], |r| r.get(0))
        .unwrap()
}
async fn race(mode: &str, phase: &str) {
    let (home, key) = setup();
    let config = match mode {
        "ask" => {
            "[[capabilities.policy]]\ntool='tasks_update'\neffect='ask'\n[[capabilities.policy]]\ntool='tasks_create'\neffect='allow'\n"
        }
        "static" => "approval_required_tools=['tasks_update']\n",
        _ => "",
    };
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        format!("[agent]\nname='alice'\nrole='fixture'\n[capabilities]\nautonomy_level='auto'\n{config}"),
    )
    .unwrap();
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "before-race").await;
    let args = json!({"task_id":id,"title":"after-race"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed_material(
        home.path(),
        &alice,
        "tasks_update",
        &args,
        false,
        vec![],
        true,
    )
    .await;
    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    assert_eq!(
        prepare.effective.approval_requirements,
        match mode {
            "ask" => vec!["policy_ask"],
            "static" => vec!["action_guard_require_approval"],
            _ => vec![],
        }
    );
    let ticket = if mode == "auto" {
        auto_ticket(home.path(), &alice, &prepare).await
    } else {
        authorized_ticket(home.path(), &alice, &prepare).await
    };
    let operation = ticket.operation_id.clone();
    let db = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
    db.execute_batch("CREATE TABLE s2_effect_count(n INTEGER);
        INSERT INTO s2_effect_count VALUES(0);
        CREATE TRIGGER s2_count AFTER UPDATE OF title ON tasks WHEN NEW.title='after-race'
        BEGIN UPDATE s2_effect_count SET n=n+1; END;")
        .unwrap();
    drop(db);
    let dir = checkpoint(home.path(), phase, &operation);
    let retry = ticket.clone();
    let retry_call = call.clone();
    let execution = tokio::spawn(async move {
        let result = alice.client.execute_workflow_call(call, ticket).await;
        (alice, result)
    });
    reached(&dir, phase, &operation).await;
    let second = ApprovalBroker::open(home.path()).unwrap();
    let stored = WorkflowStore::open(home.path())
        .unwrap()
        .get_run(&run)
        .await
        .unwrap()
        .unwrap();
    let revoked = second
        .revoke_workflow_activation(
            stored.activation_id.as_ref().unwrap(),
            &stored.grant.as_ref().unwrap().spec_hash,
            "race owner revoke",
        )
        .await
        .unwrap()
        .unwrap();
    if phase == "after_begin" {
        assert!(revoked.executing_operations.contains(&operation));
    } else {
        assert!(!revoked.executing_operations.contains(&operation));
    }
    assert_eq!(
        effects(home.path()),
        0,
        "no original handler effect before release"
    );
    std::fs::write(dir.join("release"), b"resume").unwrap();
    let (mut alice, result) = tokio::time::timeout(Duration::from_secs(15), execution)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    let row = second.inspect_operation(&operation).await.unwrap().unwrap();
    let task = duduclaw_gateway::task_store::TaskStore::open(home.path())
        .unwrap()
        .get_task(&id)
        .await
        .unwrap()
        .unwrap();
    if phase == "after_begin" {
        assert_eq!(result.unwrap().state, "succeeded");
        assert_eq!(row.state, OperationState::Succeeded);
        assert!(row.receipt.is_some());
        assert_eq!(effects(home.path()), 1);
        assert_eq!(task.title, "after-race");
        // Replay may be rejected or return the retained result; neither may execute twice.
        let replay = alice.client.execute_workflow_call(retry_call, retry).await;
        assert!(
            matches!(replay, Ok(_) | Err(McpError::Rpc { .. })),
            "transport failure cannot prove replay suppression"
        );
        assert_eq!(effects(home.path()), 1);
        assert_eq!(
            second
                .inspect_operation(&operation)
                .await
                .unwrap()
                .unwrap()
                .receipt,
            row.receipt
        );
    } else {
        denied(result, "workflow activation authoritatively revoked");
        assert_ne!(row.state, OperationState::Executing);
        assert!(row.receipt.is_none());
        assert_eq!(effects(home.path()), 0);
        assert_eq!(task.title, "before-race");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_s2_revocation_before_claim_and_begin() {
    for mode in ["auto", "ask", "static"] {
        for phase in ["before_claim", "before_begin"] {
            race(mode, phase).await;
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_s2_begin_winner_keeps_real_receipt_without_replay() {
    for mode in ["auto", "ask", "static"] {
        race(mode, "after_begin").await;
    }
}
