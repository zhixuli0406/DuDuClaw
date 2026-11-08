//! Gateway read model of one confirmed protected employee (native Linux
//! isolation v1.81, task A2).
//!
//! An [`EmployeeLifecycle`] is assembled on every read from four independent
//! records, and any disagreement refuses:
//! - the confirmed index row in `migration-authority/apply.db` (label →
//!   root receipt copy + the tenant pinned at first confirmation);
//! - the Applied Employee proposal in `migration-confirmations.db`, whose
//!   digest-covered `binding_kind`/`owner_id`/`tenant_id` must equal the
//!   receipt's `principal_binding`;
//! - the root allocator's live ReadBack of that proposal (UID, quota,
//!   employee id and principal must equal the stored copy);
//! - users.db: the installation tenant must equal the pin, and a Personal
//!   employee's `isolation_personal_agents` mirror must name the receipt
//!   owner, who must still be an Active user.
//!
//! The principal is taken only from the root receipt. A legacy (pre-v1.81)
//! receipt without `principal_binding` is Shared(pinned tenant) and never
//! Personal; it is refused when the pin is missing or differs.
//!
//! This module also owns the users.db Personal mirror writes made by the
//! employee apply path ([`record_personal_mirror`], [`remove_personal_mirror`])
//! and the admin handover path ([`tombstone_personal_employee`]). Everything
//! here is blocking SQLite/IPC and must run on the blocking pool.
use std::path::Path;

use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalBinding};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

const APPLY_DB: &str = "migration-authority/apply.db";
const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
const USERS_DB: &str = "users.db";

/// Why a lifecycle read or write was refused. Every variant is a refusal;
/// none is a hint that a weaker check may be used instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    /// A store or the root allocator could not be read or written.
    Unavailable,
    /// No confirmed employee, wrong tenant, ineligible owner, or a record
    /// combination that can never be valid.
    Denied,
    /// The records disagree (root readback, proposal, mirror owner): the
    /// caller's view is out of date and must be re-established.
    Stale,
    /// The employee was disabled or tombstoned, or replaced by a new
    /// employee under the same label.
    Tombstoned,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "employee lifecycle unavailable",
            Self::Denied => "employee lifecycle denied",
            Self::Stale => "employee lifecycle stale",
            Self::Tombstoned => "employee tombstoned",
        })
    }
}

impl std::error::Error for LifecycleError {}

/// One confirmed, live protected employee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmployeeLifecycle {
    /// Immutable root registry identity; never reused for a recreated label.
    pub(crate) employee: EmployeeId,
    pub(crate) label: String,
    /// The confirmed Employee proposal this allocation was made for.
    pub(crate) allocation: ProposalBinding,
    pub(crate) uid: u32,
    /// Principal from the root receipt (legacy receipts: Shared(pinned tenant)).
    pub(crate) binding: EmployeeBinding,
}

impl EmployeeLifecycle {
    /// The Personal owner's user id, `None` for a Shared employee.
    pub(crate) fn personal_owner(&self) -> Option<&str> {
        match &self.binding {
            EmployeeBinding::Personal(owner) => Some(owner.as_str()),
            EmployeeBinding::Shared(_) => None,
        }
    }
}

/// Root allocator ReadBack. Production uses [`RootAllocator`]; tests inject
/// a fixture because the allocation socket exists only on a protected host.
pub(crate) trait RootAllocation {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError>;
}

/// The root `duduclaw-spawnd` allocation socket (Linux only).
pub(crate) struct RootAllocator;

impl RootAllocation for RootAllocator {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        #[cfg(target_os = "linux")]
        {
            use duduclaw_migration::allocation::{Action, Request};
            let nonce = duduclaw_migration::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
            allocation::transport::request(&Request { schema: 1, nonce, action: Action::ReadBack, item: item.clone() })
                .map_err(|_| LifecycleError::Unavailable)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = item;
            Err(LifecycleError::Unavailable)
        }
    }
}

/// Read the confirmed employee currently holding `label`.
pub(crate) fn read_lifecycle(home: &Path, label: &str) -> Result<EmployeeLifecycle, LifecycleError> {
    read_lifecycle_with(home, label, &RootAllocator)
}

pub(crate) fn read_lifecycle_with(home: &Path, label: &str, root: &dyn RootAllocation)
    -> Result<EmployeeLifecycle, LifecycleError> {
    resolve(home, label, root).map(|(_, lifecycle)| lifecycle)
}

/// Re-read and require the very same employee and principal: a recreated
/// employee under the same label is `Tombstoned` for holders of the old one,
/// any other difference is `Stale`.
pub(crate) fn recheck_with(home: &Path, expected: &EmployeeLifecycle, root: &dyn RootAllocation)
    -> Result<EmployeeLifecycle, LifecycleError> {
    let current = read_lifecycle_with(home, &expected.label, root)?;
    if current.employee != expected.employee { return Err(LifecycleError::Tombstoned); }
    if current != *expected { return Err(LifecycleError::Stale); }
    Ok(current)
}

/// Full resolution, also returning the live root receipt for the existing
/// `resolve_confirmed_employee` callers.
pub(crate) fn resolve(home: &Path, label: &str, root: &dyn RootAllocation)
    -> Result<(Receipt, EmployeeLifecycle), LifecycleError> {
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    let (prior, pinned) = confirmed_row(home, label)?.ok_or(Denied)?;
    if prior.label != label { return Err(Denied); }
    if prior.disabled { return Err(Tombstoned); }
    // Legacy rows confirmed before the tenant pin existed never acquire one.
    let pinned = pinned.ok_or(Denied)?;
    let binding = principal(&prior, &pinned)?;
    let item = applied_item(home, &prior.binding)?;
    let spec = allocation::specification(&item).map_err(|_| Denied)?;
    if spec.label != label { return Err(Denied); }
    // The receipt principal is the confirmed, digest-covered proposal value.
    if spec.binding != prior.principal_binding { return Err(Stale); }
    let current = root.read_back(&item)?;
    if current.disabled { return Err(Tombstoned); }
    if current.employee != prior.employee || current.uid != prior.uid
        || current.binding != prior.binding || current.label != prior.label
        || current.bytes != prior.bytes || current.inodes != prior.inodes
        || current.principal_binding != prior.principal_binding {
        return Err(Stale);
    }
    // users.db last, so it is no older than the root readback.
    check_users(home, label, &pinned, &binding)?;
    let lifecycle = EmployeeLifecycle {
        employee: current.employee, label: current.label.clone(), allocation: current.binding,
        uid: current.uid, binding,
    };
    Ok((current, lifecycle))
}

/// Proposal-time check of the Employee proposal's principal fields
/// (`binding_kind`/`owner_id`/`tenant_id`, already parsed by
/// `allocation::specification`). Read-only; the apply path re-checks.
/// - Personal: the owner is an Active user who has logged in and has no
///   forced password change; no other user is bound to the label and no
///   other owner is mirrored for it.
/// - Shared: the tenant equals the installation tenant, which a live
///   confirmed row for the label must also be pinned to; no Personal mirror.
/// - Legacy (`None`): only where no Personal mirror exists.
pub(crate) fn validate_proposed_binding(home: &Path, label: &str, binding: Option<&EmployeeBinding>)
    -> Result<(), LifecycleError> {
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    let existing = confirmed_row(home, label)?;
    let mut users = open_read_only(&home.join(USERS_DB))?;
    let snapshot = duduclaw_auth::read_isolation_authority(&mut users, None, label).map_err(|_| Unavailable)?;
    if let Some((prior, pin)) = &existing {
        // A tombstoned row never constrains a recreate; a live one keeps its pin.
        if !prior.disabled && pin.as_deref() != Some(snapshot.tenant_id.as_str()) { return Err(Denied); }
    }
    match binding {
        None | Some(EmployeeBinding::Shared(_)) => {
            if snapshot.personal_owner.is_some() { return Err(Denied); }
            if let Some(EmployeeBinding::Shared(tenant)) = binding {
                if tenant.as_str() != snapshot.tenant_id { return Err(Denied); }
            }
        }
        Some(EmployeeBinding::Personal(owner)) => {
            let owner = owner.as_str();
            if !owner_eligible(&users, owner)? { return Err(Denied); }
            if snapshot.personal_owner.as_deref().is_some_and(|recorded| recorded != owner) { return Err(Denied); }
            let others: bool = users.query_row(
                "SELECT EXISTS(SELECT 1 FROM user_agent_bindings WHERE agent_name=?1 AND user_id!=?2)",
                params![label, owner], |r| r.get(0)).map_err(|_| Unavailable)?;
            if others { return Err(Denied); }
        }
    }
    Ok(())
}

/// Apply path: record the Personal mirror for a confirmed allocation, after
/// re-checking the owner in the same users.db transaction. `Ok(false)` is an
/// idempotent re-apply. Any refusal must refuse the apply step.
pub(crate) fn record_personal_mirror(home: &Path, label: &str, owner: &str) -> Result<bool, LifecycleError> {
    let mut users = open_read_write(&home.join(USERS_DB))?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| LifecycleError::Unavailable)?;
    if !owner_eligible(&tx, owner)? { return Err(LifecycleError::Denied); }
    let inserted = duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, label, owner).map_err(mirror_error)?;
    tx.commit().map_err(|_| LifecycleError::Unavailable)?;
    Ok(inserted)
}

/// Apply/compensation and tombstone path only: remove the Personal mirror
/// (and, per A3, every binding of the agent). `Ok(false)` when absent;
/// `Stale` when another owner is recorded (left untouched).
pub(crate) fn remove_personal_mirror(home: &Path, label: &str, owner: &str) -> Result<bool, LifecycleError> {
    let mut users = open_read_write(&home.join(USERS_DB))?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| LifecycleError::Unavailable)?;
    let removed = duduclaw_auth::db::remove_personal_agent_in_transaction(&tx, label, owner).map_err(mirror_error)?;
    tx.commit().map_err(|_| LifecycleError::Unavailable)?;
    Ok(removed)
}

/// Admin handover of a Personal employee (e.g. before offboarding its
/// owner with a transfer). The caller must already have verified a live
/// Admin with step-up; this function performs no authorization.
///
/// 1. In one apply.db transaction, the confirmed row of exactly `employee`
///    is marked disabled. From then on every lifecycle read is `Tombstoned`
///    and no later apply step can revive it (`checked_tenant` refuses).
/// 2. The users.db mirror and all bindings of the agent are removed.
///
/// The two stores cannot share a transaction; the order fails closed: if
/// step 2 fails the employee is already unusable and the lingering mirror
/// still blocks every other binding. Retrying is idempotent.
///
/// Not done here: root-side disable of an Applied allocation (UID, storage)
/// has no allocator action yet; the root resources stay reserved.
pub(crate) fn tombstone_personal_employee(home: &Path, label: &str, employee: EmployeeId) -> Result<(), LifecycleError> {
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    let owner = {
        let mut db = open_read_write(&home.join(APPLY_DB))?;
        db.execute_batch("PRAGMA synchronous=FULL;").map_err(|_| Unavailable)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| Unavailable)?;
        let encoded: String = tx.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [label], |r| r.get(0))
            .optional().map_err(|_| Unavailable)?.ok_or(Denied)?;
        let prior: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
        if prior.label != label { return Err(Denied); }
        if prior.employee != employee { return Err(Stale); }
        let Some(EmployeeBinding::Personal(owner)) = prior.principal_binding.clone() else { return Err(Denied) };
        if !prior.disabled {
            let mut dead = prior;
            dead.disabled = true;
            let changed = tx.execute("UPDATE confirmed_employees SET receipt=?1 WHERE label=?2 AND receipt=?3",
                params![serde_json::to_string(&dead).map_err(|_| Unavailable)?, label, encoded]).map_err(|_| Unavailable)?;
            if changed != 1 { return Err(Stale); }
        }
        tx.commit().map_err(|_| Unavailable)?;
        owner
    };
    remove_personal_mirror(home, label, owner.as_str()).map(|_| ())
}

fn valid_label(label: &str) -> bool {
    duduclaw_core::is_valid_agent_id(label) && !duduclaw_core::is_reserved_agent_id(label)
        && label != duduclaw_core::UNTRUSTED_AGENT_ID
}

fn principal(prior: &Receipt, pinned: &str) -> Result<EmployeeBinding, LifecycleError> {
    match &prior.principal_binding {
        // Legacy: Shared in the pinned tenant only, never Personal.
        None => Text::new(pinned.to_owned()).map(EmployeeBinding::Shared).map_err(|_| LifecycleError::Denied),
        Some(EmployeeBinding::Shared(tenant)) if tenant.as_str() == pinned => Ok(EmployeeBinding::Shared(tenant.clone())),
        Some(EmployeeBinding::Shared(_)) => Err(LifecycleError::Denied),
        Some(personal @ EmployeeBinding::Personal(_)) => Ok(personal.clone()),
    }
}

/// `None` when nothing was ever confirmed for the label (including before
/// the table exists); a corrupt row or unreadable store is `Unavailable`.
fn confirmed_row(home: &Path, label: &str) -> Result<Option<(Receipt, Option<String>)>, LifecycleError> {
    let db = open_read_only(&home.join(APPLY_DB))?;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| LifecycleError::Unavailable)?;
    if !table { return Ok(None); }
    let row: Option<(String, Option<String>)> = db.query_row(
        "SELECT receipt,tenant_id FROM confirmed_employees WHERE label=?1", [label], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional().map_err(|_| LifecycleError::Unavailable)?;
    let Some((encoded, tenant)) = row else { return Ok(None) };
    let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| LifecycleError::Unavailable)?;
    if let Some(binding) = &receipt.principal_binding {
        allocation::validate_principal_binding(binding).map_err(|_| LifecycleError::Unavailable)?;
    }
    Ok(Some((receipt, tenant)))
}

fn applied_item(home: &Path, binding: &ProposalBinding) -> Result<ConfirmationItem, LifecycleError> {
    let store = duduclaw_migration::confirmation::ConfirmationStore::open_read_only(&home.join(CONFIRMATIONS_DB))
        .map_err(|_| LifecycleError::Unavailable)?;
    store.list().map_err(|_| LifecycleError::Unavailable)?.into_iter()
        .find(|item| item.binding == *binding && item.category == ConfirmationCategory::Employee
            && item.status == ConfirmationStatus::Applied)
        .ok_or(LifecycleError::Denied)
}

fn check_users(home: &Path, label: &str, pinned: &str, binding: &EmployeeBinding) -> Result<(), LifecycleError> {
    let mut users = open_read_only(&home.join(USERS_DB))?;
    let snapshot = duduclaw_auth::read_isolation_authority(&mut users, None, label)
        .map_err(|_| LifecycleError::Unavailable)?;
    // A replaced users database cannot transfer an employee to a new tenant.
    if snapshot.tenant_id != pinned { return Err(LifecycleError::Denied); }
    match binding {
        EmployeeBinding::Shared(_) => {
            if snapshot.personal_owner.is_some() { return Err(LifecycleError::Denied); }
        }
        EmployeeBinding::Personal(owner) => {
            // A missing or different mirror owner means the users.db view was
            // reassigned or removed outside this employee's lifecycle.
            if snapshot.personal_owner.as_deref() != Some(owner.as_str()) { return Err(LifecycleError::Stale); }
            let status: Option<String> = users.query_row("SELECT status FROM users WHERE id=?1", [owner.as_str()], |r| r.get(0))
                .optional().map_err(|_| LifecycleError::Unavailable)?;
            // An offboarded or suspended owner's employee waits for admin handover.
            if status.as_deref() != Some("active") { return Err(LifecycleError::Denied); }
        }
    }
    Ok(())
}

/// Same rule as the Shared binding restoration
/// (`migration_authority_bindings.rs`): Active, logged in, no forced
/// password change.
fn owner_eligible(conn: &Connection, owner: &str) -> Result<bool, LifecycleError> {
    let row: Option<(String, bool, Option<String>)> = conn.query_row(
        "SELECT status,must_change_password,last_login FROM users WHERE id=?1", [owner],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|_| LifecycleError::Unavailable)?;
    Ok(row.is_some_and(|(status, must_change, last_login)|
        status == "active" && !must_change && last_login.is_some_and(|login| !login.is_empty())))
}

fn mirror_error(error: duduclaw_auth::db::PersonalAgentError) -> LifecycleError {
    use duduclaw_auth::db::PersonalAgentError as E;
    match error {
        E::Store(_) => LifecycleError::Unavailable,
        E::OwnerConflict => LifecycleError::Stale,
        E::BindingConflict | E::InvalidAgent | E::UnknownOwner => LifecycleError::Denied,
    }
}

fn regular(path: &Path) -> Result<(), LifecycleError> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| LifecycleError::Unavailable)?;
    if !meta.file_type().is_file() { return Err(LifecycleError::Unavailable); }
    Ok(())
}

fn open_read_only(path: &Path) -> Result<Connection, LifecycleError> {
    regular(path)?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| LifecycleError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| LifecycleError::Unavailable)?;
    Ok(db)
}

fn open_read_write(path: &Path) -> Result<Connection, LifecycleError> {
    regular(path)?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| LifecycleError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| LifecycleError::Unavailable)?;
    Ok(db)
}

#[cfg(test)]
#[path = "isolation_lifecycle_tests.rs"]
mod tests;
