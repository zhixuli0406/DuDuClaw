//! Queue lifecycle regression: an operator pause preserves the original run
//! and every pending checkpoint, even beyond the infrastructure retry limit.
use super::*;
use crate::approval::{AcceptedRevisionGrant, ApprovalId, ExecutionBinding, GrantSpec, RequestKind, payload_hash};
use crate::message_queue::{MessageQueue, MessageStatus};
use chrono::Utc;
use std::collections::BTreeMap;

async fn activation_card(
    service: &WorkflowService,
    context: &crate::approval::DecisionContext,
    draft: &crate::workflow_drafts::WorkflowDraft,
    activation_id: &str,
) -> (ApprovalId, ActivationRequest, ApprovedWorkflowRevision) {
    let expiry = (Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let spec = GrantSpec {
        schema_version: 1,
        workflow_id: draft.definition.workflow_id.clone(),
        workflow_revision: 1,
        revision_hash: draft.revision_hash.clone(),
        skill_hash: draft.definition.skill_revision_hash.clone(),
        fixtures_digest: "autonomy-fixtures".into(),
        activation_id: activation_id.into(),
        actor: "alice".into(),
        operator_context: context.clone(),
        creator_grant: draft.creator_grant.clone(),
        audience: draft.audience.clone(),
        templates: draft.effect_templates.clone(),
        input_max_age_seconds: 600,
        fixtures_expires_at: expiry.clone(),
        budget: draft.budget.clone(),
        expires_at: expiry.clone(),
        policy_revision: draft.creator_grant.policy_revision.clone(),
    };
    let revision = ApprovedWorkflowRevision {
        definition: draft.definition.clone(),
        revision_hash: draft.revision_hash.clone(),
        owner: "alice".into(),
        creator_grant: draft.creator_grant.clone(),
        audience: draft.audience.clone(),
        fixtures_digest: spec.fixtures_digest.clone(),
        acceptance_id: String::new(),
        accepted_at: String::new(),
        expires_at: expiry.clone(),
    };
    let request = ActivationRequest {
        activation_id: spec.activation_id.clone(),
        draft_id: draft.draft_id.clone(),
        draft_hash: draft.draft_hash.clone(),
        revision: revision.clone(),
        spec: spec.clone(),
        fixture_results: vec![],
        fixture_assertions: vec![],
        cron: None,
    };
    let environment = service.runner.executor.environment("alice").unwrap();
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: format!("{activation_id}-request"),
        run_origin_kind: "workflow".into(),
        actor_principal: "alice".into(),
        decision_context: context.clone(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&activation_payload(&request)),
        policy_revision: spec.policy_revision.clone(),
        cwd: Some(environment.cwd.clone()),
        environment_hash: environment.hash(),
        file_hashes: BTreeMap::new(),
        expires_at: expiry.clone(),
        resume_handler: "workflow_v1".into(),
        resume_version: 1,
    };
    let broker = &service.broker;
    let acceptance = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "accept",
            activation_payload(&request),
            binding,
        )
        .await
        .unwrap();
    (acceptance, request, revision)
}

/// Seed an accepted revision and an active grant for `draft` the way
/// `commit_activation` does. This test exercises durable dispatch, not the
/// separate fixture-acceptance gate; no CLI or external service is involved.
async fn activate(service: &WorkflowService, context: &crate::approval::DecisionContext, draft: &crate::workflow_drafts::WorkflowDraft) -> String {
    let (acceptance, request, mut revision) = activation_card(service, context, draft, "autonomy-activation").await;
    let spec = request.spec.clone();
    let broker = &service.broker;
    let operator = crate::review_evidence::audience::trusted_dashboard_principal(
        &service.home,
        &context.principal_id,
    )
    .unwrap();
    broker
        .decide_bound_dashboard(&acceptance, &operator, true)
        .await
        .unwrap();
    revision.acceptance_id = acceptance.to_string();
    revision.accepted_at = broker
        .get(&acceptance)
        .await
        .unwrap()
        .unwrap()
        .decided_at
        .unwrap();
    service
        .store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_revisions VALUES(?1,1,?2,?3)",
                rusqlite::params![
                    revision.definition.workflow_id,
                    revision.revision_hash,
                    serde_json::to_string(&revision).unwrap()
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    let accepted = AcceptedRevisionGrant {
        activation_id: spec.activation_id.clone(),
        acceptance_id: acceptance.to_string(),
        revision,
        spec: spec.clone(),
        spec_hash: spec.hash(),
    };
    let prepared = broker.prepare_revision_grant(&accepted).await.unwrap();
    let grant = broker.activate_revision_grant(&prepared).await.unwrap();
    let record = ActivationRecord {
        material_hash: payload_hash(&serde_json::to_value(&request).unwrap()),
        request,
        acceptance_id: acceptance.to_string(),
        state: ActivationState::Active,
        grant: Some(grant),
        error_code: None,
        policy_digests: crate::approval::policy_snapshot::policy_digests(&service.home, &spec.actor).ok(),
        suspension: None,
        expiry_notice_at: None,
    };
    service.store_activation(&record).await.unwrap();
    spec.activation_id
}


#[tokio::test]
async fn autonomy_pause_workflow_defers_same_handoff_without_spending_retries() {
    let (home, service, request, mut context) = super::runner_tests::process_fixture().await;
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = users.create_user("autonomy@test.invalid", "Autonomy operator", "isolated-test-password", duduclaw_auth::UserRole::Admin).unwrap();
    context.principal_id = admin.id;
    let draft = service.store.draft(&request.draft_id, request.revision).await.unwrap().unwrap();
    let activation_id = activate(&service, &context, &draft).await;
    let run_id = service.enqueue_trigger(&activation_id, Trigger::Manual { request_id: "autonomy-run".into() }, serde_json::Value::Null).await.unwrap();
    let queue = MessageQueue::open(home.path()).unwrap();
    let message_id = super::run_control::enqueue_outbox_id(&run_id);
    duduclaw_core::autonomy_gate::set_pause(home.path(), Some("alice"), "maintenance", "admin").unwrap();
    // Simulate a durable outbox row not marked delivered before restart.
    service.store.with_transaction(|tx| {
        tx.execute("UPDATE workflow_outbox SET delivered=0 WHERE outbox_id=?1", [&message_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }).await.unwrap();
    let mut report = SweepReport::default();
    service.deliver_outbox(&mut report).await.unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.runs_blocked, 0);
    for _ in 0..=super::run_control::MAX_TRANSIENT_RETRIES {
        queue.ack(&message_id).await.unwrap();
        let message = queue.get_by_id(&message_id).await.unwrap().unwrap();
        let outcome = service.dispatch_queue_message_outcome(&message).await.unwrap();
        assert!(matches!(outcome, DispatchOutcome::Deferred));
        super::queue_task::defer_autonomy_pause(&queue, &message_id).await.unwrap();
        let retained = queue.get_by_id(&message_id).await.unwrap().unwrap();
        assert_eq!(retained.status, MessageStatus::Pending);
        assert_eq!(retained.retry_count, 0);
        let run = service.store.get_run(&run_id).await.unwrap().unwrap();
        assert!(!super::run_control::run_is_terminal(run.status));
        assert!(run.failure_class.is_none());
        assert!(service.store.get_steps(&run_id).await.unwrap().iter().all(|s| s.status == StepStatus::Pending));
    }
    duduclaw_core::autonomy_gate::resume(home.path(), Some("alice")).unwrap();
    queue.ack(&message_id).await.unwrap();
    let message = queue.get_by_id(&message_id).await.unwrap().unwrap();
    match service.dispatch_queue_message_outcome(&message).await.unwrap() {
        DispatchOutcome::Done(run) => {
            assert_eq!(run.run_id, run_id);
            assert_eq!(run.status, RunStatus::Succeeded);
        }
        other => panic!("resume must continue the original run: {other:?}"),
    }
    assert!(service.store.get_steps(&run_id).await.unwrap().iter().all(|s| s.status == StepStatus::Succeeded));
}
