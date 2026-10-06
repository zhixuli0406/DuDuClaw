//! Isolated production stdio tests. No ambient credentials or global identity env.
use chrono::Utc;
use duduclaw_core::workflow_mcp::{self, ExecuteTicket, PrepareContext, PrepareTicket};
use duduclaw_gateway::{
    approval::{
        ApprovalBroker, DecisionContext, ExecutionBinding, RequestKind, payload_hash,
        policy_revision,
    },
    workflow::{schema::*, store::WorkflowStore},
};
use duduclaw_llm::{McpClient, McpError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

const CLI: &str = env!("CARGO_BIN_EXE_duduclaw");
struct Session {
    client: McpClient,
    id: String,
    secret: [u8; 32],
    actor: String,
    home: std::path::PathBuf,
}
async fn session(home: &Path, actor: &str, key: &str) -> Session {
    session_with_env(home, actor, key, &[]).await
}
async fn session_with_env(
    home: &Path,
    actor: &str,
    key: &str,
    extra_env: &[(String, String)],
) -> Session {
    let id = uuid::Uuid::new_v4().to_string();
    let secret = [19; 32];
    let mut env = duduclaw_core::agent_identity_env_vars(home, actor);
    env.extend([
        ("DUDUCLAW_HOME".into(), home.to_string_lossy().into_owned()),
        ("DUDUCLAW_MCP_API_KEY".into(), key.into()),
        ("DUDUCLAW_WORKFLOW_SESSION_ID".into(), id.clone()),
        (
            "DUDUCLAW_WORKFLOW_SESSION_SECRET".into(),
            secret.iter().map(|b| format!("{b:02x}")).collect(),
        ),
    ]);
    env.extend(extra_env.iter().cloned());
    // Production spawn receives only per-child env; no set_var impersonation.
    // Child stderr is inherited so a server that exits before the handshake
    // leaves its reason in the captured test output (not a bare `Closed`).
    let client = McpClient::connect_with_stderr(
        CLI,
        &["mcp-server".into()],
        &env,
        Duration::from_secs(30),
        std::process::Stdio::inherit(),
    )
    .await
    .unwrap();
    Session {
        client,
        id,
        secret,
        actor: actor.into(),
        home: home.to_path_buf(),
    }
}
fn setup() -> (tempfile::TempDir, String) {
    let home = tempfile::tempdir().unwrap();
    for actor in ["alice", "bob"] {
        let dir = home.path().join("agents").join(actor);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{actor}\"\nrole = \"fixture\"\n[capabilities]\n[permissions]\ncan_schedule_tasks = true\n")
        )
        .unwrap();
    }
    let key = duduclaw_gateway::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
    duduclaw_core::ensure_identity_key(home.path()).unwrap();
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let user = users
        .create_user(
            "workflow@test.invalid",
            "Workflow fixture",
            "isolated-test-password",
            duduclaw_auth::UserRole::Manager,
        )
        .unwrap();
    for actor in ["alice", "bob"] {
        users
            .bind_agent(&user.id, actor, duduclaw_auth::AccessLevel::Operator)
            .unwrap();
    }
    (home, key)
}
fn string_schema() -> TypedSchema {
    TypedSchema::String { max_length: 4096 }
}
fn object_schema(args: &Value) -> TypedSchema {
    let properties = args
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), string_schema()))
        .collect();
    TypedSchema::Object {
        properties,
        required: args.as_object().unwrap().keys().cloned().collect(),
    }
}
fn fixture_principal(home: &Path) -> String {
    rusqlite::Connection::open(home.join("users.db"))
        .unwrap()
        .query_row(
            "SELECT id FROM users WHERE email='workflow@test.invalid'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}
async fn seed(home: &Path, session: &Session, tool: &str, args: &Value) -> (String, String) {
    seed_call(home, session, tool, args, false, vec![]).await
}
async fn seed_call(
    home: &Path,
    session: &Session,
    tool: &str,
    args: &Value,
    read: bool,
    audience: Vec<String>,
) -> (String, String) {
    seed_material(home, session, tool, args, read, audience, false).await
}
/// Step layout seeded for a run. `steps` must contain the step `effect`; any
/// step before it is recorded as already succeeded.
struct SeedShape {
    input_schema: TypedSchema,
    run_input: Value,
    steps: Vec<StepDefinition>,
    grant: GrantTuning,
}
/// Activation grant material for an active run (checkpoint builds only).
#[cfg_attr(
    not(all(unix, feature = "workflow-test-checkpoints")),
    allow(dead_code)
)]
struct GrantTuning {
    /// Grant `input_max_age_seconds`.
    input_max_age_seconds: u32,
    /// Grant `resource_scope.task_id`; `None` scopes the run's own target.
    scope_task_id: Option<String>,
    /// Charge the effect step in the cost ledger the way the runner does at
    /// its running checkpoint (F1b); `false` leaves it unreserved.
    reserve_effect: bool,
}
impl Default for GrantTuning {
    fn default() -> Self {
        Self {
            input_max_age_seconds: 300,
            scope_task_id: None,
            reserve_effect: true,
        }
    }
}
/// One step whose arguments are an immutable literal (the default fixture).
fn literal_shape(tool: &str, args: &Value, read: bool) -> SeedShape {
    SeedShape {
        input_schema: object_schema(args),
        run_input: args.clone(),
        steps: vec![StepDefinition {
            step_id: "effect".into(),
            action: if read {
                StepAction::McpRead { tool: tool.into() }
            } else {
                StepAction::McpEffect {
                    tool: tool.into(),
                    template_id: "fixed".into(),
                }
            },
            input: InputRef::Literal {
                value: args.clone(),
            },
            input_schema: object_schema(args),
            output_schema: if read {
                read_schema()
            } else {
                TypedSchema::Null
            },
            timeout_seconds: 20,
            max_read_attempts: 1,
        }],
        grant: GrantTuning::default(),
    }
}
async fn seed_material(
    home: &Path,
    session: &Session,
    tool: &str,
    args: &Value,
    read: bool,
    audience: Vec<String>,
    active: bool,
) -> (String, String) {
    let shape = literal_shape(tool, args, read);
    seed_shaped(home, session, tool, args, audience, active, shape).await
}
async fn seed_shaped(
    home: &Path,
    session: &Session,
    tool: &str,
    args: &Value,
    audience: Vec<String>,
    active: bool,
    shape: SeedShape,
) -> (String, String) {
    let run_id = uuid::Uuid::new_v4().to_string();
    let workflow_id = uuid::Uuid::new_v4().to_string();
    let step_id = "effect".to_string();
    let now = Utc::now().to_rfc3339();
    let expiry = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let policy = policy_revision(home, &session.actor).unwrap();
    let creator = CreatorGrantSnapshot {
        actor: session.actor.clone(),
        allowed_tools: BTreeSet::from([tool.into()]),
        policy_revision: policy.clone(),
    };
    let definition = WorkflowDefinition {
        schema_version: 1,
        workflow_id: workflow_id.clone(),
        revision: 1,
        skill_revision_hash: "fixture-skill".into(),
        input_schema: shape.input_schema,
        output_schema: shape
            .steps
            .last()
            .map(|s| s.output_schema.clone())
            .unwrap_or(TypedSchema::Null),
        required_capabilities: BTreeSet::from([tool.into()]),
        steps: shape.steps,
    };
    let revision_hash = definition.hash();
    #[allow(unused_mut)]
    let mut revision = ApprovedWorkflowRevision {
        definition,
        revision_hash: revision_hash.clone(),
        owner: session.actor.clone(),
        creator_grant: creator.clone(),
        audience: audience.clone(),
        fixtures_digest: "fixture-evidence".into(),
        acceptance_id: "test-immutable-revision".into(),
        accepted_at: now.clone(),
        expires_at: expiry.clone(),
    };
    let environment = WorkflowEnvironment {
        schema_version: 1,
        binary_version: env!("CARGO_PKG_VERSION").into(),
        binary_hash: format!("{:x}", Sha256::digest(std::fs::read(CLI).unwrap())),
        home: std::fs::canonicalize(home)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        cwd: std::fs::canonicalize(home.join("agents").join(&session.actor))
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        receipt_adapter_version: 1,
    };
    #[allow(unused_mut)]
    let mut run = WorkflowRun {
        run_id: run_id.clone(),
        trigger_key: run_id.clone(),
        trigger: Trigger::Fixture {
            fixture_id: "stdio-fixture".into(),
            request_id: run_id.clone(),
        },
        workflow_id: workflow_id.clone(),
        revision: 1,
        workflow_hash: revision_hash.clone(),
        skill_hash: "fixture-skill".into(),
        actor: session.actor.clone(),
        decision_context: Some(DecisionContext {
            channel: "dashboard".into(),
            account_id: "fixture-account".into(),
            conversation_id: "fixture-thread".into(),
            principal_id: fixture_principal(home),
        }),
        creator_grant: creator,
        audience: audience.clone(),
        task: None,
        input: shape.run_input.clone(),
        input_hash: payload_hash(&shape.run_input),
        input_observed_at: now.clone(),
        policy_revision: policy,
        environment_hash: environment.hash(),
        grant: None,
        activation_id: None,
        deadline_at: expiry,
        budget: CostBudget {
            per_run_micros: 1000,
            monthly_micros: 10000,
            max_consecutive_failures: 2,
        },
        status: RunStatus::Running,
        cost: Default::default(),
        error_code: None,
        created_at: now.clone(),
        failure_class: None,
        cancelled_by: None,
    };
    let evidence = StepEvidence {
        step_id: step_id.clone(),
        status: StepStatus::Running,
        input_hash: payload_hash(args),
        output_hash: None,
        output: None,
        evidence_kind: ExecutionEvidenceKind::None,
        receipt: None,
        operation_id: None,
        approval_id: None,
        cost: Default::default(),
        error_code: None,
        observed_at: now,
        operator_resolution: None,
    };
    // Steps before `effect` already ran; each passed its input through.
    let prior_steps: Vec<StepEvidence> = revision
        .definition
        .steps
        .iter()
        .take_while(|s| s.step_id != step_id)
        .map(|s| StepEvidence {
            step_id: s.step_id.clone(),
            status: StepStatus::Succeeded,
            input_hash: payload_hash(&shape.run_input),
            output_hash: Some(payload_hash(args)),
            output: Some(args.clone()),
            evidence_kind: ExecutionEvidenceKind::Process,
            receipt: None,
            operation_id: None,
            approval_id: None,
            cost: Default::default(),
            error_code: None,
            observed_at: evidence.observed_at.clone(),
            operator_resolution: None,
        })
        .collect();
    let store = WorkflowStore::open(home).unwrap();
    #[cfg(all(unix, feature = "workflow-test-checkpoints"))]
    let accepted = if active {
        Some(s2_races::accept_activation(home, &mut revision, &run, &shape.grant).await)
    } else {
        None
    };
    #[cfg(not(all(unix, feature = "workflow-test-checkpoints")))]
    assert!(!active, "active race fixture requires checkpoint build");
    seed_source_draft(home, &store, &revision, &run).await;
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_revisions VALUES(?1,1,?2,?3)",
                rusqlite::params![
                    workflow_id,
                    revision_hash,
                    serde_json::to_string(&revision).unwrap()
                ],
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    #[cfg(all(unix, feature = "workflow-test-checkpoints"))]
    if let Some(accepted) = accepted {
        let broker = ApprovalBroker::open(home).unwrap();
        // A-H-1 (F1b): a grant whose pinned target differs from the one the
        // definition produces cannot be prepared; report it to the caller.
        let prepared = match broker.prepare_revision_grant(&accepted).await {
            Ok(prepared) => prepared,
            Err(error) => {
                // Also refused: an effect target that is not fixed in the
                // definition (taken from the run input).
                assert!(
                    shape.grant.scope_task_id.is_some() || error.contains("fixed in the template scope"),
                    "{error}"
                );
                return (run_id, format!("grant-refused: {error}"));
            }
        };
        run.grant = Some(broker.activate_revision_grant(&prepared).await.unwrap());
        run.activation_id = Some(accepted.activation_id);
        run.trigger = Trigger::Manual {
            request_id: run_id.clone(),
        };
    }
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_runs VALUES(?1,?1,?2,1,?3,?4,'running',?5)",
                rusqlite::params![
                    run_id,
                    workflow_id,
                    revision_hash,
                    serde_json::to_string(&run).unwrap(),
                    run.created_at
                ],
            )
            .unwrap();
            for (position, prior) in prior_steps.iter().enumerate() {
                tx.execute(
                    "INSERT INTO workflow_steps VALUES(?1,?2,?3,'succeeded',?4)",
                    rusqlite::params![
                        run_id,
                        prior.step_id,
                        position as i64,
                        serde_json::to_string(prior).unwrap()
                    ],
                )
                .unwrap();
            }
            tx.execute(
                "INSERT INTO workflow_steps VALUES(?1,?2,?3,'running',?4)",
                rusqlite::params![
                    run_id,
                    step_id,
                    prior_steps.len() as i64,
                    serde_json::to_string(&evidence).unwrap()
                ],
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    // E-H3: an active run's effect is reserved in the cost ledger by the
    // real write path before anything is prepared or claimed.
    if run.activation_id.is_some()
        && shape.grant.reserve_effect
        && revision.definition.steps.iter().any(|s| {
            s.step_id == step_id && matches!(s.action, StepAction::McpEffect { .. })
        })
    {
        store
            .charge_step(
                home,
                &run_id,
                &step_id,
                duduclaw_gateway::workflow::cost_ledger::ChargeKind::Effect,
            )
            .await
            .unwrap();
    }
    (run_id, step_id)
}
// Host-created structural source material for dispatcher contract tests. These
// tests do not claim the A service activation/fixture acceptance saga passed.
async fn seed_source_draft(
    home: &Path,
    store: &WorkflowStore,
    revision: &ApprovedWorkflowRevision,
    run: &WorkflowRun,
) {
    use duduclaw_gateway::review_evidence::{
        EvidenceKind, IntegrityStatus, ReviewArtifact, ReviewSnapshot,
    };
    use duduclaw_gateway::workflow_drafts::{DraftFixture, SourceData, WorkflowDraft};
    let source_task = uuid::Uuid::new_v4().to_string();
    let tasks = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    let db = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    db.execute(
        "INSERT INTO tasks(id,title,status,assigned_to,created_at,updated_at) VALUES(?1,'Workflow source','done',?2,
            ?3,?3)",
        rusqlite::params![source_task, run.actor, run.created_at]
    )
    .unwrap();
    drop(db);
    let authority = tasks
        .authority_snapshot(&source_task)
        .await
        .unwrap()
        .unwrap();
    let path = home
        .join("agents")
        .join(&run.actor)
        .join(format!("source-{source_task}.txt"));
    std::fs::write(&path, "Isolated workflow source DATA").unwrap();
    let source_hash = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
    let mut snapshot = ReviewSnapshot {
        schema_version: 1,
        snapshot_id: uuid::Uuid::new_v4().to_string(),
        snapshot_hash: String::new(),
        task_id: source_task.clone(),
        authority_revision: authority.revision,
        authority_snapshot_hash: authority.hash,
        criteria_ledger: None,
        artifacts: vec![ReviewArtifact {
            artifact_id: "source".into(),
            name: "source.txt".into(),
            agent_id: run.actor.clone(),
            archived_name: None,
            source_path: Some(path.to_string_lossy().into_owned()),
            source_hash: Some(source_hash),
            archived_hash: None,
            integrity: IntegrityStatus::Current,
            reasons: vec![],
            evidence_kind: EvidenceKind::Test,
            run_id: None,
            audience: run.audience.clone(),
        }],
        captured_at: run.created_at.clone(),
        audience: run.audience.clone(),
        gaps: vec![],
        waiting_for: None,
        next_check_at: None,
    };
    snapshot.snapshot_hash = snapshot.compute_hash();
    let fixtures = [
        FixtureKind::Normal,
        FixtureKind::Empty,
        FixtureKind::Expired,
        FixtureKind::Injection,
        FixtureKind::MissingPermission,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| {
        // Each case names what its kind proves (V-M-5); none is run here.
        let (input, status, error) =
            duduclaw_gateway::workflow_drafts::example_fixture_case(kind, &run.input);
        let assertions = vec![FixtureAssertion {
            assertion_id: "fixed".into(),
            expected_status: status,
            expected_error_code: error,
            expected_output_hash: None,
        }];
        DraftFixture {
            fixture_id: format!("fixture-{i}"),
            kind,
            input_hash: payload_hash(&input),
            input,
            assertion_hash: payload_hash(&serde_json::to_value(&assertions).unwrap()),
            assertions,
            decisions: Default::default(),
        }
    })
    .collect();
    let mut templates = BTreeMap::new();
    for step in &revision.definition.steps {
        if let StepAction::McpEffect { tool, template_id } = &step.action {
            templates.insert(
                template_id.clone(),
                duduclaw_gateway::approval::EffectTemplate {
                    step_id: step.step_id.clone(),
                    tool: tool.clone(),
                    input_schema: step.input_schema.clone(),
                    resource_scope: BTreeMap::new(),
                    receipt_adapter_version: 1,
                },
            );
        }
    }
    let mut draft = WorkflowDraft {
        schema_version: 1,
        draft_id: uuid::Uuid::new_v4().to_string(),
        revision: run.revision,
        owner: run.actor.clone(),
        source_task,
        skill_id: "fixture-skill".into(),
        source_snapshot_id: snapshot.snapshot_id.clone(),
        source_evidence_hash: snapshot.snapshot_hash.clone(),
        definition: revision.definition.clone(),
        revision_hash: revision.revision_hash.clone(),
        source_data: vec![SourceData {
            classification: "DATA".into(),
            value: run.input.clone(),
        }],
        fixtures,
        creator_grant: revision.creator_grant.clone(),
        effect_templates: templates,
        audience: run.audience.clone(),
        budget: run.budget.clone(),
        input_max_age_seconds: 60,
        timezone: "UTC".into(),
        routine: None,
        stop_conditions: vec!["stop on failure".into()],
        created_at: run.created_at.clone(),
        disabled: true,
        review_status: "draft".into(),
        draft_hash: String::new(),
    };
    draft.draft_hash = draft.compute_hash();
    store.initialize_evidence().await.unwrap();
    store.save_review_snapshot(&snapshot).await.unwrap();
    store.save_draft(&draft).await.unwrap();
}
fn context(session: &Session, run_id: &str, step_key: &str, args: &Value) -> Value {
    let mut ctx = PrepareContext {
        run_id: run_id.into(),
        step_key: step_key.into(),
        input_hash: payload_hash(args),
        session_id: session.id.clone(),
        expires_at: (Utc::now() + chrono::Duration::minutes(4)).to_rfc3339(),
        mac: String::new(),
    };
    ctx.mac = workflow_mcp::sign(
        &session.secret,
        "context",
        &workflow_mcp::unsigned(&ctx).unwrap(),
    )
    .unwrap();
    serde_json::to_value(ctx).unwrap()
}
async fn authorized_ticket(
    home: &Path,
    session: &Session,
    prepare: &PrepareTicket,
) -> ExecuteTicket {
    workflow_mcp::verify(
        &session.secret,
        "prepare",
        &workflow_mcp::unsigned(prepare).unwrap(),
        &prepare.mac,
    )
    .unwrap();
    let payload =
        json!({"name":prepare.effective.tool,"arguments":prepare.effective.effective_arguments});
    let ctx = DecisionContext {
        channel: "dashboard".into(),
        account_id: "fixture-account".into(),
        conversation_id: "fixture-thread".into(),
        principal_id: fixture_principal(home),
    };
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: prepare.effective.run_id.clone(),
        run_origin_kind: "workflow".into(),
        actor_principal: session.actor.clone(),
        decision_context: ctx.clone(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&payload),
        policy_revision: prepare.effective.policy_revision.clone(),
        // Same canonical directory used by the server environment and runner.
        // On macOS a temporary /var path may canonicalize to /private/var.
        cwd: Some(
            std::fs::canonicalize(home.join("agents").join(&session.actor))
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
    // Actual typed request + decision APIs, never an inserted Approved row.
    let approval = broker
        .request_bound(
            RequestKind::Approval,
            &session.actor,
            "stdio fixture",
            payload,
            binding.clone(),
        )
        .await
        .unwrap();
    broker.decide_bound(&approval, &ctx, true).await.unwrap();
    let operation = broker
        .prepare_operation(&approval, &prepare.effective.step_key, None)
        .await
        .unwrap();
    let mut ticket = ExecuteTicket {
        version: 1,
        session_id: session.id.clone(),
        operation_id: operation,
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
fn register_staging(home: &Path, field: &str, id: &str) {
    let path = home.join("config.toml");
    let mut config: toml::Table = std::fs::read_to_string(&path)
        .ok()
        .map(|s| toml::from_str(&s).unwrap())
        .unwrap_or_default();
    let workflow = config
        .entry("workflow")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    workflow.insert(
        "fixture_environment".into(),
        toml::Value::String("staging".into()),
    );
    let ids = workflow
        .entry(field)
        .or_insert_with(|| toml::Value::Array(vec![]))
        .as_array_mut()
        .unwrap();
    if !ids.iter().any(|v| v.as_str() == Some(id)) {
        ids.push(toml::Value::String(id.into()));
    }
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
}
async fn task(session: &mut Session, title: &str) -> String {
    let out = session
        .client
        .call_tool(
            "tasks_create",
            json!({"title":title,"assigned_to":session.actor}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    let value: Value = serde_json::from_str(&out.content).unwrap();
    let id = value["task"]["id"].as_str().unwrap().to_string();
    register_staging(&session.home, "fixture_task_ids", &id);
    id
}
#[path = "workflow_stdio/effect_cases.rs"]
mod effect_cases;
fn read_schema() -> TypedSchema {
    TypedSchema::Object {
        properties: BTreeMap::from([
            ("url".into(), string_schema()),
            ("status_code".into(), TypedSchema::Integer),
            ("content_type".into(), string_schema()),
            ("cached".into(), TypedSchema::Boolean),
            ("fetched_at".into(), string_schema()),
            ("body_chars".into(), TypedSchema::Integer),
            ("truncated".into(), TypedSchema::Boolean),
            ("body".into(), TypedSchema::String { max_length: 60000 }),
        ]),
        required: BTreeSet::from(["url".into(), "body".into(), "fetched_at".into()]),
    }
}
#[path = "workflow_stdio/read_cases.rs"]
mod read_cases;
#[cfg(all(unix, feature = "workflow-test-checkpoints"))]
#[path = "workflow_stdio/bha_final_begin.rs"]
mod bha_final_begin;
#[cfg(all(unix, feature = "workflow-test-checkpoints"))]
#[path = "workflow_stdio/s2_races.rs"]
mod s2_races;
#[path = "workflow_stdio/staging.rs"]
mod staging;
