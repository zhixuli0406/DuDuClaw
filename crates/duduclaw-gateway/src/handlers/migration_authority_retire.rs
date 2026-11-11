//! `EmployeeRetire` proposals on the existing approval card (native Linux
//! isolation v1.81, F6 design r5 §3.12, F6l): the `MigrationAuthorityBackend`
//! beside `EmployeeAuthority`. All logic lives in `crate::isolation_retire`;
//! this adapter only maps its refusals onto the confirmation errors.
//! `disable` is the forward-only compensation read-back (R14): the store's
//! `reject` turns its refusal into `ConfirmationError::ForwardOnly`.
use super::*;
use std::sync::Arc;
use crate::isolation_retire::{ApprovalRule, RetireBackend, RetireError, RetireNotifier, RetireRoot, Step};

pub(crate) struct EmployeeRetireAuthority { backend: RetireBackend }
impl EmployeeRetireAuthority {
    /// `actor`: the live dashboard Admin of this call (the approver of a
    /// Pending card); `rule`: F6c2's second-Admin rule.
    pub(crate) fn new(home: PathBuf, root: Arc<dyn RetireRoot>, rule: Arc<dyn ApprovalRule>,
        notifier: Arc<dyn RetireNotifier>, actor: Option<String>) -> Self {
        Self { backend: RetireBackend::new(home, root, rule, notifier, actor) }
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_backend(backend: RetireBackend) -> Self { Self { backend } }
}
fn error(error: RetireError) -> Error {
    match error {
        RetireError::Unavailable => Error::Storage,
        RetireError::Stale | RetireError::NoApprover => Error::Stale,
        RetireError::Invalid => Error::Invalid,
        RetireError::RootUnavailable | RetireError::RootInconsistent | RetireError::HostBusy => Error::Apply,
        RetireError::ForwardOnly => Error::Compensation,
        _ => Error::Denied,
    }
}
impl MigrationAuthorityBackend for EmployeeRetireAuthority {
    fn supports(&self, category: ConfirmationCategory) -> bool { category == ConfirmationCategory::EmployeeRetire }
    fn validate(&self, item: &ConfirmationItem) -> Result<()> {
        if !self.supports(item.category) { return Err(Error::Denied); }
        self.backend.validate(item).map(|_| ()).map_err(error)
    }
    fn stage(&self, item: &ConfirmationItem) -> Result<()> { self.backend.apply(item, Step::Stage).map_err(error) }
    fn publish(&self, item: &ConfirmationItem) -> Result<()> { self.backend.apply(item, Step::Publish).map_err(error) }
    fn read_back(&self, item: &ConfirmationItem) -> Result<()> { self.backend.apply(item, Step::ReadBack).map_err(error) }
    fn disable(&self, item: &ConfirmationItem) -> Result<()> { self.backend.compensate(item).map_err(error) }
}

/// F6d: the confirmation RPC has one backend slot; this serves Employee
/// items through `EmployeeAuthority` and `EmployeeRetire` items through
/// [`EmployeeRetireAuthority`]. Each item goes to the single backend whose
/// `supports` names its category; none (or more than one) is `Denied`.
pub(crate) struct CompositeBackend { backends: Vec<Arc<dyn MigrationAuthorityBackend>> }
impl CompositeBackend {
    pub(crate) fn new(backends: Vec<Arc<dyn MigrationAuthorityBackend>>) -> Self { Self { backends } }
    fn for_item(&self, category: ConfirmationCategory) -> Result<&Arc<dyn MigrationAuthorityBackend>> {
        let mut found = self.backends.iter().filter(|backend| backend.supports(category));
        match (found.next(), found.next()) {
            (Some(backend), None) => Ok(backend),
            _ => Err(Error::Denied),
        }
    }
}
impl MigrationAuthorityBackend for CompositeBackend {
    /// The Shared-employee readback belongs to the Employee backend only.
    fn readback_shared_employee(&self, agent: &str) -> Result<()> {
        self.for_item(ConfirmationCategory::Employee)?.readback_shared_employee(agent)
    }
    fn supports(&self, category: ConfirmationCategory) -> bool { self.for_item(category).is_ok() }
    fn validate(&self, item: &ConfirmationItem) -> Result<()> { self.for_item(item.category)?.validate(item) }
    fn stage(&self, item: &ConfirmationItem) -> Result<()> { self.for_item(item.category)?.stage(item) }
    fn publish(&self, item: &ConfirmationItem) -> Result<()> { self.for_item(item.category)?.publish(item) }
    fn read_back(&self, item: &ConfirmationItem) -> Result<()> { self.for_item(item.category)?.read_back(item) }
    fn disable(&self, item: &ConfirmationItem) -> Result<()> { self.for_item(item.category)?.disable(item) }
}

/// The production pair for the confirmation RPC: `EmployeeAuthority` and the
/// retire backend over spawnd, F6c2's rule and the notice outbox; `actor` is
/// the live dashboard Admin of the call; `step_up_verified_at` the
/// `verified_at` of the migration step-up proof this call read (`None`
/// without one), from which the rule computes `fresh_step_up` (F6fa).
pub(crate) fn production_backend(home: PathBuf, employee: Arc<dyn MigrationAuthorityBackend>, actor: &str,
    step_up_verified_at: Option<u64>) -> Arc<dyn MigrationAuthorityBackend> {
    use crate::isolation_restore_wiring::{unix_now, OutboxNotifier, RetireLogRule};
    let retire = EmployeeRetireAuthority::new(home, Arc::new(crate::isolation_retire::SpawndRetireRoot),
        Arc::new(RetireLogRule { now: unix_now, step_up_verified_at }), Arc::new(OutboxNotifier), Some(actor.to_owned()));
    Arc::new(CompositeBackend::new(vec![employee, Arc::new(retire)]))
}

// The retire engine reads apply.db through the hardened, Unix-only
// `fs_safe::sqlite::open_existing` (fails closed elsewhere). Unix-only tests.
#[cfg(all(test, unix))]
#[path = "migration_authority_retire_tests.rs"]
mod tests;
