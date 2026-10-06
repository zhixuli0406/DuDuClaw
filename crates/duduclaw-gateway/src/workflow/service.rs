//! Server-owned workflow entry points. No caller-provided passed or approved flags.
use super::executor::{EFFECT_TOOLS, READ_TOOLS, WorkflowExecutor};
use super::runner::WorkflowRunner;
use super::*;
use crate::approval::{
    AcceptedRevisionGrant, ApprovalBroker, ApprovalId, ApprovalStatus, CURRENT_DECISION_CONTEXT,
    ExecutionBinding, RequestKind, payload_hash, policy_revision,
};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

/// How long an activated run may wait for human decisions before it expires
/// (capped by the grant's own expiry). Decision cards expire with it.
pub const RUN_WAIT_WINDOW_HOURS: i64 = 24;

pub struct WorkflowService {
    pub store: Arc<WorkflowStore>,
    pub broker: Arc<ApprovalBroker>,
    pub home: PathBuf,
    pub runner: WorkflowRunner,
}
impl WorkflowService {
    pub async fn enqueue_trigger(
        &self,
        activation_id: &str,
        trigger: Trigger,
        input: Value,
    ) -> Result<String, String> {
        let activation = self
            .activation(activation_id)
            .await?
            .ok_or("workflow activation missing")?;
        if activation.state != ActivationState::Active {
            return Err("workflow activation not active".into());
        }
        let grant = activation
            .grant
            .clone()
            .ok_or("active workflow grant missing")?;
        let (spec, state) = self.broker.inspect_revision_grant(&grant).await?;
        spec.validate()?;
        if state != "active" || spec.hash() != activation.request.spec.hash() {
            return Err("workflow grant revoked or drifted".into());
        }
        let revision = self
            .store
            .get_revision(&spec.workflow_id, spec.workflow_revision)
            .await?
            .ok_or("approved workflow revision missing")?;
        if revision.acceptance_id != activation.acceptance_id
            || revision.definition.hash() != spec.revision_hash
        {
            return Err("workflow accepted revision mismatch".into());
        }
        // R-M3: limits may have been lowered after activation. A definition
        // that can no longer fit is suspended rather than run partially.
        if let Some(code) = super::cost_ledger::structural_limit_error(
            &revision.definition,
            spec.budget.per_run_micros,
            &super::cost_ledger::load_pricing(&self.home),
        ) {
            if code != super::cost_ledger::LIMIT_CONFIG {
                Box::pin(super::suspension::suspend_for_limit(
                    &self.store,
                    &self.broker,
                    &self.home,
                    activation_id,
                    code,
                ))
                .await?;
            }
            return Err(code.into());
        }
        revision.definition.input_schema.validate(&input)?;
        if serde_json::to_vec(&input).map_err(|e| e.to_string())?.len() > MAX_JSON_BYTES {
            return Err("workflow run input byte bound exceeded".into());
        }
        let run = WorkflowRun {
            run_id: uuid::Uuid::new_v4().to_string(),
            trigger_key: trigger.key(&spec.workflow_id, spec.workflow_revision)?,
            trigger,
            workflow_id: spec.workflow_id.clone(),
            revision: spec.workflow_revision,
            workflow_hash: spec.revision_hash.clone(),
            skill_hash: spec.skill_hash.clone(),
            actor: spec.actor.clone(),
            creator_grant: spec.creator_grant.clone(),
            audience: spec.audience.clone(),
            task: None,
            input_hash: payload_hash(&input),
            input,
            input_observed_at: Utc::now().to_rfc3339(),
            policy_revision: spec.policy_revision.clone(),
            environment_hash: self.runner.executor.environment(&spec.actor)?.hash(),
            grant: Some(grant),
            activation_id: Some(activation_id.into()),
            // How long the run may wait for people. Each dispatch has its own
            // execution limit (`runner::EXECUTION_SEGMENT_SECONDS`).
            deadline_at: std::cmp::min(
                Utc::now() + chrono::Duration::hours(RUN_WAIT_WINDOW_HOURS),
                DateTime::parse_from_rfc3339(&spec.expires_at)
                    .map_err(|_| "invalid workflow grant expiry")?
                    .with_timezone(&Utc),
            )
            .to_rfc3339(),
            budget: spec.budget,
            status: RunStatus::Pending,
            cost: CostBreakdown::default(),
            error_code: None,
            created_at: Utc::now().to_rfc3339(),
            decision_context: Some(spec.operator_context),
            failure_class: None,
            cancelled_by: None,
        };
        self.runner.guard(&run).await?;
        // The run and its handoff row commit together (enqueue_run).
        let id = self.enqueue_run(&run, &revision, false).await?;
        #[cfg(test)]
        super::security_race_tests::checkpoint("after_outbox_insert").await;
        // This caller answers only for its own handoff; other undelivered
        // rows are the sweep's business and never fail this trigger.
        let own = super::run_control::enqueue_outbox_id(&id);
        let mut report = super::handoff::SweepReport::default();
        self.deliver_outbox(&mut report).await?;
        let own_error = report
            .errors
            .iter()
            .find(|e| e.starts_with(&format!("{own}: ")))
            .cloned();
        for error in report.errors.iter().filter(|e| Some(*e) != own_error.as_ref()) {
            tracing::warn!(error = %error, "workflow outbox row not delivered");
        }
        if let Some(error) = own_error {
            return Err(error);
        }
        Ok(id)
    }
    pub fn new(
        home: PathBuf,
        binary: PathBuf,
        store: Arc<WorkflowStore>,
        broker: Arc<ApprovalBroker>,
    ) -> Result<Self, String> {
        let home = std::fs::canonicalize(home).map_err(|e| e.to_string())?;
        let executor = WorkflowExecutor::new(home.clone(), binary)?;
        Ok(Self {
            home: home.clone(),
            store: store.clone(),
            broker: broker.clone(),
            runner: WorkflowRunner {
                store,
                broker,
                executor,
                home,
            },
        })
    }
    fn validate_definition(&self, definition: &WorkflowDefinition) -> Result<(), String> {
        definition.validate(
            &READ_TOOLS.iter().map(|s| s.to_string()).collect(),
            &EFFECT_TOOLS.iter().map(|s| s.to_string()).collect(),
        )
    }
    fn staging_scope(&self, definition: &WorkflowDefinition) -> Result<(), String> {
        let raw = std::fs::read_to_string(self.home.join("config.toml"))
            .map_err(|_| "workflow staging not configured")?;
        let config: toml::Value =
            toml::from_str(&raw).map_err(|_| "workflow staging config invalid")?;
        let config = config
            .get("workflow")
            .ok_or("workflow staging not configured")?;
        if config
            .get("fixture_environment")
            .and_then(toml::Value::as_str)
            != Some("staging")
        {
            return Err("workflow fixture requires explicit staging home".into());
        }
        let origins: Vec<&str> = config
            .get("allowed_origins")
            .and_then(toml::Value::as_array)
            .ok_or("workflow staging resource origins required")?
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        if origins.is_empty() {
            return Err("workflow staging resource origins empty".into());
        }
        for step in &definition.steps {
            if let StepAction::McpEffect { tool, .. } = &step.action {
                if let InputRef::Literal { value } = &step.input {
                    super::staging::check_effect_scope(&self.home, tool, value)?;
                }
                // Computed IDs are checked on resolved, native-rewritten arguments
                // by the CLI both during prepare and immediately before effect.
            }
            if let StepAction::McpRead { .. } = &step.action {
                let InputRef::Literal { value } = &step.input else {
                    return Err("workflow staging read requires fixed URL".into());
                };
                let target = value
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("workflow staging URL missing")?;
                let target = url::Url::parse(target).map_err(|_| "workflow staging URL invalid")?;
                if !target.username().is_empty() || target.password().is_some() {
                    return Err("workflow staging URL credentials forbidden".into());
                }
                if !origins.iter().any(|o| {
                    url::Url::parse(o).is_ok_and(|origin| origin.origin() == target.origin())
                }) {
                    return Err("workflow fixture resource outside staging scope".into());
                }
            }
        }
        Ok(())
    }
    pub async fn run_fixture(
        &self,
        request: FixtureExecutionRequest,
    ) -> Result<FixtureRunEvidence, String> {
        self.validate_definition(&request.definition)?;
        self.staging_scope(&request.definition)?;
        if request.input_hash != payload_hash(&request.input)
            || request.actor != request.creator_grant.actor
            || request.creator_grant.policy_revision != policy_revision(&self.home, &request.actor)?
        {
            return Err("workflow fixture creator/input authority changed".into());
        }
        let draft = self
            .store
            .draft(&request.draft_id, request.revision)
            .await?
            .ok_or("workflow fixture draft missing")?;
        let fixed = draft
            .fixtures
            .iter()
            .find(|f| f.fixture_id == request.fixture_id)
            .ok_or("workflow fixture not in draft")?;
        if request.definition != draft.definition
            || request.input != fixed.input
            || request.input_hash != fixed.input_hash
            || request.assertion_hash != fixed.assertion_hash
            || request.kind != fixed.kind
            || request.actor != draft.owner
            || request.audience != draft.audience
            || request.creator_grant != draft.creator_grant
            || request.budget != draft.budget
        {
            return Err("workflow fixture differs from fixed draft".into());
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        let trigger = Trigger::Fixture {
            fixture_id: request.fixture_id.clone(),
            request_id: run_id.clone(),
        };
        let candidate = ApprovedWorkflowRevision {
            definition: request.definition.clone(),
            revision_hash: request.definition.hash(),
            owner: request.actor.clone(),
            creator_grant: request.creator_grant.clone(),
            audience: request.audience.clone(),
            fixtures_digest: String::new(),
            acceptance_id: String::new(),
            accepted_at: String::new(),
            expires_at: request.deadline_at.clone(),
        };
        let context = CURRENT_DECISION_CONTEXT
            .try_with(|c| c.clone())
            .ok()
            .flatten();
        let observed = request
            .input
            .get("observed_at")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let mut execution_creator_grant = request.creator_grant.clone();
        if request.kind == FixtureKind::MissingPermission {
            let tool = request
                .definition
                .steps
                .iter()
                .find_map(|s| match &s.action {
                    StepAction::McpRead { tool } | StepAction::McpEffect { tool, .. } => Some(tool),
                    _ => None,
                })
                .ok_or("permission fixture has no tool boundary")?;
            execution_creator_grant.allowed_tools.remove(tool);
        }
        let run = WorkflowRun {
            run_id: run_id.clone(),
            trigger_key: trigger.key(&request.definition.workflow_id, request.revision)?,
            trigger,
            workflow_id: request.definition.workflow_id.clone(),
            revision: request.revision,
            workflow_hash: candidate.revision_hash.clone(),
            skill_hash: request.definition.skill_revision_hash.clone(),
            actor: request.actor.clone(),
            creator_grant: execution_creator_grant,
            audience: request.audience.clone(),
            task: request.task.clone(),
            input: request.input.clone(),
            input_hash: request.input_hash.clone(),
            input_observed_at: observed,
            policy_revision: request.creator_grant.policy_revision.clone(),
            environment_hash: self.runner.executor.environment(&request.actor)?.hash(),
            grant: None,
            activation_id: None,
            deadline_at: request.deadline_at.clone(),
            budget: request.budget.clone(),
            status: RunStatus::Pending,
            cost: CostBreakdown::default(),
            error_code: None,
            created_at: Utc::now().to_rfc3339(),
            decision_context: context,
            failure_class: None,
            cancelled_by: None,
        };
        self.enqueue_run(&run, &candidate, true).await?;
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO workflow_fixture_run_requests VALUES(?1,?2,NULL)",
                    params![
                        run_id,
                        serde_json::to_string(&request).map_err(|e| e.to_string())?
                    ],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await?;
        let validation = request
            .definition
            .input_schema
            .validate(&request.input)
            .and_then(|_| {
                let scan =
                    duduclaw_security::input_guard::scan_input(&request.input.to_string(), 2);
                if scan.blocked {
                    return Err("input_injection_blocked".into());
                }
                let observed = DateTime::parse_from_rfc3339(&run.input_observed_at)
                    .map_err(|_| "workflow_input_freshness_invalid")?;
                if observed > Utc::now()
                    || Utc::now() - observed.with_timezone(&Utc)
                        > chrono::Duration::seconds(draft.input_max_age_seconds as i64)
                {
                    return Err("workflow_input_expired".into());
                }
                Ok(())
            });
        match validation {
            Ok(()) => {
                if let Err(error) = self.runner.execute(&run_id).await {
                    self.mark_blocked(&run_id, &error).await?;
                }
            }
            Err(error) => self.mark_blocked(&run_id, &error).await?,
        }
        let evidence = self.build_fixture_evidence(&run_id, &request).await?;
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE workflow_fixture_run_requests SET evidence_json=?1
                    WHERE run_id=?2 AND evidence_json IS NULL",
                    params![
                        serde_json::to_string(&evidence).map_err(|e| e.to_string())?,
                        run_id
                    ]
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await?;
        Ok(evidence)
    }
    pub async fn fixture_evidence(&self, run_id: &str) -> Result<FixtureRunEvidence, String> {
        self.store
            .with_connection(|c| {
                let raw: String = c
                    .query_row(
                        "SELECT evidence_json FROM workflow_fixture_run_requests WHERE run_id=?1",
                        params![run_id],
                        |r| r.get(0),
                    )
                    .map_err(|_| "workflow fixture evidence not finalized")?;
                serde_json::from_str(&raw).map_err(|e| e.to_string())
            })
            .await
    }
    async fn build_fixture_evidence(
        &self,
        id: &str,
        request: &FixtureExecutionRequest,
    ) -> Result<FixtureRunEvidence, String> {
        let run = self.store.get_run(id).await?.ok_or("fixture run missing")?;
        let mut steps = Vec::new();
        let mut effects = 0;
        for definition in &request.definition.steps {
            let evidence = self
                .store
                .get_step(id, &definition.step_id)
                .await?
                .ok_or("fixture checkpoint missing")?;
            if let Some(op) = &evidence.operation_id {
                if self
                    .broker
                    .inspect_operation(op)
                    .await?
                    .is_some_and(|o| o.state != crate::approval::OperationState::Prepared)
                {
                    effects += 1;
                }
            }
            steps.push(evidence);
        }
        let result_hash = steps
            .iter()
            .rev()
            .find(|s| s.status == StepStatus::Succeeded)
            .and_then(|s| s.output.as_ref())
            .map(payload_hash);
        Ok(FixtureRunEvidence {
            fixture_id: request.fixture_id.clone(),
            draft_id: request.draft_id.clone(),
            revision: request.revision,
            run_id: id.into(),
            workflow_hash: run.workflow_hash,
            skill_hash: run.skill_hash,
            input_hash: run.input_hash,
            assertion_hash: request.assertion_hash.clone(),
            policy_revision: run.policy_revision,
            kind: request.kind,
            status: run.status,
            steps,
            result_hash,
            expires_at: (Utc::now() + chrono::Duration::hours(24)).to_rfc3339(),
            effect_call_count: effects,
            isolation: FixtureIsolationEvidence {
                home_hash: payload_hash(&json!(self.home.to_string_lossy())),
                environment_hash: run.environment_hash,
                // Measured, not asserted: the staging declaration still holds.
                isolated_staging: self.staging_scope(&request.definition).is_ok(),
            },
            execution_creator_grant: run.creator_grant,
        })
    }
    pub async fn enqueue_run(
        &self,
        run: &WorkflowRun,
        revision: &ApprovedWorkflowRevision,
        candidate: bool,
    ) -> Result<String, String> {
        let table = if candidate {
            "workflow_candidate_revisions"
        } else {
            "workflow_revisions"
        };
        let limits = super::cost_ledger::load_pricing(&self.home);
        self.store
            .with_transaction(|tx| {
                // E-H3: formal runs per workflow per month is a hard count.
                // A repeated trigger reconnects to its run and is not counted.
                if run.activation_id.is_some() {
                    let known: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE trigger_key=?1)",
                            params![run.trigger_key],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    if !known {
                        if limits.limits_error.is_some() {
                            return Err(super::cost_ledger::LIMIT_CONFIG.into());
                        }
                        if super::cost_ledger::formal_runs_this_month(tx, &run.workflow_id)?
                            >= limits.limits.max_runs_per_month
                        {
                            return Err(super::cost_ledger::LIMIT_RUNS.into());
                        }
                    }
                }
                let old: Option<String> = tx
                    .query_row(
                        &format!("SELECT record_json FROM {table} WHERE workflow_id=?1 AND revision=?2"),
                        params![run.workflow_id, run.revision],
                        |r| r.get(0)
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                let encoded = serde_json::to_string(revision).map_err(|e| e.to_string())?;
                if let Some(old) = old {
                    let old: ApprovedWorkflowRevision =
                        serde_json::from_str(&old).map_err(|_| "invalid revision")?;
                    if old.definition != revision.definition
                        || old.creator_grant != revision.creator_grant
                        || old.audience != revision.audience
                    {
                        return Err("workflow immutable candidate drift".into());
                    }
                } else {
                    tx.execute(
                        &format!("INSERT INTO {table} VALUES(?1,?2,?3,?4)"),
                        params![run.workflow_id, run.revision, run.workflow_hash, encoded]
                    )
                    .map_err(|e| e.to_string())?;
                }
                tx.execute(
                    "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(trigger_key) DO NOTHING",
                    params![
                        run.run_id,
                        run.trigger_key,
                        run.workflow_id,
                        run.revision,
                        run.workflow_hash,
                        serde_json::to_string(run).map_err(|e| e.to_string())?,
                        serde_json::to_value(run.status)
                            .map_err(|e| e.to_string())?
                            .as_str()
                            .unwrap(),
                        run.created_at
                    ]
                )
                .map_err(|e| e.to_string())?;
                let (id, raw): (String, String) = tx
                    .query_row(
                        "SELECT run_id,record_json FROM workflow_runs WHERE trigger_key=?1",
                        params![run.trigger_key],
                        |r| Ok((r.get(0)?, r.get(1)?))
                    )
                    .map_err(|e| e.to_string())?;
                let existing: WorkflowRun =
                    serde_json::from_str(&raw).map_err(|_| "invalid existing workflow run")?;
                if existing.workflow_hash != run.workflow_hash
                    || existing.input_hash != run.input_hash
                    || existing.actor != run.actor
                    || existing.grant != run.grant
                {
                    return Err("duplicate workflow trigger changed contract".into());
                }
                for (position, step) in revision.definition.steps.iter().enumerate() {
                    let evidence = StepEvidence {
                        step_id: step.step_id.clone(),
                        status: StepStatus::Pending,
                        input_hash: String::new(),
                        output_hash: None,
                        output: None,
                        evidence_kind: ExecutionEvidenceKind::None,
                        receipt: None,
                        operation_id: None,
                        approval_id: None,
                        cost: CostBreakdown::default(),
                        error_code: None,
                        observed_at: run.created_at.clone(),
                        operator_resolution: None,
                    };
                    tx.execute(
                        "INSERT INTO workflow_steps VALUES(?1,?2,?3,'pending',?4)
                        ON CONFLICT(run_id,step_id) DO NOTHING",
                        params![
                            id,
                            step.step_id,
                            position as i64,
                            serde_json::to_string(&evidence).map_err(|e| e.to_string())?
                        ]
                    )
                    .map_err(|e| e.to_string())?;
                }
                // An activated run commits with its queue handoff row, so a
                // crash can never leave a run that nothing will deliver.
                if !candidate && run.grant.is_some() {
                    tx.execute(
                        "INSERT INTO workflow_outbox VALUES(?1,'enqueue',?2,?3,0)
                            ON CONFLICT(outbox_id) DO NOTHING",
                        params![
                            super::run_control::enqueue_outbox_id(&id),
                            id,
                            json!({"run_id":id,"actor":run.actor}).to_string()
                        ]
                    )
                    .map_err(|e| e.to_string())?;
                }
                Ok(id)
            })
            .await
    }
}
