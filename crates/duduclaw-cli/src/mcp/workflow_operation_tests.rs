//! Execute-boundary staging recheck, isolated from the policy fence.
//!
//! `check_effect_scope` decides from `config.toml`, the tool name and the
//! arguments. Between prepare and execute those are pinned: `policy_revision`
//! hashes the same `config.toml`, and the step input hash binds the arguments.
//! A changed allowlist therefore reaches `before_effect` only inside the window
//! between its policy read and its scope read. That window has no stdio
//! checkpoint, so this test builds the in-process call state directly: the
//! policy anchors equal the current `config.toml`, and only the scope predicate
//! can refuse.
use super::*;
use duduclaw_gateway::workflow::schema::{
    CostBudget, CreatorGrantSnapshot, ExecutionEvidenceKind, RunStatus, StepStatus, Trigger,
    TypedSchema, WorkflowDefinition,
};
use duduclaw_gateway::workflow::store::WorkflowStore;
use std::collections::{BTreeMap, BTreeSet};

const TARGET: &str = "target-task";

fn args() -> Value {
    json!({"task_id": TARGET, "title": "after"})
}

fn schema() -> TypedSchema {
    TypedSchema::Object {
        properties: BTreeMap::from([
            ("task_id".into(), TypedSchema::String { max_length: 256 }),
            ("title".into(), TypedSchema::String { max_length: 256 }),
        ]),
        required: BTreeSet::from(["task_id".into(), "title".into()]),
    }
}

/// Seed one Fixture run under `staged_ids`, then build the execute-time call
/// state whose policy anchors match the config now on disk.
async fn execute_call(staged_ids: &[&str]) -> (tempfile::TempDir, WorkflowCall) {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join("agents/alice")).unwrap();
    std::fs::write(
        home.join("agents/alice/agent.toml"),
        "[agent]\nname='alice'\nrole='fixture'\n[capabilities]\n",
    )
    .unwrap();
    let ids = staged_ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(
        home.join("config.toml"),
        format!("[workflow]\nfixture_environment='staging'\nfixture_task_ids=[{ids}]\n"),
    )
    .unwrap();

    let policy = policy_revision(&home, "alice").unwrap();
    let now = Utc::now().to_rfc3339();
    let expiry = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let creator = CreatorGrantSnapshot {
        actor: "alice".into(),
        allowed_tools: BTreeSet::from(["tasks_update".into()]),
        policy_revision: policy.clone(),
    };
    let definition = WorkflowDefinition {
        schema_version: 1,
        workflow_id: "wf".into(),
        revision: 1,
        skill_revision_hash: "skill".into(),
        input_schema: schema(),
        output_schema: TypedSchema::Null,
        required_capabilities: BTreeSet::from(["tasks_update".into()]),
        steps: vec![StepDefinition {
            step_id: "effect".into(),
            action: StepAction::McpEffect {
                tool: "tasks_update".into(),
                template_id: "fixed".into(),
            },
            input: InputRef::Literal { value: args() },
            input_schema: schema(),
            output_schema: TypedSchema::Null,
            timeout_seconds: 20,
            max_read_attempts: 1,
        }],
    };
    let revision_hash = definition.hash();
    let revision = ApprovedWorkflowRevision {
        definition,
        revision_hash: revision_hash.clone(),
        owner: "alice".into(),
        creator_grant: creator.clone(),
        audience: vec![],
        fixtures_digest: "fixtures".into(),
        acceptance_id: "acceptance".into(),
        accepted_at: now.clone(),
        expires_at: expiry.clone(),
    };
    let run = WorkflowRun {
        run_id: "run".into(),
        trigger_key: "run".into(),
        trigger: Trigger::Fixture {
            fixture_id: "fixture".into(),
            request_id: "run".into(),
        },
        workflow_id: "wf".into(),
        revision: 1,
        workflow_hash: revision_hash.clone(),
        skill_hash: "skill".into(),
        actor: "alice".into(),
        decision_context: None,
        creator_grant: creator,
        audience: vec![],
        task: None,
        input: args(),
        input_hash: payload_hash(&args()),
        input_observed_at: now.clone(),
        policy_revision: policy.clone(),
        environment_hash: "environment".into(),
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
        step_id: "effect".into(),
        status: StepStatus::Running,
        input_hash: payload_hash(&args()),
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
    let store = WorkflowStore::open(&home).unwrap();
    store.initialize_evidence().await.unwrap();
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_revisions VALUES('wf',1,?1,?2)",
                rusqlite::params![revision_hash, serde_json::to_string(&revision).unwrap()],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO workflow_runs VALUES('run','run','wf',1,?1,?2,'running',?3)",
                rusqlite::params![
                    revision_hash,
                    serde_json::to_string(&run).unwrap(),
                    run.created_at
                ],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO workflow_steps VALUES('run','effect',0,'running',?1)",
                rusqlite::params![serde_json::to_string(&evidence).unwrap()],
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let session = Arc::new(WorkflowSession {
        id: "session".into(),
        actor: "alice".into(),
        secret: vec![7; 32],
        home: home.clone(),
        prepared: Mutex::new(HashMap::new()),
    });
    let stored = StoredCall::read(&session, "run", "effect", "tasks_update", &args()).unwrap();
    let ticket = ExecuteTicket {
        version: VERSION,
        session_id: "session".into(),
        operation_id: "operation".into(),
        binding_digest: String::new(),
        prepare_digest: String::new(),
        expires_at: stored.run.deadline_at.clone(),
        mac: String::new(),
    };
    let call = WorkflowCall {
        session,
        metadata: WorkflowMetadata::Execute {
            version: VERSION,
            ticket,
        },
        stored,
        policy_before: policy,
        environment_hash: "environment".into(),
        original_arguments: args(),
        human_proof: Mutex::new(None),
        requirements: Mutex::new(Vec::new()),
        prepared: None,
        operation: None,
    };
    (tmp, call)
}

#[tokio::test]
async fn execute_boundary_staging_recheck_refuses_with_policy_unchanged() {
    let (_home, call) = execute_call(&["another-staging-task"]).await;
    let payload = json!({"name": "tasks_update", "arguments": args()});
    // Every policy fence in `effective` passes (anchors equal the current
    // config); the refusal can only come from the staging predicate, and it
    // fires before the prepare ticket, binding or broker are consulted.
    assert_eq!(
        call.before_effect(&payload).await.err().unwrap(),
        "workflow fixture resource outside staging scope"
    );
}

#[tokio::test]
async fn execute_boundary_staging_recheck_passes_listed_resource_to_next_fence() {
    let (_home, call) = execute_call(&[TARGET]).await;
    let payload = json!({"name": "tasks_update", "arguments": args()});
    // Control: with the target listed the call gets past the scope predicate and
    // stops at the next fence, the source-draft audience check (this synthetic
    // state seeds no source draft).
    assert_eq!(
        call.before_effect(&payload).await.err().unwrap(),
        "workflow immutable source draft unavailable"
    );
}

#[test]
fn binary_hash_is_cached_per_process_and_equals_a_fresh_digest() {
    use sha2::{Digest, Sha256};
    let first = current_binary_hash().unwrap();
    let direct = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    assert_eq!(first, direct);
    let started = std::time::Instant::now();
    assert_eq!(current_binary_hash().unwrap(), first);
    assert!(started.elapsed() < std::time::Duration::from_millis(50), "second call re-hashed");
}
