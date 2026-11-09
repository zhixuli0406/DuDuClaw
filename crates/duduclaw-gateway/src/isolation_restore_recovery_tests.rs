//! F6c crash recovery (§3.6, M6) and the audit trail (§3.8, M7). Child of
//! `isolation_restore_tests.rs` (same fixture and fakes). A "crash" is the
//! test hook returning `true`: the call returns at once without cleanup and
//! the open apply.db transaction rolls back, as on process death.
use super::*;

fn audit_log(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }

fn crash_at(point: HookPoint) { hook(move |at| at == point); }

/// Move requests out of the L1 30-day window so a test can file again.
fn age_requests(home: &Path) {
    sql(home, APPLY_DB, "UPDATE employee_restores SET created_at=created_at-40*86400", &[]);
}

fn resealed(state: &str) -> Vec<(String, String)> { vec![(format!("{}-0", hex::encode(EMP.0)), state.to_owned())] }

/// Crash right after the move: the tree is under `agents/` with a disabled
/// row (nothing runs); recovery moves it back, reseals and fails the request.
#[test]
fn restore_crash_rename() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    crash_at(HookPoint::AfterMove);
    let crashed = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(crashed.err(), Some(Refusal::Unavailable));
    assert!(home.join("agents").join(LABEL).join("SOUL.md").exists());
    assert_eq!(row(home, 0).seal, "restoring");
    assert_eq!(stored(home, &request.request_id).state, "applying");
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Tombstoned), "fails closed");
    assert_eq!(restore_in_progress(home, LABEL), Ok(true));
    let recovered = recover_label(home, &fx.env(), LABEL).unwrap();
    assert_eq!(recovered.rolled_back, vec![request.request_id.clone()]);
    assert_eq!(recovered.resealed, resealed("sealed"));
    assert!(in_quarantine(home));
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("interrupted")));
    assert_eq!(restore_in_progress(home, LABEL), Ok(false));
    let again = file(&f, &fx).unwrap();
    assert_eq!(approve(home, &fx.env(), &again.request_id, &f.bob).unwrap().incarnation, 1);
}

/// Crash after the users.db commit, before the apply.db commit: the mirror
/// exists but the row is disabled (reads `Tombstoned`); recovery removes the
/// mirror (the amended `remove_personal_mirror` contract) and moves back.
#[test]
fn restore_crash_users_db() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    crash_at(HookPoint::AfterUsers);
    let crashed = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(crashed.err(), Some(Refusal::Unavailable));
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Tombstoned), "a mirror without a live row");
    let generation = f.users.isolation_authority_snapshot(None, LABEL).unwrap().acl_generation;
    let recovered = recover_label(home, &fx.env(), LABEL).unwrap();
    assert_eq!((recovered.rolled_back.len(), recovered.resealed.clone()), (1, resealed("sealed")));
    assert_eq!(mirror(home, LABEL), None);
    assert!(f.users.get_agent_users(LABEL).unwrap().is_empty());
    assert!(f.users.isolation_authority_snapshot(None, LABEL).unwrap().acl_generation > generation);
    assert!(in_quarantine(home));
    let again = file(&f, &fx).unwrap();
    assert_eq!(approve(home, &fx.env(), &again.request_id, &f.bob).unwrap().incarnation, 1);
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])).unwrap().incarnation, 1);
}

/// Crash after the apply.db commit: the restore is complete; recovery has
/// nothing to undo. A `restoring` row whose live row matches the bound
/// request (an interrupted settle) rolls forward.
#[test]
fn restore_crash_commit() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    crash_at(HookPoint::AfterCommit);
    let crashed = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(crashed.err(), Some(Refusal::Unavailable));
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])).unwrap().incarnation, 1);
    assert_eq!(recover_label(home, &fx.env(), LABEL).unwrap(), Recovered::default());
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='restoring' WHERE incarnation=0", &[]);
    sql(home, APPLY_DB, "UPDATE employee_restores SET state='applying' WHERE request_id=?1", &[&request.request_id]);
    let recovered = recover_label(home, &fx.env(), LABEL).unwrap();
    assert_eq!(recovered.rolled_forward, vec![request.request_id.clone()]);
    assert_eq!(row(home, 0).seal, "restored");
    assert_eq!(stored(home, &request.request_id).state, "done");
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])).unwrap().incarnation, 1);
}

/// §3.6: a `restoring` row with a live confirmed row that is not bound to
/// the applying request alerts, fails the request, and blocks the label.
#[test]
fn restore_live_row_without_request_match_alerts() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    approve(home, &fx.env(), &request.request_id, &f.bob).unwrap();
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='restoring',closed_by_request=NULL WHERE incarnation=0", &[]);
    sql(home, APPLY_DB, "UPDATE employee_restores SET state='applying' WHERE request_id=?1", &[&request.request_id]);
    assert_eq!(recover_label(home, &fx.env(), LABEL).err(), Some(Refusal::Inconsistent));
    let log = audit_log(home);
    assert!(log.contains("isolation.employee_restore_inconsistent") && log.contains("live_row_without_request_match"), "{log}");
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("inconsistent")));
    assert_eq!(row(home, 0).seal, "restoring", "left for the operator");
    // Nothing else runs for the label.
    assert_eq!(recover_label(home, &fx.env(), LABEL).err(), Some(Refusal::Inconsistent));
    assert_eq!(restore_in_progress(home, LABEL), Ok(true));
}

/// M7: every transition goes through the security audit with the request
/// id: request, cancel, deny, expire, apply failure, approve, crash
/// recovery, reseal, restore and self-approval.
#[test]
fn restore_every_transition_audited() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let mut ids = Vec::new();
    let cancelled = file(&f, &fx).unwrap();
    close_request(home, &cancelled.request_id, &f.alice, Close::Cancelled, now()).unwrap();
    assert_eq!(stored(home, &cancelled.request_id).state, "cancelled");
    ids.push(cancelled.request_id);
    age_requests(home);
    let denied = file(&f, &fx).unwrap();
    close_request(home, &denied.request_id, &f.bob, Close::Denied, now()).unwrap();
    assert_eq!(close_request(home, &denied.request_id, &f.bob, Close::Denied, now()).err(), Some(Refusal::NotFound));
    ids.push(denied.request_id);
    age_requests(home);
    let expired = file(&f, &fx).unwrap();
    assert_eq!(expire_requests(home, now() + REQUEST_TTL_SECS + 1).unwrap(), 1);
    ids.push(expired.request_id);
    age_requests(home);
    let failed = file(&f, &fx).unwrap();
    fx.routes.0.borrow_mut().push(("acct-new".into(), 1));
    assert_eq!(approve(home, &fx.env(), &failed.request_id, &f.bob).err(), Some(Refusal::RoutesChanged));
    ids.push(failed.request_id);
    age_requests(home);
    let interrupted = file(&f, &fx).unwrap();
    crash_at(HookPoint::AfterMove);
    assert!(approve(home, &fx.env(), &interrupted.request_id, &f.bob).is_err());
    unhook();
    recover_label(home, &fx.env(), LABEL).unwrap();
    ids.push(interrupted.request_id);
    age_requests(home);
    // The F6c2 stand-in lets the requester approve: audited as self-approved.
    let done = file(&f, &fx).unwrap();
    let restored = approve(home, &fx.env(), &done.request_id, &f.alice).unwrap();
    assert!(restored.self_approved);
    ids.push(done.request_id);
    let log = audit_log(home);
    for event in ["isolation.employee_tombstoned", "isolation.employee_restore_requested", "isolation.employee_restore_cancelled",
        "isolation.employee_restore_denied", "isolation.employee_restore_expired", "isolation.employee_restore_failed",
        "isolation.employee_restore_approved", "isolation.employee_restore_recovered", "isolation.quarantine_resealed",
        "isolation.employee_restored", "isolation.employee_restore_self_approved"] {
        assert!(log.contains(event), "{event} missing");
    }
    for id in &ids { assert!(log.contains(id.as_str()), "{id} missing"); }
    assert!(log.contains(&hex::encode(EMP.0)));
}
