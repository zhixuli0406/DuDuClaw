//! Real Shared employee stores; only the privileged allocator ReadBack is doubled.
use super::*;
use duduclaw_auth::{IsolationAuthoritySnapshot, UserDb, UserRole};
use duduclaw_isolation::{bounded::{List, Text}, capability::Lane};
use duduclaw_migration::{allocation::{self, Receipt}, confirmation::{ApprovalSession,
    ConfirmationError, ConfirmationStore, ProposalApplier, ProposalValidator},
    contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
        ProposalOperationId, ProposalValue}, ids::RequestId};
use rusqlite::Connection;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

struct Accept;
impl ProposalValidator for Accept {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}

struct Root(Receipt);
impl RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, lifecycle::LifecycleError> {
        if self.0.binding != item.binding { return Err(lifecycle::LifecycleError::Unavailable); }
        Ok(self.0.clone())
    }
}

/// Owns fixture authority facts without creating a wiki or other resource grants.
pub struct SharedEmployee {
    home: PathBuf,
    label: String,
    user: String,
    epoch: u64,
    root: Root,
}
impl SharedEmployee {
    pub fn new(home: &Path, label: &str, employee: EmployeeId) -> Self {
        // Match gateway-private installer stores, independent of the test umask.
        std::fs::DirBuilder::new().mode(0o700).create(home.join("migration-authority")).unwrap();
        for name in ["users.db", "migration-authority/apply.db", "migration-confirmations.db"] {
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .open(home.join(name)).unwrap();
        }
        let users = UserDb::new(&home.join("users.db")).unwrap();
        let user = users.create_user("owner@example.invalid", "Owner", "password1", UserRole::Admin).unwrap().id;
        Connection::open(home.join("users.db")).unwrap().execute(
            "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1",
            [&user]).unwrap();
        let current = users.isolation_authority_snapshot(Some(&user), label).unwrap();
        let mut store = ConfirmationStore::from_connection(Connection::open(home.join("migration-confirmations.db")).unwrap()).unwrap();
        let fields = [("employee_id", label), ("disk_bytes", "67108864"),
            ("binding_kind", "shared"), ("tenant_id", current.tenant_id.as_str())].into_iter()
            .map(|(name, value)| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }).collect();
        let staged = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
            ConfirmationCategory::Employee, fields, &Accept).unwrap();
        let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
        store.approve(&[staged.binding], &session, [2; 32], 100, &Accept).unwrap();
        for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
        let item = store.list().unwrap().into_iter().find(|item| item.binding == staged.binding).unwrap();
        assert_eq!(item.status, ConfirmationStatus::Applied);
        let spec = allocation::specification(&item).unwrap();
        let receipt = Receipt { binding: item.binding, label: spec.label, employee, uid: 70_002,
            bytes: spec.bytes, inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
        crate::handlers::save_confirmed_receipt(home, &item, &receipt).unwrap();
        Self { home: home.to_owned(), label: label.into(), user,
            epoch: current.human.unwrap().credential_epoch, root: Root(receipt) }
    }

    pub fn snapshot(&self) -> IsolationAuthoritySnapshot {
        let mut users = Connection::open_with_flags(self.home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        duduclaw_auth::read_isolation_authority(&mut users, Some(&self.user), &self.label).unwrap()
    }

    /// Same issuer and lifecycle rechecks as production, with fixture root I/O.
    pub fn authorize(&self, current: &IsolationAuthoritySnapshot, lane: Lane,
        tools: List<Text<128>, 256>, turn: super::super::HostTurn, expiry: Option<u64>)
        -> Result<super::super::RuntimeAuthorization, VerificationError> {
        let lifecycle = lifecycle::read_lifecycle_with(&self.home, &self.label, &self.root).map_err(lifecycle_refusal)?;
        human_authorization_with(self.home.clone(), self.label.clone(), self.root.0.employee,
            self.user.clone(), self.epoch, lane, current, tools, turn, expiry, &lifecycle, &self.root)
    }
}
