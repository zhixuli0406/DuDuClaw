//! Security round 3 for F6 part (a): the adoption commit re-runs the owner
//! and approval checks (N1), the card's step-up freshness comes from the
//! call's proof (N2), and the owner cancels an Admin-filed request with a
//! password re-auth (N3). Child of `isolation_adopt_tests.rs` (same fixtures:
//! alice's Personal employee tombstoned with no second factor, real stores,
//! fake schema-2 root answering `NotFound`).
use super::*;
use crate::isolation_restore_rpc::{self as rpc, RpcError};
use duduclaw_auth::admin_approval_rule::ApprovalRefusal;
use duduclaw_auth::mfa::{OwnerFactor, OwnerFactorError};
use std::cell::Cell;

/// The third qualifying Admin (neither filer nor owner) approving the card.
const CAROL: &str = "carol-admin";

fn employee() -> String { hex::encode(EMP.0) }

fn audit_log(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }

/// bob (an Admin) files the Personal adoption for alice, who had no second
/// factor at the tombstone, and proposes it.
fn admin_filed(f: &Fixture, ax: &Ax) -> (RestoreRequest, AdoptionPlan, ConfirmationItem) {
    let filer = crate::isolation_restore::AdminFiler { admin_user_id: &f.bob, fresh_step_up: true };
    let request = file_adoption(f.home(), &env(ax), EMP, 0, None, Filer::AdminForOwner(filer)).unwrap();
    assert!(request.owner_unverified());
    let (plan, item) = propose(f, ax, &request);
    (request, plan, item)
}

/// The apply step with these collaborators and approver (production save with the N1 gate).
fn apply_as(f: &Fixture, env: &AdoptEnv<'_>, item: &ConfirmationItem, approver: &str) -> (Result<Adopted, AdoptError>, Receipt) {
    let home = f.home();
    let new = receipt(item, 9, false);
    let mut persist = |gate: &dyn Fn(&Connection) -> Result<(), ()>| save_gated(home, item, &new, gate);
    (apply_item(home, env, item, &new, approver, &mut persist), new)
}

/// The Admin-side password writer for alice (unattributed: not the owner's session).
fn reset_owner_password(f: &Fixture) -> Result<(), ()> {
    f.users.update_user(&f.alice, None, None, Some("admin-set-password")).map(|_| ()).map_err(|_| ())
}

/// The refused adoption left nothing behind: the label's row is the old,
/// disabled id, no mirror, no marker, the tree back in the sealed row, the
/// old root still `allocated`, the request `failed: <code>`.
fn assert_nothing_committed(f: &Fixture, request: &RestoreRequest, code: &str) {
    let home = f.home();
    let (row, _, _) = confirmed_row(home, LABEL).unwrap().unwrap();
    assert_eq!((row.employee, row.disabled), (EMP, true), "never saved");
    assert_eq!(mirror(home, LABEL), None);
    assert!(marker(&Connection::open(home.join(APPLY_DB)).unwrap(), LABEL).unwrap().is_none());
    assert!(quarantined(home, "SOUL.md") && !home.join("agents").join(LABEL).exists());
    assert_eq!((seal_of(home).as_str(), root_state(home).as_str()), ("sealed", "allocated"));
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some(code)));
    assert!(!audit_log(home).contains("\"isolation.employee_adopted\""));
}

/// N1 scenario: an Admin files the adoption for a no-second-factor owner, a
/// third Admin approves, and the filer resets the owner's password during the
/// transform (after every check of the validation passed). The check inside
/// the mirror's users.db write transaction refuses: nothing is committed.
#[test]
fn f6r3_admin_filed_password_reset_during_transform_refused() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (request, plan, item) = admin_filed(&f, &ax);
    // The card validates for the third Admin before the reset.
    assert!(matches!(validate_item(f.home(), &env(&ax), &item, &plan.adopt, CAROL),
        Ok(AdoptPhase::Pending { self_approved: false, .. })));
    let reset = |_: &Path| -> Result<(), ()> { reset_owner_password(&f) };
    let during = AdoptEnv { hooks: &reset, ..env(&ax) };
    let (result, _) = apply_as(&f, &during, &item, CAROL);
    assert_eq!(result.err(), Some(AdoptError::Restore(Refusal::CredentialChanged)));
    assert_nothing_committed(&f, &request, Refusal::CredentialChanged.code());
}

/// N1, resumed `applying` stage: the first stage died right after the move
/// (request `applying`, which used to skip the credential check). (a) The
/// reset lands during the resumed transform: refused in the save's users.db
/// transaction. (b) The reset lands between the interrupted stage and its
/// retry: the resumed validation refuses before anything is saved.
#[test]
fn f6r3_resumed_applying_rechecks_owner() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (request, _, item) = admin_filed(&f, &ax);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterMove)));
    let (crashed, _) = apply_as(&f, &env(&ax), &item, CAROL);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    assert_eq!(stored(f.home(), &request.request_id).state, "applying");
    let reset = |_: &Path| -> Result<(), ()> { reset_owner_password(&f) };
    let during = AdoptEnv { hooks: &reset, ..env(&ax) };
    let (result, _) = apply_as(&f, &during, &item, CAROL);
    assert_eq!(result.err(), Some(AdoptError::Restore(Refusal::CredentialChanged)));
    assert_nothing_committed(&f, &request, Refusal::CredentialChanged.code());

    let g = fixture();
    tombstoned_full(&g, "soul");
    let gx = ax();
    let (request, plan, item) = admin_filed(&g, &gx);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterMove)));
    let (crashed, _) = apply_as(&g, &env(&gx), &item, CAROL);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    reset_owner_password(&g).unwrap();
    assert_eq!(validate_item(g.home(), &env(&gx), &item, &plan.adopt, CAROL).err(),
        Some(AdoptError::Restore(Refusal::CredentialChanged)), "the resumed card no longer validates");
    let (result, _) = apply_as(&g, &env(&gx), &item, CAROL);
    assert_eq!(result.err(), Some(AdoptError::Restore(Refusal::CredentialChanged)));
    let (row, _, _) = confirmed_row(g.home(), LABEL).unwrap().unwrap();
    assert_eq!((row.employee, row.disabled), (EMP, true), "never saved");
    assert_eq!(mirror(g.home(), LABEL), None);
    assert_eq!(root_state(g.home()), "allocated");
    assert_ne!(stored(g.home(), &request.request_id).state, "done");
    // The store's compensation rolls the moved tree back.
    compensate_item(g.home(), &env(&gx), &item).unwrap();
    assert!(quarantined(g.home(), "SOUL.md") && !g.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(g.home()), "sealed");
}

/// N1, the commit itself: a non-owner credential change that lands after
/// `save()` (between the mirror and the commit) is refused by the re-check
/// the commit runs under its apply.db lock: the row is never `adopted`, the
/// old root never `root_absent`; the store's compensation then disables the
/// saved allocation and its mirror.
#[test]
fn f6r3_credential_change_before_commit_refused() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (request, _, item) = admin_filed(&f, &ax);
    let (home, alice) = (f.home().to_path_buf(), f.alice.clone());
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(move |point| {
        if point == AdoptHook::AfterSave {
            Connection::open(home.join(USERS_DB)).unwrap().execute("INSERT INTO credential_changes(user_id,field,actor_user_id,
                owner_authenticated,at) VALUES(?1,'password',NULL,0,?2)", rusqlite::params![alice, now()]).unwrap();
        }
        false
    })));
    let (result, new) = apply_as(&f, &env(&ax), &item, CAROL);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(result.err(), Some(AdoptError::Restore(Refusal::CredentialChanged)));
    assert_eq!((seal_of(f.home()).as_str(), root_state(f.home()).as_str()), ("sealed", "allocated"));
    let failed = stored(f.home(), &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some(Refusal::CredentialChanged.code())));
    assert!(quarantined(f.home(), "SOUL.md"));
    assert!(!audit_log(f.home()).contains("\"isolation.employee_adopted\""));
    save(f.home(), &item, &Receipt { disabled: true, ..new }).unwrap();
    compensate_item(f.home(), &env(&ax), &item).unwrap();
    assert_eq!(mirror(f.home(), LABEL), None);
}

/// A `save` that never runs the gate is refused (the commit never trusts a
/// save that skipped the re-check).
#[test]
fn f6r3_save_without_gate_refused() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (_, item) = propose(&f, &ax, &request);
    let home = f.home();
    let new = receipt(&item, 9, false);
    let mut ungated = |_: &dyn Fn(&Connection) -> Result<(), ()>| save(home, &item, &new).map_err(|_| ());
    assert_eq!(apply_item(home, &env(&ax), &item, &new, &f.bob, &mut ungated).err(), Some(AdoptError::Inconsistent));
    assert_ne!(seal_of(home), "adopted");
    assert_eq!(stored(home, &request.request_id).state, "failed");
}

/// F6c2 stand-in that refuses without a fresh step-up (as `EligibilityLogRule`).
struct StepUpRule;
impl AdminApprovalRule for StepUpRule {
    fn decide(&self, users: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        if !request.fresh_step_up { return ApprovalDecision::Refused(ApprovalRefusal::StepUpRequired); }
        AnyAdmin.decide(users, request)
    }
    fn decide_admin_filed(&self, users: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        if !request.fresh_step_up { return ApprovalDecision::Refused(ApprovalRefusal::StepUpRequired); }
        AnyAdmin.decide_admin_filed(users, request)
    }
    fn check_admin_filing(&self, users: &Connection, filer: &str, owner: &str, now: i64) -> Result<(), ApprovalRefusal> {
        AnyAdmin.check_admin_filing(users, filer, owner, now)
    }
}

/// N2: the adoption card's `fresh_step_up` is computed from the call's proof
/// `verified_at` (`step_up_fresh`), never hard-coded: no proof, a stale one or
/// one from the future refuses `STEP_UP_REQUIRED` at validation and apply
/// (nothing moves); a proof within the TTL approves and commits. Boot
/// recovery carries no proof.
#[test]
fn f6r3_adoption_card_uses_real_step_up() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    let now = u64::try_from(ax.now).unwrap();
    let ttl = u64::try_from(duduclaw_auth::mfa::TTL).unwrap();
    for (verified_at, why) in [(None, "no proof"), (Some(now - ttl - 1), "stale"), (Some(now + 5), "future")] {
        let gated = AdoptEnv { rule: &StepUpRule, step_up_verified_at: verified_at, ..env(&ax) };
        assert_eq!(validate_item(f.home(), &gated, &item, &plan.adopt, &f.bob).err(),
            Some(AdoptError::Approval("STEP_UP_REQUIRED")), "{why}");
    }
    let unproven = AdoptEnv { rule: &StepUpRule, step_up_verified_at: None, ..env(&ax) };
    let (refused, _) = apply_as(&f, &unproven, &item, &f.bob);
    assert_eq!(refused.err(), Some(AdoptError::Approval("STEP_UP_REQUIRED")));
    assert!(quarantined(f.home(), "SOUL.md"));
    assert_eq!(stored(f.home(), &request.request_id).state, "requested");
    let fresh = AdoptEnv { rule: &StepUpRule, step_up_verified_at: Some(now - ttl + 1), ..env(&ax) };
    assert!(matches!(validate_item(f.home(), &fresh, &item, &plan.adopt, &f.bob), Ok(AdoptPhase::Pending { .. })));
    let (adopted, _) = apply_as(&f, &fresh, &item, &f.bob);
    assert_eq!(adopted.unwrap().employee, NEW);
    crate::isolation_adopt::with_production_env(f.home(), "system", ax.now, |env| assert_eq!(env.step_up_verified_at, None));
}

/// N3: the owner of an Admin-filed (`owner_unverified`) request cancels it
/// with a current session + password re-auth (`cancel_own_unverified`); the
/// factor path stays closed to them (requester = the Admin), every other
/// caller or request answers `NOT_FOUND` without a re-auth, a failed re-auth
/// is audited and cancels nothing, the cancel is audited and notified
/// (uncapped).
#[test]
fn f6r3_owner_cancels_admin_filed_request_with_password() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let filed = rpc::adoption_request_for_owner(f.home(), &env(&ax), &f.bob, &employee(), 0, None, true).unwrap();
    let id = filed["request_id"].as_str().unwrap().to_owned();
    let any_factor = |factor: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { Ok(factor) };
    assert_eq!(rpc::cancel_own(f.home(), &f.alice, &id, "totp", "1", &any_factor, now()).err(),
        Some(RpcError::Restore(Refusal::NotFound)), "no factor path for an Admin-filed request");
    let asked = Cell::new(0);
    let reauth_ok = || -> Result<(), OwnerFactorError> { asked.set(asked.get() + 1); Ok(()) };
    for caller in [f.bob.as_str(), CAROL, "", "system", &f.alice.to_uppercase()] {
        assert_eq!(rpc::cancel_own_unverified(f.home(), caller, &id, &reauth_ok, now()).err(),
            Some(RpcError::Restore(Refusal::NotFound)), "{caller}");
    }
    assert_eq!(rpc::cancel_own_unverified(f.home(), &f.alice, "zz", &reauth_ok, now()).err(), Some(RpcError::Invalid));
    assert_eq!(asked.get(), 0, "no re-auth for a request the caller may not cancel");
    let reauth_failed = || -> Result<(), OwnerFactorError> { Err(OwnerFactorError::Failed) };
    assert_eq!(rpc::cancel_own_unverified(f.home(), &f.alice, &id, &reauth_failed, now()).err(),
        Some(RpcError::Factor(OwnerFactorError::Failed)));
    assert_eq!(stored(f.home(), &id).state, "requested");
    assert!(audit_log(f.home()).contains("isolation.employee_restore_owner_cancel_reauth_failed"));
    assert_eq!(rpc::cancel_own_unverified(f.home(), &f.alice, &id, &reauth_ok, now()).unwrap()["state"], "cancelled");
    assert_eq!(asked.get(), 1);
    assert_eq!(stored(f.home(), &id).state, "cancelled");
    assert!(audit_log(f.home()).contains("isolation.employee_restore_owner_cancelled_unverified"));
    let uncapped: i64 = Connection::open(f.home().join(APPLY_DB)).unwrap().query_row(
        "SELECT count(*) FROM employee_restore_notices WHERE event='isolation.employee_restore_cancelled' AND capped=0",
        [], |r| r.get(0)).unwrap();
    assert_eq!(uncapped, 1);
    // An owner-filed request is never cancelled this way (it needs the factor).
    let own = file_personal(&f, &ax, None).unwrap();
    assert_eq!(rpc::cancel_own_unverified(f.home(), &f.alice, &own.request_id, &reauth_ok, now()).err(),
        Some(RpcError::Restore(Refusal::NotFound)));
}
