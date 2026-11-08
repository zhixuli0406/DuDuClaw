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

#[cfg(test)]
#[path = "migration_authority_retire_tests.rs"]
mod tests;
