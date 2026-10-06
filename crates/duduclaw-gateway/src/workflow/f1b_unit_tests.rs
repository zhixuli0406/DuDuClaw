//! F1b unit cases that need no CLI binary: the cost ledger and count limits
//! (E-H3), effect target pinning (A-H-1) and the `limit` failure class.
use super::cost_ledger::{self as ledger, ChargeKind};
use super::effect_targets::{check_effect_arguments, check_pinned_target, template_target};
use super::*;
use crate::approval::{EffectTemplate, payload_hash};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn home_with(config: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    home
}

/// A run row in `workflow.db`, written the way `enqueue_run` stores it.
pub(super) async fn seed_run(
    store: &WorkflowStore,
    id: &str,
    activation: Option<&str>,
    per_run: u64,
    monthly: u64,
    failure_class: Option<&str>,
) -> WorkflowRun {
    let trigger = Trigger::Manual {
        request_id: id.into(),
    };
    let run = WorkflowRun {
        run_id: id.into(),
        trigger_key: trigger.key("wf", 1).unwrap(),
        trigger,
        workflow_id: "wf".into(),
        revision: 1,
        workflow_hash: "h".into(),
        skill_hash: "s".into(),
        actor: "alice".into(),
        creator_grant: CreatorGrantSnapshot {
            actor: "alice".into(),
            allowed_tools: BTreeSet::new(),
            policy_revision: "p".into(),
        },
        audience: vec![],
        task: None,
        input: Value::Null,
        input_hash: payload_hash(&Value::Null),
        input_observed_at: Utc::now().to_rfc3339(),
        policy_revision: "p".into(),
        environment_hash: "e".into(),
        grant: None,
        activation_id: activation.map(str::to_string),
        deadline_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        budget: CostBudget {
            per_run_micros: per_run,
            monthly_micros: monthly,
            max_consecutive_failures: 3,
        },
        status: if failure_class.is_some() {
            RunStatus::Blocked
        } else {
            RunStatus::Running
        },
        cost: CostBreakdown::default(),
        error_code: None,
        created_at: Utc::now().to_rfc3339(),
        decision_context: None,
        failure_class: failure_class.map(str::to_string),
        cancelled_by: None,
    };
    let encoded = serde_json::to_string(&run).unwrap();
    let r = run.clone();
    store
        .with_transaction(move |tx| {
            tx.execute(
                "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    r.run_id,
                    r.trigger_key,
                    r.workflow_id,
                    r.revision,
                    r.workflow_hash,
                    encoded,
                    if r.failure_class.is_some() {
                        "blocked"
                    } else {
                        "running"
                    },
                    r.created_at
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    run
}

async fn charge(
    store: &WorkflowStore,
    home: &Path,
    run: &str,
    step: &str,
    kind: ChargeKind,
) -> Result<CostBreakdown, String> {
    store.charge_step(home, run, step, kind).await
}

#[tokio::test]
async fn default_prices_charge_zero_and_say_money_limits_are_not_in_effect() {
    let home = home_with("");
    let store = WorkflowStore::open(home.path()).unwrap();
    seed_run(&store, "r", Some("act"), 10, 100, None).await;
    let cost = charge(&store, home.path(), "r", "read", ChargeKind::Read)
        .await
        .unwrap();
    assert_eq!(cost.total().unwrap(), 0);
    let pricing = ledger::load_pricing(home.path());
    assert!(!pricing.money_limits_effective());
    assert_eq!(pricing.view()["money_limits_effective"], false);
    // The run's projection is the ledger's: one entry, zero cost.
    let run = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(run.cost, cost);
}

#[tokio::test]
async fn per_run_budget_and_unknown_price_are_enforced_by_the_ledger() {
    let home = home_with("[workflow.unit_cost_micros]\nread=5\nprocess='oops'\n");
    let store = WorkflowStore::open(home.path()).unwrap();
    seed_run(&store, "r", Some("act"), 12, 1000, None).await;
    assert!(ledger::load_pricing(home.path()).money_limits_effective());
    assert_eq!(
        charge(&store, home.path(), "r", "a", ChargeKind::Read)
            .await
            .unwrap()
            .tools,
        5
    );
    // Unknown price: charged at the cap (the remaining 7).
    let after = charge(&store, home.path(), "r", "p", ChargeKind::Process)
        .await
        .unwrap();
    assert_eq!(after.compute, 7);
    assert_eq!(after.total().unwrap(), 12);
    // Nothing more fits; the refusal is a limit, not a routine failure.
    let err = charge(&store, home.path(), "r", "b", ChargeKind::Read)
        .await
        .unwrap_err();
    assert_eq!(err, ledger::BUDGET_RUN);
    assert!(ledger::is_limit_error(&err));
    assert_eq!(
        store
            .get_run("r")
            .await
            .unwrap()
            .unwrap()
            .cost
            .total()
            .unwrap(),
        12
    );
}

#[tokio::test]
async fn count_limits_bind_even_when_every_price_is_zero() {
    let home = home_with(
        "[workflow.limits]\nmax_reads_per_run=1\nmax_effects_per_run=1\nmax_steps_per_run=3\n",
    );
    let store = WorkflowStore::open(home.path()).unwrap();
    seed_run(&store, "r", Some("act"), 10, 100, None).await;
    charge(&store, home.path(), "r", "read", ChargeKind::Read)
        .await
        .unwrap();
    assert_eq!(
        charge(&store, home.path(), "r", "read", ChargeKind::Read)
            .await
            .unwrap_err(),
        ledger::LIMIT_READS
    );
    charge(&store, home.path(), "r", "fx", ChargeKind::Effect)
        .await
        .unwrap();
    // Re-entering the same effect step (after its decision) is not a new charge.
    charge(&store, home.path(), "r", "fx", ChargeKind::Effect)
        .await
        .unwrap();
    assert_eq!(
        charge(&store, home.path(), "r", "fx2", ChargeKind::Effect)
            .await
            .unwrap_err(),
        ledger::LIMIT_EFFECTS
    );
    charge(&store, home.path(), "r", "p", ChargeKind::Process)
        .await
        .unwrap();
    assert_eq!(
        charge(&store, home.path(), "r", "p2", ChargeKind::Process)
            .await
            .unwrap_err(),
        ledger::LIMIT_STEPS
    );
    // An invalid limit refuses every charge.
    std::fs::write(
        home.path().join("config.toml"),
        "[workflow.limits]\nmax_steps_per_run=-1\n",
    )
    .unwrap();
    assert_eq!(
        charge(&store, home.path(), "r", "z", ChargeKind::Process)
            .await
            .unwrap_err(),
        ledger::LIMIT_CONFIG
    );
}

#[tokio::test]
async fn monthly_budget_is_shared_by_formal_runs_and_reserved_atomically() {
    let home = home_with("[workflow.unit_cost_micros]\neffect=6\n");
    let store = WorkflowStore::open(home.path()).unwrap();
    for id in ["a", "b", "fixture"] {
        let activation = (id != "fixture").then_some("act");
        seed_run(&store, id, activation, 100, 10, None).await;
    }
    // A fixture run does not draw on the formal monthly budget.
    charge(&store, home.path(), "fixture", "fx", ChargeKind::Effect)
        .await
        .unwrap();
    // Two stores race for the only slot left: exactly one wins.
    let other = WorkflowStore::open(home.path()).unwrap();
    let (x, y) = tokio::join!(
        store.charge_step(home.path(), "a", "fx", ChargeKind::Effect),
        other.charge_step(home.path(), "b", "fx", ChargeKind::Effect),
    );
    assert_ne!(x.is_ok(), y.is_ok(), "{x:?} {y:?}");
    let refused = x.err().or(y.err()).unwrap();
    assert_eq!(refused, ledger::BUDGET_MONTH);
    let total = store
        .with_connection(|c| ledger::month_total_in(c, "", "wf", &ledger::current_month()))
        .await
        .unwrap();
    assert_eq!(total, 6);
    drop(other);
}

#[tokio::test]
async fn ledger_rows_are_immutable_and_limit_runs_do_not_trip_the_breaker() {
    let home = home_with("");
    let store = WorkflowStore::open(home.path()).unwrap();
    seed_run(&store, "r", Some("act"), 10, 10, None).await;
    charge(&store, home.path(), "r", "s", ChargeKind::Read)
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(home.path().join("workflow.db")).unwrap();
    assert!(
        conn.execute("UPDATE workflow_cost_entries SET amount_micros=0", [])
            .is_err()
    );
    assert!(
        conn.execute("DELETE FROM workflow_cost_entries", [])
            .is_err()
    );
    drop(conn);
    seed_run(
        &store,
        "limited",
        Some("act"),
        10,
        10,
        Some(run_control::FAILURE_LIMIT),
    )
    .await;
    seed_run(
        &store,
        "gated",
        Some("act"),
        10,
        10,
        Some(run_control::FAILURE_GATE),
    )
    .await;
    let counted: Vec<String> = store
        .with_connection(|c| {
            let mut q = c.prepare(&consecutive_failure_sql("")).unwrap();
            let rows = q
                .query_map(rusqlite::params!["wf", "other", "act", 10], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        counted,
        vec!["blocked".to_string()],
        "only the gate failure counts"
    );
}

// ── A-H-1: effect target pinning ──

pub(super) fn args_schema() -> TypedSchema {
    TypedSchema::Object {
        properties: BTreeMap::from([
            ("task_id".into(), TypedSchema::String { max_length: 64 }),
            ("title".into(), TypedSchema::String { max_length: 64 }),
        ]),
        required: BTreeSet::from(["task_id".into(), "title".into()]),
    }
}

pub(super) fn step(id: &str, action: StepAction, input: InputRef) -> StepDefinition {
    StepDefinition {
        step_id: id.into(),
        action,
        input,
        input_schema: args_schema(),
        output_schema: args_schema(),
        timeout_seconds: 10,
        max_read_attempts: 1,
    }
}

pub(super) fn definition(steps: Vec<StepDefinition>) -> WorkflowDefinition {
    WorkflowDefinition {
        schema_version: 1,
        workflow_id: "wf".into(),
        revision: 1,
        skill_revision_hash: "s".into(),
        input_schema: args_schema(),
        output_schema: args_schema(),
        required_capabilities: BTreeSet::from(["tasks_update".into()]),
        steps,
    }
}

pub(super) fn template(tool: &str, scope: &[(&str, Value)]) -> EffectTemplate {
    EffectTemplate {
        step_id: "update".into(),
        tool: tool.into(),
        input_schema: args_schema(),
        resource_scope: scope
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        receipt_adapter_version: 1,
    }
}

pub(super) fn effect(input: InputRef) -> StepDefinition {
    step(
        "update",
        StepAction::McpEffect {
            tool: "tasks_update".into(),
            template_id: "t".into(),
        },
        input,
    )
}

#[test]
fn effect_targets_must_be_pinned_and_fixed_by_the_definition() {
    let literal = InputRef::Literal {
        value: json!({"task_id": "task-1", "title": "x"}),
    };
    let pinned = template("tasks_update", &[("task_id", json!("task-1"))]);
    // Pinned id and a literal input producing it: accepted.
    check_pinned_target(&definition(vec![effect(literal.clone())]), &pinned).unwrap();
    // Through an approval step (passes its input through): still fixed.
    let via_approval = definition(vec![
        step(
            "confirm",
            StepAction::Approval {
                summary: "ok?".into(),
            },
            literal.clone(),
        ),
        effect(InputRef::StepOutput {
            step_id: "confirm".into(),
            pointer: String::new(),
        }),
    ]);
    check_pinned_target(&via_approval, &pinned).unwrap();
    // Empty scope: refused, by the template check and by GrantSpec::validate's helper.
    let empty = template("tasks_update", &[]);
    assert!(template_target(&empty).is_err());
    assert!(check_pinned_target(&definition(vec![effect(literal.clone())]), &empty).is_err());
    // The id comes from a read's output: not fixed, cannot activate.
    let from_read = definition(vec![
        step(
            "fetch",
            StepAction::McpRead {
                tool: "web_fetch_cached".into(),
            },
            literal.clone(),
        ),
        effect(InputRef::StepOutput {
            step_id: "fetch".into(),
            pointer: String::new(),
        }),
    ]);
    assert!(check_pinned_target(&from_read, &pinned).is_err());
    // From the run input: not fixed either.
    let from_input = definition(vec![effect(InputRef::RunInput {
        pointer: String::new(),
    })]);
    assert!(check_pinned_target(&from_input, &pinned).is_err());
    // A literal naming another task than the pinned one.
    let other = template("tasks_update", &[("task_id", json!("task-2"))]);
    assert!(check_pinned_target(&definition(vec![effect(literal)]), &other).is_err());
}

#[test]
fn unknown_effect_tools_and_cron_name_selection_fail_closed() {
    assert!(template_target(&template("send_message", &[("id", json!("x"))])).is_err());
    let cron = template("update_cron_task", &[("id", json!("c1"))]);
    template_target(&cron).unwrap();
    assert!(
        template_target(&template(
            "update_cron_task",
            &[("id", json!("c1")), ("name", json!("n"))]
        ))
        .is_err()
    );
    assert!(check_effect_arguments(&cron, &json!({"id": "c1", "name": "rename"})).is_err());
    assert!(check_effect_arguments(&cron, &json!({"id": "c2"})).is_err());
    check_effect_arguments(&cron, &json!({"id": "c1", "enabled": false})).unwrap();
}
