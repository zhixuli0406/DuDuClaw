//! Bound human approval (BHA) on an active run: a person approving the exact
//! call never replaces the activation grant's freshness, resource scope and
//! budget fences. Each case goes test host -> real `duduclaw mcp-server` stdio
//! -> `before_effect` -> broker claim/begin -> original `tasks_update` handler.
//! The approval itself is made through the broker's typed request/decision
//! APIs (`authorized_ticket`), the same as every other stdio fixture here.
use super::s2_races::{checkpoint, denied, reached};
use super::*;
use duduclaw_gateway::approval::OperationState;

const ASK_POLICY: &str = "[[capabilities.policy]]\ntool='tasks_update'\neffect='ask'\n\
    [[capabilities.policy]]\ntool='tasks_create'\neffect='allow'\n";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Case {
    /// Active run, nothing changes after approval: the original handler runs.
    ActiveControl,
    /// Fixture (no activation grant) BHA run: the original handler runs.
    FixtureControl,
    /// The effect's arguments come from the run input. Before F5-A this case
    /// let the input age lapse before begin and expected a refusal there;
    /// such an effect has no fixed target, so since F1b (A-H-1) its grant
    /// cannot be prepared at all and it never reaches claim or begin. The
    /// begin-time input-age fence for run-input effects therefore has no
    /// activated path to exercise here; it stays in `check_granted_material`
    /// as a second line.
    InputExpiresBeforeBegin,
    /// F5-A (R-M5): the same wait, but the effect's arguments are a literal
    /// fixed in the definition, so the run input's age does not apply.
    LiteralSurvivesInputExpiry,
    /// The grant's pinned target is not the task the definition updates:
    /// since F1b (A-H-1) such a grant cannot be prepared at all.
    ResourceOutsideGrantScope,
    /// F1b (E-H3): the effect was never reserved in the cost ledger (the
    /// runner always reserves it with the step's running checkpoint), so the
    /// broker refuses the claim.
    EffectNotReserved,
}

/// Grant input freshness window used by the expiry case. The CLI checkpoint
/// pauses at most 30 s, so the window must close within that pause.
const SHORT_INPUT_AGE: u32 = 20;

/// Handler writes, or 0 before the counting trigger is installed.
fn handler_writes_or_zero(home: &Path) -> i64 {
    rusqlite::Connection::open(home.join("tasks.db"))
        .unwrap()
        .query_row("SELECT n FROM bha_effect_count", [], |r| r.get(0))
        .unwrap_or(0)
}

fn handler_writes(home: &Path) -> i64 {
    rusqlite::Connection::open(home.join("tasks.db"))
        .unwrap()
        .query_row("SELECT n FROM bha_effect_count", [], |r| r.get(0))
        .unwrap()
}

async fn run_case(case: Case) {
    let (home, key) = setup();
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        format!("[agent]\nname='alice'\nrole='fixture'\n[capabilities]\nautonomy_level='auto'\n{ASK_POLICY}"),
    )
    .unwrap();
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "before-bha").await;
    let other = task(&mut alice, "other-staged-task").await;
    let args = json!({"task_id":id,"title":"after-bha"});
    let call = json!({"name":"tasks_update","arguments":args});

    let mut shape = if case == Case::InputExpiresBeforeBegin {
        super::staging::computed_shape(&args)
    } else {
        literal_shape("tasks_update", &args, false)
    };
    match case {
        Case::InputExpiresBeforeBegin | Case::LiteralSurvivesInputExpiry => {
            shape.grant.input_max_age_seconds = SHORT_INPUT_AGE
        }
        Case::ResourceOutsideGrantScope => shape.grant.scope_task_id = Some(other.clone()),
        Case::EffectNotReserved => shape.grant.reserve_effect = false,
        _ => (),
    }
    let active = case != Case::FixtureControl;
    let (run, step) = seed_shaped(
        home.path(),
        &alice,
        "tasks_update",
        &args,
        vec![],
        active,
        shape,
    )
    .await;
    if case == Case::InputExpiresBeforeBegin {
        assert!(step.starts_with("grant-refused"), "{step}");
        assert!(step.contains("fixed in the template scope"), "{step}");
        assert_eq!(handler_writes_or_zero(home.path()), 0);
        return;
    }
    if case == Case::ResourceOutsideGrantScope {
        // Refused before any grant exists: nothing can be claimed or begun.
        assert!(step.starts_with("grant-refused"), "{step}");
        assert!(step.contains("pinned"), "{step}");
        assert_eq!(handler_writes_or_zero(home.path()), 0);
        return;
    }
    let stored = WorkflowStore::open(home.path())
        .unwrap()
        .get_run(&run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.grant.is_some(), active);

    let ctx = context(&alice, &run, &step, &args);
    let prepare = alice
        .client
        .prepare_workflow_call(call.clone(), ctx)
        .await
        .unwrap();
    // A person must decide this call; the grant is not a human decision.
    assert_eq!(prepare.effective.approval_requirements, vec!["policy_ask"]);
    let ticket = authorized_ticket(home.path(), &alice, &prepare).await;
    let operation = ticket.operation_id.clone();

    let db = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE bha_effect_count(n INTEGER);
         INSERT INTO bha_effect_count VALUES(0);
         CREATE TRIGGER bha_count_update AFTER UPDATE ON tasks WHEN OLD.id='{id}'
           BEGIN UPDATE bha_effect_count SET n=n+1; END;
         CREATE TRIGGER bha_count_other AFTER UPDATE ON tasks WHEN OLD.id='{other}'
           BEGIN UPDATE bha_effect_count SET n=n+1; END;"
    ))
    .unwrap();
    drop(db);

    // Pause the CLI after the claim succeeded, immediately before begin.
    let dir = checkpoint(home.path(), "before_begin", &operation);
    let execution = tokio::spawn(async move {
        let result = alice.client.execute_workflow_call(call, ticket).await;
        (alice, result)
    });

    let result = if case == Case::EffectNotReserved {
        // The ledger fence refuses at claim, so the pause is never reached.
        let (_alice, result) = tokio::time::timeout(Duration::from_secs(60), execution)
            .await
            .unwrap()
            .unwrap();
        assert!(!dir.join("before_begin").exists());
        result
    } else {
        reached(&dir, "before_begin", &operation).await;
        match case {
            Case::InputExpiresBeforeBegin | Case::LiteralSurvivesInputExpiry => {
                // Reaching the pause proves the claim saw fresh input. Wait
                // until the grant's freshness window has closed.
                let observed = chrono::DateTime::parse_from_rfc3339(&stored.input_observed_at)
                    .unwrap()
                    .with_timezone(&Utc);
                let stale_at = observed
                    + chrono::Duration::seconds(SHORT_INPUT_AGE as i64)
                    + chrono::Duration::seconds(1);
                let wait = (stale_at - Utc::now()).to_std().unwrap_or_default();
                assert!(
                    wait < Duration::from_secs(29),
                    "freshness window would outlast the CLI pause"
                );
                tokio::time::sleep(wait).await;
            }
            _ => (),
        }
        assert_eq!(handler_writes(home.path()), 0, "no effect before begin");
        std::fs::write(dir.join("release"), b"resume").unwrap();
        let (_alice, result) = tokio::time::timeout(Duration::from_secs(60), execution)
            .await
            .unwrap()
            .unwrap();
        result
    };
    std::fs::remove_dir_all(dir).unwrap();

    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = broker.inspect_operation(&operation).await.unwrap().unwrap();
    let tasks = duduclaw_gateway::task_store::TaskStore::open(home.path()).unwrap();
    let target = tasks.get_task(&id).await.unwrap().unwrap();
    let sibling = tasks.get_task(&other).await.unwrap().unwrap();
    assert_eq!(sibling.title, "other-staged-task");
    match case {
        Case::ActiveControl | Case::FixtureControl | Case::LiteralSurvivesInputExpiry => {
            assert_eq!(result.unwrap().state, "succeeded");
            assert_eq!(row.state, OperationState::Succeeded);
            assert!(row.receipt.is_some());
            assert_eq!(target.title, "after-bha");
            assert!(handler_writes(home.path()) >= 1);
        }
        denial => {
            let expected = match denial {
                Case::InputExpiresBeforeBegin => "workflow input stale or deadline exceeded",
                Case::EffectNotReserved => "workflow effect not reserved in the cost ledger",
                _ => unreachable!(),
            };
            denied(result, expected);
            assert_ne!(row.state, OperationState::Executing);
            assert_ne!(row.state, OperationState::Succeeded);
            assert!(row.receipt.is_none());
            assert_eq!(target.title, "before-bha");
            assert_eq!(handler_writes(home.path()), 0);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_bha_fixture_and_active_controls_reach_original_handler() {
    run_case(Case::FixtureControl).await;
    run_case(Case::ActiveControl).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_bha_run_input_effect_cannot_be_granted() {
    run_case(Case::InputExpiresBeforeBegin).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_bha_literal_effect_survives_input_expiry_before_begin() {
    run_case(Case::LiteralSurvivesInputExpiry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_bha_target_outside_grant_scope_cannot_be_activated() {
    run_case(Case::ResourceOutsideGrantScope).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_bha_unreserved_effect_refused_at_claim() {
    run_case(Case::EffectNotReserved).await;
}
