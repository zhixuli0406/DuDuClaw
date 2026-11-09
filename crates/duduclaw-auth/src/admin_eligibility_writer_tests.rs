//! F6c2 (S1, S16): every writer path that can change whether an account is
//! an eligible Admin appends its `admin_eligibility_log` row in the SAME
//! transaction as the change. Method: make every log append fail; the writer
//! must then fail and leave its change rolled back; once the log accepts rows
//! again the same call succeeds and appends the row.
//!
//! Also the shared test support of the F6c2 rule tests
//! (`admin_approval_rule_tests.rs`).
use super::*;
use crate::mfa::{FirstFactor, MfaCipher};
use crate::{UserDb, UserRole, UserStatus, UserUpdate};

pub(crate) const PASSWORD: &str = "password-test";
pub(crate) const SESSION: &str = "test-session";
pub(crate) const DAY: i64 = 86_400;

/// Reversible test cipher (same shape as the MFA tests' cipher).
pub(crate) struct Cipher;
impl MfaCipher for Cipher {
    fn encrypt(&self, s: &str) -> Result<String, String> { Ok(format!("sealed:{s}")) }
    fn decrypt(&self, s: &str) -> Result<String, String> {
        s.strip_prefix("sealed:").map(str::to_owned).ok_or_else(|| "sealed".to_owned())
    }
}

pub(crate) fn open(dir: &tempfile::TempDir) -> UserDb { UserDb::new(&dir.path().join("users.db")).unwrap() }

pub(crate) fn now_secs() -> i64 { chrono::Utc::now().timestamp() }

pub(crate) fn exec(db: &UserDb, sql: &str, user: &str) { db.conn().execute(sql, [user]).unwrap(); }

pub(crate) fn admin(db: &UserDb, email: &str) -> String {
    db.create_user(email, "Admin", PASSWORD, UserRole::Admin).unwrap().id
}

/// A TOTP row inserted directly (cheap; the trigger logs `mfa_enrolled`).
/// Its secret cannot verify a code; use [`enroll_api`] where one is needed.
pub(crate) fn enroll_raw(db: &UserDb, user: &str) {
    exec(db, "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", user);
}

pub(crate) fn eligible_admin(db: &UserDb, email: &str) -> String {
    let id = admin(db, email);
    enroll_raw(db, &id);
    id
}

/// TOTP enrolment through `mfa_confirm`; returns the recovery codes.
pub(crate) fn enroll_api(db: &UserDb, user: &str) -> Vec<String> {
    let (token, secret) = crate::mfa::test_totp_prepare(db, user, PASSWORD, &Cipher);
    db.mfa_confirm(user, Some(&token), &crate::mfa::test_totp_code(&secret, 100), &Cipher, 3000).unwrap()
}

/// Move the whole log and its creation marker `secs` into the past (test
/// only: the append-only guards are dropped and reinstalled by `migrate`).
pub(crate) fn age_log(db: &UserDb, secs: i64) {
    let conn = db.conn();
    conn.execute_batch("DROP TRIGGER admin_eligibility_log_no_delete; DROP TRIGGER admin_eligibility_log_actor_once;").unwrap();
    conn.execute("UPDATE admin_eligibility_log SET at=at-?1", [secs]).unwrap();
    conn.execute("UPDATE auth_meta SET value=value-?1 WHERE key=?2", rusqlite::params![secs, SINCE_KEY]).unwrap();
    migrate(&conn).unwrap();
}

pub(crate) fn rows_for(db: &UserDb, user: &str) -> Vec<EligibilityEntry> {
    db.admin_eligibility_entries().unwrap().into_iter().filter(|e| e.user_id == user).collect()
}

pub(crate) fn last(db: &UserDb, user: &str) -> EligibilityEntry { rows_for(db, user).pop().expect("a log row") }

fn log_len(db: &UserDb) -> usize { db.admin_eligibility_entries().unwrap().len() }

fn break_log(db: &UserDb) {
    db.conn().execute_batch("CREATE TRIGGER f6c2_break_log BEFORE INSERT ON admin_eligibility_log
        BEGIN SELECT RAISE(ABORT, 'admin eligibility log unavailable'); END;").unwrap();
}

fn heal_log(db: &UserDb) { db.conn().execute_batch("DROP TRIGGER f6c2_break_log;").unwrap(); }

/// The writer fails and changes nothing while the log refuses rows, then
/// succeeds and appends at least one row.
fn assert_same_tx<T, E: std::fmt::Debug>(db: &UserDb, write: impl Fn() -> Result<T, E>, unchanged: impl Fn() -> bool) -> T {
    let before = log_len(db);
    break_log(db);
    let refused = write();
    heal_log(db);
    assert!(refused.is_err(), "the writer must fail when its log row cannot be written");
    assert!(unchanged(), "a failed log append must roll the writer's change back");
    assert_eq!(log_len(db), before, "no log row without the change");
    let done = write().expect("the writer succeeds once the log accepts rows");
    assert!(log_len(db) > before, "the change appended its log row");
    done
}

fn assert_last(db: &UserDb, user: &str, reason: &str, eligible: bool, actor: Option<&str>) {
    let row = last(db, user);
    assert_eq!((row.reason.as_str(), row.eligible, row.actor_user_id.as_deref()), (reason, eligible, actor));
}

fn fixture() -> (tempfile::TempDir, UserDb, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let actor = eligible_admin(&db, "actor@example.test");
    (dir, db, actor)
}

fn user_email(db: &UserDb, user: &str) -> String { db.get_user(user).unwrap().unwrap().email }

fn new_admin<'a>(email: &'a str) -> crate::NewUser<'a> {
    crate::NewUser { email, display_name: "New", password: PASSWORD, role: UserRole::Admin, department: None, manager: None }
}

fn admin_member(email: &str, invite: bool) -> crate::members::NewMember {
    crate::members::NewMember {
        display_name: "Member".into(), email: email.into(), role: UserRole::Admin, department: None,
        primary_agent_id: "helper".into(), invite,
        bindings: vec![crate::members::MemberBinding { agent_name: "helper".into(), access_level: crate::AccessLevel::Operator }],
    }
}

// ── Creation ─────────────────────────────────────────────────

#[test]
fn eligibility_log_written_in_same_tx_create_user() {
    let (_dir, db, _) = fixture();
    let made = assert_same_tx(&db, || db.create_user("new@example.test", "New", PASSWORD, UserRole::Admin),
        || db.get_user_by_email("new@example.test").unwrap().is_none());
    assert_last(&db, &made.id, "admin_created", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_create_user_as_and_invited() {
    let (_dir, db, actor) = fixture();
    let made = assert_same_tx(&db, || db.create_user_as(Some(&actor), &new_admin("new@example.test"), false),
        || db.get_user_by_email("new@example.test").unwrap().is_none());
    assert_last(&db, &made.id, "admin_created", false, Some(&actor));
    let invited = assert_same_tx(&db, || db.create_invited_user_with(&new_admin("invited@example.test")),
        || db.get_user_by_email("invited@example.test").unwrap().is_none());
    assert!(invited.must_change_password);
    assert_last(&db, &invited.id, "admin_created", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_create_member() {
    let (_dir, db, actor) = fixture();
    let epoch = db.get_user(&actor).unwrap().unwrap().credential_epoch;
    let made = assert_same_tx(&db, || db.create_member_at_epoch(&admin_member("member@example.test", false), &actor, epoch),
        || db.get_user_by_email("member@example.test").unwrap().is_none());
    assert_last(&db, &made.user.id, "admin_created", false, Some(&actor));
}

#[test]
fn eligibility_log_written_in_same_tx_ensure_default_admin() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    assert!(assert_same_tx(&db, || db.ensure_default_admin(), || db.list_users().unwrap().is_empty()).is_some());
    let owner = db.get_bootstrap_owner().unwrap().unwrap();
    assert_last(&db, &owner.id, "admin_created", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_claim_setup_token() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let token = db.issue_setup_token(false).unwrap().unwrap();
    let made = assert_same_tx(&db, || db.claim_setup_token(&token, "first@example.test", "First"),
        || db.list_users().unwrap().is_empty());
    assert_last(&db, &made.id, "admin_created", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_migration_invite_and_disable() {
    let (_dir, db, _) = fixture();
    let digest = [7u8; 32];
    let made = assert_same_tx(&db, || db.migration_invite_user("a1-b2", &digest, "mig@example.test", "Mig", UserRole::Admin),
        || db.get_user_by_email("mig@example.test").unwrap().is_none());
    assert_last(&db, &made.id, "admin_created", false, None);
    assert_same_tx(&db, || db.migration_user_disable("a1-b2", &digest),
        || db.get_user(&made.id).unwrap().unwrap().status == UserStatus::Active);
    assert_last(&db, &made.id, "status", false, None);
}

// ── Role, status, forced password, email ─────────────────────

#[test]
fn eligibility_log_written_in_same_tx_role_update() {
    let (_dir, db, actor) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    let demote = UserUpdate { role: Some(UserRole::Employee), ..Default::default() };
    assert_same_tx(&db, || db.apply_user_update_as(Some(&actor), &other, &demote),
        || db.get_user(&other).unwrap().unwrap().role == UserRole::Admin);
    assert_last(&db, &other, "role", false, Some(&actor));
    // `update_user` (legacy API) is the same path, unattributed.
    assert_same_tx(&db, || db.update_user(&other, None, Some(UserRole::Admin), None),
        || db.get_user(&other).unwrap().unwrap().role == UserRole::Employee);
    assert_last(&db, &other, "role", true, None);
}

#[test]
fn eligibility_log_written_in_same_tx_password_update_clears_forced_flag() {
    let (_dir, db, actor) = fixture();
    let invited = db.create_invited_user_with(&new_admin("invited@example.test")).unwrap().id;
    enroll_raw(&db, &invited);
    assert!(!last(&db, &invited).eligible, "a pending forced password change is not eligible");
    let set = UserUpdate { password: Some("brand-new-password"), ..Default::default() };
    assert_same_tx(&db, || db.apply_user_update_as(Some(&actor), &invited, &set),
        || db.get_user(&invited).unwrap().unwrap().must_change_password);
    assert_last(&db, &invited, "forced_password", true, Some(&actor));
}

#[test]
fn eligibility_log_written_in_same_tx_forced_password_flag() {
    // No production API sets the flag on an existing account; the trigger
    // covers any writer that does (raw statement in its own transaction).
    let (_dir, db, _) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    let force = || -> rusqlite::Result<usize> {
        let mut conn = db.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&other])?;
        tx.commit()?;
        Ok(changed)
    };
    assert_same_tx(&db, force, || !db.get_user(&other).unwrap().unwrap().must_change_password);
    assert_last(&db, &other, "forced_password", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_set_user_status() {
    let (_dir, db, actor) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    // Also the gateway `users.remove` path (it suspends through this call).
    assert_same_tx(&db, || db.set_user_status_as(Some(&actor), &other, UserStatus::Suspended),
        || db.get_user(&other).unwrap().unwrap().status == UserStatus::Active);
    assert_last(&db, &other, "status", false, Some(&actor));
    assert_same_tx(&db, || db.set_user_status(&other, UserStatus::Active),
        || db.get_user(&other).unwrap().unwrap().status == UserStatus::Suspended);
    assert_last(&db, &other, "status", true, None);
}

#[test]
fn eligibility_log_written_in_same_tx_offboard() {
    let (_dir, db, actor) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    assert_same_tx(&db, || db.offboard_user(&other, None, Some(&actor)),
        || db.get_user(&other).unwrap().unwrap().status == UserStatus::Active);
    assert_last(&db, &other, "status", false, Some(&actor));
}

#[test]
fn eligibility_log_written_in_same_tx_claim_default_admin() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    db.ensure_default_admin().unwrap();
    assert!(assert_same_tx(&db, || db.claim_default_admin_with_email(Some("owner@example.test"), "owner-password-1"),
        || db.is_unclaimed_default_admin()));
    let owner = db.get_bootstrap_owner().unwrap().unwrap();
    assert_last(&db, &owner.id, "forced_password", false, None);
    assert_eq!(last(&db, &owner.id).notice_address.as_deref(), Some("owner@example.test"));
}

#[test]
fn eligibility_log_written_in_same_tx_change_email() {
    let (_dir, db, actor) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    assert_same_tx(&db, || db.change_email(&actor, &other, "renamed@example.test"),
        || user_email(&db, &other) == "other@example.test");
    let row = last(&db, &other);
    assert_eq!((row.eligible, row.notice_address.as_deref()), (true, Some("renamed@example.test")));
}

#[test]
fn eligibility_log_written_in_same_tx_consume_user_invite() {
    let (_dir, db, actor) = fixture();
    let epoch = db.get_user(&actor).unwrap().unwrap().credential_epoch;
    let created = db.create_member_at_epoch(&admin_member("member@example.test", true), &actor, epoch).unwrap();
    let token = created.invitation.expect("an invitation").token;
    let member = created.user.id;
    let used = assert_same_tx(&db, || db.consume_user_invite(&token, "member-password-1"),
        || db.get_user(&member).unwrap().unwrap().must_change_password);
    assert_eq!(used.as_deref(), Some(member.as_str()));
    assert_last(&db, &member, "forced_password", false, None);
}

// ── Second factors ───────────────────────────────────────────

#[test]
fn eligibility_log_written_in_same_tx_totp_enrol() {
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    let (token, secret) = crate::mfa::test_totp_prepare(&db, &other, PASSWORD, &Cipher);
    let code = crate::mfa::test_totp_code(&secret, 100);
    assert_same_tx(&db, || db.mfa_confirm(&other, Some(&token), &code, &Cipher, 3000),
        || !db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "mfa_enrolled", true, None);
}

#[test]
fn eligibility_log_written_in_same_tx_totp_disable() {
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    let codes = enroll_api(&db, &other);
    assert_same_tx(&db, || db.mfa_change(&other, &codes[0], true, &Cipher, 3000), || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "mfa_removed", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_mfa_admin_reset() {
    let (_dir, db, _) = fixture();
    let actor = admin(&db, "resetter@example.test");
    let codes = enroll_api(&db, &actor);
    let other = eligible_admin(&db, "other@example.test");
    assert_same_tx(&db, || db.mfa_admin_reset(&actor, &other, &codes[0], &Cipher, 3000), || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "mfa_removed", false, Some(&actor));
}

#[test]
fn eligibility_log_written_in_same_tx_mfa_admin_reset_reauth() {
    let (_dir, db, actor) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    let token = db.mfa_factor_password_challenge(&actor, PASSWORD, SESSION, 3000).unwrap();
    assert_same_tx(&db, || db.mfa_admin_reset_reauth(&actor, &other, &token, SESSION, 3000), || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "mfa_removed", false, Some(&actor));
}

#[test]
fn eligibility_log_written_in_same_tx_mfa_operator_reset() {
    let (_dir, db, _) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    assert_same_tx(&db, || db.mfa_operator_reset("other@example.test"), || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "mfa_removed", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_email_factor_add() {
    let (_dir, db, _) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    let token = db.mfa_factor_password_challenge(&other, PASSWORD, SESSION, 3000).unwrap();
    let (_, code) = db.mfa_email_send(&token, true, 3000).unwrap();
    let has_email = || db.mfa_factors(&other).unwrap()["factors"]["email"].as_bool().unwrap();
    assert_same_tx(&db, || db.mfa_email_verify_and_issue(&token, &code, true, 3000, |_, _, codes| Ok(codes)),
        || !has_email());
    assert_last(&db, &other, "email_factor_added", true, None);
}

#[test]
fn eligibility_log_written_in_same_tx_email_factor_remove() {
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    exec(&db, "INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'other@example.test',0)", &other);
    assert!(last(&db, &other).eligible);
    let token = db.mfa_factor_password_challenge(&other, PASSWORD, SESSION, 3000).unwrap();
    assert_same_tx(&db, || db.mfa_email_remove_and_issue(&other, &token, SESSION, 3000, false, |_, _, codes| Ok(codes)),
        || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "email_factor_removed", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_passkey_delete() {
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    exec(&db, "INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('pk-1',?1,'key','{}',0)", &other);
    assert!(last(&db, &other).eligible);
    let token = db.mfa_factor_password_challenge(&other, PASSWORD, SESSION, 3000).unwrap();
    assert_same_tx(&db, || db.mfa_passkey_delete(&other, "pk-1", &token, SESSION, 3000, false, |_, _, codes| Ok(codes)),
        || db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "passkey_removed", false, None);
}

#[test]
fn eligibility_log_written_in_same_tx_passkey_register_statement() {
    // `mfa_passkey_register_finish` needs a WebAuthn ceremony; its insert is
    // this statement inside the ceremony's transaction.
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    let register = || -> rusqlite::Result<usize> {
        let mut conn = db.conn();
        let tx = conn.transaction()?;
        let added = tx.execute("INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('pk-2',?1,'key','{}',0)", [&other])?;
        tx.commit()?;
        Ok(added)
    };
    assert_same_tx(&db, register, || !db.mfa_active(&other).unwrap());
    assert_last(&db, &other, "passkey_added", true, None);
}

// ── Lockout and deletion ─────────────────────────────────────

#[test]
fn eligibility_log_written_in_same_tx_lockout() {
    let (_dir, db, _) = fixture();
    let other = admin(&db, "other@example.test");
    enroll_api(&db, &other);
    let failures = || -> i64 {
        db.conn().query_row("SELECT failed_attempts FROM mfa_credentials WHERE user_id=?1", [&other], |r| r.get(0)).unwrap()
    };
    let challenge = db.mfa_challenge(&other, FirstFactor::Password, false, 3000).unwrap();
    for _ in 0..4 { assert!(db.mfa_verify(&challenge, "abcdef", false, &Cipher, 3000).is_err()); }
    assert_eq!(failures(), 4);
    let before = log_len(&db);
    // The fifth failure reaches the lockout threshold: without its log row the
    // counter update does not land either (same statement, same transaction).
    break_log(&db);
    assert!(db.mfa_verify(&challenge, "abcdef", false, &Cipher, 3000).is_err());
    heal_log(&db);
    assert_eq!((failures(), log_len(&db)), (4, before));
    let challenge = db.mfa_challenge(&other, FirstFactor::Password, false, 3000).unwrap();
    assert!(db.mfa_verify(&challenge, "abcdef", false, &Cipher, 3000).is_err());
    assert_eq!(failures(), 5);
    // Logged, and never an eligibility change.
    assert_last(&db, &other, "lockout", true, None);
    assert!(is_eligible(&db.conn(), &other).unwrap());
}

#[test]
fn eligibility_log_written_in_same_tx_account_delete() {
    // No users.db API deletes an account (`users.remove` suspends); the
    // trigger covers any writer that does.
    let (_dir, db, _) = fixture();
    let other = eligible_admin(&db, "other@example.test");
    let delete = || -> rusqlite::Result<usize> {
        let mut conn = db.conn();
        let tx = conn.transaction()?;
        let removed = tx.execute("DELETE FROM users WHERE id=?1", [&other])?;
        tx.commit()?;
        Ok(removed)
    };
    assert_same_tx(&db, delete, || db.get_user(&other).unwrap().is_some());
    assert_last(&db, &other, "deleted", false, None);
    assert_eq!(last(&db, &other).notice_address.as_deref(), Some("other@example.test"));
}
