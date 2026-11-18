//! R8d D3: the gateway's start-up entitlement reconcile. Once per process,
//! off the startup path (a blocking task with bounded retries), every
//! confirmed live employee (receipt not disabled, a derivable principal) gets
//! its desired entitlement recomputed from its APPROVED settings (the Applied
//! confirmation item) and sent as `Entitle`. Root answers an identical record
//! without writing, so this is cheap when nothing changed, and it re-entitles
//! after a generation rollover dropped a profile. Each outcome is reported to
//! the per-employee readiness (`runtime_entitlement_missing`).
use super::employee::{entitle_with, planned, AllocationRoot, EntitlementPlanner};
use super::{Error, ConfirmationItem};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::confirmation::EffectGate;
use std::path::Path;
use std::time::{Duration, Instant};

/// Passes of the start-up reconcile (a pass is repeated only while some
/// employee answered retry-later), and the pause between them.
const ATTEMPTS: usize = 10;
const RETRY_EVERY: Duration = Duration::from_secs(30);
/// Granularity at which the pause notices shutdown.
const POLL: Duration = Duration::from_millis(200);
/// Bound of confirmed rows read (more refuses the listing: never a shortened list).
const MAX_EMPLOYEES: usize = 4096;

/// One confirmed employee as the reconcile sees it.
pub(crate) struct Candidate {
    pub(crate) label: String,
    pub(crate) employee: EmployeeId,
    /// Receipt not disabled (not tombstoned, retired or compensated) and a principal derivable.
    pub(crate) live: bool,
    /// The Applied Employee confirmation item (its approved settings); `Err`: unreadable.
    pub(crate) item: Result<ConfirmationItem, ()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome { InForce, RetryLater, Failed }

/// One employee: desired set from the approved item, then `Entitle`.
pub(crate) fn reconcile_one(root: &dyn AllocationRoot, planner: &dyn EntitlementPlanner, gate: &dyn EffectGate, candidate: &Candidate) -> Outcome {
    let Ok(item) = &candidate.item else { return Outcome::RetryLater };
    match planned(planner, item).and_then(|desired| entitle_with(root, gate, item, &desired.entitlement)) {
        Ok(()) => Outcome::InForce,
        Err(Error::Pending) => Outcome::RetryLater,
        Err(_) => Outcome::Failed,
    }
}

/// One pass over `candidates` (live ones only). `true`: some employee
/// answered retry-later and the gate is still open.
pub(crate) fn reconcile_pass(root: &dyn AllocationRoot, planner: &dyn EntitlementPlanner, gate: &dyn EffectGate,
    candidates: &[Candidate]) -> bool {
    let mut retry = false;
    for candidate in candidates.iter().filter(|candidate| candidate.live) {
        if gate.is_closed() { return false; }
        let outcome = reconcile_one(root, planner, gate, candidate);
        crate::isolation_readiness::reasons::record_entitlement(&candidate.label, candidate.employee, outcome == Outcome::InForce);
        match outcome {
            Outcome::InForce => {}
            Outcome::RetryLater => {
                retry = true;
                tracing::warn!(label = %candidate.label, "employee entitlement not confirmed by the isolation service yet; will retry");
            }
            Outcome::Failed => tracing::error!(label = %candidate.label,
                "employee entitlement could not be put in force; its runs stay refused (see readiness: runtime_entitlement_missing)"),
        }
    }
    retry && !gate.is_closed()
}

/// Every confirmed row with its Applied item (live rows only read the item).
fn installed_candidates(home: &Path) -> Result<Vec<Candidate>, ()> {
    use crate::isolation_lifecycle as lifecycle;
    let rows = lifecycle::confirmed_principals(home, MAX_EMPLOYEES).map_err(|_| ())?;
    Ok(rows.into_iter().map(|row| {
        let live = !row.disabled && row.binding.is_some();
        let item = if !live { Err(()) } else {
            lifecycle::confirmed_receipt(home, &row.label).ok().flatten().filter(|receipt| receipt.employee == row.employee)
                .ok_or(()).and_then(|receipt| lifecycle::applied_item(home, &receipt.binding).map_err(|_| ()))
        };
        Candidate { label: row.label, employee: row.employee, live, item }
    }).collect())
}

/// `false` when the gate closed before `delay` elapsed.
fn pause(gate: &dyn EffectGate, delay: Duration) -> bool {
    let until = Instant::now() + delay;
    loop {
        if gate.is_closed() { return false; }
        let now = Instant::now();
        if now >= until { return true; }
        std::thread::sleep((until - now).min(POLL));
    }
}

/// Bounded passes (blocking; never on an async worker thread).
pub(crate) fn reconcile_with_retries(home: &Path, root: &dyn AllocationRoot, planner: &dyn EntitlementPlanner, gate: &dyn EffectGate,
    attempts: usize, every: Duration) {
    for attempt in 0..attempts {
        if gate.is_closed() { return; }
        let retry = match installed_candidates(home) {
            Ok(candidates) => reconcile_pass(root, planner, gate, &candidates),
            Err(()) => { tracing::warn!("confirmed employees unreadable; entitlement reconcile will retry"); true }
        };
        if !retry || attempt + 1 == attempts || !pause(gate, every) { break; }
    }
}

/// Starts the one start-up reconcile on the blocking pool (does not block
/// boot). Production collaborators: spawnd's allocation socket, the fresh
/// gateway projection, the process-wide migration effect gate.
pub(crate) fn spawn_startup_reconcile(home: std::path::PathBuf) {
    drop(tokio::task::spawn_blocking(move || reconcile_with_retries(&home, &super::employee::SpawndAllocationRoot,
        &super::employee::InstalledPlanner, super::shutdown_gate(), ATTEMPTS, RETRY_EVERY)));
}

#[cfg(all(test, target_os = "linux"))]
#[path = "migration_authority_entitlement_tests.rs"]
mod tests;
