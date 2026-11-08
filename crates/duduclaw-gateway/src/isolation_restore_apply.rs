//! §3.5 same-id apply inside the approval call, and §3.6 crash recovery
//! (child module of `isolation_restore`).
//!
//! 1. Verify in quarantine, outside every lock (M1): re-walk through a held
//!    descriptor; a digest mismatch with an unchanged stat manifest is the
//!    org entry (`ORG_CHANGED`), any other mismatch is `INTEGRITY` and the
//!    row becomes `invalid`.
//! 2. tx1 (short): every common check again incl. R1; request `applying`,
//!    quarantine `restoring` bound to the request (`closed_by_request`).
//! 3. tx2 (`BEGIN IMMEDIATE`, `synchronous=FULL`, held to the end): NOREPLACE
//!    move back to `agents/<label>` with the root `(dev, ino)` checked;
//!    stat re-walk against step 1 (mismatch: moved back, `needs_reseal`);
//!    route digest and store marks again; users.db in one transaction
//!    (eligibility, approval rule, mirror or bindings, ACL + policy epoch
//!    bump); the confirmed row live with `incarnation + 1`; quarantine
//!    `restored`; request `done`.
//! 4. After commit the lifecycle must read the new incarnation (else alert).
//!
//! The registry rescan, provider cache eviction (L3) and owner/approver
//! notices run in the F6d RPC after this returns (async).
use super::*;

/// A completed restore.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Restored {
    pub(crate) request: RestoreRequest,
    /// The new incarnation (old + 1).
    pub(crate) incarnation: u32,
    pub(crate) self_approved: bool,
}

/// What a recovery pass did for one label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Recovered {
    /// Request ids rolled back (`failed: interrupted`).
    pub(crate) rolled_back: Vec<String>,
    /// Request ids rolled forward (`done`).
    pub(crate) rolled_forward: Vec<String>,
    /// `(employee hex-incarnation, new seal)` of re-verified rows.
    pub(crate) resealed: Vec<(String, String)>,
    pub(crate) expired: usize,
}

/// Points where a test may act (touch the tree, change an account) or
/// simulate a crash (the hook returns `true`: the call returns at once,
/// without any cleanup, and an open transaction rolls back as on process
/// death).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookPoint { AfterMove, AfterUsers, AfterCommit }

#[cfg(test)]
thread_local! {
    pub(crate) static RESTORE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(HookPoint) -> bool>>> =
        std::cell::RefCell::new(None);
}

fn crash_at(point: HookPoint) -> bool {
    #[cfg(test)]
    { RESTORE_HOOK.with(|hook| hook.borrow_mut().as_mut().is_some_and(|hook| hook(point))) }
    #[cfg(not(test))]
    { let _ = point; false }
}

/// A refusal that ends the request (the requester must file again).
fn terminal(refusal: Refusal) -> bool {
    matches!(refusal, Refusal::RoutesChanged | Refusal::StoresChanged | Refusal::OrgChanged | Refusal::Integrity
        | Refusal::CredentialChanged | Refusal::Expired | Refusal::Inconsistent | Refusal::OwnerIneligible
        | Refusal::NotSealed | Refusal::Root(RootClass::Gone | RootClass::Inconsistent | RootClass::Retired))
}

/// Record a terminal refusal before tx1 (`requested → failed`), optionally
/// with a quarantine transition, and audit it; transient refusals leave the
/// request open for a retry. Returns the refusal.
fn settle(home: &Path, request: &RestoreRequest, refusal: Refusal, seal: Option<(&[&str], &str)>, now: i64) -> Refusal {
    if !terminal(refusal) {
        audit(home, "isolation.employee_restore_refused", &request.label, Severity::Warning,
            audit_details(request, json!({"refusal": refusal.code()})));
        return refusal;
    }
    let written = (|| -> Result<(), Refusal> {
        let mut db = open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        set_request_state(&tx, &request.request_id, &["requested"], "failed", Some(refusal.code()), now)?;
        if let Some((from, to)) = seal { set_seal(&tx, request.employee, request.incarnation, from, to)?; }
        tx.commit().map_err(unavailable)
    })();
    audit(home, "isolation.employee_restore_failed", &request.label, Severity::Warning,
        audit_details(request, json!({"failure": refusal.code(), "recorded": written.is_ok()})));
    refusal
}

/// §3.5 step 1 (M1): the quarantined tree, re-walked through a held
/// descriptor, must still carry the sealed digest.
#[cfg(unix)]
fn verify_in_quarantine(home: &Path, q: &Quarantined) -> Result<(crate::fs_safe::SafeDir, quarantine::Sealed), Refusal> {
    let root = q.root.ok_or(Refusal::NotSealed)?;
    let dir = lifecycle::reopen_quarantined(home, &q.label, q.employee, q.incarnation).map_err(from_lifecycle)?
        .ok_or(Refusal::Integrity)?;
    let fresh = quarantine::seal(home, &q.label, &dir, root);
    if fresh.state != quarantine::SealState::Sealed { return Err(Refusal::Integrity); }
    if fresh.digest.is_some() && fresh.digest == q.digest { return Ok((dir, fresh)); }
    // Same inodes, sizes and ctimes: the tree is untouched, so the digest
    // moved through the synthetic `org:<label>` entry (H4).
    if fresh.stat_manifest.is_some() && fresh.stat_manifest == q.stat_manifest { return Err(Refusal::OrgChanged); }
    Err(Refusal::Integrity)
}

/// The approval call (`isolation.employee_restore_decide` approve, F6d):
/// the caller verified a live Admin with step-up; `approver` is that Admin.
#[cfg(unix)]
pub(crate) fn approve(home: &Path, env: &RestoreEnv<'_>, request_id: &str, approver: &str) -> Result<Restored, Refusal> {
    let request = { let db = open_apply(home)?; read_request(&db, request_id)?.ok_or(Refusal::NotFound)? };
    if !matches!(request.kind, RequestKind::PersonalRestore | RequestKind::SharedRestore) { return Err(Refusal::NotSupported); }
    recover_label(home, env, &request.label)?;
    // Recovery expired every stale request, this one included.
    let request = { let db = open_apply(home)?; read_request(&db, request_id)?.ok_or(Refusal::NotFound)? };
    match request.state.as_str() {
        "requested" if env.now <= request.expires_at => {}
        "requested" | "expired" => return Err(Refusal::Expired),
        _ => return Err(Refusal::NotFound),
    }
    let q = load_fenced(home, env, request.employee, request.incarnation).map_err(|r| settle(home, &request, r, None, env.now))?;
    let routes = check_outside(home, env, &q).map_err(|r| settle(home, &request, r, None, env.now))?;
    if route_digest(&routes) != request.route_digest {
        return Err(settle(home, &request, Refusal::RoutesChanged, None, env.now));
    }
    let sealed_digest = q.digest.clone().ok_or(Refusal::NotSealed)?;
    if request_digest(&request, &sealed_digest) != request.digest {
        return Err(settle(home, &request, Refusal::Inconsistent, None, env.now));
    }
    // R1 before any write; the binding decision is re-taken in tx2.
    precheck_rule(home, env, &request, approver).map_err(|r| settle(home, &request, r, None, env.now))?;
    // 1. Verify in quarantine, outside the lock.
    let (dir, verified) = match verify_in_quarantine(home, &q) {
        Ok(found) => found,
        Err(Refusal::Integrity) =>
            return Err(settle(home, &request, Refusal::Integrity, Some((&["sealed"][..], "invalid")), env.now)),
        Err(refusal) => return Err(settle(home, &request, refusal, None, env.now)),
    };
    // 2. tx1.
    {
        let mut db = open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        check_apply_db(&tx, &q, Stage::Approval(request_id), env.now)?;
        set_request_state(&tx, request_id, &["requested"], "applying", None, env.now)?;
        tx.execute("UPDATE employee_restores SET approver_user_id=?2 WHERE request_id=?1", params![request_id, approver])
            .map_err(unavailable)?;
        set_seal(&tx, q.employee, q.incarnation, &["sealed"], "restoring")?;
        tx.execute("UPDATE employee_quarantine SET closed_by_request=?3 WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation, request_id]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
    }
    let applying = RestoreRequest { state: "applying".into(), approver: Some(approver.to_owned()), ..request };
    audit(home, "isolation.employee_restore_approved", &q.label, Severity::Info, audit_details(&applying, json!({})));
    // 3. tx2, then 4.
    let restored = apply_tx2(home, env, &applying, &q, approver, &dir, &verified)?;
    post_commit(home, env, &q, restored)
}

#[cfg(not(unix))]
pub(crate) fn approve(_: &Path, _: &RestoreEnv<'_>, _: &str, _: &str) -> Result<Restored, Refusal> { Err(Refusal::Unavailable) }

/// R1 at tx1: owner eligibility and the approval rule in one users.db read.
fn precheck_rule(home: &Path, env: &RestoreEnv<'_>, request: &RestoreRequest, approver: &str) -> Result<(), Refusal> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(from_lifecycle)?;
    let tx = users.transaction().map_err(unavailable)?;
    if let Some(owner) = &request.owner {
        if !lifecycle::owner_eligible(&tx, owner).map_err(from_lifecycle)? { return Err(Refusal::OwnerIneligible); }
    }
    decide(env.rule, &tx, &request.requester, approver, request.kind)?;
    tx.finish().map_err(unavailable)
}

/// How tx2 ended before its commit.
enum Step {
    /// Simulated crash (tests): return at once, roll tx2 back.
    Crash,
    /// Refused: the quarantine goes to `seal` and the request `failed`.
    Refused(Refusal, &'static str),
}

#[cfg(unix)]
struct Dirs { agents: crate::fs_safe::SafeDir, quarantine: crate::fs_safe::SafeDir }

#[cfg(unix)]
fn open_dirs(home: &Path) -> Result<Dirs, Refusal> {
    use std::os::unix::fs::MetadataExt;
    let home_dir = crate::fs_safe::SafeDir::open_nofollow(home, false).map_err(unavailable)?;
    let agents = home_dir.child_dir("agents", false).map_err(unavailable)?.ok_or(Refusal::Unavailable)?;
    let quarantine = home_dir.child_dir(lifecycle::QUARANTINE_DIR, false).map_err(unavailable)?.ok_or(Refusal::Unavailable)?;
    let metadata = quarantine.metadata().map_err(unavailable)?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 { return Err(Refusal::Unavailable); }
    Ok(Dirs { agents, quarantine })
}

/// `(dev, ino)` of `agents/<label>` when it is a real directory.
#[cfg(unix)]
fn live_root(dirs: &Dirs, label: &str) -> Result<Option<(u64, u64)>, Refusal> {
    use std::os::unix::fs::MetadataExt;
    match dirs.agents.entry(label).map_err(unavailable)? {
        None => Ok(None),
        Some(entry) if entry.kind != crate::fs_safe::EntryKind::Dir => Err(Refusal::Inconsistent),
        Some(_) => {
            let dir = dirs.agents.child_dir(label, false).map_err(unavailable)?.ok_or(Refusal::Unavailable)?;
            let metadata = dir.metadata().map_err(unavailable)?;
            Ok(Some((metadata.dev(), metadata.ino())))
        }
    }
}

#[cfg(unix)]
fn move_back(dirs: &Dirs, q: &Quarantined) -> Result<(), Refusal> {
    dirs.agents.rename_noreplace(&q.label, &dirs.quarantine, &q.name()).map_err(|_| Refusal::Inconsistent)
}

#[cfg(unix)]
fn apply_tx2(home: &Path, env: &RestoreEnv<'_>, request: &RestoreRequest, q: &Quarantined, approver: &str,
    dir: &crate::fs_safe::SafeDir, verified: &quarantine::Sealed) -> Result<Restored, Refusal> {
    let mut db = open_apply(home)?;
    db.execute_batch("PRAGMA synchronous=FULL;").map_err(unavailable)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    match apply_locked(home, env, &tx, request, q, approver, dir, verified) {
        Ok(restored) => {
            tx.commit().map_err(unavailable)?;
            if crash_at(HookPoint::AfterCommit) { return Err(Refusal::Unavailable); }
            Ok(restored)
        }
        Err(Step::Crash) => Err(Refusal::Unavailable),
        Err(Step::Refused(refusal, seal)) => {
            let marked = set_seal(&tx, q.employee, q.incarnation, &["restoring"], seal)
                .and_then(|()| set_request_state(&tx, &request.request_id, &["applying"], "failed", Some(refusal.code()), env.now));
            let written = match marked {
                Ok(()) => tx.commit().map_err(unavailable),
                Err(error) => Err(error),
            };
            audit(home, "isolation.employee_restore_failed", &q.label, Severity::Warning,
                audit_details(request, json!({"failure": refusal.code(), "seal": seal, "recorded": written.is_ok()})));
            Err(refusal)
        }
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn apply_locked(home: &Path, env: &RestoreEnv<'_>, tx: &Connection, request: &RestoreRequest, q: &Quarantined, approver: &str,
    dir: &crate::fs_safe::SafeDir, verified: &quarantine::Sealed) -> Result<Restored, Step> {
    let (receipt, encoded) = check_apply_db(tx, q, Stage::Apply(&request.request_id), env.now)
        .map_err(|r| Step::Refused(r, "sealed"))?;
    let root = q.root.ok_or(Step::Refused(Refusal::NotSealed, "sealed"))?;
    let dirs = open_dirs(home).map_err(|r| Step::Refused(r, "sealed"))?;
    // a. NOREPLACE move from held descriptors; the kernel refuses an
    //    existing `agents/<label>` atomically.
    dirs.quarantine.rename_noreplace(&q.name(), &dirs.agents, &q.label).map_err(|_| Step::Refused(Refusal::LabelTaken, "sealed"))?;
    let back = |refusal: Refusal, seal: &'static str| match move_back(&dirs, q) {
        Ok(()) => Step::Refused(refusal, seal),
        // Left in place: the row stays `restoring` and recovery decides.
        Err(_) => Step::Refused(Refusal::Inconsistent, "restoring"),
    };
    if live_root(&dirs, &q.label).map_err(|r| Step::Refused(r, "restoring"))? != Some(root) {
        return Err(back(Refusal::Integrity, "needs_reseal"));
    }
    if crash_at(HookPoint::AfterMove) { return Err(Step::Crash); }
    // b. Re-walk: inodes, sizes and ctimes as verified in step 1.
    let again = quarantine::seal(home, &q.label, dir, root);
    if again.state != quarantine::SealState::Sealed || again.stat_manifest != verified.stat_manifest {
        return Err(back(Refusal::Integrity, "needs_reseal"));
    }
    let binding = q.binding().map_err(|r| back(r, "sealed"))?;
    let routes = env.routes.routes_for(&binding, &q.tenant).map_err(|_| back(Refusal::Unavailable, "sealed"))?;
    if route_digest(&routes) != request.route_digest { return Err(back(Refusal::RoutesChanged, "sealed")); }
    check_store_marks(home, env, q).map_err(|r| back(r, "sealed"))?;
    // c. users.db (after the apply.db lock, M6).
    let approval = restore_users(home, env, request, q, approver).map_err(|r| back(r, "sealed"))?;
    if crash_at(HookPoint::AfterUsers) { return Err(Step::Crash); }
    // d. The confirmed row live again, incarnation + 1 (one row, exact match).
    let undo = |refusal: Refusal| {
        let removed = match &q.owner {
            Some(owner) => lifecycle::remove_personal_mirror(home, &q.label, owner).map(|_| ()),
            None => lifecycle::remove_shared_bindings(home, &q.label).map(|_| ()),
        };
        if removed.is_err() { Step::Refused(Refusal::Inconsistent, "restoring") } else { back(refusal, "sealed") }
    };
    let next = q.incarnation.checked_add(1).ok_or_else(|| undo(Refusal::Inconsistent))?;
    let live = Receipt { disabled: false, ..receipt };
    let live = serde_json::to_string(&live).map_err(|_| undo(Refusal::Unavailable))?;
    // Exact match on the text check_apply_db read in this same transaction.
    let changed = tx.execute("UPDATE confirmed_employees SET receipt=?1,incarnation=incarnation+1
        WHERE label=?2 AND receipt=?3 AND incarnation=?4", params![live, q.label, encoded, q.incarnation]);
    if !matches!(changed, Ok(1)) { return Err(undo(Refusal::Inconsistent)); }
    let finished = set_seal(tx, q.employee, q.incarnation, &["restoring"], "restored")
        .and_then(|()| tx.execute("UPDATE employee_quarantine SET closed_by_request=?3 WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation, request.request_id]).map(|_| ()).map_err(unavailable))
        .and_then(|()| set_request_state(tx, &request.request_id, &["applying"], "done", None, env.now))
        .and_then(|()| tx.execute("UPDATE employee_restores SET approver_user_id=?2,self_approved=?3 WHERE request_id=?1",
            params![request.request_id, approver, approval.self_approved]).map(|_| ()).map_err(unavailable));
    if let Err(refusal) = finished { return Err(undo(refusal)); }
    let request = RestoreRequest { state: "done".into(), self_approved: Some(approval.self_approved), ..request.clone() };
    Ok(Restored { request, incarnation: next, self_approved: approval.self_approved })
}

fn rank(level: duduclaw_auth::AccessLevel) -> u8 {
    use duduclaw_auth::AccessLevel::*;
    match level { Owner => 3, Operator => 2, Viewer => 1 }
}

/// Shared (§3.9, S11, M5): the digest-bound selection, each member from the
/// tombstone snapshot at no more than its snapshot level.
fn shared_selection(request: &RestoreRequest, q: &Quarantined) -> Result<Vec<duduclaw_auth::db::RestoredMember>, Refusal> {
    use duduclaw_auth::AccessLevel;
    let level = |value: &Value| value.as_str().and_then(|text| text.parse::<AccessLevel>().ok()).ok_or(Refusal::Inconsistent);
    let snapshot = q.principal_snapshot["members"].as_array().ok_or(Refusal::Inconsistent)?;
    let selection = request.member_selection.as_ref().and_then(Value::as_array).ok_or(Refusal::Inconsistent)?;
    selection.iter().map(|member| {
        let user = member["user_id"].as_str().ok_or(Refusal::Inconsistent)?;
        let wanted = level(&member["access_level"])?;
        let was = snapshot.iter().find(|old| old["user_id"].as_str() == Some(user)).ok_or(Refusal::Inconsistent)?;
        if rank(wanted) > rank(level(&was["access_level"])?) { return Err(Refusal::Inconsistent); }
        Ok(duduclaw_auth::db::RestoredMember { user_id: user.to_owned(), access_level: wanted })
    }).collect()
}

fn users_error(error: duduclaw_auth::db::PersonalAgentError) -> Refusal {
    use duduclaw_auth::db::PersonalAgentError as E;
    match error {
        E::Store(_) => Refusal::Unavailable,
        E::OwnerConflict | E::BindingConflict => Refusal::TombstoneIncomplete,
        E::UnknownOwner => Refusal::OwnerIneligible,
        E::InvalidAgent => Refusal::Inconsistent,
    }
}

/// §3.5 c: one users.db IMMEDIATE transaction — tenant, eligibility, the
/// approval rule (inside the same transaction as the writes) and, for a
/// self-approval, its users.db audit row, then the mirror or the selected
/// bindings with the ACL and policy epoch bumps.
fn restore_users(home: &Path, env: &RestoreEnv<'_>, request: &RestoreRequest, q: &Quarantined, approver: &str)
    -> Result<Approval, Refusal> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadWrite).map_err(from_lifecycle)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    if duduclaw_auth::isolation::read_isolation_tenant(&tx).map_err(unavailable)? != q.tenant { return Err(Refusal::TenantMismatch); }
    if let Some(owner) = &q.owner {
        if !lifecycle::owner_eligible(&tx, owner).map_err(from_lifecycle)? { return Err(Refusal::OwnerIneligible); }
    }
    let approval = decide(env.rule, &tx, &request.requester, approver, request.kind)?;
    // F6c2: the sole-Admin users.db audit row, written once, here — in the
    // write transaction of the decision (tx1's read-only check never writes).
    if approval.self_approved {
        env.rule.record_self_approval(&tx, &request.requester, approver, request.kind, &request.request_id)
            .map_err(Refusal::Approval)?;
    }
    match &q.owner {
        Some(owner) => duduclaw_auth::db::restore_personal_mirror_in_transaction(&tx, &q.label, owner).map_err(users_error)?,
        None => { duduclaw_auth::db::restore_shared_bindings_in_transaction(&tx, &q.label, &shared_selection(request, q)?)
            .map_err(users_error)?; }
    }
    tx.commit().map_err(unavailable)?;
    Ok(approval)
}

/// §3.5 step 4: the lifecycle must read the new incarnation, else alert and
/// fail closed (the employee stays unusable until an operator looks).
fn post_commit(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined, restored: Restored) -> Result<Restored, Refusal> {
    match lifecycle::read_lifecycle_with(home, &q.label, &ProbeRoot(env.root)) {
        Ok(current) if current.employee == q.employee && current.incarnation == restored.incarnation => {}
        other => {
            audit(home, "isolation.employee_restore_inconsistent", &q.label, Severity::Critical,
                audit_details(&restored.request, json!({"stage": "post_commit", "lifecycle": other.err().map(|e| e.to_string())})));
            return Err(Refusal::Inconsistent);
        }
    }
    audit(home, "isolation.employee_restored", &q.label, Severity::Info,
        audit_details(&restored.request, json!({"new_incarnation": restored.incarnation})));
    if restored.self_approved {
        audit(home, "isolation.employee_restore_self_approved", &q.label, Severity::Warning,
            audit_details(&restored.request, json!({"new_incarnation": restored.incarnation})));
    }
    Ok(restored)
}

// ── Crash recovery (§3.6) ──────────────────────────────────────────────

/// Run before any restore on `label` (and at boot for every label with a
/// `restoring` or `needs_reseal` row): expire stale requests, roll back or
/// forward interrupted applies, re-verify `needs_reseal` rows. A live row
/// without the request match alerts and refuses (`RESTORE_INCONSISTENT`);
/// nothing else then runs for the label.
#[cfg(unix)]
pub(crate) fn recover_label(home: &Path, env: &RestoreEnv<'_>, label: &str) -> Result<Recovered, Refusal> {
    let expired = expire_requests(home, env.now)?;
    let rows = { let db = open_apply(home)?; rows_for_label(&db, label)? };
    let mut recovered = Recovered { expired, ..Recovered::default() };
    for (employee, incarnation, seal) in rows {
        match seal.as_str() {
            "restoring" => {
                let (request, forward) = recover_restoring(home, env, employee, incarnation)?;
                if forward { recovered.rolled_forward.extend(request); } else {
                    recovered.rolled_back.extend(request);
                    recovered.resealed.push(reseal(home, employee, incarnation)?);
                }
            }
            "needs_reseal" => recovered.resealed.push(reseal(home, employee, incarnation)?),
            _ => {}
        }
    }
    Ok(recovered)
}

#[cfg(not(unix))]
pub(crate) fn recover_label(_: &Path, _: &RestoreEnv<'_>, _: &str) -> Result<Recovered, Refusal> { Err(Refusal::Unavailable) }

/// One `restoring` row. Returns the request id it settled and whether it
/// rolled forward.
#[cfg(unix)]
fn recover_restoring(home: &Path, env: &RestoreEnv<'_>, employee: EmployeeId, incarnation: u32)
    -> Result<(Option<String>, bool), Refusal> {
    let (q, applying) = {
        let db = open_apply(home)?;
        let q = read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)?;
        let applying = open_request(&db, employee, incarnation)?.filter(|request| request.state == "applying");
        (q, applying)
    };
    let (receipt, _, current) = lifecycle::confirmed_row(home, &q.label).map_err(from_lifecycle)?.ok_or(Refusal::Inconsistent)?;
    let request_id = applying.as_ref().map(|request| request.request_id.clone());
    let settle_row = |seal: &str, state: &str, failure: Option<&str>| -> Result<(), Refusal> {
        let mut db = open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        if seal != "restoring" { set_seal(&tx, employee, incarnation, &["restoring"], seal)?; }
        if let Some(id) = &request_id { set_request_state(&tx, id, &["applying"], state, failure, env.now)?; }
        tx.commit().map_err(unavailable)
    };
    let details = |action: &str| json!({"action": action, "request_id": request_id, "employee": hex::encode(employee.0),
        "incarnation": incarnation});
    if receipt.employee == q.employee && receipt.disabled && current == q.incarnation {
        // Roll back: tree into the quarantine, users.db side removed.
        let dirs = open_dirs(home)?;
        match (live_root(&dirs, &q.label)?, q.root) {
            (Some(found), Some(root)) if found == root => move_back(&dirs, &q)?,
            (None, Some(root)) => {
                use std::os::unix::fs::MetadataExt;
                let dir = dirs.quarantine.child_dir(&q.name(), false).map_err(unavailable)?.ok_or(Refusal::Inconsistent)?;
                let metadata = dir.metadata().map_err(unavailable)?;
                if (metadata.dev(), metadata.ino()) != root { return Err(Refusal::Inconsistent); }
            }
            _ => {
                audit(home, "isolation.employee_restore_inconsistent", &q.label, Severity::Critical, details("foreign_tree"));
                return Err(Refusal::Inconsistent);
            }
        }
        let removed = match &q.owner {
            Some(owner) => lifecycle::remove_personal_mirror(home, &q.label, owner).map(|_| ()),
            None => lifecycle::remove_shared_bindings(home, &q.label).map(|_| ()),
        };
        match removed {
            Ok(()) => {}
            Err(LifecycleError::Stale) => {
                audit(home, "isolation.employee_restore_inconsistent", &q.label, Severity::Critical, details("foreign_mirror"));
                return Err(Refusal::Inconsistent);
            }
            Err(_) => return Err(Refusal::Unavailable),
        }
        settle_row("needs_reseal", "failed", Some("interrupted"))?;
        audit(home, "isolation.employee_restore_recovered", &q.label, Severity::Warning, details("rolled_back"));
        return Ok((request_id, false));
    }
    let matched = request_id.is_some() && q.closed_by_request == request_id;
    if receipt.employee == q.employee && !receipt.disabled && Some(current) == q.incarnation.checked_add(1) && matched {
        settle_row("restored", "done", None)?;
        audit(home, "isolation.employee_restore_recovered", &q.label, Severity::Warning, details("rolled_forward"));
        return Ok((request_id, true));
    }
    if request_id.is_some() { settle_row("restoring", "failed", Some("inconsistent"))?; }
    audit(home, "isolation.employee_restore_inconsistent", &q.label, Severity::Critical, details("live_row_without_request_match"));
    Err(Refusal::Inconsistent)
}

/// `needs_reseal` (§3.4): re-verify against the original digest; equal ⇒
/// `sealed` (fresh stat manifest recorded), anything else ⇒ `invalid`.
#[cfg(unix)]
fn reseal(home: &Path, employee: EmployeeId, incarnation: u32) -> Result<(String, String), Refusal> {
    let q = { let db = open_apply(home)?; read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)? };
    let fresh = match (lifecycle::reopen_quarantined(home, &q.label, employee, incarnation).map_err(from_lifecycle)?, q.root) {
        (Some(dir), Some(root)) => Some(quarantine::seal(home, &q.label, &dir, root)),
        _ => None,
    };
    let sealed = fresh.as_ref().filter(|fresh| fresh.state == quarantine::SealState::Sealed && fresh.digest.is_some() && fresh.digest == q.digest);
    let state = if sealed.is_some() { "sealed" } else { "invalid" };
    {
        let mut db = open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        set_seal(&tx, employee, incarnation, &["needs_reseal"], state)?;
        if let Some(manifest) = sealed.and_then(|fresh| fresh.stat_manifest.as_ref()) {
            tx.execute("UPDATE employee_quarantine SET stat_manifest=?3 WHERE employee_hex=?1 AND incarnation=?2",
                params![hex::encode(employee.0), incarnation, manifest.to_string()]).map_err(unavailable)?;
        }
        tx.commit().map_err(unavailable)?;
    }
    audit(home, "isolation.quarantine_resealed", &q.label, if state == "sealed" { Severity::Info } else { Severity::Warning },
        json!({"employee": hex::encode(employee.0), "incarnation": incarnation, "seal": state}));
    Ok((format!("{}-{incarnation}", hex::encode(employee.0)), state.to_owned()))
}
