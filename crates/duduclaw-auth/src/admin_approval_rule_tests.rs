//! F6c2 approval rule (R1, R2, S1, S2, S16): second qualifying Admin,
//! sole-Admin self-approval with step-up, cooling, audit and log notices.
//! Log history older than "now" is made with `age_log` (shifts every row and
//! the creation marker into the past); writers then act at the real clock.
use super::*;
use super::ApprovalKind::*;
use crate::admin_eligibility::writer_tests::{
    admin, age_log, eligible_admin, enroll_api, enroll_raw, exec, last, now_secs, open, Cipher, DAY, PASSWORD, SESSION,
};
use crate::mfa::FirstFactor;
use crate::{UserDb, UserRole, UserStatus, UserUpdate};

fn request<'a>(kind: ApprovalKind, requester: &'a str, approver: &'a str) -> ApprovalRequest<'a> {
    ApprovalRequest {
        kind,
        requester_user_id: requester,
        owner_user_id: kind.is_personal().then_some(requester),
        approver_user_id: approver,
        fresh_step_up: true,
        now: now_secs(),
    }
}

/// Decide inside a (read) transaction, as the apply path does.
fn decide_in_tx(db: &UserDb, request: &ApprovalRequest<'_>) -> ApprovalDecision {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    let decision = decide(&tx, request);
    drop(tx);
    decision
}

fn refused(db: &UserDb, request: &ApprovalRequest<'_>) -> ApprovalRefusal {
    match decide_in_tx(db, request) {
        ApprovalDecision::Refused(refusal) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// `(self_approved, notice_addresses)` of an approval.
fn allowed(db: &UserDb, request: &ApprovalRequest<'_>) -> (bool, Vec<String>) {
    match decide_in_tx(db, request) {
        ApprovalDecision::Allowed { self_approved, notice_addresses } => (self_approved, notice_addresses),
        other => panic!("expected an approval, got {other:?}"),
    }
}

fn employee(db: &UserDb, email: &str) -> String {
    db.create_user(email, "Staff", PASSWORD, UserRole::Employee).unwrap().id
}

fn set_role(db: &UserDb, actor: Option<&str>, user: &str, role: UserRole) {
    db.apply_user_update_as(actor, user, &UserUpdate { role: Some(role), ..Default::default() }).unwrap();
}

fn eligible(db: &UserDb, user: &str) -> bool { crate::admin_eligibility::is_eligible(&db.conn(), user).unwrap() }

/// Two Admins, both eligible for the last 30 days.
fn two_admins() -> (tempfile::TempDir, UserDb, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 30 * DAY);
    (dir, db, a, b)
}

#[test]
fn owner_admin_cannot_self_approve_with_other_admin() {
    let (_dir, db, a, b) = two_admins();
    for kind in [PersonalRestore, PersonalAdopt] {
        assert_eq!(refused(&db, &request(kind, &a, &a)), ApprovalRefusal::SecondAdminRequired, "{kind:?}");
        assert!(!allowed(&db, &request(kind, &a, &b)).0, "{kind:?}: approved by the other Admin");
    }
    assert_eq!(ApprovalRefusal::SecondAdminRequired.code(), "SECOND_ADMIN_REQUIRED");
    // The owner of a Personal row is a party on an early purge filed by
    // another Admin: it may not approve while another Admin (b) qualifies.
    let c = eligible_admin(&db, "c@example.test");
    let owner_approves = ApprovalRequest { owner_user_id: Some(c.as_str()), ..request(EarlyPurge, &a, &c) };
    assert_eq!(refused(&db, &owner_approves), ApprovalRefusal::SecondAdminRequired);
    let other_approves = ApprovalRequest { owner_user_id: Some(c.as_str()), ..request(EarlyPurge, &a, &b) };
    assert!(!allowed(&db, &other_approves).0);
}

#[test]
fn shared_requester_cannot_self_approve() {
    let (_dir, db, a, b) = two_admins();
    for kind in [SharedRestore, SharedAdopt, EarlyPurge, EarlyPurgedRetire] {
        assert_eq!(refused(&db, &request(kind, &a, &a)), ApprovalRefusal::SecondAdminRequired, "{kind:?}");
        let (self_approved, notices) = allowed(&db, &request(kind, &a, &b));
        assert!(!self_approved, "{kind:?}");
        assert_eq!(notices, vec!["a@example.test", "b@example.test"], "{kind:?}");
    }
}

#[test]
fn sole_admin_self_approves_with_step_up_and_audit() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    age_log(&db, 30 * DAY);
    let without_step_up = ApprovalRequest { fresh_step_up: false, ..request(PersonalRestore, &a, &a) };
    assert_eq!(refused(&db, &without_step_up), ApprovalRefusal::StepUpRequired);
    let personal = request(PersonalRestore, &a, &a);
    assert_eq!(allowed(&db, &personal), (true, vec!["a@example.test".to_owned()]));
    for kind in [SharedRestore, SharedAdopt, PersonalAdopt, EarlyPurge, EarlyPurgedRetire] {
        assert!(allowed(&db, &request(kind, &a, &a)).0, "{kind:?}");
    }
    // Decision and audit row share the decision transaction.
    {
        let mut conn = db.conn();
        let tx = conn.transaction().unwrap();
        let rule: &dyn AdminApprovalRule = &EligibilityLogRule;
        assert!(rule.decide(&tx, &personal).self_approved());
        record_self_approval(&tx, &personal, "req-1").unwrap();
        tx.commit().unwrap();
    }
    let audit = db.query_audit_log(Some(&a), Some(SELF_APPROVED_AUDIT_ACTION), 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].target.as_deref(), Some("req-1"));
    let detail: serde_json::Value = serde_json::from_str(audit[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["self_approved"], true);
    assert_eq!(detail["kind"], "personal_restore");
    assert_eq!(detail["approver"], a.as_str());
    // The audit row is refused outside a transaction or with a bad subject.
    assert!(record_self_approval(&db.conn(), &personal, "req-2").is_err());
    {
        let mut conn = db.conn();
        let tx = conn.transaction().unwrap();
        assert!(record_self_approval(&tx, &personal, "").is_err());
        assert!(record_self_approval(&tx, &personal, "bad\nid").is_err());
    }
}

#[test]
fn inactive_or_no_mfa_admin_not_counted() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let no_mfa = admin(&db, "nomfa@example.test");
    let suspended = eligible_admin(&db, "suspended@example.test");
    db.set_user_status(&suspended, UserStatus::Suspended).unwrap();
    let offboarded = eligible_admin(&db, "offboarded@example.test");
    db.offboard_user(&offboarded, None, None).unwrap();
    let forced = eligible_admin(&db, "forced@example.test");
    exec(&db, "UPDATE users SET must_change_password=1 WHERE id=?1", &forced);
    age_log(&db, 30 * DAY);
    // None of them counts: a is the sole Admin (their losses are 30 days old).
    assert!(allowed(&db, &request(SharedRestore, &a, &a)).0);
    for other in [&no_mfa, &suspended, &offboarded, &forced] {
        assert_eq!(refused(&db, &request(SharedRestore, &a, other)),
            ApprovalRefusal::ApproverNotQualified(NotQualified::NotEligibleNow));
    }
    assert_eq!(ApprovalRefusal::ApproverNotQualified(NotQualified::NotEligibleNow).code(), "APPROVER_NOT_QUALIFIED");
}

#[test]
fn self_approval_refused_within_cooling_window() {
    let (_dir, db, a, b) = two_admins();
    set_role(&db, Some(&a), &b, UserRole::Employee);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::CoolingWindow);
    assert_eq!(ApprovalRefusal::CoolingWindow.code(), "SELF_APPROVAL_COOLING");
    // Every loss counts: promoting b back does not end the window, and b,
    // re-promoted by the requester, cannot approve either.
    set_role(&db, Some(&a), &b, UserRole::Admin);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::CoolingWindow);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &b)),
        ApprovalRefusal::ApproverNotQualified(NotQualified::PromotedByParty));
    // After 7 days b qualifies again, so a needs b.
    age_log(&db, 8 * DAY);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::SecondAdminRequired);
    assert!(!allowed(&db, &request(SharedRestore, &a, &b)).0);
    // A suspension that lasts: refused for 7 days, then sole Admin.
    db.set_user_status_as(Some(&a), &b, UserStatus::Suspended).unwrap();
    assert_eq!(refused(&db, &request(EarlyPurge, &a, &a)), ApprovalRefusal::CoolingWindow);
    age_log(&db, 6 * DAY);
    assert_eq!(refused(&db, &request(EarlyPurge, &a, &a)), ApprovalRefusal::CoolingWindow);
    age_log(&db, 2 * DAY);
    assert!(allowed(&db, &request(EarlyPurge, &a, &a)).0);
}

#[test]
fn admin_count_read_inside_apply_tx() {
    let (_dir, db, a, b) = two_admins();
    let self_approval = request(SharedRestore, &a, &a);
    assert_eq!(decide(&db.conn(), &self_approval).refusal(), Some(ApprovalRefusal::NotInTransaction),
        "an autocommit connection is not the apply transaction");
    {
        let mut conn = db.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        assert_eq!(decide(&tx, &self_approval).refusal(), Some(ApprovalRefusal::SecondAdminRequired));
        // A change earlier in the same transaction is what the rule counts.
        tx.execute("UPDATE users SET status='suspended' WHERE id=?1", [&b]).unwrap();
        assert_eq!(decide(&tx, &self_approval).refusal(), Some(ApprovalRefusal::CoolingWindow));
        assert_eq!(decide(&tx, &request(SharedRestore, &a, &b)).refusal(),
            Some(ApprovalRefusal::ApproverNotQualified(NotQualified::NotEligibleNow)));
        tx.rollback().unwrap();
    }
    assert_eq!(refused(&db, &self_approval), ApprovalRefusal::SecondAdminRequired);
    assert!(!allowed(&db, &request(SharedRestore, &a, &b)).0);
}

#[test]
fn self_approval_refused_after_forced_pw_or_mfa_reset_within_7d() {
    for case in ["forced_password", "operator_reset", "admin_reset", "own_mfa_disable", "passkey_removed"] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let a = eligible_admin(&db, "a@example.test");
        let b = admin(&db, "b@example.test");
        let codes = match case {
            "passkey_removed" => {
                exec(&db, "INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('pk-1',?1,'key','{}',0)", &b);
                Vec::new()
            }
            "own_mfa_disable" => enroll_api(&db, &b),
            _ => { enroll_raw(&db, &b); Vec::new() }
        };
        age_log(&db, 30 * DAY);
        assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::SecondAdminRequired, "{case}");
        match case {
            "forced_password" => exec(&db, "UPDATE users SET must_change_password=1 WHERE id=?1", &b),
            "operator_reset" => db.mfa_operator_reset("b@example.test").unwrap(),
            "admin_reset" => {
                let token = db.mfa_factor_password_challenge(&a, PASSWORD, SESSION, 3000).unwrap();
                db.mfa_admin_reset_reauth(&a, &b, &token, SESSION, 3000).unwrap();
            }
            "own_mfa_disable" => { db.mfa_change(&b, &codes[0], true, &Cipher, 3000).unwrap(); }
            "passkey_removed" => {
                let token = db.mfa_factor_password_challenge(&b, PASSWORD, SESSION, 3000).unwrap();
                db.mfa_passkey_delete(&b, "pk-1", &token, SESSION, 3000, false, |_, _, codes| Ok(codes)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(!eligible(&db, &b), "{case}");
        assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::CoolingWindow, "{case}");
        age_log(&db, 8 * DAY);
        assert!(allowed(&db, &request(SharedRestore, &a, &a)).0, "{case}: sole Admin after the window");
    }
}

#[test]
fn self_approval_refused_after_admin_delete_within_7d() {
    let (_dir, db, a, b) = two_admins();
    exec(&db, "DELETE FROM users WHERE id=?1", &b);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::CoolingWindow);
    assert_eq!(refused(&db, &request(PersonalRestore, &a, &a)), ApprovalRefusal::CoolingWindow);
    age_log(&db, 8 * DAY);
    assert!(allowed(&db, &request(SharedRestore, &a, &a)).0);
}

#[test]
fn lockout_does_not_reduce_admin_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let b = admin(&db, "b@example.test");
    enroll_api(&db, &b);
    age_log(&db, 30 * DAY);
    let challenge = db.mfa_challenge(&b, FirstFactor::Password, false, 3000).unwrap();
    for _ in 0..5 { assert!(db.mfa_verify(&challenge, "abcdef", false, &Cipher, 3000).is_err()); }
    let locked = last(&db, &b);
    assert_eq!((locked.reason.as_str(), locked.eligible), ("lockout", true));
    // b still counts and still qualifies: a cannot self-approve, b can approve.
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::SecondAdminRequired);
    assert!(!allowed(&db, &request(SharedRestore, &a, &b)).0);
}

#[test]
fn self_approval_refused_when_log_unreadable_or_under_7d() {
    {
        // The log was created by this upgrade: it covers less than 7 days.
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let a = eligible_admin(&db, "a@example.test");
        assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::AdminLogUnavailable);
        assert_eq!(ApprovalRefusal::AdminLogUnavailable.code(), "ADMIN_LOG_UNAVAILABLE");
        age_log(&db, 6 * DAY);
        assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::AdminLogUnavailable);
        age_log(&db, 2 * DAY);
        assert!(allowed(&db, &request(SharedRestore, &a, &a)).0);
    }
    // Every unreadable or untrustworthy log refuses, for self-approval and
    // for a second Admin alike (qualification is read from the log).
    for breakage in [
        "DELETE FROM auth_meta WHERE key='admin_eligibility_log_since'",
        "UPDATE auth_meta SET value=99 WHERE key='admin_eligibility_log_schema'",
        "DROP TRIGGER admin_eligibility_user_update",
        "DROP TRIGGER admin_eligibility_mfa_passkeys_delete",
        "DROP TRIGGER admin_eligibility_log_no_delete",
        "DROP TABLE admin_eligibility_log",
        // The latest row of an eligible account says it is not eligible.
        "INSERT INTO admin_eligibility_log(user_id,eligible,reason,actor_user_id,notice_address,at)
         SELECT id,0,'role',NULL,email,CAST(strftime('%s','now') AS INTEGER) FROM users WHERE email='b@example.test'",
    ] {
        let (_dir, db, a, b) = two_admins();
        db.conn().execute_batch(breakage).unwrap();
        assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::AdminLogUnavailable, "{breakage}");
        assert_eq!(refused(&db, &request(SharedRestore, &a, &b)), ApprovalRefusal::AdminLogUnavailable, "{breakage}");
    }
}

#[test]
fn recently_promoted_admin_cannot_approve() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let c = eligible_admin(&db, "c@example.test");
    let b = employee(&db, "b@example.test");
    enroll_raw(&db, &b);
    let d = admin(&db, "d@example.test");
    age_log(&db, 30 * DAY);
    // c, not the requester, promotes b today; d (Admin for 30 days) enrols MFA today.
    set_role(&db, Some(&c), &b, UserRole::Admin);
    enroll_raw(&db, &d);
    for approver in [&b, &d] {
        assert_eq!(refused(&db, &request(SharedRestore, &a, approver)),
            ApprovalRefusal::ApproverNotQualified(NotQualified::EligibleUnderCooling));
    }
    assert!(!allowed(&db, &request(SharedRestore, &a, &c)).0);
    age_log(&db, 8 * DAY);
    for approver in [&b, &d] {
        assert!(!allowed(&db, &request(SharedRestore, &a, approver)).0);
    }
}

#[test]
fn admin_promoted_by_requester_cannot_approve() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let b = employee(&db, "b@example.test");
    let e = employee(&db, "e@example.test");
    enroll_raw(&db, &b);
    enroll_raw(&db, &e);
    age_log(&db, 30 * DAY);
    let promoted_by_party = ApprovalRefusal::ApproverNotQualified(NotQualified::PromotedByParty);
    set_role(&db, Some(&a), &b, UserRole::Admin);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &b)), promoted_by_party);
    assert_eq!(refused(&db, &request(PersonalRestore, &a, &b)), promoted_by_party);
    // An unattributed promotion counts as possibly by the requester.
    set_role(&db, None, &e, UserRole::Admin);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &e)), promoted_by_party);
    // So does an Admin the requester created.
    let f = db.create_user_as(Some(&a), &crate::NewUser { email: "f@example.test", display_name: "F", password: PASSWORD,
        role: UserRole::Admin, department: None, manager: None }, false).unwrap().id;
    enroll_raw(&db, &f);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &f)), promoted_by_party);
    // None qualifies, so a is the sole Admin: self-approval, and every one
    // of them is told.
    let (self_approved, notices) = allowed(&db, &request(SharedRestore, &a, &a));
    assert!(self_approved);
    assert_eq!(notices, vec!["a@example.test", "b@example.test", "e@example.test", "f@example.test"]);
    // A promotion older than 7 days no longer disqualifies.
    age_log(&db, 8 * DAY);
    assert!(!allowed(&db, &request(SharedRestore, &a, &b)).0);
    assert_eq!(refused(&db, &request(SharedRestore, &a, &a)), ApprovalRefusal::SecondAdminRequired);
}

#[test]
fn admin_promotion_notifies_all_admins() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let c = eligible_admin(&db, "c@example.test");
    admin(&db, "never-eligible@example.test");
    let b = employee(&db, "b@example.test");
    enroll_raw(&db, &b);
    age_log(&db, 30 * DAY);
    let mark = db.admin_eligibility_high_water().unwrap();
    set_role(&db, Some(&a), &b, UserRole::Admin);
    let notices = db.admin_promotion_notices_since(mark, now_secs()).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!((notices[0].user_id.as_str(), notices[0].reason.as_str(), notices[0].actor_user_id.as_deref()),
        (b.as_str(), "role", Some(a.as_str())));
    assert_eq!(notices[0].recipients, vec!["a@example.test", "b@example.test", "c@example.test"]);
    // A creation is announced; a demotion is not.
    let mark = db.admin_eligibility_high_water().unwrap();
    let f = db.create_user_as(Some(&c), &crate::NewUser { email: "f@example.test", display_name: "F", password: PASSWORD,
        role: UserRole::Admin, department: None, manager: None }, false).unwrap().id;
    set_role(&db, Some(&a), &b, UserRole::Employee);
    let notices = db.admin_promotion_notices_since(mark, now_secs()).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!((notices[0].user_id.as_str(), notices[0].reason.as_str(), notices[0].actor_user_id.as_deref()),
        (f.as_str(), "admin_created", Some(c.as_str())));
    assert!(notices[0].recipients.iter().any(|r| r == "a@example.test"));
    assert!(notices[0].recipients.iter().any(|r| r == "c@example.test"));
    // Nothing logged after the mark: nothing to announce.
    let mark = db.admin_eligibility_high_water().unwrap();
    assert!(db.admin_promotion_notices_since(mark, now_secs()).unwrap().is_empty());
}

#[test]
fn notices_use_log_addresses_after_account_delete() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 40 * DAY);
    db.change_email(&a, &b, "b-new@example.test").unwrap();
    exec(&db, "DELETE FROM users WHERE id=?1", &b);
    age_log(&db, 10 * DAY);
    assert!(db.get_user(&b).unwrap().is_none());
    // b was deleted 10 days ago: outside cooling, inside the 30-day notice
    // lookback. Both of its logged addresses are notified.
    let expected = vec!["a@example.test", "b-new@example.test", "b@example.test"];
    let (self_approved, notices) = allowed(&db, &request(SharedRestore, &a, &a));
    assert!(self_approved);
    assert_eq!(notices, expected);
    assert_eq!(db.admin_notice_addresses(now_secs()).unwrap(), expected);
    // 35 days after its last eligibility the deleted account drops out.
    age_log(&db, 25 * DAY);
    assert_eq!(allowed(&db, &request(SharedRestore, &a, &a)).1, vec!["a@example.test"]);
}

#[test]
fn malformed_requests_refused_before_any_read() {
    let (_dir, db, a, b) = two_admins();
    let cases = [
        ApprovalRequest { requester_user_id: "", ..request(SharedRestore, &a, &b) },
        ApprovalRequest { approver_user_id: "", ..request(SharedRestore, &a, &b) },
        ApprovalRequest { owner_user_id: None, ..request(PersonalRestore, &a, &b) },
        ApprovalRequest { owner_user_id: Some(b.as_str()), ..request(PersonalAdopt, &a, &b) },
        ApprovalRequest { owner_user_id: Some(""), ..request(EarlyPurge, &a, &b) },
        ApprovalRequest { now: 0, ..request(SharedRestore, &a, &b) },
    ];
    for case in &cases {
        assert_eq!(refused(&db, case), ApprovalRefusal::InvalidRequest, "{case:?}");
    }
    // A Personal request names its owner as requester; any kind accepts it.
    let fine = ApprovalRequest { owner_user_id: Some(a.as_str()), ..request(EarlyPurgedRetire, &a, &b) };
    assert!(!allowed(&db, &fine).0);
}

// ── Owner decision 2026-11-09: Admin-filed Personal requests ────────────

/// An Admin-filed Personal request (`requester` = the filing Admin, `owner` =
/// the receipt owner, who had no second factor at the tombstone).
fn admin_filed<'a>(kind: ApprovalKind, filer: &'a str, owner: &'a str, approver: &'a str) -> ApprovalRequest<'a> {
    ApprovalRequest { owner_user_id: Some(owner), ..request(kind, filer, approver) }
}

fn decide_filed(db: &UserDb, request: &ApprovalRequest<'_>) -> ApprovalDecision {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    let decision = decide_admin_filed(&tx, request);
    drop(tx);
    decision
}

fn filing(db: &UserDb, filer: &str, owner: &str) -> Result<(), ApprovalRefusal> {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    let checked = check_admin_filing(&tx, filer, owner, now_secs());
    drop(tx);
    checked
}

/// F6fa2: the filing Admin is a party; a *different* qualifying Admin must
/// approve; the sole-Admin path never exists for such a request (refused
/// `NO_SECOND_ADMIN`, at filing and at approval); the owner never approves.
#[test]
fn f6fa2_admin_filed_request_needs_different_admin() {
    let (_dir, db, a, b) = two_admins();
    let owner = employee(&db, "owner@example.test");
    for kind in [PersonalRestore, PersonalAdopt] {
        // The filer cannot approve while b qualifies.
        assert_eq!(decide_filed(&db, &admin_filed(kind, &a, &owner, &a)).refusal(), Some(ApprovalRefusal::SecondAdminRequired), "{kind:?}");
        // Neither can the owner.
        assert_eq!(decide_filed(&db, &admin_filed(kind, &a, &owner, &owner)).refusal(), Some(ApprovalRefusal::SecondAdminRequired),
            "{kind:?}");
        // b approves; never self-approved.
        match decide_filed(&db, &admin_filed(kind, &a, &owner, &b)) {
            ApprovalDecision::Allowed { self_approved, notice_addresses } => {
                assert!(!self_approved, "{kind:?}");
                assert_eq!(notice_addresses, vec!["a@example.test", "b@example.test"], "{kind:?}");
            }
            other => panic!("{kind:?}: {other:?}"),
        }
        // Fresh step-up and a transaction, as for every decision.
        let stale = ApprovalRequest { fresh_step_up: false, ..admin_filed(kind, &a, &owner, &b) };
        assert_eq!(decide_filed(&db, &stale).refusal(), Some(ApprovalRefusal::StepUpRequired));
        assert_eq!(decide_admin_filed(&db.conn(), &admin_filed(kind, &a, &owner, &b)).refusal(),
            Some(ApprovalRefusal::NotInTransaction));
    }
    // The owner must be named and differ from the filer; Shared kinds are not admin-filed.
    for case in [ApprovalRequest { owner_user_id: None, ..request(PersonalRestore, &a, &b) },
        admin_filed(PersonalRestore, &a, &a, &b), admin_filed(SharedRestore, &a, &owner, &b)] {
        assert_eq!(decide_filed(&db, &case).refusal(), Some(ApprovalRefusal::InvalidRequest), "{case:?}");
    }
    assert_eq!(filing(&db, &a, &owner), Ok(()));
    assert_eq!(filing(&db, &a, &a), Err(ApprovalRefusal::InvalidRequest));
    // A non-Admin cannot file.
    assert_eq!(filing(&db, &owner, &a), Err(ApprovalRefusal::FilerNotEligible));
    assert_eq!(ApprovalRefusal::FilerNotEligible.code(), "FILER_NOT_ELIGIBLE");
    // An Admin the filer promoted inside the window does not qualify (S2).
    let c = employee(&db, "c@example.test");
    enroll_raw(&db, &c);
    set_role(&db, Some(&a), &c, UserRole::Admin);
    assert_eq!(decide_filed(&db, &admin_filed(PersonalRestore, &a, &owner, &c)).refusal(),
        Some(ApprovalRefusal::ApproverNotQualified(NotQualified::PromotedByParty)));
}

/// F6fa2: with the filer as the only qualifying Admin there is nobody to
/// approve: filing and approval both refuse `NO_SECOND_ADMIN` — the sole-Admin
/// self-approval of `decide` is not available here.
#[test]
fn f6fa2_admin_filed_request_has_no_sole_admin_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = eligible_admin(&db, "a@example.test");
    let owner = employee(&db, "owner@example.test");
    age_log(&db, 30 * DAY);
    assert_eq!(filing(&db, &a, &owner), Err(ApprovalRefusal::NoSecondAdmin));
    assert_eq!(decide_filed(&db, &admin_filed(PersonalRestore, &a, &owner, &a)).refusal(), Some(ApprovalRefusal::NoSecondAdmin));
    assert_eq!(ApprovalRefusal::NoSecondAdmin.code(), "NO_SECOND_ADMIN");
    // The ordinary owner-filed rule would have allowed the sole Admin.
    assert!(allowed(&db, &request(SharedRestore, &a, &a)).0);
    // A second Admin, eligible for the whole window, makes it approvable.
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 8 * DAY);
    assert_eq!(filing(&db, &a, &owner), Ok(()));
    assert!(!decide_filed(&db, &admin_filed(PersonalAdopt, &a, &owner, &b)).self_approved());
}
