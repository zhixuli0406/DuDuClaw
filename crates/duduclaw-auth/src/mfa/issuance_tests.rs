use super::*;
use super::tests::{enroll, setup};
use crate::jwt::JwtConfig;
use std::cell::Cell;

fn jwt() -> JwtConfig {
    JwtConfig::new(b"issuance-race-test-key-at-least-32-bytes")
}

fn enable_after_early_check(dir: &tempfile::TempDir, db: &UserDb, id: &str) {
    // The request passed its early policy/factor check. A separate admin
    // connection commits enforcement before the request enters issuance.
    assert!(!read_enforcement(&db.conn()).unwrap());
    assert!(!db.mfa_active(id).unwrap());
    let mut admin = Connection::open(dir.path().join("users.db")).unwrap();
    record_enforcement(&mut admin, true, 123).unwrap();
}

#[test]
fn login_policy_toggle_between_first_factor_and_issuance_denies_tokens() {
    let (dir, db, id) = setup();
    let verified = db.verify_password("mfa@example.com", "password-test").unwrap();
    enable_after_early_check(&dir, &db, &id);
    let called = Cell::new(false);
    assert!(db.mfa_issue_if_unenrolled(&id, verified.credential_epoch, false, |user, access| {
        called.set(true);
        jwt().issue_session_tokens(user, access)
    }).is_err());
    assert!(!called.get());
    // Personal must obey the same stored policy, even with the legacy flag set.
    assert!(db.mfa_issue_if_unenrolled(&id, verified.credential_epoch, true,
        |_, _| -> Result<()> { panic!("issuer called") }).is_err());
}

#[test]
fn refresh_policy_toggle_after_early_check_denies_tokens() {
    let (dir, db, id) = setup();
    let jwt = jwt();
    let (_, refresh) = db.mfa_issue_if_unenrolled(&id, 0, false,
        |user, access| jwt.issue_session_tokens(user, access)).unwrap();
    let claims = jwt.verify_refresh_token(&refresh).unwrap();
    enable_after_early_check(&dir, &db, &id);
    let called = Cell::new(false);
    assert!(db.mfa_refresh_and_issue(&id, claims.credential_epoch, claims.jti.as_deref(), false, |user, access| {
        called.set(true);
        jwt.issue_access_token_in_session(user, access, claims.jti.as_deref())
    }).is_err());
    assert!(!called.get());
    assert!(db.mfa_refresh_and_issue(&id, claims.credential_epoch, claims.jti.as_deref(), true,
        |_, _| -> Result<()> { panic!("issuer called") }).is_err());
}

#[test]
fn remembered_refresh_policy_toggle_denies_before_issuer_or_rotation() {
    let (dir, db, id) = setup();
    let family = uuid::Uuid::new_v4().to_string();
    let (_, secret, _) = db.remember_create(&id, 0, &family, "Browser", None, 180, 1000, false,
        |user, access, session| jwt().issue_access_token_in_session(user, access, Some(session))).unwrap();
    enable_after_early_check(&dir, &db, &id);
    let called = Cell::new(false);
    assert!(db.remember_rotate(&secret, 1001, false, |user, access, session| {
        called.set(true);
        jwt().issue_access_token_in_session(user, access, Some(session))
    }).is_err());
    assert!(!called.get());
    let generation: i64 = db.conn().query_row(
        "SELECT generation FROM remembered_families WHERE id=?1", [&family], |r| r.get(0)).unwrap();
    assert_eq!(generation, 0);
    assert!(db.remember_rotate(&secret, 1001, true,
        |_, _, _| -> Result<()> { panic!("issuer called") }).is_err());
    assert_eq!(db.conn().query_row("SELECT generation FROM remembered_families WHERE id=?1", [&family], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn remembered_creation_policy_toggle_denies_before_issuer_or_persistence() {
    let (dir, db, id) = setup();
    let family = uuid::Uuid::new_v4().to_string();
    enable_after_early_check(&dir, &db, &id);
    let called = Cell::new(false);
    assert!(db.remember_create(&id, 0, &family, "Browser", None, 180, 1000, false, |_, _, _| {
        called.set(true);
        Ok(())
    }).is_err());
    assert!(!called.get());
    assert!(db.remember_devices(&id, 1000).unwrap().is_empty());
    assert!(!db.remember_login_exchanged(Some(&family)).unwrap());
    assert!(db.remember_create(&id, 0, &family, "Browser", None, 180, 1000, true,
        |_, _, _| -> Result<()> { panic!("issuer called") }).is_err());
    assert!(db.remember_devices(&id, 1000).unwrap().is_empty());
    assert!(!db.remember_login_exchanged(Some(&family)).unwrap());
}

#[test]
fn issuance_rechecks_account_epoch_and_factor_even_for_personal() {
    for mutation in [
        "UPDATE users SET status='suspended' WHERE id=?1",
        "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1",
    ] {
        let (_dir, db, id) = setup();
        db.conn().execute(mutation, [&id]).unwrap();
        for exempt in [false, true] {
            assert!(db.mfa_issue_if_unenrolled(&id, 0, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
            assert!(db.mfa_refresh_and_issue(&id, 0, None, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
        }
    }
    let (_dir, db, id) = setup();
    enroll(&db, &id);
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    assert!(epoch > 0);
    for exempt in [false, true] {
        assert!(db.mfa_issue_if_unenrolled(&id, epoch, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
        assert!(db.mfa_refresh_and_issue(&id, 0, None, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
    }
    record_enforcement(&mut db.conn(), true, 123).unwrap();
    let jwt = jwt();
    // A verified MFA login at the current epoch may still renew under enforcement.
    let challenge = db.mfa_challenge(&id, FirstFactor::Password, false, 3030).unwrap();
    let secret_enc: String = db.conn().query_row("SELECT secret_enc FROM mfa_credentials WHERE user_id=?1", [&id], |r| r.get(0)).unwrap();
    let secret = decrypt(&tests::Cipher, &id, &secret_enc).unwrap();
    let code = hotp(&decode_secret(&secret).unwrap(), 101, 6);
    let (_, verified_epoch) = db.mfa_verify(&challenge, &code, false, &tests::Cipher, 3030).unwrap();
    assert!(db.mfa_refresh_and_issue(&id, verified_epoch, None, false,
        |user, access| jwt.issue_access_token(user, access)).is_ok());
    let family = uuid::Uuid::new_v4().to_string();
    let (_, secret, _) = db.remember_create(&id, verified_epoch, &family, "Browser", None, 180, 3040, false,
        |_, _, _| Ok(())).unwrap();
    assert!(db.remember_rotate(&secret, 3041, false, |_, _, _| Ok(())).is_ok());
    // Simulate a missing factor without an epoch bump: enforcement still denies.
    db.conn().execute("DELETE FROM mfa_credentials WHERE user_id=?1", [&id]).unwrap();
    assert!(db.mfa_refresh_and_issue(&id, verified_epoch, None, false,
        |_, _| -> Result<()> { panic!("issuer called") }).is_err());
}

#[test]
fn all_issuers_fail_closed_on_missing_or_malformed_policy_including_personal() {
    for sql in ["DELETE FROM mfa_policy_state", "UPDATE mfa_policy_state SET enforced=2", "DROP TABLE mfa_policy_state"] {
        let (_dir, db, id) = setup();
        let family = uuid::Uuid::new_v4().to_string();
        let (_, secret, _) = db.remember_create(&id, 0, &family, "Browser", None, 180, 1000, false, |_, _, _| Ok(())).unwrap();
        db.conn().execute_batch(sql).unwrap();
        for exempt in [false, true] {
            assert!(db.mfa_issue_if_unenrolled(&id, 0, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
            assert!(db.mfa_refresh_and_issue(&id, 0, None, exempt, |_, _| -> Result<()> { panic!("issuer called") }).is_err());
            assert!(db.remember_create(&id, 0, &uuid::Uuid::new_v4().to_string(), "Browser", None, 180, 1001, exempt,
                |_, _, _| -> Result<()> { panic!("issuer called") }).is_err());
            assert!(db.remember_rotate(&secret, 1001, exempt,
                |_, _, _| -> Result<()> { panic!("issuer called") }).is_err());
        }
    }
}

#[test]
fn normal_refresh_rechecks_remembered_exchange_inside_issuance_transaction() {
    let (_dir, db, id) = setup();
    let family = uuid::Uuid::new_v4().to_string();
    assert!(!db.remember_login_exchanged(Some(&family)).unwrap());
    db.remember_create(&id, 0, &family, "Browser", None, 180, 1000, false, |_, _, _| Ok(())).unwrap();
    assert!(db.mfa_refresh_and_issue(&id, 0, Some(&family), false,
        |_, _| -> Result<()> { panic!("issuer called") }).is_err());
}

// Shared with the signed passkey ceremony test: enrollment through either new
// factor must satisfy enforcement without making one-factor issuance possible.
pub(super) fn assert_enrolled_refresh_under_policy(db: &UserDb, id: &str, now: i64) {
    record_enforcement(&mut db.conn(), true, now).unwrap();
    let user = db.get_user(id).unwrap().unwrap();
    assert!(user.credential_epoch > 0);
    assert!(db.mfa_issue_refresh(id, user.credential_epoch,
        |user, access| jwt().issue_access_token(user, access)).is_ok());
    assert!(db.mfa_issue_local_session(id, user.credential_epoch,
        |_, _| -> Result<()> { panic!("local issuer called under enforcement") }).is_err());
    for personal in [false, true] {
        assert!(db.mfa_issue_if_unenrolled(id, user.credential_epoch, personal,
            |_, _| -> Result<()> { panic!("one-factor issuer called") }).is_err());
        assert!(db.mfa_refresh_and_issue(id, user.credential_epoch - 1, None, personal,
            |_, _| -> Result<()> { panic!("stale issuer called") }).is_err());
        assert!(db.mfa_refresh_and_issue(id, user.credential_epoch, None, personal,
            |user, access| jwt().issue_access_token(user, access)).is_ok());
        let (_, secret, expires) = db.remember_create(id, user.credential_epoch,
            &uuid::Uuid::new_v4().to_string(), "Browser", None, 180, now, personal,
            |user, access, session| jwt().issue_access_token_in_session(user, access, Some(session))).unwrap();
        let (token, _, renewed) = db.remember_rotate(&secret, now + 1, personal,
            |user, access, session| jwt().issue_access_token_in_session(user, access, Some(session))).unwrap();
        assert_eq!(jwt().verify_access_token(&token).unwrap().credential_epoch, user.credential_epoch);
        assert_eq!(renewed, expires + 1);
    }
}

#[test]
fn email_only_enrollment_satisfies_policy_for_normal_and_remembered_refresh() {
    let (_dir, db, id) = setup();
    let token = db.mfa_factor_password_challenge(&id, "password-test", "session", 3000).unwrap();
    let (_, code) = db.mfa_email_send(&token, true, 3000).unwrap();
    db.mfa_email_verify_and_issue(&token, &code, true, 3000, |_, _, _| Ok(())).unwrap();
    assert_enrolled_refresh_under_policy(&db, &id, 3030);
}
