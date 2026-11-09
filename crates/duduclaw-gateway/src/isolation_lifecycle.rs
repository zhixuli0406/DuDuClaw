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
//! and the admin tombstone path ([`tombstone_employee`], F6a: Personal and
//! Shared, sealed quarantine keyed by `(employee, incarnation)`). Everything
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
    /// F6 §3.1 (H1): `confirmed_employees.incarnation`, read in the same row
    /// read as the receipt (0 before the column exists, as the F6m token
    /// reader does). Only a same-id restore changes it (+1); every persisted
    /// authority binds `(employee, incarnation)` and [`recheck_with`]
    /// refuses a holder of another incarnation.
    pub(crate) incarnation: u32,
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
    // A1: the same employee id after a restore is another incarnation; a
    // holder of the prior one is out of date (never silently upgraded).
    if current.incarnation != expected.incarnation { return Err(LifecycleError::Stale); }
    if current != *expected { return Err(LifecycleError::Stale); }
    Ok(current)
}

/// Full resolution, also returning the live root receipt for the existing
/// `resolve_confirmed_employee` callers.
pub(crate) fn resolve(home: &Path, label: &str, root: &dyn RootAllocation)
    -> Result<(Receipt, EmployeeLifecycle), LifecycleError> {
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    let (prior, pinned, incarnation) = confirmed_row(home, label)?.ok_or(Denied)?;
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
        uid: current.uid, binding, incarnation,
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
    if let Some((prior, pin, _)) = &existing {
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
    // P1h (N2): the ACL generation bump already makes cached owner maps miss;
    // drop them explicitly too (and this request's snapshot, if any).
    crate::personal_visibility::invalidate_owner_cache(home);
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
    crate::personal_visibility::invalidate_owner_cache(home);
    Ok(removed)
}

/// F6a §3.2 step 5 (Shared): remove every member binding of the agent and
/// bump the ACL generation, in one users.db transaction. A Personal mirror
/// for the label refuses (`Stale`, left untouched).
pub(crate) fn remove_shared_bindings(home: &Path, label: &str) -> Result<usize, LifecycleError> {
    let mut users = open_store(home, USERS_DB, Access::ReadWrite)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| LifecycleError::Unavailable)?;
    let removed = duduclaw_auth::db::remove_shared_bindings_in_transaction(&tx, label).map_err(mirror_error)?;
    tx.commit().map_err(|_| LifecycleError::Unavailable)?;
    Ok(removed)
}

/// Personal or Shared, as recorded in the quarantine row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmployeeKind { Personal, Shared }

/// What a tombstone call did (the RPC returns it; Q6: the seal state is
/// reported, a failed seal never blocks the tombstone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TombstoneOutcome {
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) kind: EmployeeKind,
    /// The quarantine row's state after this call (`sealed`, `oversize`,
    /// `invalid`, `absent`, or `pending` when the seal could not be written).
    pub(crate) seal: String,
    pub(crate) digest_prefix: Option<String>,
    /// Catalog routes whose C6 detail row named this employee, revoked now.
    pub(crate) routes_revoked: Vec<String>,
    /// Fences that still need a retry (H2); empty = fully fenced.
    pub(crate) pending_fences: Vec<String>,
    /// Run ids confirmed stopped (S13); `None` when the stop is unconfirmed
    /// (then [`crate::isolation_quarantine::RUN_STOP_FENCE`] is pending).
    pub(crate) runs_stopped: Option<Vec<String>>,
}

/// Collaborators of one tombstone call.
pub(crate) struct TombstoneEnv<'a> {
    /// The verified Admin (audit attribution only; the caller authorized).
    pub(crate) actor: &'a str,
    /// Stops the employee's live runs (production: [`crate::isolation_quarantine::SpawndRunStopper`]).
    pub(crate) runs: &'a dyn crate::isolation_quarantine::RunStopper,
    /// Unix seconds.
    pub(crate) now: i64,
}

/// Tombstone one Personal or Shared protected employee (F6 design r5
/// §3.2, task F6a). The caller must already have verified a live Admin with
/// step-up; this function performs no authorization.
///
/// 0. Read (no lock): the confirmed row (receipt, tenant pin, incarnation),
///    the Applied allocation item, and the principal snapshot — Personal
///    (H3): owner `credential_epoch`, email, Google `sub`, MFA/passkey-set
///    digest, notice address; Shared (R4): the pinned tenant and every member
///    binding `(user_id, access_level, is_primary)`. A legacy unpinned row
///    is refused (`Denied`), as `resolve` refuses it.
/// 1. One apply.db `BEGIN IMMEDIATE` transaction: re-read the row (same
///    employee id and incarnation, else `Stale`); move `agents/<label>` to
///    `isolation-quarantine/<hex>-<incarnation>-<label>` (0700) with one
///    `renameat2(RENAME_NOREPLACE)` between held descriptors (SR2 finding 4;
///    a refused move refuses the whole tombstone and nothing changes); then
///    strip `[channels.*]` and `[capabilities] db_sources` from the moved
///    `agent.toml` through the held descriptor (H5; names recorded in
///    `stripped`; done right after the move rather than before it so a
///    refused move never edits a live employee); mark the row disabled;
///    delete the creation marker; insert the `employee_quarantine` row keyed
///    `(employee_hex, incarnation)` (`pending`, or `absent` for a missing
///    tree, M2) and, once per employee id, the `root_allocations` row (S7).
/// 2. Seal outside the lock (M1) through the descriptor held since the move,
///    root `(dev, ino)` checked; a short transaction writes the result.
/// 3. Stop live runs (S13, [`TombstoneEnv::runs`]; unconfirmed ⇒ pending
///    fence) and revoke every catalog route whose C6 detail row names this
///    employee (H2; failures ⇒ pending fences). Tenant routes stay.
/// 4. users.db: Personal mirror and bindings, or every Shared binding, with
///    an ACL bump. A failure leaves the employee unusable (row disabled) and
///    is retried by calling again.
/// 5. Audit `isolation.employee_tombstoned`.
///
/// Retrying is idempotent: a disabled row is never moved again (a directory
/// that appeared under the label later stays where it is), the first
/// quarantine row and principal snapshot are kept, a still-`pending` seal is
/// re-run through the quarantined directory, fences are recomputed.
///
/// Not done here (later tasks): approval invalidation runs in the RPC after
/// this call (async broker); store marks and the label-keyed writer gate /
/// global route pause (F6h); provider cache eviction beyond the registry
/// rescan (F6c); root retire (F6l).
pub(crate) fn tombstone_employee(home: &Path, label: &str, employee: EmployeeId, env: &TombstoneEnv<'_>)
    -> Result<TombstoneOutcome, LifecycleError> {
    use crate::isolation_quarantine as quarantine;
    use LifecycleError::*;
    if !valid_label(label) { return Err(Denied); }
    // 0. Reads before the lock.
    let (prior, pinned, incarnation) = confirmed_row(home, label)?.ok_or(Denied)?;
    if prior.label != label { return Err(Denied); }
    if prior.employee != employee { return Err(Stale); }
    // Legacy rows confirmed before the tenant pin existed are refused (R4).
    let pinned = pinned.ok_or(Denied)?;
    let binding = principal(&prior, &pinned)?;
    let (kind, owner) = match &binding {
        EmployeeBinding::Personal(owner) => (EmployeeKind::Personal, Some(owner.as_str().to_owned())),
        EmployeeBinding::Shared(_) => (EmployeeKind::Shared, None),
    };
    let item = applied_item(home, &prior.binding)?;
    let root_item = serde_json::json!({"item": item, "receipt": prior});
    let snapshot = principal_snapshot(home, label, &binding, &pinned)?;

    // 1. The apply.db transaction.
    let (held, stripped, seal_state) = {
        let mut db = open_store(home, APPLY_DB, Access::ReadWrite)?;
        quarantine::ensure_employee_schema(&db).map_err(|_| Unavailable)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| Unavailable)?;
        let (encoded, stored_incarnation): (String, i64) = tx.query_row(
            "SELECT receipt,incarnation FROM confirmed_employees WHERE label=?1", [label], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(|_| Unavailable)?.ok_or(Denied)?;
        let current: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
        if current.label != label { return Err(Denied); }
        if current.employee != employee || stored_incarnation != i64::from(incarnation)
            || current.principal_binding != prior.principal_binding {
            return Err(Stale);
        }
        let existing = quarantine::read_quarantine(&tx, employee, incarnation).map_err(|_| Unavailable)?;
        let (held, stripped, seal_state) = if !current.disabled {
            let (held, stripped) = quarantine_agent_dir(home, label, employee, incarnation)?;
            let mut dead = current;
            dead.disabled = true;
            let changed = tx.execute("UPDATE confirmed_employees SET receipt=?1 WHERE label=?2 AND receipt=?3",
                params![serde_json::to_string(&dead).map_err(|_| Unavailable)?, label, encoded]).map_err(|_| Unavailable)?;
            if changed != 1 { return Err(Stale); }
            // A moved symlink or file (not a directory) can never be sealed.
            let state = match (&held, stripped.agent_toml) {
                (Some(_), _) => "pending",
                (None, "not_a_directory") => "invalid",
                (None, _) => "absent",
            };
            (held, stripped, state)
        } else {
            // Retry: never move again. A pending seal is re-run through the
            // quarantined directory; a disabled row without a quarantine row
            // (compensation, dev builds) reads `invalid`.
            let held = match &existing {
                // The recorded root (dev, ino) is what the seal must find.
                Some(row) if row.seal == "pending" => reopen_quarantined(home, label, employee, incarnation)?
                    .and_then(|dir| row.root.map(|root| (dir, root))),
                _ => None,
            };
            (held, quarantine::Stripped::status("unchanged"), "invalid")
        };
        let markers: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_dir_markers')",
            [], |r| r.get(0)).map_err(|_| Unavailable)?;
        if markers {
            tx.execute("DELETE FROM employee_dir_markers WHERE label=?1", [label]).map_err(|_| Unavailable)?;
        }
        if existing.is_none() {
            quarantine::insert_quarantine(&tx, &quarantine::NewQuarantine {
                employee, incarnation, label, owner: owner.as_deref(), tenant: &pinned, quarantined_at: env.now,
                root: held.as_ref().map(|(_, root)| *root), seal: seal_state,
                principal_snapshot: &snapshot, stripped: &stripped.to_value(),
            }).map_err(|_| Unavailable)?;
        }
        quarantine::insert_root_allocation(&tx, employee, &root_item).map_err(|_| Unavailable)?;
        tx.commit().map_err(|_| Unavailable)?;
        (held, stripped, seal_state)
    };
    let _ = seal_state;

    // 2. Seal outside the lock, through the held descriptor.
    #[cfg(unix)]
    {
        if let Some((dir, root)) = &held {
            let sealed = quarantine::seal(home, label, dir, *root);
            if let Some(reason) = sealed.reason {
                tracing::warn!(agent = label, reason, "tombstoned employee tree could not be sealed");
            }
            let mut db = open_store(home, APPLY_DB, Access::ReadWrite)?;
            if quarantine::record_seal(&mut db, employee, incarnation, &sealed).is_err() {
                tracing::warn!(agent = label, "tombstone seal result not recorded; the row stays pending");
            }
        }
    }
    #[cfg(not(unix))]
    { let _ = &held; }

    // 3. Stop runs and fence.
    let mut fences = Vec::new();
    let runs_stopped = match env.runs.stop_employee_runs(employee) {
        Ok(ids) => Some(ids),
        Err(()) => { fences.push(quarantine::RUN_STOP_FENCE.to_owned()); None }
    };
    let (routes_revoked, route_fences) = quarantine::revoke_employee_detail_routes(home, employee, env.actor);
    fences.extend(route_fences);
    {
        let mut db = open_store(home, APPLY_DB, Access::ReadWrite)?;
        for group in ["runs:", "route"] {
            let members: Vec<String> = fences.iter().filter(|f| f.starts_with(group)).cloned().collect();
            quarantine::replace_fence_group(&mut db, employee, incarnation, group, &members).map_err(|_| Unavailable)?;
        }
    }
    let row = {
        let db = open_store(home, APPLY_DB, Access::ReadOnly)?;
        quarantine::read_quarantine(&db, employee, incarnation).map_err(|_| Unavailable)?.ok_or(Unavailable)?
    };

    // 4. users.db.
    let users = match &owner {
        Some(owner) => remove_personal_mirror(home, label, owner).map(|_| ()),
        None => remove_shared_bindings(home, label).map(|_| ()),
    };

    // 5. Audit (also when the users.db step must be retried).
    let outcome = TombstoneOutcome {
        employee, incarnation, kind, seal: row.seal.clone(),
        digest_prefix: row.digest.as_ref().map(|digest| digest.chars().take(16).collect()),
        routes_revoked, pending_fences: row.pending_fences.clone(), runs_stopped,
    };
    let details = serde_json::json!({
        "actor_user_id": env.actor, "kind": match kind { EmployeeKind::Personal => "personal", EmployeeKind::Shared => "shared" },
        "label": label, "employee": hex::encode(employee.0), "incarnation": incarnation,
        "seal": outcome.seal, "digest_prefix": outcome.digest_prefix, "routes_revoked": outcome.routes_revoked,
        "pending_fences": outcome.pending_fences, "runs_stopped": outcome.runs_stopped,
        "stripped": stripped.to_value(), "users_removed": users.is_ok(),
    });
    let severity = if users.is_ok() && outcome.pending_fences.is_empty() {
        duduclaw_security::audit::Severity::Info
    } else {
        duduclaw_security::audit::Severity::Warning
    };
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        "isolation.employee_tombstoned", label, severity, details));
    users?;
    Ok(outcome)
}

/// The stored receipt of the confirmed row holding `label` (live or
/// disabled), without any other check. Never authority: callers use it only
/// to choose which refusal to report before the real lifecycle call.
pub(crate) fn confirmed_receipt(home: &Path, label: &str) -> Result<Option<Receipt>, LifecycleError> {
    if !valid_label(label) { return Ok(None); }
    Ok(confirmed_row(home, label)?.map(|(receipt, _, _)| receipt))
}

/// Replace one fence group (`approvals:` …) of the quarantine row of the
/// employee currently recorded for `label` (its incarnation is read from
/// the row). Refuses (`Stale`) when the label now holds another employee.
pub(crate) fn record_fence_group(home: &Path, label: &str, employee: EmployeeId, group: &str, fences: &[String])
    -> Result<(), LifecycleError> {
    let (receipt, _, incarnation) = confirmed_row(home, label)?.ok_or(LifecycleError::Denied)?;
    if receipt.employee != employee { return Err(LifecycleError::Stale); }
    let mut db = open_store(home, APPLY_DB, Access::ReadWrite)?;
    crate::isolation_quarantine::replace_fence_group(&mut db, employee, incarnation, group, fences)
        .map_err(|_| LifecycleError::Unavailable)
}

/// The Personal (H3) or Shared (R4) principal snapshot of §3.2 step 3, from
/// one users.db read transaction. Never holds a credential value: the
/// MFA/passkey set is a digest, the Google link is the `sub` only.
fn principal_snapshot(home: &Path, label: &str, binding: &EmployeeBinding, pinned: &str)
    -> Result<serde_json::Value, LifecycleError> {
    use LifecycleError::Unavailable;
    use sha2::{Digest, Sha256};
    let mut users = open_store(home, USERS_DB, Access::ReadOnly)?;
    let tx = users.transaction().map_err(|_| Unavailable)?;
    let strings = |sql: &str, key: &str| -> Result<Vec<String>, LifecycleError> {
        let mut statement = tx.prepare(sql).map_err(|_| Unavailable)?;
        let rows = statement.query_map([key], |r| r.get::<_, String>(0)).map_err(|_| Unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(|_| Unavailable)?;
        Ok(rows)
    };
    let value = match binding {
        EmployeeBinding::Personal(owner) => {
            let owner = owner.as_str();
            let account: Option<(i64, String)> = tx.query_row(
                "SELECT credential_epoch,email FROM users WHERE id=?1", [owner], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional().map_err(|_| Unavailable)?;
            let mut factors = Sha256::new();
            factors.update(b"duduclaw-owner-factors-v1\0");
            for (tag, sql) in [
                ("totp", "SELECT 'enrolled' FROM mfa_credentials WHERE user_id=?1"),
                ("email", "SELECT email FROM mfa_email_credentials WHERE user_id=?1 ORDER BY email"),
                ("passkey", "SELECT id FROM mfa_passkeys WHERE user_id=?1 ORDER BY id"),
            ] {
                for value in strings(sql, owner)? {
                    factors.update((tag.len() as u64).to_be_bytes());
                    factors.update(tag.as_bytes());
                    factors.update((value.len() as u64).to_be_bytes());
                    factors.update(value.as_bytes());
                }
            }
            let google = match crate::person_credentials::get(home, owner) {
                Ok(Some(record)) => serde_json::json!(record.metadata.google_sub),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::json!("unreadable"),
            };
            serde_json::json!({
                "kind": "personal", "owner_user_id": owner,
                "credential_epoch": account.as_ref().map(|(epoch, _)| *epoch),
                "email": account.as_ref().map(|(_, email)| email.clone()),
                "google_sub": google,
                "factors_digest": hex::encode(factors.finalize()),
                "notice_address": account.as_ref().map(|(_, email)| email.clone()),
            })
        }
        EmployeeBinding::Shared(_) => {
            let mut statement = tx.prepare(
                "SELECT user_id,access_level,is_primary FROM user_agent_bindings WHERE agent_name=?1 ORDER BY user_id")
                .map_err(|_| Unavailable)?;
            let members = statement.query_map([label], |r| Ok(serde_json::json!({
                "user_id": r.get::<_, String>(0)?, "access_level": r.get::<_, String>(1)?,
                "is_primary": r.get::<_, i64>(2)? == 1,
            }))).map_err(|_| Unavailable)?.collect::<Result<Vec<_>, _>>().map_err(|_| Unavailable)?;
            serde_json::json!({"kind": "shared", "tenant_id": pinned, "members": members})
        }
    };
    tx.finish().map_err(|_| Unavailable)?;
    Ok(value)
}

/// Test-only stopper: the employee has no live run.
#[cfg(test)]
pub(crate) struct NoLiveRuns;
#[cfg(test)]
impl crate::isolation_quarantine::RunStopper for NoLiveRuns {
    fn stop_employee_runs(&self, _: EmployeeId) -> Result<Vec<String>, ()> { Ok(Vec::new()) }
}

/// The pre-F6 Personal-only entry point, kept for the existing tests: a
/// Shared row is refused (`Denied`) exactly as before F6a.
#[cfg(test)]
pub(crate) fn tombstone_personal_employee(home: &Path, label: &str, employee: EmployeeId) -> Result<(), LifecycleError> {
    if !valid_label(label) { return Err(LifecycleError::Denied); }
    let (prior, _, _) = confirmed_row(home, label)?.ok_or(LifecycleError::Denied)?;
    if prior.employee == employee && !matches!(prior.principal_binding, Some(EmployeeBinding::Personal(_))) {
        return Err(LifecycleError::Denied);
    }
    tombstone_employee(home, label, employee, &TombstoneEnv { actor: "test-admin", runs: &NoLiveRuns, now: 1_000 })
        .map(|_| ())
}

/// One confirmed protected employee as listed for readiness (F3): the label
/// and what the stored receipt says. Never authority; every listed label is
/// still resolved through [`read_lifecycle_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmedEntry {
    pub(crate) label: String,
    pub(crate) personal: bool,
    pub(crate) disabled: bool,
}

/// Readiness (F3): every confirmed label, through the hardened opener. No
/// `apply.db` (nothing was ever confirmed) is an empty list; an unsafe or
/// unreadable store, a corrupt row or more than `limit` rows is
/// `Unavailable` (never a shortened list that hides an employee).
pub(crate) fn confirmed_entries(home: &Path, limit: usize) -> Result<Vec<ConfirmedEntry>, LifecycleError> {
    use LifecycleError::Unavailable;
    match std::fs::symlink_metadata(home.join(APPLY_DB)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(Unavailable),
        Ok(_) => {}
    }
    let db = open_store(home, APPLY_DB, Access::ReadOnly)?;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| Unavailable)?;
    if !table { return Ok(Vec::new()); }
    let bound = i64::try_from(limit.saturating_add(1)).map_err(|_| Unavailable)?;
    let mut query = db.prepare("SELECT label,receipt FROM confirmed_employees ORDER BY label LIMIT ?1")
        .map_err(|_| Unavailable)?;
    let rows = query.query_map([bound], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|_| Unavailable)?.collect::<Result<Vec<_>, _>>().map_err(|_| Unavailable)?;
    if rows.len() > limit { return Err(Unavailable); }
    rows.into_iter().map(|(label, encoded)| {
        let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
        if receipt.label != label || !valid_label(&label) { return Err(Unavailable); }
        Ok(ConfirmedEntry { personal: matches!(receipt.principal_binding, Some(EmployeeBinding::Personal(_))),
            disabled: receipt.disabled, label })
    }).collect()
}

/// Readiness (F3): one users.db read snapshot of `label` with `owner` as the
/// human, through the hardened opener, for the B1 `personal_facts` rule. The
/// owner-less read runs first in the same transaction so an unreadable store
/// is `Unavailable`; `Ok(None)` = the owner is not currently assignable.
pub(crate) fn personal_snapshot(home: &Path, label: &str, owner: &str)
    -> Result<Option<duduclaw_auth::IsolationAuthoritySnapshot>, LifecycleError> {
    let mut users = open_store(home, USERS_DB, Access::ReadOnly)?;
    let tx = users.transaction().map_err(|_| LifecycleError::Unavailable)?;
    duduclaw_auth::read_isolation_authority_in(&tx, None, label).map_err(|_| LifecycleError::Unavailable)?;
    let snapshot = duduclaw_auth::read_isolation_authority_in(&tx, Some(owner), label).ok();
    tx.finish().map_err(|_| LifecycleError::Unavailable)?;
    Ok(snapshot)
}

/// Private quarantine for tombstoned Personal employees' directories,
/// directly in the DuDuClaw home (same filesystem as `agents/`).
pub(crate) const QUARANTINE_DIR: &str = "isolation-quarantine";

/// Where the tombstone of incarnation `incarnation` of `employee` puts its
/// `agents/<label>` directory (M4: one path per quarantine row).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn quarantine_path(home: &Path, label: &str, employee: EmployeeId, incarnation: u32) -> std::path::PathBuf {
    home.join(QUARANTINE_DIR).join(crate::isolation_quarantine::quarantine_name(label, employee, incarnation))
}

/// A quarantined directory, by descriptor (retry of a pending seal). The
/// quarantine root must be private as at the move; `Ok(None)` when absent.
fn reopen_quarantined(home: &Path, label: &str, employee: EmployeeId, incarnation: u32)
    -> Result<Option<crate::fs_safe::SafeDir>, LifecycleError> {
    use LifecycleError::Unavailable;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use crate::fs_safe::SafeDir;
        let home_dir = SafeDir::open_nofollow(home, false).map_err(|_| Unavailable)?;
        let Some(root) = home_dir.child_dir(QUARANTINE_DIR, false).map_err(|_| Unavailable)? else { return Ok(None) };
        let metadata = root.metadata().map_err(|_| Unavailable)?;
        if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 { return Err(Unavailable); }
        root.child_dir(&crate::isolation_quarantine::quarantine_name(label, employee, incarnation), false)
            .map_err(|_| Unavailable)
    }
    #[cfg(not(unix))]
    { let _ = (home, label, employee, incarnation); Err(Unavailable) }
}

/// The moved tree held by descriptor, with its `(dev, ino)`.
type Held = (crate::fs_safe::SafeDir, (u64, u64));

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
///
/// F6a: the target is keyed by `(employee, incarnation)`; after the move the
/// moved directory is opened by descriptor (held for the seal, its
/// `(dev, ino)` recorded) and its `agent.toml` stripped through that
/// descriptor. `Ok((None, …))` when nothing was moved (absent tree) or the
/// moved entry is not a directory (it seals `absent`/`invalid` respectively
/// through the caller). A strip failure never undoes the move: it is
/// recorded (`unreadable`) and the tombstone continues.
fn quarantine_agent_dir(home: &Path, label: &str, employee: EmployeeId, incarnation: u32)
    -> Result<(Option<Held>, crate::isolation_quarantine::Stripped), LifecycleError> {
    use LifecycleError::Unavailable;
    use crate::isolation_quarantine::Stripped;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use crate::fs_safe::{EntryKind, SafeDir};
        let home_dir = SafeDir::open_nofollow(home, false).map_err(|_| Unavailable)?;
        let Some(agents) = home_dir.child_dir("agents", false).map_err(|_| Unavailable)? else {
            return Ok((None, Stripped::status("absent")));
        };
        if agents.entry(label).map_err(|_| Unavailable)?.is_none() { return Ok((None, Stripped::status("absent"))); }
        let root = home_dir.child_dir(QUARANTINE_DIR, true).map_err(|_| Unavailable)?.ok_or(Unavailable)?;
        let metadata = root.metadata().map_err(|_| Unavailable)?;
        if !metadata.is_dir() || metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(Unavailable);
        }
        let target = crate::isolation_quarantine::quarantine_name(label, employee, incarnation);
        // The kernel refuses an existing target atomically; there is no
        // separate existence check to race.
        agents.rename_noreplace(label, &root, &target).map_err(|_| Unavailable)?;
        tracing::info!(agent = label, incarnation, "tombstoned employee directory moved to quarantine");
        // The moved entry itself, by descriptor: never chmod through a symlink.
        if !root.entry(&target).map_err(|_| Unavailable)?.is_some_and(|entry| entry.kind == EntryKind::Dir) {
            // A symlink or file was moved as-is; nothing to strip or seal.
            return Ok((None, Stripped::status("not_a_directory")));
        }
        let moved = root.child_dir(&target, false).map_err(|_| Unavailable)?.ok_or(Unavailable)?;
        moved.set_mode(0o700).map_err(|_| Unavailable)?;
        let moved_meta = moved.metadata().map_err(|_| Unavailable)?;
        let stripped = crate::isolation_quarantine::strip_agent_dir(&root, &target).unwrap_or_else(|error| {
            tracing::warn!(agent = label, error = %error, "tombstone: agent.toml could not be stripped");
            Stripped::status("unreadable")
        });
        Ok((Some((moved, (moved_meta.dev(), moved_meta.ino()))), stripped))
    }
    #[cfg(not(unix))]
    { let _ = (home, label, employee, incarnation); Err(Unavailable) }
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
/// The incarnation comes from the same row read (0 before F6a added the
/// column); a NULL, negative or out-of-range value, or an employee schema
/// newer than this build, is `Unavailable`.
fn confirmed_row(home: &Path, label: &str) -> Result<Option<(Receipt, Option<String>, u32)>, LifecycleError> {
    let db = open_store(home, APPLY_DB, Access::ReadOnly)?;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| LifecycleError::Unavailable)?;
    if !table { return Ok(None); }
    crate::isolation_quarantine::check_readable(&db).map_err(|_| LifecycleError::Unavailable)?;
    let sql = if crate::isolation_quarantine::has_incarnation(&db).map_err(|_| LifecycleError::Unavailable)? {
        "SELECT receipt,tenant_id,incarnation FROM confirmed_employees WHERE label=?1"
    } else {
        "SELECT receipt,tenant_id,0 FROM confirmed_employees WHERE label=?1"
    };
    let row: Option<(String, Option<String>, i64)> = db.query_row(sql, [label], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional().map_err(|_| LifecycleError::Unavailable)?;
    let Some((encoded, tenant, incarnation)) = row else { return Ok(None) };
    let incarnation = u32::try_from(incarnation).map_err(|_| LifecycleError::Unavailable)?;
    let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| LifecycleError::Unavailable)?;
    if let Some(binding) = &receipt.principal_binding {
        allocation::validate_principal_binding(binding).map_err(|_| LifecycleError::Unavailable)?;
    }
    Ok(Some((receipt, tenant, incarnation)))
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
