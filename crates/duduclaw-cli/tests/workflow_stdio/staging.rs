//! Exact operator-owned resource scope must survive both prepare and execution.
use super::*;
fn denied<T: std::fmt::Debug>(result: Result<T, McpError>, expected: &str) {
    match result {
        Err(McpError::Rpc { code, message }) => {
            assert_eq!(code, -32003);
            assert_eq!(message, expected);
        }
        other => panic!("expected exact staging RPC denial, got {other:?}"),
    }
}
/// Count every UPDATE or DELETE that touches row `id` of `table` in `db`, the
/// way `s2_races.rs` counts original-handler effects. A readback alone cannot
/// show that no write happened (a write could restore the old value).
fn count_row_writes(db: &Path, table: &str, id: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE staging_write_count(n INTEGER);
         INSERT INTO staging_write_count VALUES(0);
         CREATE TRIGGER staging_count_update AFTER UPDATE ON {table} WHEN OLD.id='{id}'
           BEGIN UPDATE staging_write_count SET n=n+1; END;
         CREATE TRIGGER staging_count_delete AFTER DELETE ON {table} WHEN OLD.id='{id}'
           BEGIN UPDATE staging_write_count SET n=n+1; END;"
    ))
    .unwrap();
}
fn row_writes(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT n FROM staging_write_count", [], |r| r.get(0))
        .unwrap()
}
/// Drop `id` from the staging allowlist `field`, keeping every other key.
fn unregister_staging(home: &Path, field: &str, id: &str) {
    let path = home.join("config.toml");
    let mut config: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let ids = config
        .get_mut("workflow")
        .and_then(toml::Value::as_table_mut)
        .and_then(|w| w.get_mut(field))
        .and_then(toml::Value::as_array_mut)
        .unwrap();
    ids.retain(|v| v.as_str() != Some(id));
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
}

#[tokio::test]
async fn workflow_stdio_owned_nonstaging_resource_denied_and_scope_rechecked() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "owned-but-not-staging").await;
    count_row_writes(&home.path().join("tasks.db"), "tasks", &id);
    let args = json!({"task_id":id,"title":"forbidden-mutation"});
    let call = json!({"name":"tasks_update","arguments":args});
    let allowed = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let allowed_scope = allowed.clone();
    // Only the operator scope list changes; the rest of config.toml (notably
    // the MCP key registry this session authenticates with) stays intact.
    let remove_scope = || {
        let mut config: toml::Table = toml::from_str(&allowed_scope).unwrap();
        let workflow = config
            .get_mut("workflow")
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        workflow.insert(
            "fixture_environment".into(),
            toml::Value::String("staging".into()),
        );
        workflow.insert("fixture_task_ids".into(), toml::Value::Array(vec![]));
        std::fs::write(
            home.path().join("config.toml"),
            toml::to_string(&config).unwrap(),
        )
        .unwrap()
    };
    remove_scope();
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let ctx = context(&alice, &run, &step, &args);
    denied(
        alice.client.prepare_workflow_call(call.clone(), ctx).await,
        "workflow fixture resource outside staging scope",
    );
    let broker = ApprovalBroker::open(home.path()).unwrap();
    assert!(broker.list_operations().await.unwrap().is_empty());
    std::fs::write(home.path().join("config.toml"), allowed).unwrap();
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let operation = ticket.operation_id.clone();
    remove_scope();
    // F1b (E-H5a): the policy snapshot hashes only authority fields, and the
    // staging scope list is operator test configuration, not employee
    // authority. The repeated scope predicate right before execution is what
    // rejects the changed allowlist (was: the raw-byte policy drift fence).
    denied(
        alice.client.execute_workflow_call(call, ticket).await,
        "workflow fixture resource outside staging scope",
    );
    let row = broker.inspect_operation(&operation).await.unwrap().unwrap();
    assert!(row.receipt.is_none());
    assert_ne!(
        row.state,
        duduclaw_gateway::approval::OperationState::Executing
    );
    let task = duduclaw_gateway::task_store::TaskStore::open(home.path())
        .unwrap()
        .get_task(&id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.title, "owned-but-not-staging");
    assert_eq!(row_writes(&home.path().join("tasks.db")), 0);
}

#[tokio::test]
async fn workflow_stdio_owned_cron_outside_staging_list_denied_without_write() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let cron = duduclaw_gateway::cron_store::CronStore::open(home.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    cron.insert(&duduclaw_gateway::cron_store::CronTaskRow::new(
        id.clone(),
        "owned-production-job".into(),
        "alice".into(),
        "0 0 * * *".into(),
        "before".into(),
    ))
    .await
    .unwrap();
    // alice owns the job; the operator allowlist names only a different job.
    register_staging(home.path(), "fixture_cron_ids", "another-staging-job");
    let db = home.path().join("cron_tasks.db");
    count_row_writes(&db, "cron_tasks", &id);
    let args = json!({"id":id,"task":"after"});
    let call = json!({"name":"update_cron_task","arguments":args});
    let (run, step) = seed(home.path(), &alice, "update_cron_task", &args).await;
    let ctx = context(&alice, &run, &step, &args);
    denied(
        alice.client.prepare_workflow_call(call, ctx).await,
        "workflow fixture resource outside staging scope",
    );
    let broker = ApprovalBroker::open(home.path()).unwrap();
    assert!(broker.list_operations().await.unwrap().is_empty());
    assert_eq!(row_writes(&db), 0);
    assert_eq!(cron.get(&id).await.unwrap().unwrap().task, "before");
}

/// The effect step's target is computed from the previous step's output, not a
/// literal, so the service-side literal pre-check never sees it; the CLI
/// boundary must still apply the exact staging list.
pub(super) fn computed_shape(args: &Value) -> SeedShape {
    SeedShape {
        input_schema: TypedSchema::Object {
            properties: BTreeMap::from([("target".into(), object_schema(args))]),
            required: BTreeSet::from(["target".into()]),
        },
        run_input: json!({"target": args}),
        steps: vec![
            StepDefinition {
                step_id: "lookup".into(),
                action: StepAction::Process {
                    transform: ProcessTransform::Identity,
                },
                input: InputRef::RunInput {
                    pointer: "/target".into(),
                },
                input_schema: object_schema(args),
                output_schema: object_schema(args),
                timeout_seconds: 20,
                max_read_attempts: 1,
            },
            StepDefinition {
                step_id: "effect".into(),
                action: StepAction::McpEffect {
                    tool: "tasks_update".into(),
                    template_id: "fixed".into(),
                },
                input: InputRef::StepOutput {
                    step_id: "lookup".into(),
                    pointer: String::new(),
                },
                input_schema: object_schema(args),
                output_schema: TypedSchema::Null,
                timeout_seconds: 20,
                max_read_attempts: 1,
            },
        ],
        grant: GrantTuning::default(),
    }
}

#[tokio::test]
async fn workflow_stdio_computed_target_outside_staging_list_denied_without_write() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let staged = task(&mut alice, "staged-target").await;
    let computed = task(&mut alice, "computed-production-target").await;
    unregister_staging(home.path(), "fixture_task_ids", &computed);
    let db = home.path().join("tasks.db");
    count_row_writes(&db, "tasks", &computed);
    let args = json!({"task_id":computed,"title":"forbidden-mutation"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed_shaped(
        home.path(),
        &alice,
        "tasks_update",
        &args,
        vec![],
        false,
        computed_shape(&args),
    )
    .await;
    let ctx = context(&alice, &run, &step, &args);
    denied(
        alice.client.prepare_workflow_call(call, ctx).await,
        "workflow fixture resource outside staging scope",
    );
    let broker = ApprovalBroker::open(home.path()).unwrap();
    assert!(broker.list_operations().await.unwrap().is_empty());
    assert_eq!(row_writes(&db), 0);
    let tasks = duduclaw_gateway::task_store::TaskStore::open(home.path()).unwrap();
    let row = tasks.get_task(&computed).await.unwrap().unwrap();
    assert_eq!(row.title, "computed-production-target");
    // The staged sibling is untouched as well.
    let sibling = tasks.get_task(&staged).await.unwrap().unwrap();
    assert_eq!(sibling.title, "staged-target");
}

#[tokio::test]
async fn workflow_stdio_computed_target_inside_staging_list_reaches_original_handler() {
    let (home, key) = setup();
    let mut alice = session(home.path(), "alice", &key).await;
    let computed = task(&mut alice, "computed-staging-target").await;
    let db = home.path().join("tasks.db");
    count_row_writes(&db, "tasks", &computed);
    let args = json!({"task_id":computed,"title":"computed-after"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed_shaped(
        home.path(),
        &alice,
        "tasks_update",
        &args,
        vec![],
        false,
        computed_shape(&args),
    )
    .await;
    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let result = alice
        .client
        .execute_workflow_call(call, ticket)
        .await
        .unwrap();
    assert_eq!(result.state, "succeeded");
    // The counter is live: the one real handler write is counted.
    assert!(row_writes(&db) >= 1);
    let row = duduclaw_gateway::task_store::TaskStore::open(home.path())
        .unwrap()
        .get_task(&computed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.title, "computed-after");
}
