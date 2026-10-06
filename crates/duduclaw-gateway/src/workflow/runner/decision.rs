//! Human decision steps and the decision check a waiting step runs first.
use super::super::*;
use super::{StepFlow, WorkflowRunner};
use crate::approval::{ApprovalId, ApprovalStatus, RequestKind};
use serde_json::Value;

/// Error code a run ends with when its pending human decision did not
/// grant the step.
pub(crate) fn decision_error_code(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Denied => "workflow_approval_denied",
        ApprovalStatus::Expired => "workflow_approval_expired",
        _ => "workflow_approval_invalidated",
    }
}

fn decision_kind(status: ApprovalStatus) -> ExecutionEvidenceKind {
    if status == ApprovalStatus::Denied {
        ExecutionEvidenceKind::HumanDecision
    } else {
        ExecutionEvidenceKind::GateDenial
    }
}

/// Deterministic per-step key: one decidable card per (run, step).
pub(super) fn step_approval_key(run: &WorkflowRun, step: &StepDefinition) -> String {
    format!("workflow:{}:step:{}", run.run_id, step.step_id)
}

/// What a fixture run scripted for one decision step.
pub(super) enum Scripted {
    /// A formal run: a real person decides.
    Formal,
    /// A fixture run with no decision for this step.
    Missing,
    Decision(crate::workflow_drafts::FixtureDecision),
}

pub(super) const FIXTURE_DECISION_MISSING: &str = "fixture_decision_missing";

impl WorkflowRunner {
    /// The fixture's scripted decision for `step`, read from the draft the
    /// fixture belongs to. Formal runs never consult it.
    pub(super) async fn scripted_decision(
        &self,
        run: &WorkflowRun,
        step: &StepDefinition,
    ) -> Result<Scripted, String> {
        let Trigger::Fixture { fixture_id, .. } = &run.trigger else {
            return Ok(Scripted::Formal);
        };
        if run.activation_id.is_some() {
            return Ok(Scripted::Formal);
        }
        let draft = self
            .store
            .draft_for_workflow(&run.workflow_id, run.revision)
            .await?
            .ok_or("workflow source draft missing")?;
        Ok(draft
            .fixtures
            .iter()
            .find(|f| &f.fixture_id == fixture_id)
            .and_then(|f| f.decisions.get(&step.step_id))
            .cloned()
            .map_or(Scripted::Missing, Scripted::Decision))
    }

    /// The card for a decision step: a real, pushed card for a formal run;
    /// an already-decided, never-pushed one for a fixture run. `Ok(None)`:
    /// the fixture has no decision for this step (the caller fails it).
    pub(super) async fn decision_card(
        &self,
        run: &WorkflowRun,
        step: &StepDefinition,
        kind: RequestKind,
        summary: &str,
        payload: Value,
        binding: crate::approval::ExecutionBinding,
    ) -> Result<Option<ApprovalId>, String> {
        let key = step_approval_key(run, step);
        match Box::pin(self.scripted_decision(run, step)).await? {
            Scripted::Formal => {
                let id = Box::pin(
                    self.broker
                        .request_bound_keyed(&key, kind, &run.actor, summary, payload, binding),
                )
                .await?;
                // F5-A: tell the people who may decide it, without content.
                if let Ok(Some(draft)) = self
                    .store
                    .draft_for_workflow(&run.workflow_id, run.revision)
                    .await
                {
                    Box::pin(super::super::workflow_notify::notify_step_card(
                        &self.home,
                        run,
                        &draft.source_task,
                        kind == RequestKind::Question,
                    ))
                    .await;
                }
                Ok(Some(id))
            }
            Scripted::Missing => Ok(None),
            Scripted::Decision(decision) => Box::pin(self.broker.record_fixture_decision(
                &key, kind, &run.actor, summary, payload, binding, &decision,
            ))
            .await
            .map(Some),
        }
    }

    /// A step parked on a decision looks at the decision before doing any
    /// work. Pending keeps the run waiting; a refusal, expiry or
    /// invalidation ends it. Returns true when the run must stop here.
    pub(super) async fn decision_precheck(
        &self,
        run: &mut WorkflowRun,
        evidence: &mut StepEvidence,
    ) -> Result<bool, String> {
        let Some(id) = evidence.approval_id.clone() else {
            return Ok(false);
        };
        let status = self.broker.poll(&ApprovalId::from(id)).await?;
        match status {
            ApprovalStatus::Pending => {
                run.status = if evidence.status == StepStatus::NeedsInput {
                    RunStatus::NeedsInput
                } else {
                    RunStatus::WaitingApproval
                };
                self.save_run(run).await?;
                Ok(true)
            }
            ApprovalStatus::Approved | ApprovalStatus::Answered => Ok(false),
            other => {
                self.fail_step(
                    run,
                    evidence,
                    decision_error_code(other),
                    decision_kind(other),
                )
                .await?;
                Ok(true)
            }
        }
    }

    pub(super) async fn run_decision(
        &self,
        run: &mut WorkflowRun,
        step: &StepDefinition,
        evidence: &mut StepEvidence,
        input: Value,
        summary: &str,
        question: bool,
    ) -> Result<StepFlow, String> {
        let kind = if question {
            RequestKind::Question
        } else {
            RequestKind::Approval
        };
        let id = match &evidence.approval_id {
            Some(id) => ApprovalId::from(id.clone()),
            None => {
                let binding = self.binding(run, &input, &run.deadline_at)?;
                let Some(id) = Box::pin(self.decision_card(
                    run,
                    step,
                    kind,
                    summary,
                    input.clone(),
                    binding,
                ))
                .await?
                else {
                    self.fail_step(
                        run,
                        evidence,
                        FIXTURE_DECISION_MISSING,
                        ExecutionEvidenceKind::GateDenial,
                    )
                    .await?;
                    return Ok(StepFlow::Stop);
                };
                evidence.approval_id = Some(id.to_string());
                // Persist the card id before anything else can fail.
                self.save_step(&run.run_id, evidence).await?;
                id
            }
        };
        let status = self.broker.poll(&id).await?;
        if status == ApprovalStatus::Pending {
            evidence.status = if question {
                StepStatus::NeedsInput
            } else {
                StepStatus::WaitingApproval
            };
            self.save_step(&run.run_id, evidence).await?;
            run.status = if question {
                RunStatus::NeedsInput
            } else {
                RunStatus::WaitingApproval
            };
            self.save_run(run).await?;
            return Ok(StepFlow::Stop);
        }
        if question && status == ApprovalStatus::Answered {
            let record = self
                .broker
                .get(&id)
                .await?
                .ok_or("workflow decision missing")?;
            let answer = record.answer.ok_or("question answer missing")?;
            return Ok(StepFlow::Done(Ok((
                answer,
                None,
                ExecutionEvidenceKind::QuestionAnswer,
            ))));
        }
        if !question && status == ApprovalStatus::Approved {
            return Ok(StepFlow::Done(Ok((
                input,
                None,
                ExecutionEvidenceKind::HumanDecision,
            ))));
        }
        self.fail_step(
            run,
            evidence,
            decision_error_code(status),
            decision_kind(status),
        )
        .await?;
        Ok(StepFlow::Stop)
    }
}
