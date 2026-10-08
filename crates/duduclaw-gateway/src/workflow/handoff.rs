//! Queue handoff for workflow runs: outbox delivery (shared by enqueue, boot
//! and the resident sweep), queue-message dispatch, and the sweep that
//! wakes runs nothing else will wake.
//!
//! The queue is only a wake-up channel. `workflow.db` is authoritative, the
//! run lease is the only mutual exclusion, and the runner's step states make
//! a repeated wake-up harmless: completed steps never re-run, a waiting
//! step re-reads its decision, and an effect left `Running` is reconciled
//! with the operation ledger instead of being re-sent.
use super::run_control::{
    FAILURE_GATE, FAILURE_LIMIT, FAILURE_TRANSIENT, FAILURE_UNCERTAIN, MAX_TRANSIENT_RETRIES, RUN_CANCELLED,
    enqueue_outbox_id, is_transient_error, resume_outbox,
};
use super::*;
use crate::approval::{ApprovalId, ApprovalStatus};
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

/// What the dispatcher should do with a workflow queue message.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// The run reached a resting state (final, or waiting for a person).
    Done(WorkflowRun),
    /// Infrastructure was unavailable; dispatch the same message later.
    Retry(String),
}

/// Counts from one sweep pass, for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub outbox_delivered: usize,
    pub runs_blocked: usize,
    pub resumed: usize,
    pub requeued: usize,
    /// Activations whose validity ended in this pass (U8).
    pub activations_expired: usize,
    pub errors: Vec<String>,
}

struct OutboxRow {
    outbox_id: String,
    kind: String,
    run_id: String,
    payload: String,
}

impl WorkflowService {
    /// Deliver every undelivered outbox row, oldest first. One bad row is
    /// recorded and skipped; it never stops the others.
    pub async fn deliver_outbox(&self, report: &mut SweepReport) -> Result<(), String> {
        let rows: Vec<OutboxRow> = self
            .store
            .with_connection(|c| {
                let mut q = c
                    .prepare(
                        "SELECT outbox_id,kind,entity_id,payload_json FROM workflow_outbox
                        WHERE kind IN ('enqueue','resume') AND delivered=0 ORDER BY rowid",
                    )
                    .map_err(|e| e.to_string())?;
                q.query_map([], |r| {
                    Ok(OutboxRow {
                        outbox_id: r.get(0)?,
                        kind: r.get(1)?,
                        run_id: r.get(2)?,
                        payload: r.get(3)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())
            })
            .await?;
        for row in rows {
            match self.deliver_row(&row).await {
                Ok(blocked) => {
                    report.outbox_delivered += 1;
                    report.runs_blocked += usize::from(blocked);
                }
                Err(error) => report.errors.push(format!("{}: {error}", row.outbox_id)),
            }
        }
        Ok(())
    }

    /// Compatibility entry: deliver what can be delivered, log the rest.
    pub async fn reconcile_queue_outbox(&self) -> Result<(), String> {
        let mut report = SweepReport::default();
        self.deliver_outbox(&mut report).await?;
        for error in &report.errors {
            tracing::warn!(error = %error, "workflow outbox row not delivered");
        }
        Ok(())
    }

    /// Deliver one row. A pending run whose authority is already gone is
    /// blocked first, so its message only reports the block. Returns whether
    /// that happened.
    async fn deliver_row(&self, row: &OutboxRow) -> Result<bool, String> {
        let run = self
            .store
            .get_run(&row.run_id)
            .await?
            .ok_or("workflow handoff run missing")?;
        self.check_outbox_identity(&row.kind, &row.outbox_id, &row.payload, &run)?;
        let mut blocked = false;
        if row.kind == "enqueue" && run.status == RunStatus::Pending {
            if let Err(reason) = self.run_still_authorized(&run).await {
                self.mark_blocked(&row.run_id, &reason).await?;
                blocked = true;
            }
        }
        self.deliver_handoff(&row.outbox_id, &run, row.payload.clone())
            .await?;
        Ok(blocked)
    }

    fn check_outbox_identity(
        &self,
        kind: &str,
        outbox_id: &str,
        payload: &str,
        run: &WorkflowRun,
    ) -> Result<(), String> {
        let ok = match kind {
            "enqueue" => outbox_id == enqueue_outbox_id(&run.run_id),
            "resume" => {
                let parsed: Value =
                    serde_json::from_str(payload).map_err(|_| "invalid resume payload")?;
                let approval = parsed
                    .get("approval_id")
                    .and_then(Value::as_str)
                    .ok_or("resume payload lacks approval")?;
                resume_outbox(&run.run_id, &run.actor, approval)
                    == (outbox_id.into(), payload.into())
            }
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err("workflow handoff id mismatch".into())
        }
    }

    /// Hand one outbox row to the queue under its fixed message id, then mark
    /// it delivered. Idempotent: an existing queue message is never duplicated.
    pub(super) async fn deliver_handoff(
        &self,
        message_id: &str,
        run: &WorkflowRun,
        payload: String,
    ) -> Result<(), String> {
        let queue = MessageQueue::open(&self.home)?;
        let message = QueueMessage {
            id: message_id.into(),
            sender: "workflow".into(),
            target: run.actor.clone(),
            payload,
            status: MessageStatus::Pending,
            retry_count: 0,
            delegation_depth: 0,
            origin_agent: None,
            sender_agent: None,
            error: None,
            response: None,
            created_at: run.created_at.clone(),
            acked_at: None,
            completed_at: None,
            reply_channel: None,
            turn_id: None,
            session_id: None,
            // A workflow step has no upstream channel turn to lose.
            upstream_unknown: false,
            lane: None,
        };
        if queue.get_by_id(message_id).await?.is_none() {
            queue.enqueue(&message).await?;
        }
        #[cfg(test)]
        super::security_race_tests::checkpoint("after_queue_enqueue").await;
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE workflow_outbox SET delivered=1 WHERE outbox_id=?1",
                    params![message_id],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await
    }

    /// Test and compatibility form of [`Self::dispatch_queue_message_outcome`]:
    /// a retry is reported as an error.
    pub async fn dispatch_queue_message(
        &self,
        message: &QueueMessage,
    ) -> Result<WorkflowRun, String> {
        match self.dispatch_queue_message_outcome(message).await? {
            DispatchOutcome::Done(run) => Ok(run),
            DispatchOutcome::Retry(reason) => Err(reason),
        }
    }

    /// Execute the run a server-owned handoff names. The message must match
    /// its outbox row exactly; the runner then re-checks every gate.
    pub async fn dispatch_queue_message_outcome(
        &self,
        message: &QueueMessage,
    ) -> Result<DispatchOutcome, String> {
        let raw: Option<(String, String, String)> = self
            .store
            .with_connection(|c| {
                c.query_row(
                    "SELECT kind,entity_id,payload_json FROM workflow_outbox WHERE outbox_id=?1
                        AND kind IN ('enqueue','resume')",
                    params![message.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())
            })
            .await?;
        let (kind, id, payload) = raw.ok_or("workflow queue item has no server handoff")?;
        let run = self
            .store
            .get_run(&id)
            .await?
            .ok_or("workflow run missing")?;
        if message.sender != "workflow"
            || message.target != run.actor
            || payload != message.payload
            || run.grant.is_none()
            || self
                .check_outbox_identity(&kind, &message.id, &payload, &run)
                .is_err()
        {
            return Err("workflow queue authority mismatch".into());
        }
        if run.status == RunStatus::Cancelled {
            return Ok(DispatchOutcome::Done(run));
        }
        match Box::pin(self.runner.execute(&id)).await {
            Ok(run) => Ok(DispatchOutcome::Done(run)),
            Err(error) if error == super::runner::WORKFLOW_BUSY => {
                Ok(DispatchOutcome::Retry(error))
            }
            Err(error)
                if is_transient_error(&error)
                    && i64::from(message.retry_count) < MAX_TRANSIENT_RETRIES =>
            {
                Ok(DispatchOutcome::Retry(error))
            }
            Err(error) => {
                let stored = self
                    .store
                    .get_run(&id)
                    .await?
                    .ok_or("workflow run missing")?;
                if stored.status == RunStatus::Cancelled || error == RUN_CANCELLED {
                    return Ok(DispatchOutcome::Done(stored));
                }
                if is_transient_error(&error) {
                    let code = format!("workflow_transient_retry_exhausted: {error}");
                    self.mark_blocked_as(&id, &code, Some(FAILURE_TRANSIENT))
                        .await?;
                    // R-M6: policy files that stayed unreadable through every
                    // retry suspend the activation (fail closed).
                    if error == super::run_control::POLICY_UNREADABLE {
                        if let Some(activation_id) = &stored.activation_id {
                            Box::pin(super::suspension::suspend_for_policy(
                                &self.store,
                                &self.broker,
                                &self.home,
                                activation_id,
                            ))
                            .await?;
                        }
                    }
                } else {
                    self.mark_blocked(&id, &error).await?;
                }
                Ok(DispatchOutcome::Done(
                    self.store
                        .get_run(&id)
                        .await?
                        .ok_or("workflow run missing")?,
                ))
            }
        }
    }

    /// Gate refusal (or unknown effect outcome when an operation exists).
    pub(super) async fn mark_blocked(&self, id: &str, error: &str) -> Result<(), String> {
        // A budget or count limit is its own class: terminal, and not a
        // failure of the routine (E-H3).
        let class = if super::cost_ledger::is_limit_error(error) {
            Some(super::run_control::FAILURE_LIMIT)
        } else if super::run_control::is_stale_error(error) {
            Some(super::run_control::FAILURE_STALE)
        } else {
            None
        };
        self.mark_blocked_as(id, error, class).await
    }

    pub(super) async fn mark_blocked_as(
        &self,
        id: &str,
        error: &str,
        class: Option<&str>,
    ) -> Result<(), String> {
        let mut run = self
            .store
            .get_run(id)
            .await?
            .ok_or("workflow run missing")?;
        if super::run_control::run_is_terminal(run.status) {
            return Ok(());
        }
        let revision = self
            .store
            .get_revision(&run.workflow_id, run.revision)
            .await?
            .ok_or("workflow revision missing")?;
        let mut uncertain = false;
        for step in &revision.definition.steps {
            let mut evidence = self
                .store
                .get_step(id, &step.step_id)
                .await?
                .ok_or("workflow step missing")?;
            if evidence.status != StepStatus::Succeeded {
                uncertain = evidence.operation_id.is_some();
                evidence.status = if uncertain {
                    StepStatus::Uncertain
                } else {
                    StepStatus::Failed
                };
                evidence.evidence_kind = if uncertain {
                    ExecutionEvidenceKind::None
                } else {
                    ExecutionEvidenceKind::GateDenial
                };
                evidence.error_code = Some(error.into());
                self.runner.save_step(id, &evidence).await?;
                break;
            }
        }
        run.status = if uncertain {
            RunStatus::Uncertain
        } else {
            RunStatus::Blocked
        };
        run.error_code = Some(error.into());
        run.failure_class = Some(
            if uncertain {
                FAILURE_UNCERTAIN
            } else {
                class.unwrap_or(FAILURE_GATE)
            }
            .into(),
        );
        self.runner.save_run(&run).await?;
        // R-M3: a routine that keeps ending on a limit repeats the same
        // partial work every slot; after as many limit blocks in a row as the
        // breaker allows, suspend it (re-approval needed).
        if !uncertain && class == Some(FAILURE_LIMIT) {
            if let Some(activation_id) = &run.activation_id {
                if self
                    .consecutive_limit_blocks(activation_id, run.budget.max_consecutive_failures)
                    .await?
                {
                    Box::pin(super::suspension::suspend_for_limit(
                        &self.store,
                        &self.broker,
                        &self.home,
                        activation_id,
                        error,
                    ))
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// The last `n` finished runs of `activation_id` all ended on a limit.
    pub(crate) async fn consecutive_limit_blocks(&self, activation_id: &str, n: u32) -> Result<bool, String> {
        let id = activation_id.to_string();
        let classes: Vec<Option<String>> = self
            .store
            .with_connection(move |c| {
                let mut q = c
                    .prepare(
                        "SELECT json_extract(record_json,'$.failure_class') FROM workflow_runs
                            WHERE json_extract(record_json,'$.activation_id')=?1
                            AND status IN ('succeeded','failed','blocked','uncertain')
                            ORDER BY created_at DESC,run_id DESC LIMIT ?2",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = q
                    .query_map(rusqlite::params![id, n.max(1)], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?;
                Ok(rows)
            })
            .await?;
        Ok(classes.len() >= n.max(1) as usize
            && classes.iter().all(|c| c.as_deref() == Some(FAILURE_LIMIT)))
    }

    /// Resident sweep, also run once at boot: deliver the outbox, resume runs
    /// whose decision arrived or expired, and re-wake runs nobody owns.
    pub async fn sweep(&self) -> SweepReport {
        let mut report = SweepReport::default();
        // U8: expire activations past their validity (and warn Admins three
        // days ahead); retry ledger revocation of stopped activations.
        match Box::pin(super::suspension::settle_lifetimes(
            &self.store,
            &self.broker,
            &self.home,
        ))
        .await
        {
            Ok(n) => report.activations_expired = n,
            Err(error) => report.errors.push(format!("activation lifetimes: {error}")),
        }
        if let Err(error) = self.deliver_outbox(&mut report).await {
            report.errors.push(format!("outbox: {error}"));
        }
        match self.broker.drain_grant_revocations().await {
            Ok(revocations) => {
                for (grant, epoch, reason, executing) in revocations {
                    if !executing.is_empty() {
                        tracing::warn!(
                            grant = %grant, epoch, reason = %reason, operations = ?executing,
                            "workflow grant revoked while operations were executing"
                        );
                    }
                }
            }
            Err(error) => report.errors.push(format!("revocations: {error}")),
        }
        let runs: Result<Vec<String>, String> = self
            .store
            .with_connection(|c| {
                let mut q = c
                    .prepare(
                        "SELECT run_id FROM workflow_runs WHERE status IN
                        ('pending','running','waiting_approval','needs_input')
                        AND json_extract(record_json,'$.activation_id') IS NOT NULL ORDER BY created_at",
                    )
                    .map_err(|e| e.to_string())?;
                q.query_map([], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())
            })
            .await;
        let runs = match runs {
            Ok(runs) => runs,
            Err(error) => {
                report.errors.push(format!("runs: {error}"));
                return report;
            }
        };
        for run_id in runs {
            if let Err(error) = self.sweep_run(&run_id, &mut report).await {
                report.errors.push(format!("{run_id}: {error}"));
            }
        }
        report
    }

    async fn sweep_run(&self, run_id: &str, report: &mut SweepReport) -> Result<(), String> {
        let run = self
            .store
            .get_run(run_id)
            .await?
            .ok_or("workflow run missing")?;
        if super::run_control::run_is_terminal(run.status) || self.lease_live(run_id).await? {
            return Ok(());
        }
        if matches!(
            run.status,
            RunStatus::WaitingApproval | RunStatus::NeedsInput
        ) {
            let waiting = self.store.get_steps(run_id).await?.into_iter().find(|s| {
                matches!(
                    s.status,
                    StepStatus::WaitingApproval | StepStatus::NeedsInput
                )
            });
            if let Some(approval) = waiting.and_then(|s| s.approval_id) {
                // `poll` turns a past-due card into Expired: expiry is not an
                // event anything else would deliver.
                if self
                    .broker
                    .poll(&ApprovalId::from(approval.clone()))
                    .await?
                    == ApprovalStatus::Pending
                {
                    return Ok(());
                }
                let (outbox_id, payload) = resume_outbox(run_id, &run.actor, &approval);
                self.store
                    .with_transaction(|tx| {
                        tx.execute(
                            "INSERT INTO workflow_outbox VALUES(?1,'resume',?2,?3,0)
                                ON CONFLICT(outbox_id) DO NOTHING",
                            params![outbox_id, run_id, payload],
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(())
                    })
                    .await?;
                if !self.in_flight(&outbox_id).await? {
                    self.redeliver(&outbox_id, &run, payload).await?;
                    report.resumed += 1;
                }
                return Ok(());
            }
            // A waiting run without a waiting step is inconsistent: let the
            // runner look at it again.
        }
        let prefix = enqueue_outbox_id(run_id);
        let queue = MessageQueue::open(&self.home)?;
        if queue
            .statuses_with_prefix(&prefix)
            .await?
            .iter()
            .any(|(_, s)| {
                matches!(
                    s,
                    MessageStatus::Pending | MessageStatus::Acked | MessageStatus::Processing
                )
            })
        {
            return Ok(());
        }
        let payload: String = self
            .store
            .with_connection(|c| {
                c.query_row(
                    "SELECT payload_json FROM workflow_outbox WHERE outbox_id=?1 AND kind='enqueue'",
                    params![prefix],
                    |r| r.get(0),
                )
                .map_err(|_| "workflow handoff missing".to_string())
            })
            .await?;
        self.redeliver(&prefix, &run, payload).await?;
        report.requeued += 1;
        Ok(())
    }

    /// Put a message back in the queue: reopen a finished one, or create it.
    async fn redeliver(
        &self,
        message_id: &str,
        run: &WorkflowRun,
        payload: String,
    ) -> Result<(), String> {
        let queue = MessageQueue::open(&self.home)?;
        match queue.get_by_id(message_id).await? {
            Some(m)
                if matches!(
                    m.status,
                    MessageStatus::Pending | MessageStatus::Acked | MessageStatus::Processing
                ) =>
            {
                Ok(())
            }
            Some(_) => queue.reset_to_pending(message_id).await,
            None => self.deliver_handoff(message_id, run, payload).await,
        }
    }

    async fn in_flight(&self, message_id: &str) -> Result<bool, String> {
        Ok(MessageQueue::open(&self.home)?
            .get_by_id(message_id)
            .await?
            .is_some_and(|m| {
                matches!(
                    m.status,
                    MessageStatus::Pending | MessageStatus::Acked | MessageStatus::Processing
                )
            }))
    }

    async fn lease_live(&self, run_id: &str) -> Result<bool, String> {
        let until: Option<i64> = self
            .store
            .with_connection(|c| {
                c.query_row(
                    "SELECT lease_until FROM workflow_run_leases WHERE run_id=?1",
                    params![run_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())
            })
            .await?;
        Ok(until.is_some_and(|u| u > chrono::Utc::now().timestamp()))
    }
}
