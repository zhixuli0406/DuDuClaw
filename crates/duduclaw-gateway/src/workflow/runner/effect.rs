//! Effect steps: prepare through the verified stdio route, take authority
//! from the grant or a keyed human approval, execute once through the
//! ledger, and recover a step a crash left `Running`.
use super::super::executor::WorkflowSession;
use super::super::*;
use super::{StepFlow, WorkflowRunner};
use crate::approval::{ApprovalId, ApprovalStatus, OperationState, RequestKind, payload_hash};
use chrono::Utc;
use serde_json::{Value, json};
use std::time::Duration;

/// What a `Running` effect step found in the operation ledger.
pub(super) enum Recovery {
    /// No operation ever began: run the step normally.
    Rerun,
    /// The effect completed; its verified output feeds later steps.
    Continue(Value),
    /// The run already reached a final state here.
    Stop,
}

impl WorkflowRunner {
    pub(super) async fn recover_running_effect(
        &self,
        run: &mut WorkflowRun,
        step: &StepDefinition,
        evidence: &mut StepEvidence,
    ) -> Result<Recovery, String> {
        let operation = match &evidence.operation_id {
            Some(id) => self.broker.inspect_operation(id).await?,
            None => {
                self.broker
                    .operation_for_step(&run.run_id, &step.step_id)
                    .await?
            }
        };
        let Some(operation) = operation else {
            return Ok(Recovery::Rerun);
        };
        evidence.operation_id = Some(operation.operation_id.clone());
        match operation.state {
            // Prepared never began: the ledger refuses a second begin, and
            // re-preparing the same step returns this same operation.
            OperationState::Prepared => Ok(Recovery::Rerun),
            OperationState::Succeeded => {
                let receipt = operation
                    .receipt
                    .ok_or("successful operation lacks receipt")?;
                evidence.receipt = Some(receipt.clone());
                evidence.evidence_kind = ExecutionEvidenceKind::OperationReceipt;
                evidence.operator_resolution = operation
                    .operator_resolution
                    .map(|r| serde_json::to_value(r).unwrap_or(Value::Null));
                // Same output contract as the normal path, even though the
                // side effect already happened.
                let output = receipt
                    .get("evidence")
                    .cloned()
                    .filter(|o| step.output_schema.validate(o).is_ok());
                let Some(output) = output else {
                    self.fail_step(
                        run,
                        evidence,
                        "workflow_effect_output_invalid_after_side_effect",
                        ExecutionEvidenceKind::OperationReceipt,
                    )
                    .await?;
                    return Ok(Recovery::Stop);
                };
                evidence.output_hash = Some(payload_hash(&output));
                evidence.output = Some(output.clone());
                evidence.status = StepStatus::Succeeded;
                evidence.observed_at = Utc::now().to_rfc3339();
                self.save_step(&run.run_id, evidence).await?;
                Ok(Recovery::Continue(output))
            }
            OperationState::Failed => {
                let code = operation
                    .error_code
                    .unwrap_or_else(|| "workflow_effect_rejected".into());
                self.fail_step(run, evidence, &code, ExecutionEvidenceKind::GateDenial)
                    .await?;
                Ok(Recovery::Stop)
            }
            OperationState::Executing | OperationState::Uncertain => {
                self.uncertain_step(run, evidence, "effect_outcome_unconfirmed")
                    .await?;
                Ok(Recovery::Stop)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_effect(
        &self,
        run: &mut WorkflowRun,
        revision: &ApprovedWorkflowRevision,
        step: &StepDefinition,
        evidence: &mut StepEvidence,
        input: Value,
        session: &mut Option<WorkflowSession>,
        token: &str,
    ) -> Result<StepFlow, String> {
        let StepAction::McpEffect { tool, .. } = &step.action else {
            return Err("not an effect step".into());
        };
        let timeout = Duration::from_secs(step.timeout_seconds as u64);
        if session.is_none() {
            *session = Some(
                tokio::time::timeout(timeout, self.executor.connect(&run.actor))
                    .await
                    .map_err(|_| "workflow_step_timeout")??,
            );
        }
        let call = json!({"name":tool,"arguments":input});
        let prepared = tokio::time::timeout(
            timeout,
            session
                .as_mut()
                .unwrap()
                .prepare(run, evidence, call.clone()),
        )
        .await
        .map_err(|_| "workflow_step_timeout")??;
        let effective = json!({
            "name": prepared.effective.tool,
            "arguments": prepared.effective.effective_arguments
        });
        if prepared.effective.actor != run.actor
            || prepared.effective.run_id != run.run_id
            || prepared.effective.step_key != step.step_id
            || prepared.effective.payload_hash != payload_hash(&effective)
            || prepared.effective.policy_revision != run.policy_revision
            || prepared.effective.environment_hash != run.environment_hash
            || prepared.effective.authority_digest
                != super::super::executor::authority_digest(run, revision, step, evidence)
        {
            return Err("effective workflow authority drift".into());
        }
        let binding = self.binding(run, &effective, &prepared.effective.expires_at)?;
        let operation_id =
            if prepared.effective.approval_requirements.is_empty() && run.grant.is_some() {
                self.broker
                    .prepare_granted_operation(
                        run.grant.as_ref().unwrap(),
                        &step.step_id,
                        &effective,
                        &binding,
                        None,
                    )
                    .await?
            } else {
                let approval = match &evidence.approval_id {
                    Some(id) => ApprovalId::from(id.clone()),
                    None => {
                        let Some(id) = Box::pin(self.decision_card(
                                run,
                                step,
                                RequestKind::Approval,
                                "執行工作流操作",
                                effective.clone(),
                                binding.clone(),
                            ))
                            .await?
                        else {
                            self.fail_step(
                                run,
                                evidence,
                                super::decision::FIXTURE_DECISION_MISSING,
                                ExecutionEvidenceKind::GateDenial,
                            )
                            .await?;
                            return Ok(StepFlow::Stop);
                        };
                        evidence.approval_id = Some(id.to_string());
                        self.save_step(&run.run_id, evidence).await?;
                        id
                    }
                };
                let status = self.broker.poll(&approval).await?;
                if status == ApprovalStatus::Pending {
                    evidence.status = StepStatus::WaitingApproval;
                    self.save_step(&run.run_id, evidence).await?;
                    run.status = RunStatus::WaitingApproval;
                    self.save_run(run).await?;
                    return Ok(StepFlow::Stop);
                }
                if status != ApprovalStatus::Approved {
                    self.fail_step(
                        run,
                        evidence,
                        super::decision::decision_error_code(status),
                        ExecutionEvidenceKind::HumanDecision,
                    )
                    .await?;
                    return Ok(StepFlow::Stop);
                }
                self.broker
                    .prepare_operation(&approval, &step.step_id, None)
                    .await?
            };
        evidence.operation_id = Some(operation_id.clone());
        self.save_step(&run.run_id, evidence).await?;
        self.renew_lease(&run.run_id, token).await?;
        #[cfg(test)]
        super::super::security_race_tests::checkpoint("runner_before_effect_execute").await;
        let reply = match tokio::time::timeout(
            timeout,
            session.as_mut().unwrap().execute(
                &self.broker,
                &operation_id,
                &binding,
                &prepared,
                call,
            ),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                *session = None;
                return Err("workflow_effect_timeout_outcome_unconfirmed".into());
            }
        };
        let error = reply.error_code;
        match reply.state {
            OperationState::Succeeded => {
                let receipt = reply.receipt.ok_or("workflow actual receipt missing")?;
                Ok(StepFlow::Done(Ok((
                    receipt
                        .get("evidence")
                        .cloned()
                        .ok_or("workflow effect result missing")?,
                    Some(receipt),
                    ExecutionEvidenceKind::OperationReceipt,
                ))))
            }
            OperationState::Failed => Ok(StepFlow::Done(Err(
                error.unwrap_or("workflow_effect_rejected".into())
            ))),
            // Never began. A gate's answer is a definite refusal; a lost
            // reply is not, and retrying a never-begun operation is safe.
            OperationState::Prepared => match reply.refusal {
                Some(refusal) => Ok(StepFlow::Done(Err(refusal))),
                None => Err(super::WORKFLOW_EFFECT_NOT_STARTED.into()),
            },
            OperationState::Executing | OperationState::Uncertain => {
                let code = error.unwrap_or("workflow_effect_unknown".into());
                self.uncertain_step(run, evidence, &code).await?;
                Ok(StepFlow::Stop)
            }
        }
    }
}
