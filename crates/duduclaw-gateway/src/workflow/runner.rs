//! One durable sequence runner for fixtures, manual requests and scheduled runs.
mod decision;
mod effect;
mod guard;
mod persist;

use super::executor::{WorkflowExecutor, WorkflowSession};
use super::*;
use crate::approval::{ApprovalBroker, payload_hash};
use chrono::{DateTime, Utc};
use guard::GuardPoint;
use persist::{process, referenced_steps, resolve_input};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// Another execution of the same run holds the lease.
pub const WORKFLOW_BUSY: &str = "workflow run already executing";
/// This execution's lease could not be extended.
pub const WORKFLOW_LEASE_LOST: &str = "workflow run lease lost";
/// This dispatch used up its own execution time; the next one continues.
pub const WORKFLOW_SEGMENT_EXPIRED: &str = "workflow_execution_segment_expired";
/// The ledger did not start the effect and no gate refused it.
pub const WORKFLOW_EFFECT_NOT_STARTED: &str = "workflow_effect_not_started";

/// Run lease length. A background heartbeat extends it while the run
/// executes, so the length only bounds how long a dead holder blocks.
pub const RUN_LEASE_SECONDS: i64 = 300;
const LEASE_HEARTBEAT_SECONDS: u64 = 60;
/// Time one dispatch may spend executing steps. Waiting for a human is not
/// part of it; the run's `deadline_at` bounds that separately.
pub const EXECUTION_SEGMENT_SECONDS: i64 = 15 * 60;

type StepResult = Result<(Value, Option<Value>, ExecutionEvidenceKind), String>;

/// Outcome of one step handler.
pub(super) enum StepFlow {
    /// The step finished; `Err` is a definite step failure.
    Done(StepResult),
    /// The run state was already saved (waiting, failed or uncertain).
    Stop,
}

pub struct WorkflowRunner {
    pub store: Arc<WorkflowStore>,
    pub broker: Arc<ApprovalBroker>,
    pub executor: WorkflowExecutor,
    pub home: PathBuf,
}

async fn renew_lease(store: &WorkflowStore, run_id: &str, token: &str) -> Result<bool, String> {
    store
        .with_transaction(|tx| {
            let now = Utc::now().timestamp();
            let n = tx
                .execute(
                    "UPDATE workflow_run_leases SET lease_until=?1 WHERE run_id=?2 AND token=?3
                        AND lease_until>?4",
                    params![now + RUN_LEASE_SECONDS, run_id, token, now],
                )
                .map_err(|e| e.to_string())?;
            Ok(n == 1)
        })
        .await
}

/// Aborts the lease heartbeat when the execution ends or is dropped.
struct Heartbeat(tokio::task::JoinHandle<()>);
impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl WorkflowRunner {
    pub async fn execute(&self, id: &str) -> Result<WorkflowRun, String> {
        let mut run = self
            .store
            .get_run(id)
            .await?
            .ok_or("workflow run missing")?;
        if super::run_control::run_is_terminal(run.status) {
            return Ok(run);
        }
        let revision = self
            .store
            .get_revision(&run.workflow_id, run.revision)
            .await?
            .ok_or("workflow revision missing")?;
        if revision.revision_hash != run.workflow_hash
            || revision.definition.hash() != run.workflow_hash
            || revision.definition.skill_revision_hash != run.skill_hash
        {
            return Err("workflow revision drift".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.store
            .with_transaction(|tx| {
                let now = Utc::now().timestamp();
                let n = tx
                    .execute(
                        "INSERT INTO workflow_run_leases(run_id,token,lease_until) VALUES(?1,?2,?3)
                            ON CONFLICT(run_id) DO UPDATE SET token=excluded.token,lease_until=excluded.lease_until
                            WHERE workflow_run_leases.lease_until<=?4",
                        params![id, token, now + RUN_LEASE_SECONDS, now],
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    return Err(WORKFLOW_BUSY.into());
                }
                Ok(())
            })
            .await?;
        let heartbeat = {
            let store = self.store.clone();
            let (run_id, token) = (id.to_string(), token.clone());
            Heartbeat(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(LEASE_HEARTBEAT_SECONDS))
                        .await;
                    match renew_lease(&store, &run_id, &token).await {
                        Ok(true) => (),
                        Ok(false) => {
                            tracing::warn!(run_id = %run_id, "workflow run lease lost");
                            return;
                        }
                        Err(e) => tracing::warn!(run_id = %run_id, error = %e, "lease renewal failed"),
                    }
                }
            }))
        };
        let segment = Utc::now() + chrono::Duration::seconds(EXECUTION_SEGMENT_SECONDS);
        // Boxed: the step state machine is large; keep it off the caller's stack.
        let result = Box::pin(self.execute_claimed(&mut run, &revision, &token, segment)).await;
        drop(heartbeat);
        let release = self
            .store
            .with_transaction(|tx| {
                tx.execute(
                    "DELETE FROM workflow_run_leases WHERE run_id=?1 AND token=?2",
                    params![id, token],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await;
        result?;
        release?;
        Ok(run)
    }

    async fn execute_claimed(
        &self,
        run: &mut WorkflowRun,
        revision: &ApprovedWorkflowRevision,
        token: &str,
        segment: DateTime<Utc>,
    ) -> Result<(), String> {
        let mut session: Option<WorkflowSession> = None;
        let mut outputs = BTreeMap::<String, Value>::new();
        let mut read_times = BTreeMap::<String, String>::new();
        run.status = RunStatus::Running;
        self.save_run(run).await?;
        for (position, step) in revision.definition.steps.iter().enumerate() {
            let mut evidence = self
                .store
                .get_step(&run.run_id, &step.step_id)
                .await?
                .ok_or("workflow checkpoint missing")?;
            if evidence.status == StepStatus::Succeeded {
                if matches!(step.action, StepAction::McpRead { .. }) {
                    read_times.insert(step.step_id.clone(), evidence.observed_at.clone());
                }
                if let Some(value) = evidence.output {
                    outputs.insert(step.step_id.clone(), value);
                }
                continue;
            }
            // B1 / R-L2: a card's lifetime is the run's waiting window, so a
            // card that lapsed with the run must end the run as the card's
            // expiry (`workflow_approval_expired`), not as a gate failure.
            if matches!(
                evidence.status,
                StepStatus::WaitingApproval | StepStatus::NeedsInput
            ) && chrono::DateTime::parse_from_rfc3339(&run.deadline_at)
                .is_ok_and(|d| d <= Utc::now())
                && self.decision_precheck(run, &mut evidence).await?
            {
                return Ok(());
            }
            let effect = matches!(step.action, StepAction::McpEffect { .. });
            let mut consumed = Vec::new();
            referenced_steps(&step.input, &mut consumed);
            Box::pin(self.guard_at(
                run,
                &GuardPoint {
                    // The run input's age matters when the run starts and
                    // right before an effect (the ledger checks it again).
                    // R-M5: before an effect only if it reads the run input.
                    input_freshness: (effect
                        && super::effect_targets::step_uses_run_input(
                            &revision.definition,
                            &step.step_id,
                        ))
                        || (position == 0 && evidence.status == StepStatus::Pending),
                    read_observations: if effect {
                        consumed
                            .iter()
                            .filter_map(|id| read_times.get(*id).map(String::as_str))
                            .collect()
                    } else {
                        Vec::new()
                    },
                    segment_deadline: Some(segment),
                    live_status: true,
                },
            ))
            .await?;
            if matches!(evidence.status, StepStatus::Failed | StepStatus::Uncertain) {
                run.status = if evidence.status == StepStatus::Uncertain {
                    RunStatus::Uncertain
                } else {
                    RunStatus::Failed
                };
                run.error_code = evidence.error_code;
                self.save_run(run).await?;
                return Ok(());
            }
            if matches!(
                evidence.status,
                StepStatus::WaitingApproval | StepStatus::NeedsInput
            ) && self.decision_precheck(run, &mut evidence).await?
            {
                return Ok(());
            }
            if evidence.status == StepStatus::Running && effect {
                match Box::pin(self.recover_running_effect(run, step, &mut evidence)).await?
                {
                    effect::Recovery::Rerun => (),
                    effect::Recovery::Continue(output) => {
                        outputs.insert(step.step_id.clone(), output);
                        continue;
                    }
                    effect::Recovery::Stop => return Ok(()),
                }
            }
            let input = resolve_input(&step.input, &run.input, &outputs)?;
            step.input_schema.validate(&input)?;
            evidence.input_hash = payload_hash(&input);
            // Durable checkpoint precedes dispatch. Database failure cannot fall through.
            // The dispatch is charged in the same transaction (E-H3).
            evidence.status = StepStatus::Running;
            Box::pin(self.start_step_charged(run, step, &evidence)).await?;
            self.renew_lease(&run.run_id, token).await?;
            let flow = match &step.action {
                StepAction::Process { transform } => StepFlow::Done(
                    process(transform, input).map(|v| (v, None, ExecutionEvidenceKind::Process)),
                ),
                StepAction::Artifact { label } => {
                    self.commit_artifact(run, step, &mut evidence, &input, label)
                        .await?;
                    outputs.insert(step.step_id.clone(), input);
                    continue;
                }
                StepAction::McpRead { .. } => {
                    Box::pin(self.run_read(run, revision, step, &evidence, input, &mut session))
                        .await?
                }
                StepAction::McpEffect { .. } => {
                    Box::pin(self.run_effect(
                        run,
                        revision,
                        step,
                        &mut evidence,
                        input,
                        &mut session,
                        token,
                    ))
                    .await?
                }
                StepAction::Approval { summary } => {
                    Box::pin(self.run_decision(run, step, &mut evidence, input, summary, false))
                        .await?
                }
                StepAction::Question { summary } => {
                    Box::pin(self.run_decision(run, step, &mut evidence, input, summary, true))
                        .await?
                }
            };
            let result = match flow {
                StepFlow::Stop => return Ok(()),
                StepFlow::Done(result) => result,
            };
            match result {
                Ok((output, receipt, kind)) => {
                    if kind == ExecutionEvidenceKind::OperationReceipt
                        && step.output_schema.validate(&output).is_err()
                    {
                        // The side effect happened; say so instead of
                        // reporting the outcome as unknown.
                        evidence.receipt = receipt;
                        self.fail_step(
                            run,
                            &mut evidence,
                            "workflow_effect_output_invalid_after_side_effect",
                            ExecutionEvidenceKind::OperationReceipt,
                        )
                        .await?;
                        return Ok(());
                    }
                    step.output_schema.validate(&output)?;
                    evidence.status = StepStatus::Succeeded;
                    evidence.output_hash = Some(payload_hash(&output));
                    evidence.output = Some(output.clone());
                    evidence.receipt = receipt;
                    evidence.evidence_kind = kind;
                    evidence.observed_at = Utc::now().to_rfc3339();
                    self.save_step(&run.run_id, &evidence).await?;
                    if matches!(step.action, StepAction::McpRead { .. }) {
                        read_times.insert(step.step_id.clone(), evidence.observed_at.clone());
                    }
                    outputs.insert(step.step_id.clone(), output);
                }
                Err(error) => {
                    self.fail_step(run, &mut evidence, &error, ExecutionEvidenceKind::GateDenial)
                        .await?;
                    return Ok(());
                }
            }
        }
        let output = outputs
            .get(
                &revision
                    .definition
                    .steps
                    .last()
                    .ok_or("workflow has no steps")?
                    .step_id,
            )
            .ok_or("workflow final output missing")?;
        revision.definition.output_schema.validate(output)?;
        run.status = RunStatus::Succeeded;
        run.error_code = None;
        run.failure_class = None;
        self.save_run(run).await
    }

    async fn run_read(
        &self,
        run: &WorkflowRun,
        revision: &ApprovedWorkflowRevision,
        step: &StepDefinition,
        evidence: &StepEvidence,
        input: Value,
        session: &mut Option<WorkflowSession>,
    ) -> Result<StepFlow, String> {
        let timeout = std::time::Duration::from_secs(step.timeout_seconds as u64);
        let mut outcome = Err("workflow_read_not_attempted".to_string());
        for attempt in 0..step.max_read_attempts {
            // Retries re-check authority; input age is a run-start and
            // pre-effect gate only.
            Box::pin(self.guard_at(run, &GuardPoint::default())).await?;
            // Every read attempt is a dispatch and is charged; the first was
            // charged with the step's running checkpoint.
            if attempt > 0 {
                self.store
                    .charge_step(
                        &self.home,
                        &run.run_id,
                        &step.step_id,
                        super::cost_ledger::ChargeKind::Read,
                    )
                    .await?;
            }
            if session.is_none() {
                *session = Some(
                    tokio::time::timeout(timeout, self.executor.connect(&run.actor))
                        .await
                        .map_err(|_| "workflow_read_transport_unavailable")??,
                );
            }
            outcome = match tokio::time::timeout(
                timeout,
                session
                    .as_mut()
                    .unwrap()
                    .read(run, revision, step, evidence, input.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err("workflow_read_transport_unavailable".into()),
            };
            if !matches!(&outcome, Err(error) if error == "workflow_read_transport_unavailable")
                || attempt + 1 >= step.max_read_attempts
            {
                break;
            }
            // Only reads may retry. Dropping the failed session kills its child.
            *session = None;
        }
        Ok(StepFlow::Done(outcome.map(|(output, receipt)| {
            (output, Some(receipt), ExecutionEvidenceKind::McpRead)
        })))
    }

    async fn commit_artifact(
        &self,
        run: &WorkflowRun,
        step: &StepDefinition,
        evidence: &mut StepEvidence,
        input: &Value,
        label: &str,
    ) -> Result<(), String> {
        step.output_schema.validate(input)?;
        let receipt =
            super::artifact::publish(&self.home, &self.store, run, step, input, label).await?;
        let record_id = receipt["record_id"]
            .as_str()
            .ok_or("artifact receipt lacks record id")?
            .to_string();
        evidence.status = StepStatus::Succeeded;
        evidence.output_hash = Some(payload_hash(input));
        evidence.output = Some(input.clone());
        evidence.receipt = Some(receipt.clone());
        evidence.evidence_kind = ExecutionEvidenceKind::ArtifactCommit;
        evidence.observed_at = Utc::now().to_rfc3339();
        let evidence = &*evidence;
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO workflow_artifact_commits(record_id,run_id,step_id,content_hash,
                        content_json,audience_json,receipt_json) VALUES(?1,?2,?3,?4,?5,?6,?7)
                        ON CONFLICT(record_id) DO NOTHING",
                    params![
                        record_id,
                        run.run_id,
                        step.step_id,
                        payload_hash(input),
                        input.to_string(),
                        json!(run.audience).to_string(),
                        receipt.to_string()
                    ],
                )
                .map_err(|e| e.to_string())?;
                let (hash, body, audience, stored_receipt): (String, String, String, String) = tx
                    .query_row(
                        "SELECT content_hash,content_json,audience_json,receipt_json
                            FROM workflow_artifact_commits WHERE record_id=?1",
                        params![record_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .map_err(|e| e.to_string())?;
                if hash != payload_hash(input)
                    || body != input.to_string()
                    || audience != json!(run.audience).to_string()
                    || stored_receipt != receipt.to_string()
                {
                    return Err("workflow artifact immutable content changed".into());
                }
                let n = tx
                    .execute(
                        "UPDATE workflow_steps SET record_json=?1,status='succeeded' WHERE run_id=?2
                            AND step_id=?3 AND status='running'",
                        params![
                            serde_json::to_string(evidence).map_err(|e| e.to_string())?,
                            run.run_id,
                            step.step_id
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    return Err("workflow artifact checkpoint conflict".into());
                }
                Ok(())
            })
            .await
    }
}
