use super::*;

#[tokio::test]
async fn workflow_stdio_real_owner_write_receipt_forgery_replay_and_foreign_owner_denial() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let mut bob = session(home.path(), "bob", &key).await;
    let id = task(&mut alice, "before").await;
    let args = json!({"task_id":id,"title":"after"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let prepare_context = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), prepare_context)
        .await
        .unwrap();
    let store = duduclaw_gateway::task_store::TaskStore::open(home.path()).unwrap();
    assert_eq!(
        store.get_task(&id).await.unwrap().unwrap().title,
        "before",
        "prepare has no effect"
    );
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    #[cfg(all(unix, not(feature = "workflow-test-checkpoints")))]
    {
        // The default binary must ignore the same private host fixture files.
        use std::os::unix::fs::PermissionsExt;
        let dir = home.path().join(".workflow-test-checkpoint");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.join("request"), &ticket.operation_id).unwrap();
        std::fs::set_permissions(dir.join("request"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    let mut forged = ticket.clone();
    forged.operation_id = "forged".into();
    assert!(
        alice
            .client
            .execute_workflow_call(call.clone(), forged)
            .await
            .is_err()
    );
    let reply = alice
        .client
        .execute_workflow_call(call.clone(), ticket.clone())
        .await
        .unwrap();
    assert_eq!(reply.state, "succeeded");
    #[cfg(not(feature = "workflow-test-checkpoints"))]
    assert!(
        !home
            .path()
            .join(".workflow-test-checkpoint/executing")
            .exists()
    );

    assert!(reply.receipt_digest.is_some());
    assert_eq!(store.get_task(&id).await.unwrap().unwrap().title, "after");
    let ledger = ApprovalBroker::open(home.path())
        .unwrap()
        .inspect_operation(&ticket.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(ledger.receipt.unwrap()["evidence"]["row_hash"].is_string());
    assert!(
        alice
            .client
            .execute_workflow_call(call.clone(), ticket.clone())
            .await
            .is_err(),
        "same ticket never repeats effect"
    );
    assert!(
        bob.client
            .execute_workflow_call(call.clone(), ticket)
            .await
            .is_err(),
        "cross-session cannot borrow ticket"
    );
    let foreign_args = json!({"task_id":id,"title":"foreign"});
    let foreign_call = json!({"name":"tasks_update","arguments":foreign_args});
    let (run, step) = seed(home.path(), &bob, "tasks_update", &foreign_args).await;
    let prepare_context = context(&bob, &run, &step, &foreign_args);
    let prepare = bob
        .client
        .prepare_workflow_call(foreign_call.clone(), prepare_context)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &bob, &prepare).await;
    let reply = bob
        .client
        .execute_workflow_call(foreign_call, ticket)
        .await
        .unwrap();
    assert_eq!(reply.state, "failed");
    assert_eq!(
        reply.error_code.as_deref(),
        Some("handler_rejected_before_effect")
    );
    assert_eq!(store.get_task(&id).await.unwrap().unwrap().title, "after");
}
#[tokio::test]
async fn workflow_stdio_real_cron_adapter_readback_and_policy_revoke_before_effect() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let cron = duduclaw_gateway::cron_store::CronStore::open(home.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    cron.insert(&duduclaw_gateway::cron_store::CronTaskRow::new(
        id.clone(),
        "fixture".into(),
        "alice".into(),
        "0 0 * * *".into(),
        "before".into(),
    ))
    .await
    .unwrap();
    let args = json!({"id":id,"task":"after"});
    register_staging(home.path(), "fixture_cron_ids", &id);
    let call = json!({"name":"update_cron_task","arguments":args});
    let (run, step) = seed(home.path(), &alice, "update_cron_task", &args).await;
    let prepare_context = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), prepare_context)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let reply = alice
        .client
        .execute_workflow_call(call.clone(), ticket)
        .await
        .unwrap();
    assert_eq!(reply.state, "succeeded");
    assert_eq!(cron.get(&id).await.unwrap().unwrap().task, "after");
    let (run, step) = seed(home.path(), &alice, "update_cron_task", &args).await;
    let prepare_context = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), prepare_context)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let agent = home.path().join("agents/alice/agent.toml");
    // F1b (E-H5a): a comment no longer counts as an authority change; a
    // permission flag does.
    let text = std::fs::read_to_string(&agent).unwrap();
    assert!(text.contains("can_schedule_tasks = true"));
    std::fs::write(
        agent,
        text.replace("can_schedule_tasks = true", "can_schedule_tasks = true\ncan_create_agents = false"),
    )
    .unwrap();
    assert!(
        alice
            .client
            .execute_workflow_call(call, ticket)
            .await
            .is_err()
    );
    assert_eq!(cron.get(&id).await.unwrap().unwrap().task, "after");
}

#[tokio::test]
async fn workflow_stdio_two_employee_namespace_isolation_uses_production_identity() {
    let (home, key) = setup();
    let (mut alice, mut bob) = tokio::join!(
        session(home.path(), "alice", &key),
        session(home.path(), "bob", &key)
    );
    let (a, b) = tokio::join!(
        alice
            .client
            .call_tool("memory_store", json!({"content":"alice-namespace-fixture"})),
        bob.client
            .call_tool("memory_store", json!({"content":"bob-namespace-fixture"}))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert!(!a.is_error);
    assert!(!b.is_error);
    let a: Value = serde_json::from_str(&a.content).unwrap();
    let b: Value = serde_json::from_str(&b.content).unwrap();
    assert_eq!(a["namespace"], "alice");
    assert_eq!(b["namespace"], "bob");
    let (a_read, b_read) = tokio::join!(
        alice.client.call_tool("memory_read", json!({"id":b["id"]})),
        bob.client.call_tool("memory_read", json!({"id":a["id"]}))
    );
    assert!(a_read.unwrap().is_error);
    assert!(b_read.unwrap().is_error);
}

#[cfg(all(unix, feature = "workflow-test-checkpoints"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_kill_after_begin_keeps_unknown_and_never_replays() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "before-kill").await;
    let args = json!({"task_id":id,"title":"after-kill"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let op_id = ticket.operation_id.clone();
    // Host-only fault-injection build: pause after the real ledger begin CAS.
    // The shipping/default binary contains no checkpoint code.
    use std::os::unix::fs::PermissionsExt;
    let checkpoint = home.path().join(".workflow-test-checkpoint");
    std::fs::create_dir(&checkpoint).unwrap();
    std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(checkpoint.join("request"), &op_id).unwrap();
    std::fs::set_permissions(
        checkpoint.join("request"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let task = tokio::spawn(async move { alice.client.execute_workflow_call(call, ticket).await });
    let broker = ApprovalBroker::open(home.path()).unwrap();
    // Waiting bound for the debug CLI to reach the after-begin pause; measured
    // >10 s even when this test runs alone. Not a latency requirement.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let op = broker.inspect_operation(&op_id).await.unwrap().unwrap();
            if op.state == duduclaw_gateway::approval::OperationState::Executing
                && std::fs::read_to_string(checkpoint.join("executing"))
                    .ok()
                    .as_deref()
                    == Some(op_id.as_str())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    std::fs::remove_dir_all(&checkpoint).unwrap();
    let operation = broker.inspect_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(
        operation.state,
        duduclaw_gateway::approval::OperationState::Executing
    );
    assert!(
        operation.receipt.is_none(),
        "missing effect receipt stays unknown"
    );
    let store = duduclaw_gateway::task_store::TaskStore::open(home.path()).unwrap();
    assert_eq!(
        store.get_task(&id).await.unwrap().unwrap().title,
        "before-kill"
    );
    let mut restarted = session(home.path(), "alice", &key).await;
    let bad = ExecuteTicket {
        version: 1,
        session_id: restarted.id.clone(),
        operation_id: op_id.clone(),
        binding_digest: workflow_mcp::digest(&operation.binding).unwrap(),
        prepare_digest: workflow_mcp::digest(&prepare).unwrap(),
        expires_at: operation.binding.expires_at.clone(),
        mac: String::new(),
    };
    let mut bad = bad;
    bad.mac = workflow_mcp::sign(
        &restarted.secret,
        "execute",
        &workflow_mcp::unsigned(&bad).unwrap(),
    )
    .unwrap();
    assert!(
        restarted
            .client
            .execute_workflow_call(json!({"name":"tasks_update","arguments":args}), bad)
            .await
            .is_err(),
        "new session cannot replay lost execution"
    );
    assert_eq!(
        broker
            .inspect_operation(&op_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        duduclaw_gateway::approval::OperationState::Executing
    );
}

/// Every task row, column for column, as SQLite literals.
fn task_rows(home: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        home.join("tasks.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('tasks') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let select = columns
        .iter()
        .map(|c| format!("quote(\"{c}\")"))
        .collect::<Vec<_>>()
        .join("||','||");
    db.prepare(&format!("SELECT {select} FROM tasks ORDER BY id"))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
#[tokio::test]
async fn workflow_stdio_native_gates_block_prepare_without_effect_or_operation() {
    for (capabilities, permissions, tool, args) in [
        (
            "denied_tools = [\"tasks_update\"]",
            "",
            "tasks_update",
            json!({"task_id":"fixture-target","title":"forbidden"}),
        ),
        (
            "allowed_tools = [\"memory_search\"]",
            "",
            "tasks_update",
            json!({"task_id":"fixture-target","title":"forbidden"}),
        ),
        (
            "scoped_tools = [\"tasks_update\"]",
            "",
            "tasks_update",
            json!({"task_id":"fixture-target","title":"forbidden"}),
        ),
        (
            "",
            "can_schedule_tasks = false",
            "update_cron_task",
            json!({"id":"fixture-cron","task":"forbidden"}),
        ),
    ] {
        let (home, key) = setup();
        std::fs::write(
            home.path().join("agents/alice/agent.toml"),
            format!("[agent]\nname=\"alice\"\nrole=\"fixture\"\n[capabilities]\n{capabilities}\n[permissions]\n{permissions}\n")
        )
        .unwrap();
        let mut alice = session(home.path(), "alice", &key).await;
        let (run, step) = seed(home.path(), &alice, tool, &args).await;
        // The structural source draft is host-seeded into tasks.db, so the
        // store already exists; prepare must leave every task row untouched.
        let tasks_before = task_rows(home.path());
        let ctx = context(&alice, &run, &step, &args);
        assert!(
            alice
                .client
                .prepare_workflow_call(json!({"name":tool,"arguments":args}), ctx)
                .await
                .is_err()
        );
        // Prepare cannot create a durable effect authority or touch a target store.
        if home.path().join("approvals.db").exists() {
            let broker = ApprovalBroker::open(home.path()).unwrap();
            assert!(broker.list_operations().await.unwrap().is_empty());
        }
        let tasks_after = task_rows(home.path());
        assert_eq!(tasks_after, tasks_before);
        assert!(!tasks_after.iter().any(|row| row.contains("fixture-target")));
        assert!(!home.path().join("cron.db").exists());
    }
}

#[tokio::test]
async fn workflow_stdio_static_human_required_never_accepts_json_approved() {
    let (home, key) = setup();
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[agent]\nname=\"alice\"\nrole=\"fixture\"\n[capabilities]\napproval_required_tools=[\"tasks_update\"]\n"
    )
    .unwrap();
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "before-human").await;
    let args = json!({"task_id":id,"title":"after-human"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let mut ctx = context(&alice, &run, &step, &args);
    ctx["approved"] = json!(true);
    assert!(
        alice
            .client
            .prepare_workflow_call(call.clone(), ctx)
            .await
            .is_err()
    );
    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    assert_eq!(
        prepare.effective.approval_requirements,
        vec!["action_guard_require_approval"]
    );
    let store = duduclaw_gateway::task_store::TaskStore::open(home.path()).unwrap();
    assert_eq!(
        store.get_task(&id).await.unwrap().unwrap().title,
        "before-human"
    );
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let result = alice
        .client
        .execute_workflow_call(call, ticket)
        .await
        .unwrap();
    assert_eq!(result.state, "succeeded");
    assert_eq!(
        store.get_task(&id).await.unwrap().unwrap().title,
        "after-human"
    );
}

