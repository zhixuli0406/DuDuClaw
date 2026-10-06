//! F5-A unit cases (no CLI binary): structural limits (R-M3), run-input
//! dependence (R-M5), infrastructure refusals (R-M2), activation lifetime
//! (U8), the stale and limit classes, and the grant's fixture rule.
use super::cost_ledger::{self as ledger};
use super::effect_targets::step_uses_run_input;
use super::f1b_unit_tests::{definition, home_with, seed_run, step};
use super::suspension::{LifetimeAction, lifetime_action};
use super::*;
use chrono::Utc;
use serde_json::json;

fn effect_step(id: &str, input: InputRef) -> StepDefinition {
    step(
        id,
        StepAction::McpEffect {
            tool: "tasks_update".into(),
            template_id: id.into(),
        },
        input,
    )
}

fn literal() -> InputRef {
    InputRef::Literal {
        value: json!({"task_id": "t", "title": "x"}),
    }
}

#[test]
fn a_definition_over_the_limits_cannot_be_activated() {
    let eleven: Vec<_> = (0..11)
        .map(|i| effect_step(&format!("e{i}"), literal()))
        .collect();
    let home = home_with("");
    let pricing = ledger::load_pricing(home.path());
    assert_eq!(
        ledger::structural_limit_error(&definition(eleven), 1_000, &pricing),
        Some(ledger::LIMIT_EFFECTS)
    );
    let two = definition(vec![
        effect_step("a", literal()),
        effect_step("b", literal()),
    ]);
    assert_eq!(ledger::structural_limit_error(&two, 1_000, &pricing), None);
    // With prices, the minimum cost of one pass must fit the per-run budget.
    let priced = home_with("[workflow.unit_cost_micros]\neffect=600\n");
    let pricing = ledger::load_pricing(priced.path());
    assert_eq!(
        ledger::structural_limit_error(&two, 1_000, &pricing),
        Some(ledger::BUDGET_RUN)
    );
    assert_eq!(ledger::structural_limit_error(&two, 1_200, &pricing), None);
    // Lowered limits after activation are caught the same way.
    let low = home_with("[workflow.limits]\nmax_steps_per_run=1\n");
    assert_eq!(
        ledger::structural_limit_error(&two, 1_000, &ledger::load_pricing(low.path())),
        Some(ledger::LIMIT_STEPS)
    );
    // A broken limits table is a configuration error, not a limit class.
    let bad = home_with("[workflow.limits]\nmax_reads_per_run='x'\n");
    assert_eq!(
        ledger::structural_limit_error(&two, 1_000, &ledger::load_pricing(bad.path())),
        Some(ledger::LIMIT_CONFIG)
    );
    assert!(!ledger::is_limit_error(ledger::LIMIT_CONFIG));
    assert!(ledger::is_limit_error(ledger::LIMIT_EFFECTS));
}

#[test]
fn only_effects_that_read_the_run_input_are_held_to_its_age() {
    let def = definition(vec![
        step(
            "fetch",
            StepAction::McpRead {
                tool: "web_fetch_cached".into(),
            },
            InputRef::RunInput {
                pointer: String::new(),
            },
        ),
        step(
            "confirm",
            StepAction::Approval {
                summary: "ok?".into(),
            },
            literal(),
        ),
        effect_step(
            "literal",
            InputRef::StepOutput {
                step_id: "confirm".into(),
                pointer: String::new(),
            },
        ),
        effect_step(
            "from_input",
            InputRef::StepOutput {
                step_id: "fetch".into(),
                pointer: String::new(),
            },
        ),
    ]);
    assert!(!step_uses_run_input(&def, "literal"));
    assert!(step_uses_run_input(&def, "from_input"));
    assert!(
        step_uses_run_input(&def, "missing"),
        "unknown counts as dependent"
    );
}

#[test]
fn infrastructure_answers_before_begin_are_not_refusals() {
    use super::executor::is_infrastructure_refusal;
    assert!(is_infrastructure_refusal(
        crate::approval::OPERATION_LEASE_HELD
    ));
    assert!(is_infrastructure_refusal("claim: database is locked"));
    assert!(!is_infrastructure_refusal(
        "effective resource outside accepted scope"
    ));
    assert!(!is_infrastructure_refusal(
        "workflow effect not reserved in the cost ledger"
    ));
}

#[test]
fn activation_days_default_bounds_and_errors() {
    assert_eq!(activation::activation_days(home_with("").path()), Ok(30));
    assert_eq!(
        activation::activation_days(home_with("[workflow]\nactivation_days=7\n").path()),
        Ok(7)
    );
    for bad in ["0", "366", "'30'", "-1"] {
        let home = home_with(&format!("[workflow]\nactivation_days={bad}\n"));
        assert!(activation::activation_days(home.path()).is_err(), "{bad}");
    }
    assert!(activation::activation_days(home_with("[[[").path()).is_err());
}

#[test]
fn lifetime_expires_at_the_end_and_warns_once_three_days_ahead() {
    let now = Utc::now();
    let at = |d: chrono::Duration| (now + d).to_rfc3339();
    assert_eq!(
        lifetime_action(&at(chrono::Duration::days(10)), false, now),
        LifetimeAction::Nothing
    );
    assert_eq!(
        lifetime_action(&at(chrono::Duration::days(2)), false, now),
        LifetimeAction::Notice
    );
    assert_eq!(
        lifetime_action(&at(chrono::Duration::days(2)), true, now),
        LifetimeAction::Nothing
    );
    assert_eq!(
        lifetime_action(&at(chrono::Duration::seconds(-1)), true, now),
        LifetimeAction::Expire
    );
    assert_eq!(
        lifetime_action("not a time", false, now),
        LifetimeAction::Expire
    );
}

#[test]
fn stale_data_is_its_own_class_and_does_not_trip_the_breaker() {
    assert!(run_control::is_stale_error("workflow_input_expired"));
    assert!(run_control::is_stale_error("workflow_read_data_expired"));
    assert!(!run_control::is_stale_error("workflow_policy_changed"));
    assert!(consecutive_failure_sql("").contains(run_control::FAILURE_STALE));
    assert!(run_control::is_transient_error(
        run_control::POLICY_UNREADABLE
    ));
}

#[tokio::test]
async fn limit_streak_counts_only_consecutive_limit_endings() {
    let home = home_with("");
    let store = std::sync::Arc::new(WorkflowStore::open(home.path()).unwrap());
    let broker = std::sync::Arc::new(crate::approval::ApprovalBroker::open(home.path()).unwrap());
    let service = WorkflowService::new(
        home.path().to_path_buf(),
        std::env::current_exe().unwrap(),
        store.clone(),
        broker,
    )
    .unwrap();
    for i in 0..2 {
        seed_run(
            &store,
            &format!("l{i}"),
            Some("act"),
            10,
            10,
            Some(run_control::FAILURE_LIMIT),
        )
        .await;
    }
    assert!(!service.consecutive_limit_blocks("act", 3).await.unwrap());
    seed_run(
        &store,
        "l2",
        Some("act"),
        10,
        10,
        Some(run_control::FAILURE_LIMIT),
    )
    .await;
    assert!(service.consecutive_limit_blocks("act", 3).await.unwrap());
    // A different class in the window breaks the streak.
    seed_run(
        &store,
        "g",
        Some("act"),
        10,
        10,
        Some(run_control::FAILURE_GATE),
    )
    .await;
    assert!(!service.consecutive_limit_blocks("act", 3).await.unwrap());
}
