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
    // Personal is an explicit exemption, not an implicit bypass in the handler.
    assert!(db.mfa_issue_if_unenrolled(&id, verified.credential_epoch, true,
        |user, access| jwt().issue_session_tokens(user, access)).is_ok());
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
        |user, access| jwt.issue_access_token_in_session(user, access, claims.jti.as_deref())).is_ok());
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
    assert!(db.remember_rotate(&secret, 1001, true, |user, access, session|
        jwt().issue_access_token_in_session(user, access, Some(session))).is_ok());
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
        |user, access, session| jwt().issue_access_token_in_session(user, access, Some(session))).is_ok());
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
