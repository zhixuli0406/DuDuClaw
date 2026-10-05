//! Run-level control shared by the queue handoff, the resident sweep, the
//! approval ledger and the dashboard: wake-up ids, failure classes,
//! cancellation and the consecutive-failure reset.
use super::*;
use crate::approval::ApprovalId;
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

/// The run reached a final state on a gate refusal.
pub const FAILURE_GATE: &str = "gate";
/// A step definitely failed (handler, schema or human refusal).
pub const FAILURE_DEFINITE: &str = "definite";
/// An effect's outcome is unknown.
pub const FAILURE_UNCERTAIN: &str = "uncertain";
/// Infrastructure retries ran out; says nothing about the routine itself and
/// is not counted toward the consecutive-failure limit.
pub const FAILURE_TRANSIENT: &str = "transient";
/// A budget or count limit ended the run; not counted toward the breaker
/// either (the routine did nothing wrong, it ran out of allowance).
pub const FAILURE_LIMIT: &str = "limit";
/// Input or read data aged out (e.g. while a person decided); the routine
/// did nothing wrong, so this does not count toward the breaker (R-M5).
pub const FAILURE_STALE: &str = "stale";

/// The employee's policy files could not be read cleanly (e.g. caught mid
/// write). Retried as transient; only when it persists through every retry
/// is the activation suspended (R-M6).
pub const POLICY_UNREADABLE: &str = "workflow_policy_unreadable";

/// Error codes that mean "data aged out", classed [`FAILURE_STALE`].
pub fn is_stale_error(error: &str) -> bool {
    matches!(error, "workflow_input_expired" | "workflow_read_data_expired")
}
/// `error_code` of a run an operator cancelled.
pub const RUN_CANCELLED: &str = "workflow_run_cancelled";
/// Transient dispatch retries before a run is blocked.
pub const MAX_TRANSIENT_RETRIES: i64 = 5;

pub fn run_is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded
            | RunStatus::Failed
            | RunStatus::Uncertain
            | RunStatus::Cancelled
            | RunStatus::Blocked
    )
}

/// Queue/outbox id of a run's first handoff.
pub fn enqueue_outbox_id(run_id: &str) -> String {
    format!("workflow:{run_id}")
}

/// Queue/outbox id and payload of the wake-up a decision on `approval_id`
/// produces. The decision transaction and the sweep both use this, so
/// whichever writes first wins and the other is a no-op.
pub fn resume_outbox(run_id: &str, actor: &str, approval_id: &str) -> (String, String) {
    (
        format!("workflow:{run_id}:resume:{approval_id}"),
        json!({"run_id": run_id, "actor": actor, "approval_id": approval_id}).to_string(),
    )
}

/// Last `max_consecutive_failures` finished runs of one activation that
/// count toward its breaker. `schema` is `""` or an attached alias with a
/// trailing dot. Parameters: workflow, current run, activation, limit.
pub fn consecutive_failure_sql(schema: &str) -> String {
    format!(
        "SELECT status FROM {schema}workflow_runs WHERE workflow_id=?1 AND run_id<>?2
            AND json_extract(record_json,'$.activation_id')=?3
            AND status IN ('succeeded','failed','blocked','uncertain')
            AND COALESCE(json_extract(record_json,'$.failure_class'),'') NOT IN ('{FAILURE_TRANSIENT}','{FAILURE_LIMIT}','{FAILURE_STALE}')
            AND julianday(created_at)>COALESCE((SELECT MAX(julianday(reset_at))
                FROM {schema}workflow_failure_resets WHERE activation_id=?3),0)
            ORDER BY created_at DESC,run_id DESC LIMIT ?4"
    )
}

/// Errors about the machinery rather than the run: retrying cannot repeat a
/// side effect, because a step left `Running` is reconciled against the
/// operation ledger before anything is re-sent.
pub fn is_transient_error(error: &str) -> bool {
    const EXACT: &[&str] = &[
        super::runner::WORKFLOW_BUSY,
        super::runner::WORKFLOW_LEASE_LOST,
        super::runner::WORKFLOW_SEGMENT_EXPIRED,
        super::runner::WORKFLOW_EFFECT_NOT_STARTED,
        "workflow_step_timeout",
        "workflow_read_transport_unavailable",
        POLICY_UNREADABLE,
    ];
    EXACT.contains(&error)
        || error.starts_with("workflow stdio unavailable:")
        || error.ends_with("database is locked")
        || error.ends_with("database table is locked")
}

impl WorkflowService {
    /// Stop one run. Takes effect at the next step boundary and at the
    /// ledger's claim/begin check (a cancelled run is not an authorized
    /// state there); an effect that already began is not undone and stays
    /// visible on its step. Pending decision cards of the run are withdrawn.
    pub async fn cancel_run(&self, run_id: &str, by: &str) -> Result<WorkflowRun, String> {
        if by.is_empty() {
            return Err("cancellation needs an actor".into());
        }
        let run = self
            .store
            .with_transaction(|tx| {
                let raw: String = tx
                    .query_row(
                        "SELECT record_json FROM workflow_runs WHERE run_id=?1",
                        params![run_id],
                        |r| r.get(0),
                    )
                    .map_err(|_| "workflow run missing")?;
                let mut run: WorkflowRun =
                    serde_json::from_str(&raw).map_err(|_| "invalid workflow run")?;
                if run.activation_id.is_none() {
                    return Err("only activated runs can be cancelled".into());
                }
                if run_is_terminal(run.status) {
                    return Ok(run);
                }
                run.status = RunStatus::Cancelled;
                run.error_code = Some(RUN_CANCELLED.into());
                run.failure_class = None;
                run.cancelled_by = Some(by.into());
                tx.execute(
                    "UPDATE workflow_runs SET record_json=?1,status='cancelled' WHERE run_id=?2",
                    params![
                        serde_json::to_string(&run).map_err(|e| e.to_string())?,
                        run_id
                    ],
                )
                .map_err(|e| e.to_string())?;
                Ok(run)
            })
            .await?;
        if run.status == RunStatus::Cancelled {
            for step in self.store.get_steps(run_id).await? {
                if step.status == StepStatus::Succeeded {
                    continue;
                }
                if let Some(id) = step.approval_id {
                    self.broker
                        .invalidate_request(&ApprovalId::from(id), RUN_CANCELLED)
                        .await?;
                }
            }
        }
        Ok(run)
    }

    /// Server state of one run for the dashboard: status and error codes as
    /// stored, every step's state, and the live state of its decision card
    /// or operation. Step outputs are not included (hash only).
    pub async fn run_view(&self, run: &WorkflowRun) -> Result<serde_json::Value, String> {
        // E-H5b: a suspended activation shows why, so the reader knows what
        // a new revision must re-approve.
        let activation = match &run.activation_id {
            Some(id) => self.activation(id).await?.map(|a| {
                json!({
                    "activation_id": id,
                    "state": a.state,
                    "error_code": a.error_code,
                    "suspension": a.suspension,
                })
            }),
            None => None,
        };
        let mut steps = Vec::new();
        for step in self.store.get_steps(&run.run_id).await? {
            let decision = match &step.approval_id {
                Some(id) => self
                    .broker
                    .get(&ApprovalId::from(id.clone()))
                    .await?
                    .map(|r| json!({"approval_id": id, "status": r.status})),
                None => None,
            };
            let operation = match &step.operation_id {
                Some(id) => self.broker.inspect_operation(id).await?.map(|o| {
                    json!({
                        "operation_id": id,
                        "state": o.state,
                        "error_code": o.error_code,
                        "operator_resolved": o.operator_resolution.is_some()
                    })
                }),
                None => None,
            };
            steps.push(json!({
                "step_id": step.step_id,
                "status": step.status,
                "error_code": step.error_code,
                "evidence_kind": step.evidence_kind,
                "output_hash": step.output_hash,
                "observed_at": step.observed_at,
                "decision": decision,
                "operation": operation,
                "operator_resolution": step.operator_resolution,
            }));
        }
        Ok(json!({
            "run_id": run.run_id,
            "workflow_id": run.workflow_id,
            "revision": run.revision,
            "activation_id": run.activation_id,
            "trigger": run.trigger,
            "status": run.status,
            "error_code": run.error_code,
            "failure_class": run.failure_class,
            "cancelled_by": run.cancelled_by,
            "created_at": run.created_at,
            "deadline_at": run.deadline_at,
            "input_observed_at": run.input_observed_at,
            "cost": run.cost,
            "budget": run.budget,
            // Whether the money budgets can bind at all: with every unit
            // price at 0 only the count limits are in effect (E-H3).
            "pricing": super::cost_ledger::load_pricing(&self.home).view(),
            "activation": activation,
            "steps": steps,
        }))
    }

    /// Clear the consecutive-failure breaker of one activation without
    /// re-activating it: runs before this moment no longer count.
    pub async fn reset_consecutive_failures(
        &self,
        activation_id: &str,
        by: &str,
        reason: &str,
    ) -> Result<String, String> {
        if by.is_empty() || reason.trim().is_empty() || reason.chars().count() > 500 {
            return Err("reset needs an actor and a reason of at most 500 characters".into());
        }
        let record = self
            .activation(activation_id)
            .await?
            .ok_or("workflow activation missing")?;
        if record.state != ActivationState::Active {
            return Err("workflow activation not active".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO workflow_failure_resets VALUES(?1,?2,?3,?4,?5)",
                    params![
                        id,
                        activation_id,
                        Utc::now().to_rfc3339(),
                        by,
                        reason.trim()
                    ],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await?;
        Ok(id)
    }
}
