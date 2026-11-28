use super::*;

struct Cipher;
impl MfaCipher for Cipher {
    fn encrypt(&self, text: &str) -> Result<String> { Ok(text.into()) }
    fn decrypt(&self, text: &str) -> Result<String> { Ok(text.into()) }
}
fn setup() -> (UserDb, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let user = db.create_user("google@example.test", "Google", "password-test", UserRole::Employee).unwrap();
    db.conn().execute("INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
        VALUES(?1,'123','example.test','google@example.test','jit_direct',?1,1)", [&user.id]).unwrap();
    (db, dir, user.id)
}
fn passwordless(db: &UserDb, id: &str) -> u64 {
    db.conn().execute("UPDATE users SET password_hash='' WHERE id=?1", [id]).unwrap();
    db.get_user(id).unwrap().unwrap().credential_epoch
}
#[test]
fn password_account_cannot_use_grant() {
    let (db, _dir, id) = setup();
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    assert!(db.mfa_reauth_challenge(&id, "session", epoch, 3000).is_err());
    assert!(db.mfa_factor_reauth_challenge(&id, "session", epoch, 3000).is_err());
}
#[test]
fn reauth_challenge_requires_link_session_and_current_epoch() {
    let (db, _dir, id) = setup();
    let epoch = passwordless(&db, &id);
    assert!(db.mfa_reauth_challenge(&id, "", epoch, 3000).is_err());
    assert!(db.mfa_reauth_challenge(&id, "session", epoch+1, 3000).is_err());
    let token = db.mfa_reauth_challenge(&id, "session", epoch, 3000).unwrap();
    assert!(db.mfa_check_session(&token, &id, "other", 3000).is_err());
    assert!(db.mfa_http_enrollment(&token, None, 3000).is_err());
    db.delete_google_signin_link(&id).unwrap();
    assert!(db.mfa_reauth_challenge(&id, "session", epoch, 3000).is_err());
}
#[test]
fn reauth_grant_allows_totp_enrollment_and_passkey_delete() {
    let (db, _dir, id) = setup();
    // A real signed WebAuthn ceremony creates the factor that Google reauth removes.
    let _key = passkey::ceremony_tests::register(&db, &id, "password-test", 2900);
    let epoch = passwordless(&db, &id);
    let token = db.mfa_reauth_challenge(&id, "session", epoch, 3000).unwrap();
    db.mfa_prepare_enrollment(&token, false, 3000).unwrap();
    let first: String = db.conn().query_row("SELECT first_factor FROM mfa_challenges WHERE token_hash=?1", [token_hash(&token)], |r| r.get(0)).unwrap();
    assert_eq!(first, "google_oidc");
    assert!(db.mfa_email_send(&token, true, 3000).is_err());
    let secret = db.mfa_begin(&id, Some(&token), &Cipher, 3000).unwrap();
    db.mfa_confirm(&id, Some(&token), &test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    let token = db.mfa_factor_reauth_challenge(&id, "session", epoch, 3061).unwrap();
    let credential: String = db.conn().query_row("SELECT id FROM mfa_passkeys WHERE user_id=?1", [&id], |r| r.get(0)).unwrap();
    assert!(db.mfa_passkey_delete(&id, &credential, &token, "other", 3061, false, |_,_,_| Ok(())).is_err());
    db.mfa_passkey_delete(&id, &credential, &token, "session", 3061, false, |_,_,_| Ok(())).unwrap();
    assert!(db.mfa_passkeys(&id).unwrap().is_empty());
    assert!(db.mfa_active(&id).unwrap());
    assert!(db.mfa_passkey_delete(&id, &credential, &token, "session", 3061, false, |_,_,_| Ok(())).is_err());
}
#[test]
fn reauth_first_enrollment_keeps_independent_proof_and_passkey_support() {
    let (db, _dir, id) = setup();
    let epoch = passwordless(&db, &id);
    record_enforcement(&mut db.conn(), true, 2990).unwrap();
    let token = db.mfa_reauth_challenge(&id, "session", epoch, 3000).unwrap();
    db.mfa_prepare_enrollment(&token, true, 3000).unwrap();
    assert!(db.mfa_require_independent_proof(&token, 3000).is_err());
    assert!(db.mfa_passkey_register_begin(&id, &token, Some("session"), "https://dashboard.example.test", "Key", 3000, false).is_err());
    record_enforcement(&mut db.conn(), false, 3061).unwrap();
    let token = db.mfa_reauth_challenge(&id, "session", epoch, 3061).unwrap();
    db.mfa_prepare_enrollment(&token, false, 3061).unwrap();
    assert!(db.mfa_passkey_register_begin(&id, &token, Some("session"), "https://dashboard.example.test", "Key", 3061, false).is_ok());
    assert!(db.mfa_email_send(&token, true, 3061).is_err());
}
