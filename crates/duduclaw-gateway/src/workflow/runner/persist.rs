//! Checkpoint writes, bindings and pure step transforms for the runner.
use super::super::*;
use super::WorkflowRunner;
use crate::approval::{ExecutionBinding, payload_hash};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::BTreeMap;

impl WorkflowRunner {
    pub(super) fn binding(
        &self,
        run: &WorkflowRun,
        payload: &Value,
        expiry: &str,
    ) -> Result<ExecutionBinding, String> {
        let context = run
            .decision_context
            .clone()
            .ok_or("workflow trusted decision context unavailable")?;
        Ok(ExecutionBinding {
            schema_version: 1,
            run_id: run.run_id.clone(),
            run_origin_kind: "workflow".into(),
            actor_principal: run.actor.clone(),
            decision_context: context,
            task_id: run.task.as_ref().map(|t| t.task_id.clone()),
            task_revision: run.task.as_ref().map(|t| t.revision),
            task_snapshot_hash: run.task.as_ref().map(|t| t.snapshot_hash.clone()),
            payload_hash: payload_hash(payload),
            policy_revision: run.policy_revision.clone(),
            cwd: Some(self.executor.environment(&run.actor)?.cwd),
            environment_hash: run.environment_hash.clone(),
            file_hashes: BTreeMap::new(),
            expires_at: expiry.into(),
            resume_handler: "workflow_v1".into(),
            resume_version: 1,
        })
    }

    /// Persist the run projection. A cancelled run is final: a runner that
    /// raced the cancellation can never write it back to another state.
    pub async fn save_run(&self, run: &WorkflowRun) -> Result<(), String> {
        self.store
            .with_transaction(|tx| {
                // The cost projection always comes from the ledger, so a
                // stale in-memory copy can never lower it.
                let mut run = run.clone();
                run.cost = super::super::cost_ledger::run_cost_in(tx, "", &run.run_id)?;
                let n = tx
                    .execute(
                        "UPDATE workflow_runs SET record_json=?1,status=?2 WHERE run_id=?3
                            AND status<>'cancelled'",
                        params![
                            serde_json::to_string(&run).map_err(|e| e.to_string())?,
                            serde_json::to_value(run.status)
                                .map_err(|e| e.to_string())?
                                .as_str()
                                .unwrap(),
                            run.run_id
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    let cancelled: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE run_id=?1
                                AND status='cancelled')",
                            params![run.run_id],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    return Err(if cancelled {
                        super::super::run_control::RUN_CANCELLED.into()
                    } else {
                        "workflow run write missing".into()
                    });
                }
                Ok(())
            })
            .await
    }

    /// The `running` checkpoint plus this attempt's charge and count-limit
    /// check, in one IMMEDIATE transaction; the run's cost is rebuilt from
    /// the ledger and mirrored into `run`.
    pub(super) async fn start_step_charged(
        &self,
        run: &mut WorkflowRun,
        step: &StepDefinition,
        evidence: &StepEvidence,
    ) -> Result<(), String> {
        let pricing = super::super::cost_ledger::load_pricing(&self.home);
        let kind = super::super::cost_ledger::ChargeKind::of(&step.action);
        let encoded = serde_json::to_string(evidence).map_err(|e| e.to_string())?;
        let status = serde_json::to_value(evidence.status).map_err(|e| e.to_string())?;
        let snapshot = run.clone();
        let cost = self
            .store
            .with_transaction(|tx| {
                let n = tx
                    .execute(
                        "UPDATE workflow_steps SET record_json=?1,status=?2 WHERE run_id=?3 AND step_id=?4
                            AND status<>'succeeded'",
                        params![encoded, status.as_str().unwrap(), snapshot.run_id, evidence.step_id],
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    return Err("workflow step checkpoint immutable or missing".into());
                }
                super::super::cost_ledger::charge_in_tx(tx, &snapshot, &evidence.step_id, kind, &pricing)
            })
            .await?;
        run.cost = cost;
        Ok(())
    }

    pub async fn save_step(&self, run_id: &str, evidence: &StepEvidence) -> Result<(), String> {
        self.store
            .with_transaction(|tx| {
                let n = tx
                    .execute(
                        "UPDATE workflow_steps SET record_json=?1,status=?2 WHERE run_id=?3 AND step_id=?4
                            AND status<>'succeeded'",
                        params![
                            serde_json::to_string(evidence).map_err(|e| e.to_string())?,
                            serde_json::to_value(evidence.status)
                                .map_err(|e| e.to_string())?
                                .as_str()
                                .unwrap(),
                            run_id,
                            evidence.step_id
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    return Err("workflow step checkpoint immutable or missing".into());
                }
                Ok(())
            })
            .await
    }

    /// A definite step failure ends the run as `Failed` and counts toward
    /// the activation's consecutive-failure limit.
    pub(super) async fn fail_step(
        &self,
        run: &mut WorkflowRun,
        evidence: &mut StepEvidence,
        code: &str,
        kind: ExecutionEvidenceKind,
    ) -> Result<(), String> {
        evidence.status = StepStatus::Failed;
        evidence.evidence_kind = kind;
        evidence.error_code = Some(code.into());
        self.save_step(&run.run_id, evidence).await?;
        run.status = RunStatus::Failed;
        run.error_code = Some(code.into());
        run.failure_class = Some(super::super::run_control::FAILURE_DEFINITE.into());
        self.save_run(run).await
    }

    /// Ends the run as `Uncertain` without retrying: the effect may or may
    /// not have happened and only an operator can say which.
    pub(super) async fn uncertain_step(
        &self,
        run: &mut WorkflowRun,
        evidence: &mut StepEvidence,
        code: &str,
    ) -> Result<(), String> {
        evidence.status = StepStatus::Uncertain;
        evidence.error_code = Some(code.into());
        self.save_step(&run.run_id, evidence).await?;
        run.status = RunStatus::Uncertain;
        run.error_code = Some(code.into());
        run.failure_class = Some(super::super::run_control::FAILURE_UNCERTAIN.into());
        self.save_run(run).await
    }

    /// Extend this execution's lease; losing it is an infrastructure error,
    /// never a verdict on the run.
    pub(super) async fn renew_lease(&self, run_id: &str, token: &str) -> Result<(), String> {
        let renewed = super::renew_lease(&self.store, run_id, token).await?;
        if renewed {
            Ok(())
        } else {
            Err(super::WORKFLOW_LEASE_LOST.into())
        }
    }
}

pub(super) fn resolve_input(
    reference: &InputRef,
    input: &Value,
    outputs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    match reference {
        InputRef::Array { items } => items
            .iter()
            .map(|item| resolve_input(item, input, outputs))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        InputRef::Literal { value } => Ok(value.clone()),
        InputRef::RunInput { pointer } => input
            .pointer(pointer)
            .cloned()
            .ok_or("workflow run input reference missing".into()),
        InputRef::StepOutput { step_id, pointer } => outputs
            .get(step_id)
            .and_then(|v| v.pointer(pointer))
            .cloned()
            .ok_or("workflow step output reference missing".into()),
    }
}

/// Step ids whose outputs this reference consumes.
pub(super) fn referenced_steps<'a>(reference: &'a InputRef, out: &mut Vec<&'a str>) {
    match reference {
        InputRef::Array { items } => items.iter().for_each(|i| referenced_steps(i, out)),
        InputRef::StepOutput { step_id, .. } => out.push(step_id),
        InputRef::Literal { .. } | InputRef::RunInput { .. } => (),
    }
}

pub(super) fn process(transform: &ProcessTransform, input: Value) -> Result<Value, String> {
    match transform {
        ProcessTransform::Identity => Ok(input),
        ProcessTransform::Collect => {
            if input.is_array() {
                Ok(input)
            } else {
                Err("collect expects typed array".into())
            }
        }
        ProcessTransform::Summary => {
            let array = input.as_array().ok_or("summary expects typed array")?;
            Ok(json!({"items":array,"count":array.len()}))
        }
    }
}
