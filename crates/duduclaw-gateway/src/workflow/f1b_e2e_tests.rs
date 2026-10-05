//! F1b end to end on the real runner, broker and dashboard RPCs: who may
//! decide a bound card (A-M-3), the Admin-only activation card (U3), the
//! suspension on authority drift (E-H5) and the run/money limits (E-H3).
//! Requires `DUDUCLAW_P1_PILOT_BINARY`, like the pilot.
use super::resume_tests::{Resume, activation_card};
use super::*;
use crate::protocol::WsFrame;
use serde_json::{Value, json};

async fn call(
    r: &Resume,
    method: &str,
    params: Value,
    ctx: &duduclaw_auth::UserContext,
) -> Result<Value, String> {
    match Box::pin(r.handler.handle(method, params, ctx)).await {
        WsFrame::Response {
            ok: true, payload, ..
        } => Ok(payload.unwrap_or(Value::Null)),
        WsFrame::Response { error, .. } => Err(error.map(|e| e.to_string()).unwrap_or_default()),
        other => Err(format!("{other:?}")),
    }
}

/// A dashboard account with the given role and optional Operator binding
/// to alice, read back through the same fresh-identity path the RPCs use.
fn account(
    r: &Resume,
    email: &str,
    role: duduclaw_auth::UserRole,
    bind_alice: bool,
) -> duduclaw_auth::UserContext {
    let home = r.pilot.home.path();
    let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let user = users
        .create_user(email, email, "isolated-test-password", role)
        .unwrap();
    if bind_alice {
        users
            .bind_agent(&user.id, "alice", duduclaw_auth::AccessLevel::Operator)
            .unwrap();
    }
    crate::review_evidence::audience::trusted_dashboard_principal(home, &user.id).unwrap()
}

fn listed<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
}

#[tokio::test]
async fn bound_step_card_needs_employee_access_and_run_audience() {
    let r = Resume::new().await;
    let run = r.trigger("authority").await;
    r.dispatch(&format!("workflow:{run}")).await;
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    let stranger = account(
        &r,
        "stranger@test.invalid",
        duduclaw_auth::UserRole::Manager,
        false,
    );
    let bound = account(
        &r,
        "bound@test.invalid",
        duduclaw_auth::UserRole::Manager,
        true,
    );
    // No access to the employee: refused and not even listed.
    let err = call(
        &r,
        "approvals.decide",
        json!({"id": card, "approve": true}),
        &stranger,
    )
    .await
    .unwrap_err();
    assert!(err.contains("access"), "{err}");
    let list = call(&r, "approvals.list", json!({}), &stranger)
        .await
        .unwrap();
    assert!(listed(&list, &card).is_none());
    // Access to the employee but outside the run's audience: refused; the
    // card is listed without its binding.
    let err = call(
        &r,
        "approvals.decide",
        json!({"id": card, "approve": true}),
        &bound,
    )
    .await
    .unwrap_err();
    assert!(err.contains("audience"), "{err}");
    let list = call(&r, "approvals.list", json!({}), &bound).await.unwrap();
    let item = listed(&list, &card).unwrap();
    assert_eq!(item["may_decide"], false);
    assert!(item["binding"].is_null() && item["answer"].is_null());
    assert_eq!(
        r.pilot
            .service
            .broker
            .poll(&crate::approval::ApprovalId::from(card.clone()))
            .await
            .unwrap(),
        crate::approval::ApprovalStatus::Pending
    );
    // The audience member decides it, and sees the binding.
    let list = call(&r, "approvals.list", json!({}), &r.operator)
        .await
        .unwrap();
    let item = listed(&list, &card).unwrap();
    assert_eq!(item["may_decide"], true);
    assert!(!item["binding"].is_null());
    r.decide(&card, true).await;
}

#[tokio::test]
async fn activation_card_is_admin_only_and_reports_self_approval() {
    let r = Resume::new().await;
    let (card, _, _) = activation_card(&r.pilot, &r.draft, "f1b-second-activation").await;
    let card = card.to_string();
    let manager = account(
        &r,
        "manager@test.invalid",
        duduclaw_auth::UserRole::Manager,
        true,
    );
    let err = call(
        &r,
        "approvals.decide",
        json!({"id": card, "approve": true}),
        &manager,
    )
    .await
    .unwrap_err();
    // Refused either by the draft-audience check or by the Admin-only rule.
    assert!(
        err.contains("Admin") || err.contains("permission denied"),
        "{err}"
    );
    assert_eq!(
        r.pilot
            .service
            .broker
            .poll(&crate::approval::ApprovalId::from(card.clone()))
            .await
            .unwrap(),
        crate::approval::ApprovalStatus::Pending
    );
    // The card names each effect's fixed target and who submitted it.
    let list = call(&r, "approvals.list", json!({}), &r.operator)
        .await
        .unwrap();
    let item = listed(&list, &card).unwrap();
    assert_eq!(item["kind"], "workflow_activation");
    assert_eq!(item["decided_in_dashboard_only"], true);
    assert_eq!(item["submitter_is_viewer"], true);
    assert_eq!(
        item["workflow_activation"]["effect_targets"][0]["target"],
        super::resume_tests::TARGET
    );
    // The submitting Admin may approve it; the answer says so.
    let decided = call(
        &r,
        "approvals.decide",
        json!({"id": card, "approve": true}),
        &r.operator,
    )
    .await
    .unwrap();
    assert_eq!(decided["submitter_is_decider"], true);
}

#[tokio::test]
async fn authority_change_suspends_the_activation_and_names_the_category() {
    let r = Resume::new().await;
    let toml_path = r.pilot.home.path().join("agents/alice/agent.toml");
    let run = r.trigger("drift").await;
    // An unrelated edit (heartbeat interval) does not stop the run.
    let text = std::fs::read_to_string(&toml_path).unwrap();
    std::fs::write(
        &toml_path,
        text.replace("interval_seconds=3600", "interval_seconds=7200"),
    )
    .unwrap();
    let waiting = r.dispatch(&format!("workflow:{run}")).await;
    assert_eq!(waiting.status, RunStatus::WaitingApproval, "{waiting:?}");
    // A capability change suspends the activation at the next gate.
    let text = std::fs::read_to_string(&toml_path).unwrap();
    std::fs::write(
        &toml_path,
        text.replace(
            "'web_fetch_cached','tasks_update'",
            "'web_fetch_cached','tasks_update','tasks_list'",
        ),
    )
    .unwrap();
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    // The decision itself is refused: the card's policy no longer matches.
    let err = call(
        &r,
        "approvals.decide",
        json!({"id": card, "approve": true}),
        &r.operator,
    )
    .await
    .unwrap_err();
    assert!(err.contains("policy"), "{err}");
    let blocked = r.dispatch(&format!("workflow:{run}")).await;
    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(
        blocked.error_code.as_deref(),
        Some("workflow_policy_changed")
    );
    let record = r
        .pilot
        .service
        .activation(&r.activation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, ActivationState::Suspended);
    let suspension = record.suspension.unwrap();
    assert_eq!(
        suspension.changed_categories,
        vec!["capabilities".to_string()]
    );
    // No new run starts, and the reason is visible through the run RPC.
    assert!(
        r.pilot
            .service
            .enqueue_trigger(
                &r.activation_id,
                Trigger::Manual {
                    request_id: "after".into()
                },
                json!({})
            )
            .await
            .is_err()
    );
    let view = call(&r, "workflow_runs.get", json!({"run_id": run}), &r.operator)
        .await
        .unwrap();
    assert_eq!(view["activation"]["state"], "suspended");
    assert_eq!(
        view["activation"]["suspension"]["changed_categories"][0],
        "capabilities"
    );
    let activity: i64 = rusqlite::Connection::open(r.pilot.home.path().join("tasks.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM activity WHERE event_type='workflow_activation_suspended'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(activity, 1);
}

#[tokio::test]
async fn run_count_limit_and_unit_prices_follow_the_ledger() {
    let r = Resume::new().await;
    let config = r.pilot.home.path().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[workflow.limits]\nmax_runs_per_month=1\n[workflow.unit_cost_micros]\napproval=3\neffect=4\n");
    std::fs::write(&config, text).unwrap();
    let run = r.trigger("only").await;
    // A second trigger this month is refused; repeating the first reconnects.
    let err = r
        .pilot
        .service
        .enqueue_trigger(
            &r.activation_id,
            Trigger::Manual {
                request_id: "second".into(),
            },
            json!({}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, cost_ledger::LIMIT_RUNS);
    assert_eq!(r.trigger("only").await, run);
    // Confirm, then the human-gated effect: one approval and one effect charge.
    r.dispatch(&format!("workflow:{run}")).await;
    let confirm = r.step(&run, "confirm").await.approval_id.unwrap();
    r.decide(&confirm, true).await;
    let resume = r.sweep_resume(&run, &confirm).await;
    r.dispatch(&resume).await;
    let effect = r.step(&run, "update").await.approval_id.unwrap();
    r.decide(&effect, true).await;
    let resume = r.sweep_resume(&run, &effect).await;
    let done = r.dispatch(&resume).await;
    assert_eq!(done.status, RunStatus::Succeeded, "{done:?}");
    assert_eq!(done.cost.tools, 7);
    let view = call(&r, "workflow_runs.get", json!({"run_id": run}), &r.operator)
        .await
        .unwrap();
    assert_eq!(view["pricing"]["money_limits_effective"], true);
    assert_eq!(view["cost"]["tools"], 7);
}

/// A revocation race driven through the real runner (not a stdio fixture):
/// the person approved the effect, the runner prepared the operation and is
/// about to hand it to the CLI when an Admin revokes the activation. The CLI
/// claim is then refused; the task is untouched and the run never succeeds.
#[tokio::test]
async fn runner_effect_revoked_between_prepare_and_execute_never_runs() {
    let r = Resume::new().await;
    let (run, _, effect_card) = r.to_effect_card().await;
    r.decide(&effect_card, true).await;
    let resume = r.sweep_resume(&run, &effect_card).await;
    let message = r.message(&resume).await;
    let runner_side = r.pilot.reopen();
    let revoker = r.pilot.reopen();
    let activation = r.activation_id.clone();
    let (outcome, revoked) = super::security_race_tests::race_at(
        "runner_before_effect_execute",
        runner_side.dispatch_queue_message(&message),
        revoker.revoke_activation(&activation, "runner race"),
    )
    .await;
    assert_eq!(revoked.unwrap().state, ActivationState::Revoked);
    let stored = r.pilot.service.store.get_run(&run).await.unwrap().unwrap();
    assert_ne!(
        stored.status,
        RunStatus::Succeeded,
        "{outcome:?} {stored:?}"
    );
    assert_eq!(r.title().await, "before-resume");
    for (_, state) in r.operations(&run) {
        assert_ne!(state, "succeeded");
        assert_ne!(state, "executing");
    }
}
