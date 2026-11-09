//! Common checks of §3.4, run at request, in the approval call (tx1) and in
//! the apply transaction (tx2) (child module of `isolation_restore`,
//! re-exported there).
use super::*;

const MAX_CREDENTIAL_CHANGES: usize = 1_000;
const MAX_CONFIRMED: usize = 100_000;

/// Where the common checks run: what seal and which open request they accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage<'a> {
    /// Filing: the row `sealed`, no open request.
    Request,
    /// Approval tx1: the row `sealed`, only this request open (`requested`).
    Approval(&'a str),
    /// Apply tx2: the row `restoring`, only this request open (`applying`).
    Apply(&'a str),
}

/// The apply.db half, inside the caller's transaction (`db`). Returns the
/// confirmed receipt (disabled) of the row's employee and its stored text
/// (the apply step's exact-match update compares that text).
pub(crate) fn check_apply_db(db: &Connection, q: &Quarantined, stage: Stage<'_>, now: i64) -> Result<(Receipt, String), Refusal> {
    let (expected_seal, open_state, own) = match stage {
        Stage::Request => ("sealed", "requested", None),
        Stage::Approval(id) => ("sealed", "requested", Some(id)),
        Stage::Apply(id) => ("restoring", "applying", Some(id)),
    };
    let fresh = read_row(db, q.employee, q.incarnation)?.ok_or(Refusal::NotFound)?;
    if fresh.label != q.label || fresh.digest != q.digest || fresh.root != q.root { return Err(Refusal::Inconsistent); }
    if matches!(fresh.seal.as_str(), "restoring" | "adopting") && expected_seal != fresh.seal { return Err(Refusal::RestoreInProgress); }
    if fresh.seal != expected_seal || fresh.digest.is_none() || fresh.root.is_none() { return Err(Refusal::NotSealed); }
    if now < fresh.quarantined_at || now > fresh.quarantined_at.saturating_add(RESTORE_WINDOW_SECS) { return Err(Refusal::Expired); }
    let state: Option<String> = db.query_row("SELECT root_state FROM root_allocations WHERE employee_hex=?1",
        [hex::encode(q.employee.0)], |r| r.get(0)).optional().map_err(unavailable)?;
    match state.as_deref() {
        Some("allocated") => {}
        Some("retiring") => return Err(Refusal::RetireInProgress),
        Some("retired" | "root_absent") => return Err(Refusal::Root(RootClass::Retired)),
        _ => return Err(Refusal::Unavailable),
    }
    for (employee, incarnation, seal) in rows_for_label(db, &q.label)? {
        if (employee, incarnation) != (q.employee, q.incarnation) && matches!(seal.as_str(), "restoring" | "adopting") {
            return Err(Refusal::RestoreInProgress);
        }
    }
    match (open_request(db, q.employee, q.incarnation)?, own) {
        (None, None) => {}
        (Some(open), Some(id)) if open.request_id == id && open.state == open_state => {}
        _ => return Err(Refusal::RestoreInProgress),
    }
    if !fresh.pending_fences.is_empty() { return Err(Refusal::FencePending); }
    let markers: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_dir_markers')",
        [], |r| r.get(0)).map_err(unavailable)?;
    if markers {
        let marked: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM employee_dir_markers WHERE label=?1)", [&q.label], |r| r.get(0))
            .map_err(unavailable)?;
        if marked { return Err(Refusal::LabelTaken); }
    }
    let (encoded, pinned, incarnation): (String, Option<String>, i64) = db.query_row(
        "SELECT receipt,tenant_id,incarnation FROM confirmed_employees WHERE label=?1", [&q.label],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(unavailable)?.ok_or(Refusal::NotFound)?;
    let receipt: Receipt = serde_json::from_str(&encoded).map_err(unavailable)?;
    if receipt.label != q.label || receipt.employee != q.employee { return Err(Refusal::NotFound); }
    if !receipt.disabled || incarnation != i64::from(q.incarnation) { return Err(Refusal::Inconsistent); }
    if pinned.as_deref() != Some(q.tenant.as_str()) { return Err(Refusal::TenantMismatch); }
    let receipt_owner = match &receipt.principal_binding {
        Some(EmployeeBinding::Personal(owner)) => Some(owner.as_str()),
        _ => None,
    };
    if q.owner.as_deref() != receipt_owner { return Err(Refusal::Inconsistent); }
    Ok((receipt, encoded))
}

/// Retry the tombstone's fences once (H2): the run stop and the C6 detail
/// route revocation; `approvals:` fences are the RPC's (async broker).
pub(crate) fn retry_fences(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined) -> Result<(), Refusal> {
    let runs_pending = q.pending_fences.iter().any(|fence| fence.starts_with("runs:"));
    let runs = if runs_pending && env.runs.stop_employee_runs(q.employee).is_err() {
        vec![quarantine::RUN_STOP_FENCE.to_owned()]
    } else {
        Vec::new()
    };
    let mut db = lifecycle::open_store(home, APPLY_DB, Access::ReadWrite).map_err(from_lifecycle)?;
    if runs_pending {
        quarantine::replace_fence_group(&mut db, q.employee, q.incarnation, "runs:", &runs).map_err(unavailable)?;
    }
    let routes_pending = q.pending_fences.iter().any(|fence| fence.starts_with("route"))
        || !env.routes.detail_routes_naming(q.employee).map_err(unavailable)?.is_empty();
    if routes_pending {
        let (_, fences) = quarantine::revoke_employee_detail_routes(home, q.employee, env.actor);
        quarantine::replace_fence_group(&mut db, q.employee, q.incarnation, "route", &fences).map_err(unavailable)?;
    }
    Ok(())
}

/// The users.db side of the tombstone is finished: no mirror, no binding,
/// no pending removal grant for the label.
fn users_side_removed(home: &Path, label: &str) -> Result<bool, Refusal> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(from_lifecycle)?;
    let tx = users.transaction().map_err(unavailable)?;
    let mirror = duduclaw_auth::db::personal_agent_owner(&tx, label).map_err(unavailable)?;
    let (bound, removal): (bool, bool) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_agent_bindings WHERE agent_name=?1),
                EXISTS(SELECT 1 FROM isolation_personal_agent_removals WHERE agent_name=?1)",
        [label], |r| Ok((r.get(0)?, r.get(1)?))).map_err(unavailable)?;
    tx.finish().map_err(unavailable)?;
    Ok(mirror.is_none() && !bound && !removal)
}

/// H1a: the tombstone must have finished; the precheck retries its users.db
/// step once, then refuses `TOMBSTONE_INCOMPLETE`.
pub(crate) fn ensure_tombstone_finished(home: &Path, q: &Quarantined) -> Result<(), Refusal> {
    if users_side_removed(home, &q.label)? { return Ok(()); }
    let retried = match &q.owner {
        Some(owner) => lifecycle::remove_personal_mirror(home, &q.label, owner).map(|_| ()),
        None => lifecycle::remove_shared_bindings(home, &q.label).map(|_| ()),
    };
    if retried.is_err() || !users_side_removed(home, &q.label)? { return Err(Refusal::TombstoneIncomplete); }
    Ok(())
}

/// Q3 (F6fa): each compared snapshot field and the `credential_changes`
/// fields whose (owner-stamped) rows may explain a difference in it. The
/// password has no snapshot field: any password row not by the owner refuses
/// through the log (the log is written by triggers on every writer).
/// `google_sub` is not logged in users.db, so any change refuses.
///
/// **Owner decision 2026-11-09 (F6fa2, decision 1):** `credential_epoch` is
/// **not** compared. It also moves on a session revocation ("log out all
/// devices", by the owner or an Admin, `remember_revoke` /
/// `remember_admin_revoke_all`), which changes no password, email or factor
/// and writes no `credential_changes` row; such a revocation never blocks a
/// restore. Only password, email and factor (TOTP / passkey / email-factor)
/// changes by someone other than the owner refuse. The snapshot still records
/// the epoch (informational only).
pub(crate) const SNAPSHOT_LOG_FIELDS: [(&str, &[&str]); 2] = [
    ("email", &["email"]),
    ("factors_digest", &["totp", "passkey", "email_factor"]),
];

/// Q3: the owner's identity facts compared with the tombstone snapshot, on
/// the caller's users.db connection (`users`: the read transaction of a
/// check, or the restore apply's write transaction). Only log rows after the
/// `credential_seq` recorded at the tombstone count (by sequence, never wall
/// clock). Any such row not by the owner (owner-authenticated) refuses; each
/// differing snapshot field needs at least one owner row of a field that can
/// change it ([`SNAPSHOT_LOG_FIELDS`]); a changed Google `sub` refuses. A
/// changed `credential_epoch` alone (a session revocation) is ignored
/// (F6fa2). A snapshot without `credential_seq` (tombstoned before F6fa)
/// refuses `SNAPSHOT_INCOMPLETE` (no wall-clock fallback).
pub(crate) fn check_credentials(home: &Path, users: &Connection, q: &Quarantined, owner: &str) -> Result<(), Refusal> {
    let since = q.principal_snapshot["credential_seq"].as_i64().filter(|seq| *seq >= 0).ok_or(Refusal::SnapshotIncomplete)?;
    let changes = duduclaw_auth::credential_changes::changes_after(users, owner, since, MAX_CREDENTIAL_CHANGES)
        .map_err(unavailable)?;
    if changes.iter().any(|change| !change.by_owner()) { return Err(Refusal::CredentialChanged); }
    let current = lifecycle::principal_snapshot_in(users, home, &q.label, &q.binding()?, &q.tenant).map_err(from_lifecycle)?;
    if current["google_sub"] == json!("unreadable") { return Err(Refusal::Unavailable); }
    if current["google_sub"] != q.principal_snapshot["google_sub"] { return Err(Refusal::CredentialChanged); }
    for (field, logged) in SNAPSHOT_LOG_FIELDS {
        if current[field] == q.principal_snapshot[field] { continue; }
        if !changes.iter().any(|change| logged.contains(&change.field.as_str())) { return Err(Refusal::CredentialChanged); }
    }
    Ok(())
}

/// Owner decision 2026-11-09 (F6fa2, decision 2): may `admin` file a
/// Personal request (restore or adoption) for the owner of `q`?
/// - the filing Admin presented a fresh step-up in this call (else
///   `STEP_UP_REQUIRED`) and is not the owner (`FILER_IS_OWNER`);
/// - the tombstone snapshot records the owner's factor baseline (else
///   `SNAPSHOT_INCOMPLETE`: nothing proves the owner had no factor) and it
///   shows **no** second factor then (else `OWNER_HAD_SECOND_FACTOR`: owners
///   who had one keep the `FACTOR_*` path only, never this as a fallback);
/// - in one users.db read transaction, `filing` (the F6c2 rule's
///   `check_admin_filing`): the filer is an eligible Admin now and another
///   qualifying Admin — neither filer nor owner — exists to approve it
///   (`FILER_NOT_ELIGIBLE` / `NO_SECOND_ADMIN`).
///
/// Every other check (owner eligibility, Q3 credential changes, 90 days, …)
/// is the owner-filed request's, unchanged.
pub(crate) fn check_admin_filing(home: &Path, q: &Quarantined, admin: &str, fresh_step_up: bool,
    filing: &dyn Fn(&Connection, &str, &str) -> Result<(), &'static str>) -> Result<(), Refusal> {
    let owner = match (&q.kind, &q.owner) {
        (EmployeeKind::Personal, Some(owner)) => owner.as_str(),
        _ => return Err(Refusal::NotFound),
    };
    if !fresh_step_up { return Err(Refusal::Approval("STEP_UP_REQUIRED")); }
    if admin.is_empty() || admin == owner { return Err(Refusal::FilerIsOwner); }
    let baseline = duduclaw_auth::mfa::owner_factor::FactorBaseline::from_snapshot(&q.principal_snapshot)
        .ok_or(Refusal::SnapshotIncomplete)?;
    if baseline.strongest.is_some() { return Err(Refusal::OwnerHadSecondFactor); }
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(from_lifecycle)?;
    let tx = users.transaction().map_err(unavailable)?;
    let read: &Connection = &tx;
    filing(read, admin, owner).map_err(Refusal::Approval)?;
    tx.finish().map_err(unavailable)
}

/// Whether the tombstone snapshot of a Personal row allows an Admin to file
/// for its owner (no second factor recorded then), for the listing views.
pub(crate) fn admin_may_file_for_owner(q: &Quarantined) -> bool {
    q.kind == EmployeeKind::Personal && q.owner.is_some()
        && duduclaw_auth::mfa::owner_factor::FactorBaseline::from_snapshot(&q.principal_snapshot)
            .is_some_and(|baseline| baseline.strongest.is_none())
}

/// The owner side (Personal) and tenant, in one users.db read transaction.
pub(crate) fn check_users(home: &Path, q: &Quarantined) -> Result<(), Refusal> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(from_lifecycle)?;
    let tx = users.transaction().map_err(unavailable)?;
    let tenant = duduclaw_auth::isolation::read_isolation_tenant(&tx).map_err(unavailable)?;
    if tenant != q.tenant { return Err(Refusal::TenantMismatch); }
    if let Some(owner) = &q.owner {
        if !lifecycle::owner_eligible(&tx, owner).map_err(from_lifecycle)? { return Err(Refusal::OwnerIneligible); }
        check_credentials(home, &tx, q, owner)?;
    }
    tx.finish().map_err(unavailable)
}

fn proposal_text<'a>(item: &'a ConfirmationItem, name: &str) -> Option<&'a str> {
    item.fields.iter().find(|field| field.name == name).and_then(|field| match &field.value {
        ProposalValue::Text { value } => Some(value.as_str()),
        _ => None,
    })
}

/// No open Employee proposal names the label (`label_pending`).
pub(crate) fn check_no_open_proposal(home: &Path, label: &str) -> Result<(), Refusal> {
    let store = duduclaw_migration::confirmation::ConfirmationStore::from_read_only_connection(
        lifecycle::open_store(home, CONFIRMATIONS_DB, Access::ReadOnly).map_err(from_lifecycle)?).map_err(unavailable)?;
    let items = store.list().map_err(unavailable)?;
    let open = items.iter().any(|item| item.category == ConfirmationCategory::Employee
        && matches!(item.status, ConfirmationStatus::Pending | ConfirmationStatus::Approved
            | ConfirmationStatus::Applying | ConfirmationStatus::Rejecting)
        && proposal_text(item, "employee_id") == Some(label));
    if open { Err(Refusal::LabelPending) } else { Ok(()) }
}

/// `agents/<label>` is absent (never followed).
pub(crate) fn check_target_absent(home: &Path, label: &str) -> Result<(), Refusal> {
    #[cfg(unix)]
    {
        let home_dir = crate::fs_safe::SafeDir::open_nofollow(home, false).map_err(unavailable)?;
        let Some(agents) = home_dir.child_dir("agents", false).map_err(unavailable)? else { return Ok(()) };
        if agents.entry(label).map_err(unavailable)?.is_some() { return Err(Refusal::LabelTaken); }
        Ok(())
    }
    #[cfg(not(unix))]
    { let _ = (home, label); Err(Refusal::Unavailable) }
}

/// L2: the agent cap counts live confirmed employees; the restored one
/// would be one more.
pub(crate) fn check_quota(home: &Path, cap: usize) -> Result<(), Refusal> {
    if cap == 0 { return Ok(()); }
    let live = lifecycle::confirmed_entries(home, MAX_CONFIRMED).map_err(from_lifecycle)?.iter().filter(|e| !e.disabled).count();
    if live >= cap { Err(Refusal::QuotaExceeded) } else { Ok(()) }
}

/// L2 inside the apply transaction (F6fa item 2): the live confirmed
/// employees counted on the caller's apply.db connection, which holds the
/// writer lock, so no proposal apply can add one between this count and the
/// restore's commit.
pub(crate) fn check_quota_in(db: &Connection, cap: usize) -> Result<(), Refusal> {
    if cap == 0 { return Ok(()); }
    let bound = i64::try_from(MAX_CONFIRMED.saturating_add(1)).map_err(unavailable)?;
    let mut statement = db.prepare("SELECT label,receipt FROM confirmed_employees ORDER BY label LIMIT ?1").map_err(unavailable)?;
    let rows = statement.query_map([bound], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    if rows.len() > MAX_CONFIRMED { return Err(Refusal::Unavailable); }
    let mut live = 0usize;
    for (label, encoded) in rows {
        let receipt: Receipt = serde_json::from_str(&encoded).map_err(unavailable)?;
        if receipt.label != label { return Err(Refusal::Unavailable); }
        if !receipt.disabled { live += 1; }
    }
    if live >= cap { Err(Refusal::QuotaExceeded) } else { Ok(()) }
}

/// Every check that reads outside apply.db (users.db, confirmations, the
/// filesystem, the catalog, the store registry, the root): run at request
/// and in the approval call before tx1. Returns the current route list.
pub(crate) fn check_outside(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined) -> Result<Vec<(String, u64)>, Refusal> {
    ensure_tombstone_finished(home, q)?;
    if !env.routes.detail_routes_naming(q.employee).map_err(unavailable)?.is_empty() { return Err(Refusal::FencePending); }
    check_users(home, q)?;
    check_no_open_proposal(home, &q.label)?;
    check_target_absent(home, &q.label)?;
    check_quota(home, env.agent_cap)?;
    check_store_marks(home, env, q)?;
    match restore_precheck(home, &q.label, q.employee, env.root).map_err(from_lifecycle)? {
        RootClass::Ok => {}
        class => return Err(Refusal::Root(class)),
    }
    env.routes.routes_for(&q.binding()?, &q.tenant).map_err(unavailable)
}

/// §3.3: the recorded marks must exist and equal the live ones.
pub(crate) fn check_store_marks(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined) -> Result<(), Refusal> {
    let recorded = q.store_marks.as_ref().ok_or(Refusal::StoresUnavailable)?;
    let current = env.stores.marks(home, &q.label).map_err(|_| Refusal::StoresUnavailable)?;
    if &current != recorded { return Err(Refusal::StoresChanged); }
    Ok(())
}

/// Read the row, retry pending fences once, read it again.
pub(crate) fn load_fenced(home: &Path, env: &RestoreEnv<'_>, employee: EmployeeId, incarnation: u32) -> Result<Quarantined, Refusal> {
    let read = || -> Result<Quarantined, Refusal> {
        let db = open_apply(home)?;
        read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)
    };
    let row = read()?;
    if row.pending_fences.is_empty() && env.routes.detail_routes_naming(employee).map_err(unavailable)?.is_empty() {
        return Ok(row);
    }
    retry_fences(home, env, &row)?;
    read()
}
