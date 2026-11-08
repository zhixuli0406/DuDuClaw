//! H13 job-store tests: D8 queueing, parent identity, bounds.
use super::*;
use std::cell::Cell;
use duduclaw_isolation::{bounded::Text, ids::RunId};

const PARENT: EmployeeId = EmployeeId([4; 16]);
const RUN: RunHandle = RunHandle(RunId([5; 16]));

fn parent(binding: EmployeeBinding, requester: &str, run: RunHandle) -> ParentRun {
    ParentRun { agent: "employee-a".into(), employee: PARENT, incarnation: 3, binding, run, lane: Lane::Standard,
        requester: requester.into(), task: Some("own-task".into()) }
}
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()) }
fn request() -> EphemeralRequest {
    EphemeralRequest { instruction: "You review invoices.".into(), context: "Check the March batch.".into(),
        tier: "cheap".into(), tools: vec!["wiki_read".into(), "memory_search".into()] }
}
fn store() -> EphemeralJobs { EphemeralJobs::from_connection(Connection::open_in_memory().unwrap()).unwrap() }

/// The parent run's D8 state, switchable mid-test.
struct Runs(Cell<Result<bool, VerificationError>>);
impl ParentRuns for Runs {
    fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError> {
        assert_eq!((employee, run), (PARENT, RUN), "liveness is asked for the job's own parent run");
        self.0.get()
    }
}
struct Identity(Result<(EmployeeId, u32), VerificationError>);
impl ParentIdentity for Identity {
    fn current(&self, agent: &str) -> Result<(EmployeeId, u32), VerificationError> {
        assert_eq!(agent, "employee-a");
        self.0
    }
}

#[test]
fn spawn_ephemeral_queued_while_parent_holds_capability() {
    let mut jobs = store();
    let job = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
    jobs.queue(&job).unwrap();
    let same = Identity(Ok((PARENT, 3)));
    // D8: while the parent run holds the employee's capability (or its state
    // cannot be read) the child is never started — it stays pending.
    let runs = Runs(Cell::new(Ok(true)));
    assert!(jobs.take_startable(&runs, &same, 1_001).unwrap().is_empty());
    runs.0.set(Err(VerificationError::Unavailable));
    assert!(jobs.take_startable(&runs, &same, 1_002).unwrap().is_empty());
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Pending));
    // After the parent exits it is released exactly once.
    runs.0.set(Ok(false));
    assert_eq!(jobs.take_startable(&runs, &same, 1_003).unwrap(), vec![job.clone()]);
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Released));
    assert!(jobs.take_startable(&runs, &same, 1_004).unwrap().is_empty());
}

#[test]
fn spawn_ephemeral_child_carries_parent_identity() {
    let job = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
    assert_eq!((job.parent.employee, job.parent.incarnation, job.parent.run), (PARENT, 3, RUN));
    assert_eq!(job.parent.binding, shared());
    assert_eq!(job.tools, vec!["wiki_read".to_owned(), "memory_search".to_owned()]);
    let exited = Runs(Cell::new(Ok(false)));
    // The label now holds a restored incarnation or another employee, or the
    // employee was tombstoned: the job is dropped, never run as someone else.
    for current in [Ok((PARENT, 4)), Ok((EmployeeId([9; 16]), 3)), Err(VerificationError::Revoked)] {
        let mut jobs = store();
        let job = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
        jobs.queue(&job).unwrap();
        assert!(jobs.take_startable(&exited, &Identity(current), 1_001).unwrap().is_empty());
        assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Dropped));
    }
    // An unreadable lifecycle keeps it waiting.
    let mut jobs = store();
    let job = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
    jobs.queue(&job).unwrap();
    assert!(jobs.take_startable(&exited, &Identity(Err(VerificationError::Unavailable)), 1_001).unwrap().is_empty());
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Pending));
}

#[test]
fn ephemeral_job_personal_parent_owner_only() {
    let personal = EmployeeBinding::Personal(Text::new("owner".into()).unwrap());
    assert!(EphemeralJob::new(parent(personal.clone(), "owner", RUN), request(), 1).is_ok());
    assert_eq!(EphemeralJob::new(parent(personal, "intruder", RUN), request(), 1), Err(VerificationError::BindingMismatch));
    let mut duplicated = request();
    duplicated.tools.push("wiki_read".into());
    for bad in [EphemeralRequest { tools: vec![], ..request() }, duplicated,
        EphemeralRequest { context: " ".into(), ..request() }] {
        assert_eq!(EphemeralJob::new(parent(shared(), "alice", RUN), bad, 1), Err(VerificationError::Invalid));
    }
}

#[test]
fn ephemeral_jobs_are_bounded_and_expire() {
    let mut jobs = store();
    for _ in 0..MAX_PENDING_PER_RUN {
        jobs.queue(&EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap()).unwrap();
    }
    let over = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
    assert_eq!(jobs.queue(&over), Err(VerificationError::Unavailable));
    assert_eq!(jobs.state(&over.id).unwrap(), None);
    // Never started within the TTL: dropped, even once the parent exits.
    let exited = Runs(Cell::new(Ok(false)));
    assert!(jobs.take_startable(&exited, &Identity(Ok((PARENT, 3))), 1_000 + JOB_TTL_SECONDS).unwrap().is_empty());
    // A row whose stored job names another parent run is dropped.
    let mut jobs = store();
    let job = EphemeralJob::new(parent(shared(), "alice", RUN), request(), 1_000).unwrap();
    jobs.queue(&job).unwrap();
    jobs.db.execute("UPDATE ephemeral_jobs SET run='00' WHERE id=?1", [&job.id]).unwrap();
    assert!(jobs.take_startable(&exited, &Identity(Ok((PARENT, 3))), 1_001).unwrap().is_empty());
    assert_eq!(jobs.state(&job.id).unwrap(), Some(JobState::Dropped));
}
