//! RM-136: resetting another Administrator's second factor needs a fresh
//! second factor of the actor and a qualifying (7-day) Admin; other targets
//! keep the password confirmation; every refusal fails closed.
use super::*;
use crate::admin_eligibility::writer_tests::{
    admin, age_log, eligible_admin, enroll_api, exec, last, now_secs, open, Cipher, DAY, PASSWORD, SESSION,
};
use crate::UserUpdate;
use AdminResetRefusal::*;

/// `a` (an Admin with recovery codes) and `b` (another Admin with a
/// factor), both eligible for the last 30 days.
fn seasoned() -> (tempfile::TempDir, UserDb, String, Vec<String>, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let codes = enroll_api(&db, &a);
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 30 * DAY);
    (dir, db, a, codes, b)
}

/// An Employee with a TOTP factor; returns the id and its recovery codes.
fn employee_with_codes(db: &UserDb, email: &str) -> (String, Vec<String>) {
    let id = db.create_user(email, "Staff", PASSWORD, UserRole::Employee).unwrap().id;
    let codes = enroll_api(db, &id);
    (id, codes)
}

fn password(db: &UserDb, actor: &str, now: i64) -> String {
    db.mfa_factor_password_challenge(actor, PASSWORD, SESSION, now).unwrap()
}

fn reset(db: &UserDb, actor: &str, target: &str, proof: AdminResetProof<'_>, now: i64) -> Refused<AdminResetOutcome> {
    db.mfa_admin_reset_checked(actor, target, proof, &Cipher, now)
}

fn with_password<'a>(token: &'a str) -> AdminResetProof<'a> {
    AdminResetProof::Password { reauth_token: token, session: SESSION }
}

const ADMIN_TARGET: Refused<AdminResetOutcome> = Ok(AdminResetOutcome { target_admin: true });
const STAFF_TARGET: Refused<AdminResetOutcome> = Ok(AdminResetOutcome { target_admin: false });

#[test]
fn admin_reset_refused_with_password_only_for_an_admin_target() {
    let (_dir, db, a, _, b) = seasoned();
    let now = now_secs();
    let token = password(&db, &a, now);
    assert_eq!(reset(&db, &a, &b, with_password(&token), now), Err(SecondFactorRequired));
    assert!(db.mfa_active(&b).unwrap());
    assert_eq!(SecondFactorRequired.code(), "MFA_RESET_SECOND_FACTOR_REQUIRED");
}

#[test]
fn admin_reset_allowed_with_fresh_factor_after_seven_days() {
    let (_dir, db, a, codes, b) = seasoned();
    let now = now_secs();
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor("wrong"), now), Err(FactorFailed));
    assert!(db.mfa_active(&b).unwrap());
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor(&codes[0]), now), ADMIN_TARGET);
    assert!(!db.mfa_active(&b).unwrap());
    assert_eq!(last(&db, &b).actor_user_id.as_deref(), Some(a.as_str()));
    assert_eq!(db.query_audit_log(Some(&b), Some("mfa.reset"), 10).unwrap().len(), 1);
    // The recovery code was consumed by the reset.
    let c = eligible_admin(&db, "c@example.test");
    assert_eq!(reset(&db, &a, &c, AdminResetProof::Factor(&codes[0]), now), Err(FactorFailed));
}

#[test]
fn admin_reset_refused_inside_the_seven_day_window_of_a_new_admin() {
    let (_dir, db, a, _, b) = seasoned();
    let (c, c_codes) = employee_with_codes(&db, "c@example.test");
    db.apply_user_update_as(Some(&a), &c, &UserUpdate { role: Some(UserRole::Admin), ..Default::default() }).unwrap();
    let now = now_secs();
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&c_codes[0]), now), Err(Cooling));
    assert!(db.mfa_active(&b).unwrap());
    // The window rule is only about Administrator targets: staff still work.
    let (staff, _) = employee_with_codes(&db, "staff@example.test");
    let token = password(&db, &c, now);
    assert_eq!(reset(&db, &c, &staff, with_password(&token), now), STAFF_TARGET);
    // Eight days later the promoted Admin qualifies; the code verified during
    // the cooling refusal stays spent, a fresh one works.
    age_log(&db, 8 * DAY);
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&c_codes[0]), now_secs()), Err(FactorFailed));
    assert!(db.mfa_active(&b).unwrap());
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&c_codes[1]), now_secs()), ADMIN_TARGET);
}

/// An Employee with a TOTP factor promoted to Admin just now (inside its
/// cooling window); returns the id, its TOTP secret and recovery codes.
fn new_admin_with_totp(db: &UserDb, by: &str, email: &str) -> (String, String, Vec<String>) {
    let id = db.create_user(email, "Staff", PASSWORD, UserRole::Employee).unwrap().id;
    let (token, secret) = crate::mfa::test_totp_prepare(db, &id, PASSWORD, &Cipher);
    let codes = db.mfa_confirm(&id, Some(&token), &crate::mfa::test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
    db.apply_user_update_as(Some(by), &id, &UserUpdate { role: Some(UserRole::Admin), ..Default::default() }).unwrap();
    (id, secret, codes)
}

fn not_reset(db: &UserDb, target: &str) -> bool {
    db.mfa_active(target).unwrap() && db.query_audit_log(Some(target), Some("mfa.reset"), 10).unwrap().is_empty()
}

#[test]
fn cooling_refusal_after_a_valid_recovery_code_keeps_the_code_spent() {
    let (_dir, db, a, _, b) = seasoned();
    let (c, _, codes) = new_admin_with_totp(&db, &a, "c@example.test");
    let remaining = |db: &UserDb| -> i64 {
        db.conn().query_row("SELECT COUNT(*) FROM mfa_recovery_codes WHERE user_id=?1", [&c], |r| r.get(0)).unwrap()
    };
    let before = remaining(&db);
    let now = now_secs();
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&codes[0]), now), Err(Cooling));
    assert!(not_reset(&db, &b));
    assert_eq!(remaining(&db), before - 1, "the verified recovery code was consumed");
    // The same code is rejected afterwards, also once the actor qualifies.
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&codes[0]), now), Err(FactorFailed));
    age_log(&db, 8 * DAY);
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&codes[0]), now_secs()), Err(FactorFailed));
    assert!(not_reset(&db, &b));
}

#[test]
fn cooling_refusal_after_a_valid_totp_keeps_the_step_used() {
    let (_dir, db, a, _, b) = seasoned();
    let (c, secret, _) = new_admin_with_totp(&db, &a, "c@example.test");
    let now = now_secs();
    let code = crate::mfa::test_totp_code(&secret, now / 30);
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&code), now), Err(Cooling));
    assert!(not_reset(&db, &b));
    let last_step: i64 = db.conn().query_row("SELECT last_step FROM mfa_credentials WHERE user_id=?1", [&c], |r| r.get(0)).unwrap();
    assert_eq!(last_step, now / 30, "the replay marker was committed");
    // The same step is a replay now, also once the actor qualifies.
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&code), now), Err(FactorFailed));
    age_log(&db, 8 * DAY);
    assert_eq!(reset(&db, &c, &b, AdminResetProof::Factor(&code), now), Err(FactorFailed));
    assert!(not_reset(&db, &b));
}

#[test]
fn password_only_caller_in_cooling_learns_only_that_a_second_factor_is_required() {
    let (_dir, db, a, _, b) = seasoned();
    let (c, _, _) = new_admin_with_totp(&db, &a, "c@example.test");
    let now = now_secs();
    let token = password(&db, &c, now);
    assert_eq!(reset(&db, &c, &b, with_password(&token), now), Err(SecondFactorRequired));
    assert!(not_reset(&db, &b));
}

#[test]
fn single_admin_fresh_install_manages_staff_but_not_other_admins_for_seven_days() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let codes = enroll_api(&db, &a);
    let (staff, _) = employee_with_codes(&db, "staff@example.test");
    let now = now_secs();
    let token = password(&db, &a, now);
    assert_eq!(reset(&db, &a, &staff, with_password(&token), now), STAFF_TARGET);
    // The log is younger than the window: when `a` became an Admin cannot be
    // shown, so resetting another Administrator fails closed.
    let b = eligible_admin(&db, "b@example.test");
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor(&codes[0]), now), Err(Cooling));
    assert!(db.mfa_active(&b).unwrap());
    // Without a verified second factor the cooling state is not disclosed.
    let token = password(&db, &a, now);
    assert_eq!(reset(&db, &a, &b, with_password(&token), now), Err(SecondFactorRequired));
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor("wrong"), now), Err(FactorFailed));
    assert!(db.mfa_active(&b).unwrap());
}

#[test]
fn demoted_admin_is_still_an_admin_target_for_seven_days() {
    let (_dir, db, a, codes, b) = seasoned();
    db.apply_user_update_as(Some(&a), &b, &UserUpdate { role: Some(UserRole::Employee), ..Default::default() }).unwrap();
    let now = now_secs();
    let token = password(&db, &a, now);
    assert_eq!(reset(&db, &a, &b, with_password(&token), now), Err(SecondFactorRequired));
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor(&codes[0]), now), ADMIN_TARGET);
}

#[test]
fn passkey_only_admin_confirms_with_password_and_email_code() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let start = now_secs();
    crate::mfa::passkey::ceremony_tests::register(&db, &a, PASSWORD, start);
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 30 * DAY);
    // Finding: the code check accepts only TOTP / recovery codes, and a first
    // passkey enrollment issues no recovery codes, so this admin has no code.
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor("123456"), start), Err(FactorFailed));
    let token = password(&db, &a, start);
    assert_eq!(reset(&db, &a, &b, with_password(&token), start), Err(SecondFactorRequired));

    let now = start + 61;
    // Mail unavailable or a wrong password: refused before a code is minted.
    assert!(matches!(db.mfa_admin_reset_email_begin(&a, &b, PASSWORD, SESSION, false, now), Err(EmailUnavailable)));
    assert!(matches!(db.mfa_admin_reset_email_begin(&a, &b, "wrong-password", SESSION, true, now), Err(FactorFailed)));
    let mail = db.mfa_admin_reset_email_begin(&a, &b, PASSWORD, SESSION, true, now).ok().expect("code issued");
    assert_eq!(mail.email, "a@example.test");
    assert_eq!(mail.code.len(), 6);
    let stored: String = db.conn().query_row("SELECT code_hash FROM mfa_restore_email_codes WHERE user_id=?1", [&a], |r| r.get(0)).unwrap();
    assert!(!stored.contains(&mail.code));
    let wrong = if mail.code == "000000" { "111111" } else { "000000" };
    let email = |target: &str, token: &str, session: &str, code: &str, now: i64|
        reset(&db, &a, target, AdminResetProof::EmailCode { reset_token: token, session, code }, now);
    assert_eq!(email(&b, &mail.reset_token, "other-session", &mail.code, now), Err(FactorFailed));
    assert_eq!(email(&b, &mail.reset_token, SESSION, wrong, now), Err(FactorFailed));
    // Any presentation consumed the code.
    assert_eq!(email(&b, &mail.reset_token, SESSION, &mail.code, now), Err(FactorFailed));
    assert!(db.mfa_active(&b).unwrap());

    // Bound to its target: a code minted for `b` cannot reset `c`.
    let now = now + 61;
    let mail = db.mfa_admin_reset_email_begin(&a, &b, PASSWORD, SESSION, true, now).ok().expect("code issued");
    let c = eligible_admin(&db, "c@example.test");
    assert_eq!(email(&c, &mail.reset_token, SESSION, &mail.code, now), Err(FactorFailed));
    assert!(db.mfa_active(&c).unwrap());

    let now = now + 61;
    let mail = db.mfa_admin_reset_email_begin(&a, &b, PASSWORD, SESSION, true, now).ok().expect("code issued");
    assert_eq!(email(&b, &mail.reset_token, SESSION, &mail.code, now), ADMIN_TARGET);
    assert!(!db.mfa_active(&b).unwrap());
}

#[test]
fn actor_with_an_authenticator_must_use_it() {
    let (_dir, db, a, _, b) = seasoned();
    assert!(matches!(db.mfa_admin_reset_email_begin(&a, &b, PASSWORD, SESSION, true, now_secs()), Err(UseAuthenticator)));
}

#[test]
fn self_reset_inactive_target_and_non_admin_actor_refused() {
    let (_dir, db, a, codes, b) = seasoned();
    let now = now_secs();
    assert_eq!(reset(&db, &a, &a, AdminResetProof::Factor(&codes[0]), now), Err(SelfReset));
    let (staff, staff_codes) = employee_with_codes(&db, "staff@example.test");
    assert_eq!(reset(&db, &staff, &b, AdminResetProof::Factor(&staff_codes[0]), now), Err(ActorNotAdmin));
    exec(&db, "UPDATE users SET status='suspended' WHERE id=?1", &b);
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor(&codes[0]), now), Err(TargetUnavailable));
    assert!(db.mfa_active(&b).unwrap());
}

#[test]
fn admin_reset_fails_closed_when_the_admin_log_is_unreadable() {
    let (_dir, db, a, codes, b) = seasoned();
    db.conn().execute_batch("DROP TRIGGER admin_eligibility_totp_lockout;").unwrap();
    assert_eq!(reset(&db, &a, &b, AdminResetProof::Factor(&codes[0]), now_secs()), Err(AdminLogUnavailable));
    assert!(db.mfa_active(&b).unwrap());
}

fn outbox(db: &UserDb) -> Vec<ResetAuditRow> {
    db.mfa_reset_outbox_pending(true).unwrap()
}

/// An in-memory audit log with the sink contract: positions are line
/// indexes; `fail_on` makes the append of that attempt fail.
#[derive(Default)]
struct Log {
    lines: Vec<ResetAuditRow>,
    fail_on: Option<String>,
}

impl ResetAuditSink for Log {
    fn position(&mut self) -> std::result::Result<u64, String> { Ok(self.lines.len() as u64) }
    fn append_once(&mut self, row: &ResetAuditRow, since: u64) -> std::result::Result<bool, String> {
        if self.fail_on.as_deref() == Some(row.attempt_id.as_str()) { return Err("audit file unavailable".into()); }
        let since = usize::try_from(since).unwrap_or(usize::MAX);
        if self.lines.iter().skip(since).any(|line| line.attempt_id == row.attempt_id) { return Ok(false); }
        self.lines.push(row.clone());
        Ok(true)
    }
}

impl Log {
    fn ids(&self) -> Vec<&str> { self.lines.iter().map(|row| row.attempt_id.as_str()).collect() }
}

fn marker(db: &UserDb, attempt: &str) -> Option<i64> {
    db.conn().query_row("SELECT delivery_from FROM mfa_reset_audit_outbox WHERE attempt_id=?1", [attempt], |r| r.get(0)).unwrap()
}

fn recorded(db: &UserDb, attempt: &str, actor: &str, target: &str, proof: AdminResetProof<'_>) -> (Refused<AdminResetOutcome>, bool) {
    db.mfa_admin_reset_recorded(attempt, proof.kind(), actor, target, proof, &Cipher, now_secs())
}

#[test]
fn reset_outbox_row_commits_with_the_reset_and_with_each_refusal() {
    let (_dir, db, a, codes, b) = seasoned();
    assert!(outbox(&db).is_empty());
    // Allowed: the reset and its row in one transaction.
    assert_eq!(recorded(&db, "attempt-1", &a, &b, AdminResetProof::Factor(&codes[0])), (ADMIN_TARGET, true));
    assert!(!db.mfa_active(&b).unwrap());
    let rows = outbox(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].attempt_id.as_str(), rows[0].outcome.as_str(), rows[0].reason.as_str()), ("attempt-1", "allowed", "allowed"));
    assert_eq!((rows[0].actor_user_id.as_str(), rows[0].target_user_id.as_deref()), (a.as_str(), Some(b.as_str())));
    assert_eq!((rows[0].proof.as_str(), rows[0].target_admin, rows[0].held), ("factor", Some(true), false));
    // Refusals decided inside the transaction, self-reset included.
    let c = eligible_admin(&db, "c@example.test");
    assert_eq!(recorded(&db, "attempt-2", &a, &c, AdminResetProof::Factor("wrong")), (Err(FactorFailed), true));
    assert_eq!(recorded(&db, "attempt-3", &a, &a, AdminResetProof::Factor(&codes[1])), (Err(SelfReset), true));
    let token = password(&db, &a, now_secs());
    assert_eq!(recorded(&db, "attempt-4", &a, &c, with_password(&token)), (Err(SecondFactorRequired), true));
    assert!(db.mfa_active(&c).unwrap());
    let reasons: Vec<_> = outbox(&db).into_iter().map(|r| (r.attempt_id, r.reason)).collect();
    assert_eq!(reasons, [("attempt-1".to_owned(), "allowed".to_owned()), ("attempt-2".into(), "factor_failed".into()),
        ("attempt-3".into(), "self_reset".into()), ("attempt-4".into(), "second_factor_required".into())]);
    // No secret in any row.
    let text = format!("{:?}", outbox(&db));
    assert!(!text.contains(&codes[0]) && !text.contains(PASSWORD) && !text.contains(&token));
    // A reused attempt id cannot commit a second reset row: the reset rolls back.
    let d = eligible_admin(&db, "d@example.test");
    assert_eq!(recorded(&db, "attempt-1", &a, &d, AdminResetProof::Factor(&codes[2])), (Err(Unavailable), false));
    assert!(db.mfa_active(&d).unwrap(), "no reset without its outbox row");
}

#[test]
fn reset_outbox_full_refuses_before_any_proof_is_checked() {
    let (_dir, db, a, codes, b) = seasoned();
    for n in 0..RESET_OUTBOX_MAX_ROWS {
        db.mfa_reset_outbox_record(&ResetAuditRow::new(&format!("fill-{n}"), &a, None, "factor", "refused", "rate_limited", None)).unwrap();
    }
    assert!(db.mfa_reset_outbox_record(&ResetAuditRow::new("one-more", &a, None, "factor", "refused", "rate_limited", None)).is_err());
    assert_eq!(recorded(&db, "attempt-x", &a, &b, AdminResetProof::Factor(&codes[0])), (Err(Unavailable), false));
    assert!(db.mfa_active(&b).unwrap());
    // The recovery code was not spent: after a drain the same code works.
    let mut log = Log::default();
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), RESET_OUTBOX_MAX_ROWS as usize);
    assert_eq!(log.lines.len(), RESET_OUTBOX_MAX_ROWS as usize);
    assert_eq!(recorded(&db, "attempt-y", &a, &b, AdminResetProof::Factor(&codes[0])), (ADMIN_TARGET, true));
}

#[test]
fn reset_outbox_drain_deletes_only_after_a_successful_append() {
    let (_dir, db, a, codes, b) = seasoned();
    assert_eq!(recorded(&db, "attempt-1", &a, &b, AdminResetProof::Factor(&codes[0])), (ADMIN_TARGET, true));
    db.mfa_reset_outbox_record(&ResetAuditRow::new("attempt-2", &a, None, "none", "refused", "unavailable", None)).unwrap();
    // Simulated append failure on the second row: the first is delivered and
    // deleted, the second stays (marked, nothing appended).
    let mut log = Log { fail_on: Some("attempt-2".into()), ..Log::default() };
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap_err(), "audit file unavailable");
    assert_eq!(log.ids(), ["attempt-1"]);
    assert_eq!(outbox(&db).iter().map(|r| r.attempt_id.as_str()).collect::<Vec<_>>(), ["attempt-2"]);
    assert_eq!(marker(&db, "attempt-2"), Some(1), "marked before its first append");
    // The next drain delivers the rest exactly once; a third drain is a no-op.
    log.fail_on = None;
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 1);
    assert_eq!(log.ids(), ["attempt-1", "attempt-2"]);
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 0);
    assert_eq!(log.lines.len(), 2);
}

#[test]
fn reset_outbox_crash_between_append_and_delete_replays_to_one_record() {
    let (_dir, db, a, codes, b) = seasoned();
    assert_eq!(recorded(&db, "attempt-1", &a, &b, AdminResetProof::Factor(&codes[0])), (ADMIN_TARGET, true));
    let mut log = Log { lines: vec![ResetAuditRow::new("earlier", &a, None, "factor", "refused", "rate_limited", None)],
        ..Log::default() };
    // The append succeeds, the delete does not: the on-disk state a crash
    // right after the append leaves behind.
    db.conn().execute_batch("CREATE TRIGGER crash_before_delete BEFORE DELETE ON mfa_reset_audit_outbox
        BEGIN SELECT RAISE(ABORT, 'crashed'); END;").unwrap();
    assert!(db.mfa_reset_outbox_drain(false, &mut log).is_err());
    assert_eq!(log.ids(), ["earlier", "attempt-1"]);
    assert_eq!(marker(&db, "attempt-1"), Some(1));
    // Replay after restart (start-up drain): the record is found after the
    // marker, so the row is only deleted.
    db.conn().execute_batch("DROP TRIGGER crash_before_delete;").unwrap();
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), 1);
    assert_eq!(log.ids(), ["earlier", "attempt-1"], "exactly one record of the attempt");
    assert!(outbox(&db).is_empty());
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), 0);
}

/// Rebuild the outbox in its shape from before the `delivery_from` column,
/// keeping its rows, then reopen the database so the real migration runs.
fn reopen_pre_marker(dir: &tempfile::TempDir, db: UserDb) -> UserDb {
    db.conn().execute_batch("CREATE TABLE outbox_old AS SELECT attempt_id,actor_user_id,target_user_id,proof,outcome,
            reason,target_admin,held,created_at FROM mfa_reset_audit_outbox;
        DROP TABLE mfa_reset_audit_outbox;
        CREATE TABLE mfa_reset_audit_outbox (
            attempt_id TEXT PRIMARY KEY,
            actor_user_id TEXT NOT NULL,
            target_user_id TEXT,
            proof TEXT NOT NULL,
            outcome TEXT NOT NULL,
            reason TEXT NOT NULL,
            target_admin INTEGER CHECK(target_admin IS NULL OR target_admin IN (0,1)),
            held INTEGER NOT NULL DEFAULT 0 CHECK(held IN (0,1)),
            created_at INTEGER NOT NULL);
        INSERT INTO mfa_reset_audit_outbox SELECT * FROM outbox_old;
        DROP TABLE outbox_old;").unwrap();
    drop(db);
    open(dir)
}

#[test]
fn reset_outbox_pre_marker_row_already_delivered_is_only_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let row = ResetAuditRow::new("legacy-1", &a, None, "factor", "refused", "rate_limited", None);
    db.mfa_reset_outbox_record(&row).unwrap();
    // Code without the marker appended the record and crashed before the
    // delete; later events followed it in the log.
    let mut log = Log { lines: vec![row.clone(), ResetAuditRow::new("later", &a, None, "factor", "refused", "rate_limited", None)],
        ..Log::default() };
    let db = reopen_pre_marker(&dir, db);
    assert_eq!(marker(&db, "legacy-1"), Some(0), "checked from the start of the log");
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 1);
    assert_eq!(log.ids(), ["legacy-1", "later"], "nothing appended");
    assert!(outbox(&db).is_empty());
}

#[test]
fn reset_outbox_pre_marker_row_not_yet_delivered_is_appended_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    db.mfa_reset_outbox_record(&ResetAuditRow::new("legacy-2", &a, None, "factor", "refused", "rate_limited", None)).unwrap();
    let mut log = Log { lines: vec![ResetAuditRow::new("earlier", &a, None, "factor", "refused", "rate_limited", None)],
        ..Log::default() };
    let db = reopen_pre_marker(&dir, db);
    assert_eq!(marker(&db, "legacy-2"), Some(0));
    // A row written after the migration stays unmarked until its first drain.
    db.mfa_reset_outbox_record(&ResetAuditRow::new("fresh", &a, None, "factor", "refused", "rate_limited", None)).unwrap();
    assert_eq!(marker(&db, "fresh"), None);
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 2);
    assert_eq!(log.ids(), ["earlier", "legacy-2", "fresh"], "exactly one record each");
    assert!(outbox(&db).is_empty());
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 0);
    assert_eq!(log.lines.len(), 3);
}

#[test]
fn reset_outbox_held_release_failure_keeps_one_state_until_retried() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let start = now_secs();
    crate::mfa::passkey::ceremony_tests::register(&db, &a, PASSWORD, start);
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 30 * DAY);
    db.mfa_admin_reset_email_begin_recorded("attempt-h", "email_code_begin", &a, &b, PASSWORD, SESSION, true, start + 61)
        .ok().expect("code issued");
    // Releasing fails: the held row stays the attempt's only state, unchanged.
    db.conn().execute_batch("CREATE TRIGGER release_fails BEFORE UPDATE OF held ON mfa_reset_audit_outbox
        BEGIN SELECT RAISE(ABORT, 'disk full'); END;").unwrap();
    assert!(db.mfa_reset_outbox_release("attempt-h", "refused", "email_unavailable").is_err());
    let rows = outbox(&db);
    assert_eq!((rows.len(), rows[0].held, rows[0].outcome.as_str()), (1, true, "challenge_sent"));
    // The retry succeeds; the start-up drain then delivers one final record.
    db.conn().execute_batch("DROP TRIGGER release_fails;").unwrap();
    assert!(db.mfa_reset_outbox_release("attempt-h", "refused", "email_unavailable").unwrap());
    let mut log = Log::default();
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), 1);
    assert_eq!(log.lines.len(), 1);
    assert_eq!((log.lines[0].outcome.as_str(), log.lines[0].reason.as_str(), log.lines[0].held),
        ("refused", "email_unavailable", false));
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), 0);

    // A held row a start-up drain has marked is frozen: no late release can
    // change the outcome its (possibly already appended) record carries.
    db.mfa_admin_reset_email_begin_recorded("attempt-i", "email_code_begin", &a, &b, PASSWORD, SESSION, true, start + 122)
        .ok().expect("code issued");
    let mut log = Log { fail_on: Some("attempt-i".into()), ..Log::default() };
    assert!(db.mfa_reset_outbox_drain(true, &mut log).is_err());
    assert!(marker(&db, "attempt-i").is_some());
    assert!(!db.mfa_reset_outbox_release("attempt-i", "challenge_sent", "challenge_sent").unwrap());
    // Marked, so even a per-attempt drain (no held rows) delivers it once.
    log.fail_on = None;
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 1);
    assert_eq!((log.lines.len(), log.lines[0].held), (1, true));
}

#[test]
fn reset_outbox_email_start_row_is_held_until_released() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let a = admin(&db, "a@example.test");
    let start = now_secs();
    crate::mfa::passkey::ceremony_tests::register(&db, &a, PASSWORD, start);
    let b = eligible_admin(&db, "b@example.test");
    age_log(&db, 30 * DAY);
    let now = start + 61;
    db.mfa_admin_reset_email_begin_recorded("attempt-e", "email_code_begin", &a, &b, PASSWORD, SESSION, true, now)
        .ok().expect("code issued");
    let rows = outbox(&db);
    assert_eq!((rows.len(), rows[0].held, rows[0].outcome.as_str()), (1, true, "challenge_sent"));
    // The per-attempt drain skips held rows.
    let mut log = Log::default();
    assert_eq!(db.mfa_reset_outbox_drain(false, &mut log).unwrap(), 0);
    assert!(log.lines.is_empty());
    assert!(db.mfa_reset_outbox_release("attempt-e", "refused", "email_unavailable").unwrap());
    assert!(!db.mfa_reset_outbox_release("attempt-e", "refused", "email_unavailable").unwrap());
    db.mfa_reset_outbox_drain(false, &mut log).unwrap();
    let seen: Vec<_> = log.lines.iter().map(|row| (row.outcome.as_str(), row.reason.as_str())).collect();
    assert_eq!(seen, [("refused", "email_unavailable")]);
    // A refused start records nothing itself (the caller records it).
    assert!(db.mfa_admin_reset_email_begin_recorded("attempt-f", "email_code_begin", &a, &a, PASSWORD, SESSION, true, now).is_err());
    assert!(outbox(&db).is_empty());
    // A start-up drain also delivers a held row left by a dead process.
    db.mfa_admin_reset_email_begin_recorded("attempt-g", "email_code_begin", &a, &b, PASSWORD, SESSION, true, now + 61)
        .ok().expect("code issued");
    let mut log = Log::default();
    assert_eq!(db.mfa_reset_outbox_drain(true, &mut log).unwrap(), 1);
    assert!(log.lines[0].held);
}

#[test]
fn refusal_codes_and_reasons_are_a_closed_set() {
    let all = [InvalidRequest, SelfReset, ActorNotAdmin, TargetUnavailable, SecondFactorRequired, UseAuthenticator,
        NoSecondFactor, FactorFailed, EmailUnavailable, RateLimited, Cooling, AdminLogUnavailable, Unavailable];
    let codes: std::collections::BTreeSet<_> = all.iter().map(|r| r.code()).collect();
    let reasons: std::collections::BTreeSet<_> = all.iter().map(|r| r.reason()).collect();
    assert_eq!((codes.len(), reasons.len()), (all.len(), all.len()));
    assert!(all.iter().all(|r| r.code().starts_with("MFA_RESET_") && !r.message().is_empty()));
}
