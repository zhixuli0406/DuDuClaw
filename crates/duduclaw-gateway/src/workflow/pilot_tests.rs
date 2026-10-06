//! P1 pilot: same production service/runner, verified-env CLI and real web handler.
//! The public HTTP transport provider is fake. No external-provider SLA claim.
use super::pilot_test_factory::Pilot;
use super::*;
use crate::approval::{ApprovalId, ApprovalStatus, CURRENT_DECISION_CONTEXT, payload_hash};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

async fn run_fixed(pilot: &Pilot, index: usize) -> FixtureRunEvidence {
    CURRENT_DECISION_CONTEXT
        .scope(
            Some(pilot.context.clone()),
            pilot.service.run_fixture(pilot.fixture(index)),
        )
        .await
        .unwrap()
}
fn assert_three_sources(evidence: &FixtureRunEvidence) {
    assert_eq!(evidence.status, RunStatus::Succeeded, "{evidence:#?}");
    assert_eq!(evidence.steps.len(), 5);
    assert!(
        evidence
            .steps
            .iter()
            .all(|s| s.status == StepStatus::Succeeded)
    );
    assert_eq!(evidence.effect_call_count, 0);
    let items = evidence.steps.last().unwrap().output.as_ref().unwrap()["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        evidence.steps.last().unwrap().output.as_ref().unwrap()["count"],
        3
    );
    for (i, page) in ["a", "b", "c"].into_iter().enumerate() {
        let step = &evidence.steps[i];
        assert_eq!(step.evidence_kind, ExecutionEvidenceKind::McpRead);
        let value = step.output.as_ref().unwrap();
        let url = format!("http://example.com/p1-pilot/page-{page}");
        assert_eq!(value["url"], url);
        assert_eq!(items[i], *value);
        assert!(
            value["body"]
                .as_str()
                .unwrap()
                .contains(&format!("page-{page}"))
        );
        let observed =
            chrono::DateTime::parse_from_rfc3339(value["fetched_at"].as_str().unwrap()).unwrap();
        assert!(observed <= Utc::now());
        assert!(Utc::now() - observed.with_timezone(&Utc) < chrono::Duration::minutes(10));
        assert_eq!(
            step.output_hash.as_deref(),
            Some(payload_hash(value).as_str())
        );
        let receipt = step
            .receipt
            .as_ref()
            .expect("read evidence must come from actual handler");
        assert_eq!(receipt["source_url"], url);
        assert_eq!(receipt["observed_at"], value["fetched_at"]);
        assert_eq!(
            receipt["source_hash"],
            format!(
                "{:x}",
                Sha256::digest(value["body"].as_str().unwrap().as_bytes())
            )
        );
        assert_eq!(receipt["authenticated"], false);
        assert_eq!(receipt["redirects_followed"], 0);
        assert_eq!(receipt["login_state"], "public_anonymous");
    }
    let output = evidence.steps.last().unwrap().output.as_ref().unwrap();
    assert_eq!(evidence.result_hash, Some(payload_hash(output)));
    assert_eq!(
        evidence.steps[3].evidence_kind,
        ExecutionEvidenceKind::Process
    );
    assert_eq!(
        evidence.steps[4].evidence_kind,
        ExecutionEvidenceKind::ArtifactCommit
    );
}
async fn assert_artifact(pilot: &Pilot, evidence: &FixtureRunEvidence) {
    let step = evidence.steps.last().unwrap();
    let output = step.output.as_ref().unwrap();
    let receipt = step.receipt.as_ref().unwrap();
    assert_eq!(receipt["content_hash"], payload_hash(output));
    assert_eq!(receipt["audience"], json!(pilot.draft.audience));
    let body: String = pilot
        .service
        .store
        .with_connection(|c|
            c.query_row(
                "SELECT content_json FROM workflow_artifact_commits WHERE run_id=?1 AND step_id='artifact'",
                [&evidence.run_id],
                |r| r.get(0)
            )
            .map_err(|e| e.to_string())
        )
        .await
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), *output);
    // File publication must be a production runner receipt, never a test export.
    let path = pilot.home.path().join(
        receipt["path"]
            .as_str()
            .expect("production artifact receipt needs a real path"),
    );
    let path = path.canonicalize().unwrap();
    assert!(
        path.starts_with(
            pilot
                .home
                .path()
                .canonicalize()
                .unwrap()
                .join("agents/alice")
        )
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), *output);
    assert_eq!(receipt["file_hash"], format!("{:x}", Sha256::digest(bytes)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
async fn run_all(pilot: &Pilot) -> Vec<FixtureRunEvidence> {
    let mut results = Vec::new();
    for index in 0..5 {
        let before = pilot.call_count();
        let evidence = run_fixed(pilot, index).await;
        let assertions = crate::workflow_drafts::evaluate_fixture(
            &pilot.draft.fixtures[index].assertions,
            &evidence,
        );
        assert!(
            assertions
                .iter()
                .all(|a| a.outcome == AssertionOutcome::Matched),
            "fixture {index}: {assertions:#?}; evidence {evidence:#?}"
        );
        assert_eq!(
            pilot
                .service
                .fixture_evidence(&evidence.run_id)
                .await
                .unwrap(),
            evidence
        );
        if index == 0 {
            assert_three_sources(&evidence);
            assert_artifact(pilot, &evidence).await;
            assert_eq!(pilot.call_count() - before, 3);
        } else {
            assert_ne!(evidence.status, RunStatus::Succeeded);
            assert_eq!(
                pilot.call_count(),
                before,
                "negative fixture must not reach HTTP handler"
            );
            assert_eq!(evidence.effect_call_count, 0);
            assert!(evidence.steps[0].receipt.is_none());
            assert_eq!(
                evidence.steps[0].evidence_kind,
                ExecutionEvidenceKind::GateDenial
            );
            if index == 4 {
                assert!(
                    !evidence
                        .execution_creator_grant
                        .allowed_tools
                        .contains("web_fetch_cached")
                );
                assert_eq!(
                    evidence.steps[0].error_code.as_deref(),
                    Some("workflow_read_denied:-32003")
                );
            }
        }
        results.push(evidence);
    }
    results
}
async fn accept(pilot: &Pilot, results: Vec<FixtureRunEvidence>) -> ActivationRecord {
    let request = pilot.activation_request(results);
    let binding = pilot.binding(&request);
    let id = CURRENT_DECISION_CONTEXT
        .scope(
            Some(pilot.context.clone()),
            pilot.service.request_activation(request.clone(), binding),
        )
        .await
        .unwrap();
    assert_eq!(
        pilot
            .service
            .broker
            .get(&ApprovalId::from(id.clone()))
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
    assert!(
        pilot
            .service
            .commit_activation(&request.activation_id)
            .await
            .is_err()
    );
    let users = duduclaw_auth::UserDb::new(&pilot.home.path().join("users.db")).unwrap();
    let outsider = users
        .create_user(
            "outsider@test.invalid",
            "Outsider",
            "isolated-test-password",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap();
    let outsider_ctx = crate::review_evidence::audience::trusted_dashboard_principal(
        pilot.home.path(),
        &outsider.id,
    )
    .unwrap();
    assert!(
        pilot
            .service
            .broker
            .decide_bound_dashboard(&ApprovalId::from(id.clone()), &outsider_ctx, true)
            .await
            .is_err()
    );
    let operator = crate::review_evidence::audience::trusted_dashboard_principal(
        pilot.home.path(),
        &pilot.context.principal_id,
    )
    .unwrap();
    pilot
        .service
        .broker
        .decide_bound_dashboard(&ApprovalId::from(id.clone()), &operator, true)
        .await
        .unwrap();
    let record = pilot
        .service
        .commit_activation(&request.activation_id)
        .await
        .unwrap();
    assert_eq!(record.state, ActivationState::Active);
    assert_eq!(record.acceptance_id, id);
    let cron = crate::cron_store::CronStore::open(pilot.home.path())
        .unwrap()
        .get("pilot-monday")
        .await
        .unwrap()
        .unwrap();
    assert!(cron.enabled);
    assert_eq!(cron.cron_timezone.as_deref(), Some("Asia/Taipei"));
    record
}

#[tokio::test]
async fn three_page_pilot_same_service_five_host_fixtures_real_artifact_and_restart() {
    let pilot = Pilot::new().await;
    let results = run_all(&pilot).await;
    let before = pilot.call_count();
    let normal = &results[0];
    let reopened = pilot.reopen();
    let resumed = reopened.runner.execute(&normal.run_id).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Succeeded);
    assert_eq!(
        pilot.call_count(),
        before,
        "restart must reuse all completed steps"
    );
    for step in &normal.steps {
        assert_eq!(
            reopened
                .store
                .get_step(&normal.run_id, &step.step_id)
                .await
                .unwrap()
                .unwrap(),
            *step
        );
    }
    let record = accept(&pilot, results.clone()).await;
    let activation_id = &record.request.activation_id;
    let trigger = Trigger::Scheduled {
        cron_id: "pilot-monday".into(),
        timezone: "Asia/Taipei".into(),
        scheduled_at: "2026-10-05T09:00:00+08:00".into(),
    };
    let (a, b) = tokio::join!(
        pilot
            .service
            .enqueue_trigger(activation_id, trigger.clone(), json!({})),
        pilot
            .service
            .enqueue_trigger(activation_id, trigger.clone(), json!({}))
    );
    let id = a.unwrap();
    assert_eq!(b.unwrap(), id);
    let queue = crate::message_queue::MessageQueue::open(pilot.home.path()).unwrap();
    let message = queue
        .get_by_id(&format!("workflow:{id}"))
        .await
        .unwrap()
        .unwrap();
    let run = pilot
        .service
        .dispatch_queue_message(&message)
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(run.trigger, trigger);
    assert_eq!(pilot.call_count() - before, 3);
    let count = pilot.call_count();
    let reopened = pilot.reopen();
    reopened.reconcile_queue_outbox().await.unwrap();
    let replay = reopened.dispatch_queue_message(&message).await.unwrap();
    assert_eq!(replay.run_id, id);
    assert_eq!(replay.status, RunStatus::Succeeded);
    assert_eq!(pilot.call_count(), count);
    assert_eq!(
        pilot
            .service
            .enqueue_trigger(activation_id, trigger, json!({}))
            .await
            .unwrap(),
        id
    );
    let next = pilot
        .service
        .enqueue_trigger(
            activation_id,
            Trigger::Scheduled {
                cron_id: "pilot-monday".into(),
                timezone: "Asia/Taipei".into(),
                scheduled_at: "2026-10-12T09:00:00+08:00".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    assert_ne!(next, id);
    let manual = Trigger::Manual {
        request_id: "fixed-manual-request".into(),
    };
    let manual_id = pilot
        .service
        .enqueue_trigger(activation_id, manual.clone(), json!({}))
        .await
        .unwrap();
    assert_eq!(
        pilot
            .service
            .enqueue_trigger(activation_id, manual, json!({}))
            .await
            .unwrap(),
        manual_id
    );
    assert_ne!(manual_id, id);
    let revoked = pilot
        .service
        .revoke_activation(activation_id, "pilot operator disabled future work")
        .await
        .unwrap();
    assert_eq!(revoked.state, ActivationState::Revoked);
    assert!(
        !crate::cron_store::CronStore::open(pilot.home.path())
            .unwrap()
            .get("pilot-monday")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(
        pilot
            .service
            .enqueue_trigger(
                activation_id,
                Trigger::Manual {
                    request_id: "post-revoke".into()
                },
                json!({})
            )
            .await
            .is_err()
    );
    let pending = queue
        .get_by_id(&format!("workflow:{next}"))
        .await
        .unwrap()
        .unwrap();
    let blocked = reopened.dispatch_queue_message(&pending).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(pilot.call_count(), count);
    pilot.record_evidence("three-pages-five-fixtures-activation-replay",json!({
        "fixture_results": results,
        "activation": record,
        "scheduled_run": run,
        "replay_run": replay,
        "next_occurrence_run_id": next,
        "manual_run_id": manual_id,
        "revoked_activation": revoked,
        "blocked_after_revoke": blocked
    }));
    let ledger = rusqlite::Connection::open(pilot.home.path().join("approvals.db")).unwrap();
    assert_eq!(
        ledger
            .query_row(
                "SELECT COUNT(*) FROM approvals WHERE status='approved'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1,
        "no synthetic human approvals"
    );
}

#[tokio::test]
async fn three_page_pilot_private_audience_and_current_acl_denial_no_tool_calls() {
    let pilot = Pilot::new().await;
    let normal = run_fixed(&pilot, 0).await;
    assert_three_sources(&normal);
    assert_artifact(&pilot, &normal).await;
    let users = duduclaw_auth::UserDb::new(&pilot.home.path().join("users.db")).unwrap();
    let other = users
        .create_user(
            "other@test.invalid",
            "Other manager",
            "isolated-test-password",
            duduclaw_auth::UserRole::Manager,
        )
        .unwrap();
    users
        .bind_agent(&other.id, "alice", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    let mut ctx = pilot.context.clone();
    ctx.principal_id = other.id;
    let before = pilot.call_count();
    let denied = CURRENT_DECISION_CONTEXT
        .scope(Some(ctx), pilot.service.run_fixture(pilot.fixture(0)))
        .await
        .unwrap();
    assert_eq!(denied.status, RunStatus::Blocked);
    assert_eq!(
        denied.steps[0].error_code.as_deref(),
        Some("permission denied")
    );
    assert_eq!(pilot.call_count(), before);
    users
        .unbind_agent(&pilot.context.principal_id, "alice")
        .unwrap();
    users
        .update_user(
            &pilot.context.principal_id,
            None,
            Some(duduclaw_auth::UserRole::Employee),
            None,
        )
        .unwrap();
    let denied = run_fixed(&pilot, 0).await;
    assert_eq!(denied.status, RunStatus::Blocked);
    assert_eq!(
        denied.steps[0].error_code.as_deref(),
        Some("permission denied")
    );
    assert_eq!(pilot.call_count(), before);
    pilot.record_evidence(
        "private-audience-current-acl",
        json!({"normal":normal,"revoked":denied}),
    );
}

#[tokio::test]
async fn three_page_pilot_source_and_skill_hash_drift_stop_before_stdio() {
    for (path, error) in [
        ("source.md", "workflow_source_evidence_changed"),
        ("SKILLS/report.md", "workflow_skill_changed"),
    ] {
        let pilot = Pilot::new().await;
        let normal = run_fixed(&pilot, 0).await;
        assert_three_sources(&normal);
        let before = pilot.call_count();
        std::fs::write(
            pilot.home.path().join("agents/alice").join(path),
            "changed after accepted snapshot",
        )
        .unwrap();
        let denied = run_fixed(&pilot, 0).await;
        assert_eq!(denied.status, RunStatus::Blocked);
        assert_eq!(denied.steps[0].error_code.as_deref(), Some(error));
        assert_eq!(pilot.call_count(), before);
        pilot.record_evidence(
            "source-skill-drift",
            json!({"mutated_path":path,"normal":normal,"denied":denied}),
        );
    }
}

#[tokio::test]
async fn three_page_pilot_accepted_revision_drift_rules_after_activation() {
    // E-H5c (F1b): after activation the source task and its artifacts no
    // longer gate runs (was: source.md drift blocked them); the installed
    // skill is still part of the accepted revision and still blocks.
    for (path, error) in [
        ("source.md", None),
        ("SKILLS/report.md", Some("workflow_skill_changed")),
    ] {
        let pilot = Pilot::new().await;
        let results = run_all(&pilot).await;
        let activation = accept(&pilot, results.clone()).await;
        let id = pilot
            .service
            .enqueue_trigger(
                &activation.request.activation_id,
                Trigger::Manual {
                    request_id: "pre-drift-handoff".into(),
                },
                json!({}),
            )
            .await
            .unwrap();
        let queue = crate::message_queue::MessageQueue::open(pilot.home.path()).unwrap();
        let message = queue
            .get_by_id(&format!("workflow:{id}"))
            .await
            .unwrap()
            .unwrap();
        let before = pilot.call_count();
        std::fs::write(
            pilot.home.path().join("agents/alice").join(path),
            "changed after real human acceptance",
        )
        .unwrap();
        let reopened = pilot.reopen();
        let blocked = reopened.dispatch_queue_message(&message).await.unwrap();
        let again = reopened
            .enqueue_trigger(
                &activation.request.activation_id,
                Trigger::Manual {
                    request_id: "after-drift".into(),
                },
                json!({}),
            )
            .await;
        match error {
            Some(code) => {
                assert_eq!(blocked.status, RunStatus::Blocked);
                assert_eq!(blocked.error_code.as_deref(), Some(code));
                assert_eq!(pilot.call_count(), before);
                assert!(again.is_err());
            }
            None => {
                assert_eq!(blocked.status, RunStatus::Succeeded, "{:?}", blocked.error_code);
                assert!(again.is_ok(), "{again:?}");
            }
        }
        pilot.record_evidence("accepted-revision-drift",json!({
            "mutated_path": path,
            "fixtures": results,
            "activation": activation,
            "blocked_run": blocked
        }));
    }
}
