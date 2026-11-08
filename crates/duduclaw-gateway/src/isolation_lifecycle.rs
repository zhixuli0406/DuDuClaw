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
//!   owner, who must still be eligible ([`owner_eligible`]: Active, logged
//!   in, no forced password change) — all read in one read transaction.
//!
//! All three stores are opened only through [`open_store`] (the hardened
//! `fs_safe::sqlite::open_existing`); a symlinked, hardlinked, swapped,
//! foreign-owned or group/other-writable store or sidecar is `Unavailable`.
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
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::fs_safe::sqlite::Access;

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
    let mut connection = open_store(home, USERS_DB, Access::ReadOnly)?;
    // Tenant, mirror, owner row and other bindings: one read snapshot.
    let users = connection.transaction().map_err(|_| Unavailable)?;
    let snapshot = duduclaw_auth::read_isolation_authority_in(&users, None, label).map_err(|_| Unavailable)?;
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
    users.finish().map_err(|_| Unavailable)?;
    Ok(())
}

/// C5 legacy proof: whether `user` is a users.db row (any status). The
/// legacy connector import proposes a Personal route only for an owner this
/// returns `true` for; an unreadable users.db is `Unavailable` (refuse).
pub(crate) fn user_exists(home: &Path, user: &str) -> Result<bool, LifecycleError> {
    let connection = open_store(home, USERS_DB, Access::ReadOnly)?;
    connection.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1)", [user], |r| r.get(0))
        .map_err(|_| LifecycleError::Unavailable)
}

/// C5 apply path: the binding of a migrated connector route, from one
/// users.db read transaction. `Some(owner)`: the owner must be eligible
/// ([`owner_eligible`]: Active, logged in, no forced password change), same
/// rule as C3 provisioning. `None`: Shared in the installation tenant; no
/// request field can name a tenant.
pub(crate) fn connector_route_binding(home: &Path, owner: Option<&str>) -> Result<EmployeeBinding, LifecycleError> {
    use LifecycleError::*;
    let mut connection = open_store(home, USERS_DB, Access::ReadOnly)?;
    let users = connection.transaction().map_err(|_| Unavailable)?;
    let binding = match owner {
        Some(owner) => {
            if !owner_eligible(&users, owner)? { return Err(Denied); }
            EmployeeBinding::Personal(Text::new(owner.to_owned()).map_err(|_| Denied)?)
        }
        None => {
            let tenant = duduclaw_auth::isolation::read_isolation_tenant(&users).map_err(|_| Unavailable)?;
            EmployeeBinding::Shared(Text::new(tenant).map_err(|_| Unavailable)?)
        }
    };
    users.finish().map_err(|_| Unavailable)?;
    Ok(binding)
}

/// Apply path: record the Personal mirror for a confirmed allocation, after
/// re-checking the owner in the same users.db transaction, and bind the owner
/// to the employee with Owner access in that same transaction (A4), so the
/// employee has exactly one member. `Ok(false)` is an idempotent re-apply of
/// the mirror. Any refusal must refuse the apply step.
pub(crate) fn record_personal_mirror(home: &Path, label: &str, owner: &str) -> Result<bool, LifecycleError> {
    let mut users = open_store(home, USERS_DB, Access::ReadWrite)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| LifecycleError::Unavailable)?;
    if !owner_eligible(&tx, owner)? { return Err(LifecycleError::Denied); }
    let inserted = duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, label, owner).map_err(mirror_error)?;
    duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, label, owner).map_err(mirror_error)?;
    tx.commit().map_err(|_| LifecycleError::Unavailable)?;
    Ok(inserted)
}

/// Apply/compensation and tombstone path only: remove the Personal mirror
/// (and, per A3, every binding of the agent). `Ok(false)` when absent;
/// `Stale` when another owner is recorded (left untouched).
pub(crate) fn remove_personal_mirror(home: &Path, label: &str, owner: &str) -> Result<bool, LifecycleError> {
    let mut users = open_store(home, USERS_DB, Access::ReadWrite)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| LifecycleError::Unavailable)?;
    let removed = duduclaw_auth::db::remove_personal_agent_in_transaction(&tx, label, owner).map_err(mirror_error)?;
    tx.commit().map_err(|_| LifecycleError::Unavailable)?;
    Ok(removed)
}

/// Admin handover of a Personal employee (e.g. before offboarding its
/// owner with a transfer). The caller must already have verified a live
/// Admin with step-up; this function performs no authorization.
///
/// 1. In one apply.db transaction (writer lock held throughout), the
///    employee's `agents/<label>` directory is first moved into
///    [`QUARANTINE_DIR`]`/<employee id>-<label>` (0700) by an atomic
///    same-filesystem rename (SR2 finding 4); a failed move refuses the
///    whole tombstone and the row stays live. Then the confirmed row of
///    exactly `employee` is marked disabled and the proposal flow's creation
///    marker for the label is deleted. From then on every lifecycle read is
///    `Tombstoned`, no later apply step can revive it (`checked_tenant`
///    refuses), and a later Personal proposal for the label never adopts a
///    directory: the former owner's data never passes to a new owner and no
///    directory is reused across employee ids.
/// 2. The users.db mirror and all bindings of the agent are removed.
///
/// The two stores cannot share a transaction; the order fails closed: if
/// step 2 fails the employee is already unusable and the lingering mirror
/// still blocks every other binding. Retrying is idempotent (an already
/// quarantined directory is not moved again; an already disabled row never
/// moves a directory that appeared under the label afterwards).
///
/// Not done here: root-side disable of an Applied allocation (UID, storage)
/// has no allocator action yet; the root resources stay reserved.
pub(crate) fn tombstone_personal_employee(home: &Path, label: &str, employee: EmployeeId) -> Result<(), LifecycleError> {
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    let owner = {
        let mut db = open_store(home, APPLY_DB, Access::ReadWrite)?;
        db.execute_batch("PRAGMA synchronous=FULL;").map_err(|_| Unavailable)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| Unavailable)?;
        let encoded: String = tx.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [label], |r| r.get(0))
            .optional().map_err(|_| Unavailable)?.ok_or(Denied)?;
        let prior: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
        if prior.label != label { return Err(Denied); }
        if prior.employee != employee { return Err(Stale); }
        let Some(EmployeeBinding::Personal(owner)) = prior.principal_binding.clone() else { return Err(Denied) };
        if !prior.disabled {
            quarantine_agent_dir(home, label, employee)?;
            let mut dead = prior;
            dead.disabled = true;
            let changed = tx.execute("UPDATE confirmed_employees SET receipt=?1 WHERE label=?2 AND receipt=?3",
                params![serde_json::to_string(&dead).map_err(|_| Unavailable)?, label, encoded]).map_err(|_| Unavailable)?;
            if changed != 1 { return Err(Stale); }
        }
        let markers: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_dir_markers')",
            [], |r| r.get(0)).map_err(|_| Unavailable)?;
        if markers {
            tx.execute("DELETE FROM employee_dir_markers WHERE label=?1", [label]).map_err(|_| Unavailable)?;
        }
        tx.commit().map_err(|_| Unavailable)?;
        owner
    };
    remove_personal_mirror(home, label, owner.as_str()).map(|_| ())
}

/// Private quarantine for tombstoned Personal employees' directories,
/// directly in the DuDuClaw home (same filesystem as `agents/`).
pub(crate) const QUARANTINE_DIR: &str = "isolation-quarantine";

/// Where the tombstone of `employee` puts its `agents/<label>` directory.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn quarantine_path(home: &Path, label: &str, employee: EmployeeId) -> std::path::PathBuf {
    home.join(QUARANTINE_DIR).join(quarantine_name(label, employee))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn quarantine_name(label: &str, employee: EmployeeId) -> String {
    format!("{}-{label}", hex::encode(employee.0))
}

/// SR2 finding 4 (round 3, N5: descriptor-relative, no check-then-rename):
/// move `agents/<label>` (whatever it is; the rename never follows a
/// symlink) to [`quarantine_path`] with one
/// [`crate::fs_safe::SafeDir::rename_noreplace`] (`renameat2(RENAME_NOREPLACE)`)
/// between pinned descriptors of `agents/` and the quarantine root, both
/// opened without following any symlink from `/`. Absent `agents/` or
/// source: nothing to move (never created, or moved by an earlier attempt).
/// The quarantine root must be a real directory owned by this uid with no
/// group/other access (checked on its descriptor); an existing target is
/// refused by the kernel, never overwritten (not even an empty directory);
/// a cross-filesystem or any other failure is `Unavailable` (fail closed).
fn quarantine_agent_dir(home: &Path, label: &str, employee: EmployeeId) -> Result<(), LifecycleError> {
    use LifecycleError::Unavailable;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use crate::fs_safe::{EntryKind, SafeDir};
        let home_dir = SafeDir::open_nofollow(home, false).map_err(|_| Unavailable)?;
        let Some(agents) = home_dir.child_dir("agents", false).map_err(|_| Unavailable)? else { return Ok(()) };
        if agents.entry(label).map_err(|_| Unavailable)?.is_none() { return Ok(()); }
        let root = home_dir.child_dir(QUARANTINE_DIR, true).map_err(|_| Unavailable)?.ok_or(Unavailable)?;
        let metadata = root.metadata().map_err(|_| Unavailable)?;
        if !metadata.is_dir() || metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(Unavailable);
        }
        let target = quarantine_name(label, employee);
        // The kernel refuses an existing target atomically; there is no
        // separate existence check to race.
        agents.rename_noreplace(label, &root, &target).map_err(|_| Unavailable)?;
        // The moved entry itself, by descriptor: never chmod through a symlink.
        if root.entry(&target).map_err(|_| Unavailable)?.is_some_and(|entry| entry.kind == EntryKind::Dir) {
            root.child_dir(&target, false).map_err(|_| Unavailable)?.ok_or(Unavailable)?
                .set_mode(0o700).map_err(|_| Unavailable)?;
        }
        tracing::info!(agent = label, "tombstoned personal employee directory moved to quarantine");
        Ok(())
    }
    #[cfg(not(unix))]
    { let _ = (home, label, employee); Err(Unavailable) }
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
    let db = open_store(home, APPLY_DB, Access::ReadOnly)?;
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
    let store = duduclaw_migration::confirmation::ConfirmationStore::from_read_only_connection(
        open_store(home, CONFIRMATIONS_DB, Access::ReadOnly)?).map_err(|_| LifecycleError::Unavailable)?;
    store.list().map_err(|_| LifecycleError::Unavailable)?.into_iter()
        .find(|item| item.binding == *binding && item.category == ConfirmationCategory::Employee
            && item.status == ConfirmationStatus::Applied)
        .ok_or(LifecycleError::Denied)
}

/// Use-time users.db check, run on every read and recheck. The tenant, the
/// Personal mirror and the owner's account row (status, forced password
/// change, login) are read in ONE read transaction, i.e. one consistent
/// snapshot: a concurrent owner change is either wholly before or wholly
/// after this check, never half-observed.
fn check_users(home: &Path, label: &str, pinned: &str, binding: &EmployeeBinding) -> Result<(), LifecycleError> {
    let mut users = open_store(home, USERS_DB, Access::ReadOnly)?;
    let tx = users.transaction().map_err(|_| LifecycleError::Unavailable)?;
    let snapshot = duduclaw_auth::read_isolation_authority_in(&tx, None, label)
        .map_err(|_| LifecycleError::Unavailable)?;
    #[cfg(test)]
    USERS_SNAPSHOT_HOOK.with(|hook| if let Some(hook) = hook.borrow_mut().as_mut() { hook() });
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
            // The complete creation-time rule, not just `status`: an
            // offboarded/suspended owner, a forced password change or a
            // missing login all stop existing capabilities and receipts
            // until the owner is eligible again (or an admin hands over).
            if !owner_eligible(&tx, owner.as_str())? { return Err(LifecycleError::Denied); }
        }
    }
    tx.finish().map_err(|_| LifecycleError::Unavailable)?;
    Ok(())
}

/// Test-only pause point inside [`check_users`]' read transaction, after the
/// tenant/mirror reads and before the owner row read.
#[cfg(test)]
thread_local! {
    pub(crate) static USERS_SNAPSHOT_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> = std::cell::RefCell::new(None);
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

/// The single opener for the three authority stores on every lifecycle path
/// (users.db, migration-authority/apply.db, migration-confirmations.db):
/// [`crate::fs_safe::sqlite::open_existing`] — no-follow descriptor walk to
/// the store's directory, owner/mode/link-count/dev+inode checks on the
/// store and its -journal/-wal/-shm sidecars before and after the open,
/// `SQLITE_OPEN_NOFOLLOW`, never created. Any violation is `Unavailable`.
fn open_store(home: &Path, store: &str, access: Access) -> Result<Connection, LifecycleError> {
    let (directory, name) = match store.rsplit_once('/') {
        Some((subdirectory, name)) => (home.join(subdirectory), name),
        None => (home.to_path_buf(), store),
    };
    let db = crate::fs_safe::sqlite::open_existing(&directory, name, access).map_err(|_| LifecycleError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| LifecycleError::Unavailable)?;
    Ok(db)
}

#[cfg(test)]
#[path = "isolation_lifecycle_tests.rs"]
mod tests;
