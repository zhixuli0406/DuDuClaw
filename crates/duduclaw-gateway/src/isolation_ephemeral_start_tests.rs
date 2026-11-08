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
    // H13c: plus the digest of the job's tools.
    assert_eq!(ephemeral_child::with_job_tools(stored.clone(), &job).unwrap(), ephemeral_child::child_material(&job).unwrap());
    assert_ne!(stored, ephemeral_child::child_material(&job).unwrap());
    // One-shot, and the gate exception now holds for exactly this row.
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_005).unwrap().is_empty());
    assert_eq!(self_delegation_check(&jobs, &SAME, &message.id, AGENT, Some("user:owner")), Ok(()));
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Started));
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
    // Any other queue row, label or principal is refused (and consumes nothing).
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
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Released));
    assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Ok(()));
}

#[test]
fn h13c_self_delegation_exception_once_per_job() {
    let mut ledger = RunLedger::new();
    let mut jobs = store();
    let job = job(personal());
    jobs.queue(&job).unwrap();
    let id = ephemeral_child::message_id(&job.id);
    ledger.bound(RUN, PARENT);
    ledger.exited(RUN);
    assert_eq!(release_children(&mut jobs, &ledger, &SAME, 1_001).unwrap().len(), 1);
    // Released but not admitted: no child run is authorized from it yet.
    assert_eq!(jobs.started(&job.id).unwrap(), None);
    // The first admission consumes the exception: released → started …
    assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Ok(()));
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Started));
    assert_eq!(jobs.released(&job.id).unwrap(), None);
    // … and only a started job authorizes the child run (`child_tools`).
    assert_eq!(jobs.started(&job.id).unwrap(), Some(job.clone()));
    // A retried or duplicated queue row for the same job is refused.
    for _ in 0..2 {
        assert_eq!(self_delegation_check(&jobs, &SAME, &id, AGENT, Some("user:owner")), Err(VerificationError::Revoked));
    }
    // Nothing releases it again.
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_002).unwrap().is_empty());
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Started));
}

#[test]
fn h13c_d8_one_child_per_employee_at_a_time() {
    let mut ledger = RunLedger::new();
    let mut jobs = store();
    let (first, second) = (job(personal()), job(personal()));
    jobs.queue(&first).unwrap();
    jobs.queue(&second).unwrap();
    ledger.bound(RUN, PARENT);
    ledger.exited(RUN);
    // Both are startable, but only one child of the employee is released per tick …
    let started = release_children(&mut jobs, &ledger, &SAME, 1_001).unwrap();
    assert_eq!(started.len(), 1);
    let (released, other) = if started[0].0 == first { (&first, &second) } else { (&second, &first) };
    assert_eq!(jobs.state(&other.id).unwrap(), Some(JobState::Pending));
    // … and none while that child is released but its run not admitted yet
    // (enqueued, not reserved/bound): it holds the employee's slot.
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_002).unwrap().is_empty());
    assert_eq!(jobs.state(&other.id).unwrap(), Some(JobState::Pending));
    // Once admitted, its run binds (holding the capability: still waiting) …
    assert_eq!(self_delegation_check(&jobs, &SAME, &ephemeral_child::message_id(&released.id), AGENT,
        Some("user:owner")), Ok(()));
    let child_run = RunHandle(RunId([7; 16]));
    ledger.bound(child_run, PARENT);
    assert!(release_children(&mut jobs, &ledger, &SAME, 1_003).unwrap().is_empty());
    // … and only after it exited is the next child released.
    ledger.exited(child_run);
    let next = release_children(&mut jobs, &ledger, &SAME, 1_004).unwrap();
    assert_eq!(next.iter().map(|(job, _)| job.id.clone()).collect::<Vec<_>>(), vec![other.id.clone()]);
    // A released child whose start failed gives the slot back (dropped).
    jobs.drop_released(&other.id).unwrap();
    assert_eq!(jobs.state(&other.id).unwrap(), Some(JobState::Dropped));
}

#[test]
fn h13c_job_tools_bound_into_child_material() {
    let job = job(personal());
    let message = ephemeral_child::child_message(&job).unwrap();
    let derived = crate::isolation_background::material::delegation(&message.sender, &message.target,
        &duduclaw_security::credential_detect::mask(&message.payload), None, None, None, 1, Some(AGENT), None,
        Some(AGENT), false, 0, false, message.on_behalf_of.as_deref());
    let bound = |found: Result<Option<EphemeralJob>, VerificationError>| ephemeral_child::bind_job_tools_with(
        "delegation", &message.id, derived.clone(), |id| { assert_eq!(id, job.id); found });
    // The stored job, unchanged: exactly the captured material.
    assert_eq!(bound(Ok(Some(job.clone()))), Ok(ephemeral_child::child_material(&job).unwrap()));
    // A job row whose tools changed after the capture no longer matches it.
    for tools in [vec!["wiki_read".to_owned()], vec!["memory_search".to_owned(), "wiki_read".to_owned()],
        vec!["wiki_read".to_owned(), "memory_search".to_owned(), "web_extract".to_owned()]] {
        let edited = EphemeralJob { tools, ..job.clone() };
        assert_ne!(bound(Ok(Some(edited))).unwrap(), ephemeral_child::child_material(&job).unwrap());
    }
    // A missing or unreadable job refuses; a job under another id refuses.
    assert_eq!(bound(Ok(None)), Err(VerificationError::Revoked));
    assert_eq!(bound(Err(VerificationError::Unavailable)), Err(VerificationError::Unavailable));
    let other = EphemeralJob { id: uuid::Uuid::new_v4().to_string(), ..job.clone() };
    assert_eq!(bound(Ok(Some(other))), Err(VerificationError::Revoked));
    // Ordinary delegations and other record kinds are unchanged, no lookup.
    for (kind, id) in [("delegation", "message-1"), ("cron", message.id.as_str())] {
        assert_eq!(ephemeral_child::bind_job_tools_with(kind, id, derived.clone(), |_| unreachable!()), Ok(derived.clone()));
    }
}

#[test]
fn h13c_job_store_migrates_started_state() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE ephemeral_jobs(
            id TEXT PRIMARY KEY, employee TEXT NOT NULL, run TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('pending','released','dropped')),
            job TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
        CREATE INDEX ephemeral_jobs_state ON ephemeral_jobs(state, created_at);").unwrap();
    let job = job(personal());
    let encoded = serde_json::to_string(&job).unwrap();
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    db.execute("INSERT INTO ephemeral_jobs VALUES(?1,?2,?3,'released',?4,1000,?5)",
        rusqlite::params![job.id, hex(&PARENT.0[..]), hex(&(RUN.0).0[..]), encoded, job.expires_at as i64]).unwrap();
    // An H13b store keeps its rows and accepts the `started` state.
    let jobs = EphemeralJobs::from_connection(db).unwrap();
    assert_eq!(jobs.released(&job.id).unwrap(), Some(job.clone()));
    assert_eq!(self_delegation_check(&jobs, &SAME, &ephemeral_child::message_id(&job.id), AGENT, Some("user:owner")), Ok(()));
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Started));
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
