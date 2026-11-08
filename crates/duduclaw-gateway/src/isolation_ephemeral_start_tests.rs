//! H13b: releasing and starting ephemeral children; the dispatcher's
//! self-delegation exception.
use super::*;
use duduclaw_isolation::{bounded::Text, capability::{EmployeeBinding, Lane}, execution::RunHandle,
    ids::{EmployeeId, RunId}};
use crate::isolation_ephemeral::{EphemeralRequest, JobState, ParentRun};
use crate::isolation_ephemeral_runs::RunLedger;
use rusqlite::Connection;

const PARENT: EmployeeId = EmployeeId([4; 16]);
const RUN: RunHandle = RunHandle(RunId([5; 16]));
const AGENT: &str = "employee-a";

fn personal() -> EmployeeBinding { EmployeeBinding::Personal(Text::new("owner".into()).unwrap()) }
fn job(binding: EmployeeBinding) -> EphemeralJob {
    let parent = ParentRun { agent: AGENT.into(), employee: PARENT, incarnation: 3, binding, run: RUN,
        lane: Lane::Standard, requester: "owner".into(), task: None };
    EphemeralJob::new(parent, EphemeralRequest { instruction: "You review invoices.".into(),
        context: "Check the March batch.".into(), tier: "cheap".into(),
        tools: vec!["wiki_read".into(), "memory_search".into()] }, 1_000).unwrap()
}
fn store() -> EphemeralJobs { EphemeralJobs::from_connection(Connection::open_in_memory().unwrap()).unwrap() }

struct Identity(Result<(EmployeeId, u32), VerificationError>);
impl ParentIdentity for Identity {
    fn current(&self, agent: &str) -> Result<(EmployeeId, u32), VerificationError> {
        assert_eq!(agent, AGENT);
        self.0
    }
}
const SAME: Identity = Identity(Ok((PARENT, 3)));

#[test]
fn h13b_personal_child_runs_after_parent_exit_as_same_identity() {
    let mut ledger = RunLedger::new();
    let mut jobs = store();
    let job = job(personal());
    jobs.queue(&job).unwrap();
    // Unknown run (not bound in this epoch): fail closed, the job waits.
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_001).unwrap().is_empty());
    // D8: while the parent run holds the capability the child never starts.
    ledger.bound(RUN, PARENT);
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_002).unwrap().is_empty());
    // The parent exited, but another run of the same employee is bound: wait.
    let other = RunHandle(RunId([6; 16]));
    ledger.exited(RUN);
    ledger.bound(other, PARENT);
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_003).unwrap().is_empty());
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Pending));
    // No capability-holding run left: released once, as a self-delegation of
    // the parent employee for the parent's own requester.
    ledger.exited(other);
    let started = release_children(&mut jobs, &ledger, &SAME, 1_004).unwrap();
    assert_eq!(started.len(), 1);
    let (released, message) = &started[0];
    assert_eq!(released, &job);
    assert_eq!((released.parent.employee, released.parent.incarnation, &released.parent.binding), (PARENT, 3, &personal()));
    let message = message.as_ref().unwrap();
    assert_eq!(message.id, ephemeral_child::message_id(&job.id));
    assert_eq!((message.sender.as_str(), message.target.as_str()), (AGENT, AGENT));
    assert_eq!((message.sender_agent.as_deref(), message.origin_agent.as_deref()), (Some(AGENT), Some(AGENT)));
    assert_eq!(message.on_behalf_of.as_deref(), Some("user:owner"));
    assert_eq!(message.delegation_depth, 1);
    assert!(message.payload.contains("You review invoices.") && message.payload.contains("Check the March batch."));
    // The receipt captured at queue time binds exactly what the dispatcher
    // derives from this row once stored (the queue masks the payload).
    let stored = crate::isolation_background::material::delegation(&message.sender, &message.target,
        &duduclaw_security::credential_detect::mask(&message.payload), message.turn_id.as_deref(),
        message.session_id.as_deref(), message.lane.as_deref(), 1, message.origin_agent.as_deref(),
        message.reply_channel.as_deref(), message.sender_agent.as_deref(), message.upstream_unknown, 0, false,
        message.on_behalf_of.as_deref());
    assert_eq!(stored, ephemeral_child::child_material(&job).unwrap());
    // One-shot, and the gate exception now holds for exactly this row.
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_005).unwrap().is_empty());
    assert_eq!(self_delegation_check(&jobs, &SAME, &message.id, AGENT, Some("user:owner")), Ok(()));
}

#[test]
fn h13b_self_delegation_exception_only_for_matching_job() {
    let mut ledger = RunLedger::new();
    let mut jobs = store();
    let job = job(personal());
    jobs.queue(&job).unwrap();
    let id = ephemeral_child::message_id(&job.id);
    // Pending (the parent may still be running): no exception.
    assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Err(VerificationError::Revoked));
    ledger.bound(RUN, PARENT);
    ledger.exited(RUN);
    assert_eq!(release_children(&mut jobs, &ledger, &SAME, 1_001).unwrap().len(), 1);
    assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Ok(()));
    // Any other queue row, label or principal is refused.
    let unknown = ephemeral_child::message_id(&uuid::Uuid::new_v4().to_string());
    assert_eq!(self_delegation_check(&jobs, &SAME, "message-1", AGENT, Some("user:owner")), Err(VerificationError::Invalid));
    assert_eq!(self_delegation_check(&jobs, &SAME, &unknown, AGENT, Some("user:owner")), Err(VerificationError::Revoked));
    assert_eq!(self_delegation_check(&jobs, &SAME, &id, "employee-b", Some("user:owner")), Err(VerificationError::BindingMismatch));
    for principal in [None, Some("user:intruder"), Some("owner")] {
        assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, principal), Err(VerificationError::BindingMismatch));
    }
    // The live employee must be the very same id and incarnation; an
    // unreadable lifecycle is never a pass.
    for current in [Ok((PARENT, 4)), Ok((EmployeeId([9; 16]), 3))] {
        assert_eq!(self_delegation_check(&jobs, &Identity(current), &id, AGENT, Some("user:owner")), Err(VerificationError::Revoked));
    }
    assert!(self_delegation_check(&jobs, &Identity(Err(VerificationError::Unavailable)), &id, AGENT, Some("user:owner")).is_err());
}

#[test]
fn h13b_stale_incarnation_job_never_starts() {
    let mut ledger = RunLedger::new();
    ledger.bound(RUN, PARENT);
    ledger.exited(RUN);
    // Restored (incarnation 4) or recreated (another id) before the start:
    // dropped, never released, no queue row, no exception.
    for current in [Ok((PARENT, 4)), Ok((EmployeeId([9; 16]), 3)), Err(VerificationError::Revoked)] {
        let mut jobs = store();
        let job = job(personal());
        jobs.queue(&job).unwrap();
        assert!(release_children(&mut jobs, &ledger, &Identity(current), 1_001).unwrap().is_empty());
        assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Dropped));
        let id = ephemeral_child::message_id(&job.id);
        assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Err(VerificationError::Revoked));
        // A later restore back to the same values does not revive it.
        assert!(release_children(&mut jobs, &ledger, &SAME, 1_002).unwrap().is_empty());
    }
    // Released, then the employee is restored before the dispatcher dequeues
    // it: the gate exception refuses the self-delegation.
    let mut jobs = store();
    let job = job(EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()));
    jobs.queue(&job).unwrap();
    assert_eq!(release_children(&mut jobs, &ledger, &SAME, 1_001).unwrap().len(), 1);
    let id = ephemeral_child::message_id(&job.id);
    assert_eq!(self_delegation_check(&jobs, &Identity(Ok((PARENT, 4))), &id, AGENT, Some("user:owner")),
        Err(VerificationError::Revoked));
}
