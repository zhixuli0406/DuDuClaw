//! H13b: production [`ParentRuns`] — "has the parent run exited?" — from the
//! run state the gateway already tracks for this epoch.
//!
//! The product authority (`isolation_mcp::authority::GatewayAuthority`) binds
//! every protected run before D issues its capability (`bind_run`) and drops
//! the association when the run is revoked: Cancel, terminal status, control
//! disconnect, a failed prepare or the launch guard's drop (`revoke_run`).
//! Both points report here, so the ledger knows, per run handle, whether the
//! run still holds the employee's capability (D8).
//!
//! Fail closed: a run this gateway epoch never bound (an earlier epoch, or a
//! forged handle) is `Unavailable`, so its job keeps waiting and expires
//! after its TTL rather than starting on a guess. A known exited run still
//! counts as holding while ANOTHER run of the same employee is bound (D8:
//! one capability-holding run per employee; a released child waits for it).
//!
//! H13c: the ledger only sees runs from `bind_run` on, so a run that A has
//! reserved but D has not bound yet is invisible here. The job store covers
//! the child's own window (a `released`, not yet admitted job holds its
//! employee; one release per employee per tick: `take_startable`), and
//! spawnd's admission coordinator is the authoritative per-employee slot: a
//! second run of the same employee is queued behind the first
//! (`deferred_until_current_run_ends`) whatever the gateway releases.
use std::{collections::{BTreeMap, VecDeque}, path::PathBuf, sync::Mutex};
use duduclaw_isolation::{execution::RunHandle, ids::{EmployeeId, RunId}};
use duduclaw_mcp_boundary::VerificationError;
use crate::isolation_ephemeral::ParentRuns;

/// Exited runs remembered per epoch (oldest forgotten first; a forgotten run
/// is unknown again, i.e. its job waits — never a start).
pub const MAX_EXITED: usize = 4096;

/// Bound and exited runs of this gateway epoch.
#[derive(Debug, Default)]
pub struct RunLedger {
    live: BTreeMap<RunId, EmployeeId>,
    exited: VecDeque<(RunId, EmployeeId)>,
    /// H13d: bound runs that are ephemeral children: run → (home, job id).
    children: BTreeMap<RunId, (PathBuf, String)>,
}

impl RunLedger {
    pub const fn new() -> Self { Self { live: BTreeMap::new(), exited: VecDeque::new(), children: BTreeMap::new() } }

    /// The bound `run` is the child of job `job` (store under `home`).
    pub fn child_bound(&mut self, run: RunHandle, home: PathBuf, job: String) {
        self.children.insert(run.0, (home, job));
    }

    /// The (home, job) of a child run, once: forgotten when taken.
    pub fn take_child(&mut self, run: RunHandle) -> Option<(PathBuf, String)> { self.children.remove(&run.0) }

    /// The run now holds `employee`'s capability.
    pub fn bound(&mut self, run: RunHandle, employee: EmployeeId) {
        self.exited.retain(|(id, _)| *id != run.0);
        self.live.insert(run.0, employee);
    }

    /// The run's capability was revoked. Idempotent; an unbound run is ignored.
    pub fn exited(&mut self, run: RunHandle) {
        if let Some(employee) = self.live.remove(&run.0) {
            if self.exited.len() >= MAX_EXITED { self.exited.pop_front(); }
            self.exited.push_back((run.0, employee));
        }
    }

    /// `Ok(true)` while the run (or another run of the same employee) holds
    /// the capability, `Ok(false)` once it exited and the employee has no
    /// bound run; another employee's run is `BindingMismatch`; unknown is
    /// `Unavailable`.
    pub fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError> {
        if let Some(owner) = self.live.get(&run.0) {
            return if *owner == employee { Ok(true) } else { Err(VerificationError::BindingMismatch) };
        }
        match self.exited.iter().rev().find(|(id, _)| *id == run.0) {
            Some((_, owner)) if *owner == employee => Ok(self.live.values().any(|bound| *bound == employee)),
            Some(_) => Err(VerificationError::BindingMismatch),
            None => Err(VerificationError::Unavailable),
        }
    }
}

impl ParentRuns for RunLedger {
    fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError> {
        RunLedger::holds_capability(self, employee, run)
    }
}

static LEDGER: Mutex<RunLedger> = Mutex::new(RunLedger::new());

/// Called by the product authority after it bound `run` to `employee`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn record_bound(run: RunHandle, employee: EmployeeId) {
    LEDGER.lock().unwrap_or_else(|error| error.into_inner()).bound(run, employee);
}

/// Called by the product authority whenever it revokes `run`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn record_exited(run: RunHandle) {
    let child = {
        let mut ledger = LEDGER.lock().unwrap_or_else(|error| error.into_inner());
        ledger.exited(run);
        ledger.take_child(run)
    };
    // H13d: the child's run ended: its job is terminal (best effort; a job
    // that cannot be marked stops holding its slot at its TTL).
    if let Some((home, job)) = child {
        if crate::isolation_ephemeral::EphemeralJobs::open(&home).and_then(|jobs| jobs.finish(&job)).is_err() {
            tracing::warn!(job = job.as_str(), "ephemeral job: finish not recorded; it expires with its TTL");
        }
    }
}

/// Called by the product authority after it bound `run` of an ephemeral
/// child of job `job` (H13d).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn record_child_bound(run: RunHandle, home: PathBuf, job: String) {
    LEDGER.lock().unwrap_or_else(|error| error.into_inner()).child_bound(run, home, job);
}

/// The production [`ParentRuns`] over this epoch's ledger. A poisoned ledger
/// is `Unavailable` (keep waiting).
pub struct LiveRuns;
impl ParentRuns for LiveRuns {
    fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError> {
        LEDGER.lock().map_err(|_| VerificationError::Unavailable)?.holds_capability(employee, run)
    }
}
