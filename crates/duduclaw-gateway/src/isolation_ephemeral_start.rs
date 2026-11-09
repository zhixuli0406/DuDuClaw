//! H13b: start released `spawn_ephemeral` children (D-H3, D8).
//!
//! - [`start_released`] runs on every dispatcher tick, before the SQLite queue
//!   is polled. It releases (one-shot) the jobs whose parent run has exited
//!   (`isolation_ephemeral_runs::LiveRuns`, fail closed when unknown) and whose
//!   employee is still the same id and incarnation (`LiveLifecycle`), then
//!   enqueues each child as a self-delegation of the parent employee
//!   (`ephemeral_child::child_message`). The same tick's poll picks it up.
//! - [`self_delegation_allowed`] is the dispatcher gate's only exception to
//!   "self-delegation is refused under every policy": the queue row must name
//!   a job of `isolation-ephemeral-jobs.db` that was released, belongs to this
//!   employee label, works for the job's requester, and whose employee id +
//!   incarnation equal the live lifecycle's. H13c: passing it moves the job
//!   to `started` — the exception holds once per job (a retried or duplicated
//!   queue row is refused), and only a started job authorizes the child run.
//!
//! Nothing here creates the job store: without it there is nothing to start
//! and no exception.
use std::path::Path;
use duduclaw_mcp_boundary::VerificationError;
use crate::isolation_background::ephemeral_child;
use crate::isolation_ephemeral::{EphemeralJob, EphemeralJobs, LiveLifecycle, ParentIdentity, ParentRuns, JOBS_DB};
use crate::message_queue::{MessageQueue, QueueMessage};

/// Audit event: a released child was enqueued.
pub const STARTED_EVENT: &str = "isolation_ephemeral_job_started";
/// Audit event: a released child could not be enqueued (it is not retried:
/// release is one-shot).
pub const START_FAILED_EVENT: &str = "isolation_ephemeral_job_start_failed";

/// Release the startable jobs and build each child's queue row.
pub fn release_children(jobs: &mut EphemeralJobs, runs: &dyn ParentRuns, identity: &dyn ParentIdentity, now: u64)
    -> Result<Vec<(EphemeralJob, Result<QueueMessage, VerificationError>)>, VerificationError> {
    Ok(jobs.take_startable(runs, identity, now)?.into_iter()
        .map(|job| { let message = ephemeral_child::child_message(&job); (job, message) }).collect())
}

/// The exception itself (store and identity injected). `Ok` only for the
/// released job named by `message_id`, of employee label `agent`, for its
/// own requester, still the same employee id and incarnation. H13c: a pass
/// consumes the exception (`released → started`), so it holds once per job;
/// a refused check changes nothing. H13d: an expired job (`now >=
/// expires_at`) is dropped, never started.
pub fn self_delegation_check_at(jobs: &EphemeralJobs, identity: &dyn ParentIdentity, message_id: &str, agent: &str,
    on_behalf_of: Option<&str>, now: u64) -> Result<(), VerificationError> {
    let id = ephemeral_child::job_id_of(message_id).ok_or(VerificationError::Invalid)?;
    jobs.begin_start(id, now, |job| {
        if job.parent.agent != agent || on_behalf_of != Some(ephemeral_child::owner_principal(job)?.as_str()) {
            return Err(VerificationError::BindingMismatch);
        }
        if identity.current(agent)? != (job.parent.employee, job.parent.incarnation) {
            return Err(VerificationError::Revoked);
        }
        Ok(())
    })?.map(|_| ()).ok_or(VerificationError::Revoked)
}

/// Dispatcher gate entry: `sender == target == agent` was already compared by
/// the caller. False outside protected mode, for any other message id, and
/// whenever a store or the lifecycle cannot be read.
pub(crate) async fn self_delegation_allowed(home: &Path, message_id: &str, agent: &str, on_behalf_of: Option<&str>)
    -> bool {
    if !duduclaw_core::execution_gate::isolation_required() || ephemeral_child::job_id_of(message_id).is_none() {
        return false;
    }
    let (home, message_id, agent, on_behalf_of) =
        (home.to_owned(), message_id.to_owned(), agent.to_owned(), on_behalf_of.map(str::to_owned));
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        if !home.join(JOBS_DB).exists() { return Err(VerificationError::Unavailable); }
        let jobs = EphemeralJobs::open(&home)?;
        self_delegation_check_at(&jobs, &LiveLifecycle { home: &home }, &message_id, &agent, on_behalf_of.as_deref(),
            duduclaw_mcp_boundary::authority::now_seconds()?)
    }).await.is_ok()
}

fn audit(home: &Path, event: &str, job: &EphemeralJob, stage: &str) {
    duduclaw_security::audit::append_audit_event(home, &duduclaw_security::audit::AuditEvent::new(
        event, &job.parent.agent,
        if event == STARTED_EVENT { duduclaw_security::audit::Severity::Info } else { duduclaw_security::audit::Severity::Warning },
        serde_json::json!({"job": job.id, "message": ephemeral_child::message_id(&job.id),
            "incarnation": job.parent.incarnation, "tools": job.tools.len(), "stage": stage})));
}

/// One dispatcher tick: release and enqueue every startable child. Errors
/// are logged; a job whose state cannot be read stays pending.
pub(crate) async fn start_released(home: &Path, queue: &MessageQueue) {
    if !duduclaw_core::execution_gate::isolation_required() { return; }
    let blocking_home = home.to_owned();
    let released = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        if !blocking_home.join(JOBS_DB).exists() { return Ok(Vec::new()); }
        let mut jobs = EphemeralJobs::open(&blocking_home)?;
        release_children(&mut jobs, &crate::isolation_ephemeral_runs::LiveRuns,
            &LiveLifecycle { home: &blocking_home }, duduclaw_mcp_boundary::authority::now_seconds()?)
    }).await;
    let released = match released {
        Ok(released) => released,
        Err(error) => {
            tracing::warn!(?error, "ephemeral jobs: release skipped this tick");
            return;
        }
    };
    for (job, message) in released {
        let enqueued = match message {
            Ok(message) => queue.enqueue(&message).await.map_err(|_| VerificationError::Unavailable),
            Err(error) => Err(error),
        };
        match enqueued {
            Ok(()) => {
                tracing::info!(agent = job.parent.agent.as_str(), job = job.id.as_str(), "ephemeral child started after parent exit");
                audit(home, STARTED_EVENT, &job, "enqueued");
            }
            Err(error) => {
                tracing::warn!(agent = job.parent.agent.as_str(), job = job.id.as_str(), ?error, "ephemeral child not started");
                audit(home, START_FAILED_EVENT, &job, "enqueue");
                // H13c (D8): a child that never got a queue row must not keep
                // its employee's slot; it is dropped (release is one-shot).
                let (blocking_home, id) = (home.to_owned(), job.id.clone());
                if duduclaw_mcp_boundary::authority::verify_blocking(move ||
                    EphemeralJobs::open(&blocking_home)?.drop_released(&id)).await.is_err() {
                    tracing::warn!(job = job.id.as_str(), "ephemeral job: failed start not recorded; it expires with its TTL");
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "isolation_ephemeral_start_tests.rs"]
mod tests;
