//! F5-A end to end on the real runner, broker and RPCs (needs
//! `DUDUCLAW_P1_PILOT_BINARY`, like the pilot): a suspended activation never
//! returns to active (R-H1), an approved literal effect survives a long human
//! wait (R-M5), a card that lapsed with its run ends as `approval_expired`
//! (B1), lowered limits suspend the routine (R-M3), and step-card notices go
//! only to people who may decide (F5-A notices).
use super::resume_tests::Resume;
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

/// Rewrite fields of a run's stored record that the immutability trigger
/// protects. Test-only: the trigger is recreated when the store reopens.
fn rewrite_run(r: &Resume, run: &str, path: &str, value: &str) {
    let conn = rusqlite::Connection::open(r.pilot.home.path().join("workflow.db")).unwrap();
    conn.execute_batch("DROP TRIGGER IF EXISTS workflow_run_authority_immutable")
        .unwrap();
    conn.execute(
        &format!(
            "UPDATE workflow_runs SET record_json=json_set(record_json,'{path}',?1) WHERE run_id=?2"
        ),
        rusqlite::params![value, run],
    )
    .unwrap();
}

fn age_card(r: &Resume, card: &str) {
    rusqlite::Connection::open(r.pilot.home.path().join("approvals.db"))
        .unwrap()
        .execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [card],
        )
        .unwrap();
}

#[tokio::test]
async fn suspended_activation_never_returns_to_active() {
    let r = Resume::new().await;
    let toml_path = r.pilot.home.path().join("agents/alice/agent.toml");
    let original = std::fs::read_to_string(&toml_path).unwrap();
    let run = r.trigger("suspend-then-revert").await;
    std::fs::write(
        &toml_path,
        original.replace(
            "'web_fetch_cached','tasks_update'",
            "'web_fetch_cached','tasks_update','tasks_list'",
        ),
    )
    .unwrap();
    let blocked = r.dispatch(&format!("workflow:{run}")).await;
    assert_eq!(
        blocked.error_code.as_deref(),
        Some("workflow_policy_changed")
    );
    let service = r.pilot.reopen();
    assert_eq!(
        service
            .activation(&r.activation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ActivationState::Suspended
    );
    // The ledger knows too: the grant behind the activation is revoked.
    assert!(
        service
            .broker
            .activation_revoked(&r.activation_id)
            .await
            .unwrap()
    );
    // Settings restored, fixtures still fresh: re-arming is still refused,
    // for a Manager and for the Admin who approved it.
    std::fs::write(&toml_path, &original).unwrap();
    let manager = {
        let users = duduclaw_auth::UserDb::new(&r.pilot.home.path().join("users.db")).unwrap();
        let user = users
            .create_user(
                "mgr@test.invalid",
                "mgr",
                "isolated-test-password",
                duduclaw_auth::UserRole::Manager,
            )
            .unwrap();
        users
            .bind_agent(&user.id, "alice", duduclaw_auth::AccessLevel::Operator)
            .unwrap();
        crate::review_evidence::audience::trusted_dashboard_principal(r.pilot.home.path(), &user.id)
            .unwrap()
    };
    let params = json!({
        "draft_id": r.draft.draft_id,
        "revision": r.draft.revision,
        "draft_hash": r.draft.draft_hash,
    });
    for ctx in [&manager, &r.operator] {
        assert!(
            call(&r, "workflow_drafts.commit_activation", params.clone(), ctx)
                .await
                .is_err()
        );
    }
    assert!(service.commit_activation(&r.activation_id).await.is_err());
    let record = service.activation(&r.activation_id).await.unwrap().unwrap();
    assert_eq!(record.state, ActivationState::Suspended);
    // The projection cannot be stored back to active either.
    let mut forged = record.clone();
    forged.state = ActivationState::Active;
    assert!(service.store_activation(&forged).await.is_err());
    // And no new run starts.
    assert!(
        service
            .enqueue_trigger(
                &r.activation_id,
                Trigger::Manual {
                    request_id: "again".into()
                },
                json!({})
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn approved_literal_effect_survives_a_long_human_wait() {
    let r = Resume::new().await;
    let (run, _, effect_card) = r.to_effect_card().await;
    // The person took an hour; the grant's input age is ten minutes.
    rewrite_run(
        &r,
        &run,
        "$.input_observed_at",
        &(chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
    );
    r.decide(&effect_card, true).await;
    let resume = r.sweep_resume(&run, &effect_card).await;
    let done = r.dispatch(&resume).await;
    assert_eq!(done.status, RunStatus::Succeeded, "{done:?}");
    assert_eq!(r.title().await, "after-resume");
}

#[tokio::test]
async fn a_card_that_lapsed_with_its_run_ends_as_approval_expired() {
    let r = Resume::new().await;
    let run = r.trigger("lapsed").await;
    r.dispatch(&format!("workflow:{run}")).await;
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    rewrite_run(
        &r,
        &run,
        "$.deadline_at",
        &(chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
    );
    age_card(&r, &card);
    let ended = r.dispatch(&format!("workflow:{run}")).await;
    assert_eq!(ended.status, RunStatus::Failed, "{ended:?}");
    assert_eq!(
        ended.error_code.as_deref(),
        Some("workflow_approval_expired")
    );
}

#[tokio::test]
async fn lowered_limits_suspend_the_routine_at_the_next_trigger() {
    let r = Resume::new().await;
    let config = r.pilot.home.path().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[workflow.limits]\nmax_effects_per_run=0\n");
    std::fs::write(&config, text).unwrap();
    let err = r
        .pilot
        .service
        .enqueue_trigger(
            &r.activation_id,
            Trigger::Manual {
                request_id: "low".into(),
            },
            json!({}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, cost_ledger::LIMIT_EFFECTS);
    let record = r
        .pilot
        .service
        .activation(&r.activation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, ActivationState::Suspended);
    assert!(record.error_code.unwrap().starts_with("limit:"));
}

#[tokio::test]
async fn step_card_notices_go_only_to_people_who_may_decide() {
    let r = Resume::new().await;
    let home = r.pilot.home.path();
    let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    // A Manager with access to the employee but outside the run's audience.
    let outsider = users
        .create_user(
            "out@test.invalid",
            "out",
            "isolated-test-password",
            duduclaw_auth::UserRole::Manager,
        )
        .unwrap();
    users
        .bind_agent(&outsider.id, "alice", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    // An Employee account in no position to decide.
    users
        .create_user(
            "emp@test.invalid",
            "emp",
            "isolated-test-password",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap();
    let ids =
        workflow_notify::step_card_user_ids(home, "alice", &r.draft.source_task, &r.draft.audience)
            .await;
    assert_eq!(ids, vec![r.operator.user_id.clone()]);
    // Nobody has a verified chat here: the notice has no destination and
    // the card is only in the dashboard inbox.
    assert!(workflow_notify::links_of(home, &ids).is_empty());
    let text = workflow_notify::step_card_notice("alice", false, "2026-10-07 09:00 UTC");
    assert!(text.contains("儀表板") && !text.contains("resume-target"));
}
