//! F1a end to end: an activated run that stops for a person continues on the
//! same run after the real `approvals.decide` RPC, through the decision's
//! resume outbox, the sweep and the queue handoff; refusals and expiry end
//! it; a crash before or during an effect never repeats it.
//! Requires `DUDUCLAW_P1_PILOT_BINARY` (verified stdio CLI), like the pilot.
use super::pilot_test_factory::Pilot;
use super::*;
use crate::approval::{
    AcceptedRevisionGrant, ApprovalId, ApprovalStatus, EffectTemplate, ExecutionBinding, GrantSpec,
    OperationState, RequestKind, payload_hash,
};
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const TARGET: &str = "resume-target";

/// Exact typed schema of one sample value (objects require every key).
fn schema_of(value: &Value) -> TypedSchema {
    match value {
        Value::Null => TypedSchema::Null,
        Value::Bool(_) => TypedSchema::Boolean,
        Value::Number(n) if n.is_i64() || n.is_u64() => TypedSchema::Integer,
        Value::Number(_) => TypedSchema::Number,
        Value::String(_) => TypedSchema::String { max_length: 4096 },
        Value::Array(items) => TypedSchema::Array {
            items: Box::new(
                items
                    .first()
                    .map(schema_of)
                    .unwrap_or(TypedSchema::String { max_length: 4096 }),
            ),
            max_items: 64,
        },
        Value::Object(map) => TypedSchema::Object {
            properties: map.iter().map(|(k, v)| (k.clone(), schema_of(v))).collect(),
            required: map.keys().cloned().collect(),
        },
    }
}

/// Mirror of the CLI's `tasks_update` receipt evidence for the target row.
fn task_evidence(row: &crate::task_store::TaskRow) -> Value {
    let tags: Vec<&str> = row
        .tags
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let task = json!({
        "id": row.id, "kind": row.kind, "discovery_run_id": row.discovery_run_id,
        "title": row.title, "description": row.description, "status": row.status,
        "priority": row.priority, "assigned_to": row.assigned_to, "created_by": row.created_by,
        "created_at": row.created_at, "updated_at": row.updated_at,
        "completed_at": row.completed_at, "blocked_reason": row.blocked_reason,
        "parent_task_id": row.parent_task_id, "tags": tags, "message_id": row.message_id,
        "claimed_by": row.claimed_by, "lease_expires_at": row.lease_expires_at,
        "lease_renewed_at": row.lease_renewed_at, "goal_id": row.goal_id,
        "depends_on": crate::task_store::parse_depends_on(&row.depends_on),
        "retry_count": row.retry_count, "max_retries": row.max_retries,
        "goal_mode": row.goal_mode, "acceptance_criteria": row.acceptance_criteria,
        "acceptance_criteria_baseline": row.acceptance_criteria_baseline,
        "result_summary": crate::goal_loop::criteria_ledger::display_result_summary(
            row.criteria_ledger.as_deref(), row.result_summary.as_deref()),
        "judge_feedback": row.judge_feedback, "revision_round": row.revision_round,
        "diminishing": row.diminishing, "agent_seconds": row.agent_seconds,
    });
    json!({"adapter":"tasks_update","adapter_version":1,"task_id":row.id,"row":task,"row_hash":"h"})
}

pub(super) struct Resume {
    pub(super) pilot: Pilot,
    pub(super) activation_id: String,
    pub(super) handler: crate::handlers::MethodHandler,
    pub(super) operator: duduclaw_auth::UserContext,
    pub(super) draft: crate::workflow_drafts::WorkflowDraft,
}

impl Resume {
    /// Alice may update tasks only with a person's approval (`effect='ask'`).
    /// The routine is: confirm (approval step) -> update (effect step).
    pub(super) async fn new() -> Self {
        let pilot = Pilot::new().await;
        let root = pilot.home.path().to_path_buf();
        let toml_path = root.join("agents/alice/agent.toml");
        let toml = std::fs::read_to_string(&toml_path).unwrap().replace(
            "allowed_tools=['web_fetch_cached']",
            "allowed_tools=['web_fetch_cached','tasks_update']\n[[capabilities.policy]]\ntool='tasks_update'\neffect='ask'",
        );
        std::fs::write(&toml_path, toml).unwrap();
        // The handler's boot migrations may rewrite policy files; build it
        // before the policy revision is pinned into the grant.
        let users =
            std::sync::Arc::new(duduclaw_auth::UserDb::new(&root.join("users.db")).unwrap());
        let jwt = std::sync::Arc::new(duduclaw_auth::JwtConfig::load_or_generate(&root).unwrap());
        let handler = crate::handlers::MethodHandler::new(root.clone()).await;
        handler.set_user_db(users, jwt).await;
        handler
            .set_task_store(std::sync::Arc::new(
                crate::task_store::TaskStore::open(&root).unwrap(),
            ))
            .await;
        let tasks = crate::task_store::TaskStore::open(&root).unwrap();
        let row = crate::task_store::TaskRow::new(
            TARGET.into(),
            "before-resume".into(),
            "workflow effect target".into(),
            "normal".into(),
            "alice".into(),
            "alice".into(),
        );
        tasks.insert_task(&row).await.unwrap();
        let output = schema_of(&task_evidence(
            &tasks.get_task(TARGET).await.unwrap().unwrap(),
        ));
        let string = TypedSchema::String { max_length: 4096 };
        let args = TypedSchema::Object {
            properties: BTreeMap::from([
                ("task_id".into(), string.clone()),
                ("title".into(), string),
            ]),
            required: BTreeSet::from(["task_id".into(), "title".into()]),
        };
        let mut draft = pilot.draft.clone();
        draft.definition = WorkflowDefinition {
            schema_version: 1,
            workflow_id: "resume-approval-effect".into(),
            revision: 1,
            skill_revision_hash: pilot.draft.definition.skill_revision_hash.clone(),
            input_schema: TypedSchema::Object {
                properties: BTreeMap::new(),
                required: BTreeSet::new(),
            },
            output_schema: output.clone(),
            required_capabilities: BTreeSet::from(["tasks_update".into()]),
            steps: vec![
                StepDefinition {
                    step_id: "confirm".into(),
                    action: StepAction::Approval {
                        summary: "Rename the target task".into(),
                    },
                    input: InputRef::Literal {
                        value: json!({"task_id": TARGET, "title": "after-resume"}),
                    },
                    input_schema: args.clone(),
                    output_schema: args.clone(),
                    timeout_seconds: 60,
                    max_read_attempts: 1,
                },
                StepDefinition {
                    step_id: "update".into(),
                    action: StepAction::McpEffect {
                        tool: "tasks_update".into(),
                        template_id: "update".into(),
                    },
                    input: InputRef::StepOutput {
                        step_id: "confirm".into(),
                        pointer: String::new(),
                    },
                    input_schema: args.clone(),
                    output_schema: output,
                    timeout_seconds: 120,
                    max_read_attempts: 1,
                },
            ],
        };
        draft.draft_id = "resume-draft".into();
        draft.revision_hash = draft.definition.hash();
        draft.creator_grant = crate::workflow_draft_context::creator_grant(&root, "alice").unwrap();
        draft.effect_templates = BTreeMap::from([(
            "update".into(),
            EffectTemplate {
                step_id: "update".into(),
                tool: "tasks_update".into(),
                input_schema: args,
                resource_scope: BTreeMap::from([("task_id".into(), json!(TARGET))]),
                receipt_adapter_version: 1,
            },
        )]);
        draft.routine = None;
        draft.draft_hash = draft.compute_hash();
        pilot.service.store.save_draft(&draft).await.unwrap();
        let activation_id = activate(&pilot, &draft).await;
        let operator = crate::review_evidence::audience::trusted_dashboard_principal(
            &root,
            &pilot.context.principal_id,
        )
        .unwrap();
        Self {
            pilot,
            activation_id,
            handler,
            operator,
            draft,
        }
    }

    pub(super) async fn trigger(&self, request: &str) -> String {
        self.pilot
            .service
            .enqueue_trigger(
                &self.activation_id,
                Trigger::Manual {
                    request_id: request.into(),
                },
                json!({}),
            )
            .await
            .unwrap()
    }

    pub(super) fn queue(&self) -> MessageQueue {
        MessageQueue::open(self.pilot.home.path()).unwrap()
    }

    pub(super) async fn message(&self, id: &str) -> QueueMessage {
        self.queue().get_by_id(id).await.unwrap().unwrap()
    }

    pub(super) async fn dispatch(&self, id: &str) -> WorkflowRun {
        let message = self.message(id).await;
        let service = self.pilot.reopen();
        Box::pin(service.dispatch_queue_message(&message))
            .await
            .unwrap()
    }

    pub(super) async fn step(&self, run: &str, step: &str) -> StepEvidence {
        self.pilot
            .service
            .store
            .get_step(run, step)
            .await
            .unwrap()
            .unwrap()
    }

    pub(super) async fn decide(&self, approval: &str, approve: bool) {
        match Box::pin(self.handler.handle(
            "approvals.decide",
            json!({"id": approval, "approve": approve}),
            &self.operator,
        ))
        .await
        {
            crate::protocol::WsFrame::Response { ok: true, .. } => (),
            other => panic!("approvals.decide failed: {other:?}"),
        }
    }

    /// Sweep, then return the resume message id the decision produced.
    pub(super) async fn sweep_resume(&self, run: &str, approval: &str) -> String {
        let id = resume_outbox(run, "alice", approval).0;
        let service = self.pilot.reopen();
        let report = Box::pin(service.sweep()).await;
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(self.message(&id).await.status, MessageStatus::Pending);
        id
    }

    pub(super) async fn title(&self) -> String {
        crate::task_store::TaskStore::open(self.pilot.home.path())
            .unwrap()
            .get_task(TARGET)
            .await
            .unwrap()
            .unwrap()
            .title
    }

    pub(super) fn operations(&self, run: &str) -> Vec<(String, String)> {
        let conn = rusqlite::Connection::open(self.pilot.home.path().join("approvals.db")).unwrap();
        let mut q = conn
            .prepare(
                "SELECT step_key,state FROM approval_operations WHERE run_id=?1 ORDER BY rowid",
            )
            .unwrap();
        q.query_map([run], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub(super) fn cards(&self, run: &str) -> i64 {
        rusqlite::Connection::open(self.pilot.home.path().join("approvals.db"))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM approvals WHERE json_extract(binding_json,'$.run_id')=?1",
                [run],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Run until the confirm card is approved and the effect waits for its
    /// own card. Returns (run id, confirm card, effect card).
    pub(super) async fn to_effect_card(&self) -> (String, String, String) {
        let run = self.trigger("to-effect").await;
        let first = self.dispatch(&format!("workflow:{run}")).await;
        assert_eq!(first.status, RunStatus::WaitingApproval);
        let confirm = self.step(&run, "confirm").await;
        assert_eq!(confirm.status, StepStatus::WaitingApproval);
        let card = confirm.approval_id.unwrap();
        self.decide(&card, true).await;
        let resume = self.sweep_resume(&run, &card).await;
        let waiting = self.dispatch(&resume).await;
        assert_eq!(waiting.run_id, run);
        assert_eq!(waiting.status, RunStatus::WaitingApproval, "{waiting:?}");
        assert_eq!(
            self.step(&run, "confirm").await.status,
            StepStatus::Succeeded
        );
        let update = self.step(&run, "update").await;
        assert_eq!(update.status, StepStatus::WaitingApproval);
        assert!(
            update.operation_id.is_none(),
            "no operation before the person decides"
        );
        let effect_card = update.approval_id.unwrap();
        assert_ne!(effect_card, card);
        (run, card, effect_card)
    }
}

/// An activation card for `draft` filed the way `request_activation` files
/// it (real activation payload, dashboard context), still pending.
pub(super) async fn activation_card(
    pilot: &Pilot,
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
        fixtures_digest: "resume-fixtures".into(),
        activation_id: activation_id.into(),
        actor: "alice".into(),
        operator_context: pilot.context.clone(),
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
    let environment = pilot.service.runner.executor.environment("alice").unwrap();
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: format!("{activation_id}-request"),
        run_origin_kind: "workflow".into(),
        actor_principal: "alice".into(),
        decision_context: pilot.context.clone(),
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
    let broker = &pilot.service.broker;
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
/// `commit_activation` does, without the fixture gate (whose runs stop at
/// the approval step and can never be evaluated).
async fn activate(pilot: &Pilot, draft: &crate::workflow_drafts::WorkflowDraft) -> String {
    let (acceptance, request, mut revision) = activation_card(pilot, draft, "resume-activation").await;
    let spec = request.spec.clone();
    let broker = &pilot.service.broker;
    let operator = crate::review_evidence::audience::trusted_dashboard_principal(
        pilot.home.path(),
        &pilot.context.principal_id,
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
    pilot
        .service
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
        policy_digests: crate::approval::policy_snapshot::policy_digests(pilot.home.path(), &spec.actor).ok(),
        suspension: None,
        expiry_notice_at: None,
    };
    pilot.service.store_activation(&record).await.unwrap();
    spec.activation_id
}

#[tokio::test]
async fn resume_approval_step_then_human_gated_effect_completes_the_same_run() {
    let r = Resume::new().await;
    let (run, _, effect_card) = r.to_effect_card().await;
    // A duplicate wake-up while the card is pending changes nothing.
    let again = r.dispatch(&format!("workflow:{run}")).await;
    assert_eq!(again.status, RunStatus::WaitingApproval);
    assert_eq!(r.cards(&run), 2, "one decidable card per step");
    assert_eq!(r.title().await, "before-resume");
    r.decide(&effect_card, true).await;
    let resume = r.sweep_resume(&run, &effect_card).await;
    let done = r.dispatch(&resume).await;
    assert_eq!(done.run_id, run);
    assert_eq!(
        done.status,
        RunStatus::Succeeded,
        "{done:?} ops={:?} title={}",
        r.operations(&run),
        r.title().await
    );
    assert_eq!(r.title().await, "after-resume");
    assert_eq!(
        r.operations(&run),
        vec![("update".into(), "succeeded".into())]
    );
    let update = r.step(&run, "update").await;
    assert_eq!(update.status, StepStatus::Succeeded);
    assert_eq!(
        update.evidence_kind,
        ExecutionEvidenceKind::OperationReceipt
    );
    // Replaying every wake-up is a no-op: no second effect, same run.
    for id in [format!("workflow:{run}"), resume] {
        assert_eq!(r.dispatch(&id).await.status, RunStatus::Succeeded);
    }
    assert_eq!(r.operations(&run).len(), 1);
    let view = r.pilot.reopen().run_view(&done).await.unwrap();
    assert_eq!(view["status"], "succeeded");
    assert_eq!(view["steps"][1]["operation"]["state"], "succeeded");
}

#[tokio::test]
async fn resume_after_refusals_and_expiry_ends_the_run_without_an_effect() {
    let r = Resume::new().await;
    // Refused approval step.
    let run = r.trigger("refused-step").await;
    r.dispatch(&format!("workflow:{run}")).await;
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    r.decide(&card, false).await;
    let resume = r.sweep_resume(&run, &card).await;
    let refused = r.dispatch(&resume).await;
    assert_eq!(refused.status, RunStatus::Failed);
    assert_eq!(
        refused.error_code.as_deref(),
        Some("workflow_approval_denied")
    );
    assert_eq!(r.step(&run, "update").await.status, StepStatus::Pending);
    // Refused effect card.
    let (run, _, effect_card) = r.to_effect_card().await;
    r.decide(&effect_card, false).await;
    let resume = r.sweep_resume(&run, &effect_card).await;
    let refused = r.dispatch(&resume).await;
    assert_eq!(refused.status, RunStatus::Failed);
    assert_eq!(
        refused.error_code.as_deref(),
        Some("workflow_approval_denied")
    );
    assert!(
        r.operations(&run).is_empty(),
        "a refused card prepares nothing"
    );
    // Expiry is not an event: only the sweep notices it.
    let run = r.trigger("expired-step").await;
    r.dispatch(&format!("workflow:{run}")).await;
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    rusqlite::Connection::open(r.pilot.home.path().join("approvals.db"))
        .unwrap()
        .execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [&card],
        )
        .unwrap();
    let resume = r.sweep_resume(&run, &card).await;
    assert_eq!(
        r.pilot
            .service
            .broker
            .get(&ApprovalId::from(card.clone()))
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Expired
    );
    let expired = r.dispatch(&resume).await;
    assert_eq!(expired.status, RunStatus::Failed);
    assert_eq!(
        expired.error_code.as_deref(),
        Some("workflow_approval_expired")
    );
    assert_eq!(r.title().await, "before-resume");
}

/// Crash after the operation was prepared, before it began (the review's
/// "kill before execute, restart within seconds"): the dead execution's
/// lease and acked message are released at boot and the run completes once.
#[tokio::test]
async fn crash_before_effect_begins_recovers_and_completes_once() {
    let r = Resume::new().await;
    let (run, _, effect_card) = r.to_effect_card().await;
    r.decide(&effect_card, true).await;
    let resume = resume_outbox(&run, "alice", &effect_card).0;
    crash_state(&r, &run, &effect_card, false).await;
    let report = r.pilot.reopen().reconcile_on_boot().await.unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.leases_cleared, 1);
    assert_eq!(r.message(&resume).await.status, MessageStatus::Pending);
    let done = r.dispatch(&resume).await;
    assert_eq!(done.status, RunStatus::Succeeded, "{done:?}");
    assert_eq!(r.title().await, "after-resume");
    assert_eq!(
        r.operations(&run),
        vec![("update".into(), "succeeded".into())]
    );
}

/// Crash after the effect began: the run never re-sends it and shows the
/// unknown outcome instead of staying `Running`.
#[tokio::test]
async fn crash_during_effect_shows_uncertain_and_never_resends() {
    let r = Resume::new().await;
    let (run, _, effect_card) = r.to_effect_card().await;
    r.decide(&effect_card, true).await;
    let resume = resume_outbox(&run, "alice", &effect_card).0;
    crash_state(&r, &run, &effect_card, true).await;
    r.pilot.reopen().reconcile_on_boot().await.unwrap();
    let after = r.dispatch(&resume).await;
    assert_eq!(after.status, RunStatus::Uncertain, "{after:?}");
    assert_eq!(
        after.error_code.as_deref(),
        Some("effect_outcome_unconfirmed")
    );
    assert_eq!(
        r.title().await,
        "before-resume",
        "the handler never ran again"
    );
    assert_eq!(
        r.operations(&run),
        vec![("update".into(), "executing".into())]
    );
    let view = r.pilot.reopen().run_view(&after).await.unwrap();
    assert_eq!(view["steps"][1]["status"], "uncertain");
    assert_eq!(view["steps"][1]["operation"]["state"], "executing");
}

/// The state a gateway killed inside the effect step leaves behind: step
/// `Running` with its operation id, run `Running`, a live run lease of the
/// dead process and its acked queue message; optionally the CLI had begun.
async fn crash_state(r: &Resume, run: &str, card: &str, began: bool) {
    let broker = &r.pilot.service.broker;
    let id = ApprovalId::from(card.to_string());
    let operation = broker.prepare_operation(&id, "update", None).await.unwrap();
    if began {
        let binding = broker.get(&id).await.unwrap().unwrap().binding.unwrap();
        let claim = broker
            .claim_operation(&operation, &binding, "dead-cli", 300)
            .await
            .unwrap();
        broker.begin_execution(&claim, &binding).await.unwrap();
        assert_eq!(
            broker
                .inspect_operation(&operation)
                .await
                .unwrap()
                .unwrap()
                .state,
            OperationState::Executing
        );
    }
    let store = &r.pilot.service.store;
    let mut step = store.get_step(run, "update").await.unwrap().unwrap();
    step.status = StepStatus::Running;
    step.operation_id = Some(operation);
    r.pilot.service.runner.save_step(run, &step).await.unwrap();
    let mut stored = store.get_run(run).await.unwrap().unwrap();
    stored.status = RunStatus::Running;
    r.pilot.service.runner.save_run(&stored).await.unwrap();
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_run_leases VALUES(?1,'dead-process',?2)",
                rusqlite::params![run, Utc::now().timestamp() + 300],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    let resume = resume_outbox(run, "alice", card).0;
    let report = r.pilot.reopen().sweep().await;
    assert_eq!(
        report.resumed, 0,
        "a live lease means somebody owns the run"
    );
    r.queue().ack(&resume).await.ok();
    if r.queue().get_by_id(&resume).await.unwrap().is_none() {
        // Deliver the decision's resume row, then mark it picked up.
        let mut report = SweepReport::default();
        r.pilot.reopen().deliver_outbox(&mut report).await.unwrap();
        r.queue().ack(&resume).await.unwrap();
    }
    assert_eq!(r.message(&resume).await.status, MessageStatus::Acked);
    // Before the restart, the busy run is never failed by a duplicate.
    let message = r.message(&resume).await;
    match r
        .pilot
        .reopen()
        .dispatch_queue_message_outcome(&message)
        .await
        .unwrap()
    {
        DispatchOutcome::Retry(reason) => assert_eq!(reason, runner::WORKFLOW_BUSY),
        other => panic!("busy run must be retried, got {other:?}"),
    }
}

/// One undeliverable outbox row neither fails another trigger nor stops
/// the rows after it.
#[tokio::test]
async fn bad_outbox_row_is_isolated_from_other_handoffs() {
    let r = Resume::new().await;
    r.pilot
        .service
        .store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_outbox VALUES('workflow:ghost','enqueue','ghost','{}',0)",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    let run = r.trigger("after-bad-row").await;
    assert_eq!(
        r.message(&format!("workflow:{run}")).await.status,
        MessageStatus::Pending
    );
    let report = r.pilot.reopen().sweep().await;
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].starts_with("workflow:ghost: "));
    // Cancelling the waiting run withdraws its card and stops it for good.
    r.dispatch(&format!("workflow:{run}")).await;
    let card = r.step(&run, "confirm").await.approval_id.unwrap();
    let cancelled = r
        .pilot
        .reopen()
        .cancel_run(&run, "dashboard:test")
        .await
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(
        r.pilot
            .service
            .broker
            .get(&ApprovalId::from(card))
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Invalidated
    );
    assert_eq!(
        r.dispatch(&format!("workflow:{run}")).await.status,
        RunStatus::Cancelled
    );
}
