//! RM-135: the bootstrap proof is accepted only for the bootstrap owner of an
//! enforced installation on which no Administrator has a second factor and
//! the closed latch is not set; it is granted only by the host setup code,
//! which is hashed, single use, expiring and attempt-limited.
use super::*;
use crate::mfa::tests::Cipher;

const PASSWORD: &str = "password-test";

/// A claimed team installation under an enforced policy: only the
/// bootstrap owner exists and nobody has a second factor.
fn fresh() -> (tempfile::TempDir, UserDb, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    db.ensure_default_admin().unwrap().expect("owner created");
    assert!(db.claim_default_admin_with_email(Some("owner@example.test"), PASSWORD).unwrap());
    let owner = db.get_bootstrap_owner().unwrap().unwrap().id;
    record_enforcement(&mut db.conn(), true, 1).unwrap();
    (dir, db, owner)
}

/// What `/api/login` creates for an unenrolled account under enforcement:
/// a public enrollment challenge with an unverified `admin` proof.
fn login_challenge(db: &UserDb, user: &str, now: i64) -> String {
    let token = db.mfa_challenge(user, FirstFactor::Password, true, now).unwrap();
    assert!(db.mfa_prepare_enrollment(&token, true, now).unwrap().is_none());
    token
}

/// The rest of the HTTP enrollment (begin + confirm) for `token`.
fn finish_totp(db: &UserDb, user: &str, token: &str, now: i64) -> Result<Vec<String>> {
    db.mfa_require_independent_proof(token, now)?;
    let secret = db.mfa_begin(user, Some(token), &Cipher, now)?;
    db.mfa_confirm(user, Some(token), &hotp(&decode_secret(&secret).unwrap(), now / 30, 6), &Cipher, now)
}

fn outstanding_codes(db: &UserDb) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM mfa_bootstrap_tokens", [], |r| r.get(0)).unwrap()
}

fn give_factor(db: &UserDb, user: &str) {
    db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", [user]).unwrap();
}

/// Complete the owner's first enrollment through the setup code.
fn bootstrap_owner(db: &UserDb, owner: &str) {
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().expect("code issued");
    let token = login_challenge(db, owner, 3000);
    db.mfa_prove_bootstrap(&token, &code, 3000).unwrap();
    finish_totp(db, owner, &token, 3000).unwrap();
}

#[test]
fn bootstrap_setup_code_completes_first_enrollment_on_fresh_install() {
    let (_dir, db, owner) = fresh();
    assert!(db.mfa_bootstrap_available(&owner).unwrap());
    assert!(db.mfa_bootstrap_pending().unwrap());
    assert!(!db.mfa_bootstrap_closed().unwrap(), "a fresh installation starts open");
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().expect("code issued");
    let token = login_challenge(&db, &owner, 3000);
    // Without a bootstrap proof the enforced enrollment needs an independent proof.
    assert!(db.mfa_require_independent_proof(&token, 3000).is_err());
    db.mfa_prove_bootstrap(&token, &code, 3000).unwrap();
    assert_eq!(finish_totp(&db, &owner, &token, 3000).unwrap().len(), 10);
    assert!(db.mfa_active(&owner).unwrap());
    assert!(!db.mfa_bootstrap_available(&owner).unwrap());
    assert!(!db.mfa_bootstrap_pending().unwrap());
    let methods = db.query_audit_log(Some(&owner), Some("mfa.bootstrap_proved"), 10).unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].detail.as_deref(), Some("setup_code"));
}

#[test]
fn bootstrap_has_no_proof_without_a_setup_code() {
    let (_dir, db, owner) = fresh();
    let token = login_challenge(&db, &owner, 3000);
    // No outstanding code: an empty, malformed or guessed code is refused.
    for code in ["", "loopback", &"0".repeat(32)] {
        assert!(db.mfa_prove_bootstrap(&token, code, 3000).is_err(), "{code:?}");
    }
    assert!(db.mfa_require_independent_proof(&token, 3000).is_err());
    assert!(!db.mfa_active(&owner).unwrap());
}

#[test]
fn bootstrap_refused_once_any_admin_has_a_factor() {
    let (_dir, db, owner) = fresh();
    let other = db.create_user("other@example.test", "Other", PASSWORD, UserRole::Admin).unwrap().id;
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().expect("code issued");
    assert_eq!(outstanding_codes(&db), 1);
    give_factor(&db, &other);
    // The trigger invalidated the outstanding code and set the latch in the same statement.
    assert_eq!(outstanding_codes(&db), 0);
    assert!(db.mfa_bootstrap_closed().unwrap());
    assert!(!db.mfa_bootstrap_available(&owner).unwrap());
    let token = login_challenge(&db, &owner, 3000);
    assert!(db.mfa_prove_bootstrap(&token, &code, 3000).is_err());
    assert_eq!(db.mfa_issue_bootstrap_token("test", 3001).unwrap_err(), BOOTSTRAP_CLOSED);
}

#[test]
fn bootstrap_proof_is_rechecked_when_the_factor_is_written() {
    let (_dir, db, owner) = fresh();
    let other = db.create_user("other@example.test", "Other", PASSWORD, UserRole::Admin).unwrap().id;
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().unwrap();
    let token = login_challenge(&db, &owner, 3000);
    db.mfa_prove_bootstrap(&token, &code, 3000).unwrap();
    let secret = db.mfa_begin(&owner, Some(&token), &Cipher, 3000).unwrap();
    give_factor(&db, &other);
    let code = hotp(&decode_secret(&secret).unwrap(), 100, 6);
    assert!(db.mfa_confirm(&owner, Some(&token), &code, &Cipher, 3000).is_err());
    assert!(!db.mfa_active(&owner).unwrap());
}

#[test]
fn bootstrap_only_for_the_owner_under_enforcement_and_public_login_challenge() {
    let (_dir, db, owner) = fresh();
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().unwrap();
    // Another Admin (not the bootstrap owner) never qualifies, even with the code.
    let other = db.create_user("other@example.test", "Other", PASSWORD, UserRole::Admin).unwrap().id;
    let token = login_challenge(&db, &other, 3000);
    assert!(db.mfa_prove_bootstrap(&token, &code, 3000).is_err());
    // A dashboard step-up capability (session-bound) is not a login challenge.
    let code = db.mfa_issue_bootstrap_token("test", 3100).unwrap().unwrap();
    let session = db.mfa_password_challenge(&owner, PASSWORD, "session", 3100).unwrap();
    assert!(db.mfa_prove_bootstrap(&session, &code, 3100).is_err());
    // Not enforced: no bootstrap at all (the password proof suffices there).
    record_enforcement(&mut db.conn(), false, 2).unwrap();
    assert!(!db.mfa_bootstrap_available(&owner).unwrap());
    assert!(!db.mfa_bootstrap_pending().unwrap());
    assert!(db.mfa_issue_bootstrap_token("test", 3200).unwrap().is_none());
    // A malformed enrollment capability is refused before any read.
    assert!(db.mfa_prove_bootstrap("not-a-token", &code, 3200).is_err());
}

#[test]
fn bootstrap_setup_code_is_hashed_single_use_and_bound_to_the_owner() {
    let (_dir, db, owner) = fresh();
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().expect("code issued");
    assert_eq!(code.len(), 32);
    let stored: String = db.conn().query_row("SELECT token_hash FROM mfa_bootstrap_tokens", [], |r| r.get(0)).unwrap();
    assert_ne!(stored, code);
    assert!(!stored.contains(&code));
    let token = login_challenge(&db, &owner, 3000);
    db.mfa_prove_bootstrap(&token, &code.to_ascii_uppercase(), 3000).unwrap();
    assert_eq!(outstanding_codes(&db), 0, "consumed by the first correct use");
    let again = login_challenge(&db, &owner, 3100);
    assert!(db.mfa_prove_bootstrap(&again, &code, 3100).is_err());
    finish_totp(&db, &owner, &token, 3000).unwrap();
    // The code never reaches the audit log.
    let leaked: i64 = db.conn().query_row("SELECT COUNT(*) FROM auth_audit_log
        WHERE instr(COALESCE(detail,''),?1)>0 OR instr(COALESCE(target,''),?1)>0", [&code], |r| r.get(0)).unwrap();
    assert_eq!(leaked, 0);
    let methods = db.query_audit_log(Some(&owner), Some("mfa.bootstrap_proved"), 10).unwrap();
    assert_eq!(methods[0].detail.as_deref(), Some("setup_code"));
}

#[test]
fn bootstrap_setup_code_expires_and_wrong_codes_are_limited() {
    let (_dir, db, owner) = fresh();
    // Expiry: one second past the TTL is refused and drops the code.
    let code = db.mfa_issue_bootstrap_token("test", 3000).unwrap().unwrap();
    let token = login_challenge(&db, &owner, 3000 + BOOTSTRAP_TOKEN_TTL - 100);
    assert!(db.mfa_prove_bootstrap(&token, &code, 3000 + BOOTSTRAP_TOKEN_TTL).is_err());
    assert_eq!(outstanding_codes(&db), 0);

    // A wrong (well-formed) code counts; a malformed one is refused untouched.
    let code = db.mfa_issue_bootstrap_token("test", 10_000).unwrap().unwrap();
    let wrong = if code.starts_with('0') { "1".repeat(32) } else { "0".repeat(32) };
    let attempts = || -> i64 { db.conn().query_row("SELECT attempts FROM mfa_bootstrap_tokens", [], |r| r.get(0)).unwrap() };
    let token = login_challenge(&db, &owner, 10_000);
    assert!(db.mfa_prove_bootstrap(&token, "short", 10_000).is_err());
    assert_eq!(attempts(), 0);
    for expected in 1..BOOTSTRAP_TOKEN_MAX_ATTEMPTS {
        assert!(db.mfa_prove_bootstrap(&token, &wrong, 10_000).is_err());
        assert_eq!(attempts(), expected);
    }
    let token = login_challenge(&db, &owner, 10_100);
    assert!(db.mfa_prove_bootstrap(&token, &wrong, 10_100).is_err());
    assert_eq!(outstanding_codes(&db), 0, "dropped after the attempt limit");
    assert!(db.mfa_prove_bootstrap(&token, &code, 10_100).is_err());
    assert!(!db.mfa_active(&owner).unwrap());
}

#[test]
fn bootstrap_new_code_replaces_the_old_one() {
    let (_dir, db, owner) = fresh();
    let old = db.mfa_issue_bootstrap_token("test", 3000).unwrap().unwrap();
    let new = db.mfa_issue_bootstrap_token("test", 3001).unwrap().unwrap();
    assert_ne!(old, new);
    assert_eq!(outstanding_codes(&db), 1);
    let token = login_challenge(&db, &owner, 3002);
    assert!(db.mfa_prove_bootstrap(&token, &old, 3002).is_err());
    let token = login_challenge(&db, &owner, 3100);
    db.mfa_prove_bootstrap(&token, &new, 3100).unwrap();
}

#[test]
fn bootstrap_latch_is_set_with_the_first_admin_factor_and_survives_deletion_and_demotion() {
    let (_dir, db, owner) = fresh();
    let other = db.create_user("other@example.test", "Other", PASSWORD, UserRole::Admin).unwrap().id;
    bootstrap_owner(&db, &owner);
    let (completed_at, reason): (i64, String) = db.conn().query_row(
        "SELECT completed_at,reason FROM mfa_bootstrap_state WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(completed_at > 0);
    assert_eq!(reason, "admin_factor");
    // Re-open attempt: drop the owner's factor, demote and suspend the other
    // Admin, delete it — the installation has no Admin factor again.
    db.mfa_operator_reset("owner@example.test").unwrap();
    db.conn().execute("UPDATE users SET role='employee' WHERE id=?1", [&other]).unwrap();
    db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1", [&other]).unwrap();
    db.conn().execute("DELETE FROM users WHERE id=?1", [&other]).unwrap();
    assert!(!db.mfa_active(&owner).unwrap());
    assert!(db.mfa_bootstrap_closed().unwrap());
    assert!(!db.mfa_bootstrap_available(&owner).unwrap());
    assert!(!db.mfa_bootstrap_pending().unwrap());
    assert_eq!(db.mfa_issue_bootstrap_token("test", 9000).unwrap_err(), BOOTSTRAP_CLOSED);
    assert!(BOOTSTRAP_CLOSED.contains("duduclaw ops users mfa-reset"));
    // The latch itself cannot be cleared or rewritten by any writer.
    assert!(db.conn().execute("DELETE FROM mfa_bootstrap_state", []).is_err());
    assert!(db.conn().execute("UPDATE mfa_bootstrap_state SET completed_at=0", []).is_err());
    assert!(db.mfa_bootstrap_closed().unwrap());
    // A new sign-in cannot make a bootstrap proof with any code either.
    let token = login_challenge(&db, &owner, 9000);
    assert!(db.mfa_prove_bootstrap(&token, &"a".repeat(32), 9000).is_err());
}

#[test]
fn bootstrap_unreadable_latch_refuses() {
    let (_dir, db, owner) = fresh();
    db.mfa_issue_bootstrap_token("test", 3000).unwrap().unwrap();
    db.conn().execute_batch("DROP TABLE mfa_bootstrap_state;").unwrap();
    assert!(db.mfa_bootstrap_available(&owner).is_err());
    assert!(db.mfa_bootstrap_pending().is_err());
    assert!(db.mfa_issue_bootstrap_token("test", 3001).is_err());
    let token = login_challenge(&db, &owner, 3000);
    assert!(db.mfa_prove_bootstrap(&token, &"a".repeat(32), 3000).is_err());
}

#[test]
fn bootstrap_migration_closes_an_existing_installation_with_an_admin_factor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("users.db");
    let owner = {
        let db = UserDb::new(&path).unwrap();
        db.ensure_default_admin().unwrap().expect("owner created");
        assert!(db.claim_default_admin_with_email(Some("owner@example.test"), PASSWORD).unwrap());
        let owner = db.get_bootstrap_owner().unwrap().unwrap().id;
        record_enforcement(&mut db.conn(), true, 1).unwrap();
        owner
    };
    // Simulate a users.db written by an older build: no latch table yet,
    // an Administrator already enrolled.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER mfa_bootstrap_state_no_delete; DROP TRIGGER mfa_bootstrap_state_no_update;
            DROP TABLE mfa_bootstrap_state;
            DROP TRIGGER mfa_bootstrap_latch_mfa_credentials_insert;").unwrap();
        conn.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", [&owner]).unwrap();
    }
    let db = UserDb::new(&path).unwrap();
    assert!(db.mfa_bootstrap_closed().unwrap());
    let reason: String = db.conn().query_row("SELECT reason FROM mfa_bootstrap_state", [], |r| r.get(0)).unwrap();
    assert_eq!(reason, "migration_existing_enrollment");
    // Removing that factor afterwards does not re-open it.
    db.mfa_operator_reset("owner@example.test").unwrap();
    assert!(!db.mfa_bootstrap_available(&owner).unwrap());
    assert!(db.mfa_issue_bootstrap_token("test", 3000).is_err());
}

#[test]
fn bootstrap_migration_keeps_a_never_enrolled_installation_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("users.db");
    {
        let db = UserDb::new(&path).unwrap();
        db.ensure_default_admin().unwrap().expect("owner created");
        assert!(db.claim_default_admin_with_email(Some("owner@example.test"), PASSWORD).unwrap());
        record_enforcement(&mut db.conn(), true, 1).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER mfa_bootstrap_state_no_delete; DROP TRIGGER mfa_bootstrap_state_no_update;
            DROP TABLE mfa_bootstrap_state;").unwrap();
    }
    // Only `admin_created` / non-eligible log rows, no factor: still open.
    let db = UserDb::new(&path).unwrap();
    assert!(!db.mfa_bootstrap_closed().unwrap());
    assert!(db.mfa_bootstrap_pending().unwrap());
    // Re-running the migration later never closes it by itself either.
    drop(db);
    let db = UserDb::new(&path).unwrap();
    assert!(!db.mfa_bootstrap_closed().unwrap());
}

#[test]
fn bootstrap_setup_code_completes_email_enrollment_and_cannot_be_reused() {
    let (_dir,db,owner)=fresh();
    let token=login_challenge(&db,&owner,3000);
    assert!(db.mfa_email_send(&token,true,3000).is_err());
    let setup=db.mfa_issue_bootstrap_token("test",3000).unwrap().unwrap();
    db.mfa_prove_bootstrap(&token,&setup,3000).unwrap();
    db.mfa_prove_enrollment(&token,"",3000).unwrap();
    let other=login_challenge(&db,&owner,3000);
    assert!(db.mfa_prove_bootstrap(&other,&setup,3000).is_err());
    let (_,code)=db.mfa_email_send(&token,true,3000).unwrap();
    let recovery=db.mfa_email_verify_and_issue(&token,&code,true,3000,|_,_,codes|Ok(codes)).unwrap();
    assert_eq!(recovery.len(),10);
    assert!(db.mfa_bootstrap_closed().unwrap());
    assert!(db.mfa_email_verify_and_issue(&token,&code,true,3000,|_,_,_| -> Result<()> {
        panic!("replayed bootstrap issued tokens")
    }).is_err());
}

#[test]
fn bootstrap_email_write_rechecks_window_after_delivery() {
    let (_dir,db,owner)=fresh();
    let other=db.create_user("other@example.test","Other",PASSWORD,UserRole::Admin).unwrap();
    let token=login_challenge(&db,&owner,3000);
    let setup=db.mfa_issue_bootstrap_token("test",3000).unwrap().unwrap();
    db.mfa_prove_bootstrap(&token,&setup,3000).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,3000).unwrap();
    give_factor(&db,&other.id);
    assert!(db.mfa_email_send(&token,true,3060).is_err());
    assert!(db.mfa_email_verify_and_issue(&token,&code,true,3001,|_,_,_| -> Result<()> {
        panic!("closed bootstrap issued tokens")
    }).is_err());
    assert!(!db.mfa_active(&owner).unwrap());
}
