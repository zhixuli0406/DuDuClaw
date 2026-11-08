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
use std::{collections::{BTreeMap, VecDeque}, sync::Mutex};
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
}

impl RunLedger {
    pub const fn new() -> Self { Self { live: BTreeMap::new(), exited: VecDeque::new() } }

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
    LEDGER.lock().unwrap_or_else(|error| error.into_inner()).exited(run);
}

/// The production [`ParentRuns`] over this epoch's ledger. A poisoned ledger
/// is `Unavailable` (keep waiting).
pub struct LiveRuns;
impl ParentRuns for LiveRuns {
    fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError> {
        LEDGER.lock().map_err(|_| VerificationError::Unavailable)?.holds_capability(employee, run)
    }
}
