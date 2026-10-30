use super::*;
struct Cipher;
impl MfaCipher for Cipher {
    fn encrypt(&self, s: &str) -> Result<String> {
        Ok(format!("sealed:{s}"))
    }
    fn decrypt(&self, s: &str) -> Result<String> {
        s.strip_prefix("sealed:")
            .map(str::to_owned)
            .ok_or_else(|| DENIED.into())
    }
}
struct Broken;
impl MfaCipher for Broken {
    fn encrypt(&self, _: &str) -> Result<String> {
        Err(DENIED.into())
    }
    fn decrypt(&self, _: &str) -> Result<String> {
        Err(DENIED.into())
    }
}
fn setup() -> (tempfile::TempDir, UserDb, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let user = db
        .create_user("mfa@example.com", "MFA", "password-test", UserRole::Admin)
        .unwrap();
    (dir, db, user.id)
}
fn enroll(db: &UserDb, id: &str) -> (String, Vec<String>) {
    assert!(db.mfa_begin(id,None,&Cipher,3000).is_err());
    let token=db.mfa_password_challenge(id,"password-test","test-session",3000).unwrap();
    db.mfa_prepare_enrollment(&token,false,3000).unwrap();
    let secret = db.mfa_begin(id, Some(&token), &Cipher, 3000).unwrap();
    let code = hotp(&decode_secret(&secret).unwrap(), 100, 6);
    let codes = db.mfa_confirm(id, Some(&token), &code, &Cipher, 3000).unwrap();
    (secret, codes)
}
#[test]
fn rfc6238_sha1_vectors_and_six_digit_truncation() {
    for (time, expected) in [
        (59, "94287082"),
        (1111111109, "07081804"),
        (1111111111, "14050471"),
        (1234567890, "89005924"),
        (2000000000, "69279037"),
        (20000000000, "65353130"),
    ] {
        assert_eq!(hotp(b"12345678901234567890", time / 30, 8), expected);
        assert_eq!(
            hotp(b"12345678901234567890", time / 30, 6),
            format!("{:06}", expected.parse::<u32>().unwrap() % 1000000)
        );
    }
}
#[test]
fn mfa_window_and_replay() {
    let secret = base32(b"12345678901234567890");
    for step in [99, 100, 101] {
        let code = hotp(b"12345678901234567890", step, 6);
        assert_eq!(accepted_step(&secret, &code, 3000, -1).unwrap(), step);
        assert!(accepted_step(&secret, &code, 3000, step).is_err());
    }
    assert!(accepted_step(&secret, &hotp(b"12345678901234567890", 98, 6), 3000, -1).is_err());
}
#[test]
fn mfa_challenge_single_use_replay_restart_and_recovery() {
    let (dir, db, id) = setup();
    let (secret, codes) = enroll(&db, &id);
    assert_eq!(codes.len(), 10);
    let challenge = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3030)
        .unwrap();
    let db2 = UserDb::new(&dir.path().join("users.db")).unwrap();
    let code = hotp(&decode_secret(&secret).unwrap(), 101, 6);
    assert_eq!(
        db2.mfa_verify(&challenge, &code, false, &Cipher, 3030)
            .unwrap().0,
        id
    );
    assert!(
        db.mfa_verify(&challenge, &codes[0], true, &Cipher, 3030)
            .is_err()
    );
    let other = db
        .mfa_challenge(&id, FirstFactor::ChannelOtp, false, 3030)
        .unwrap();
    assert!(db.mfa_verify(&other, &code, false, &Cipher, 3030).is_err());
    assert_eq!(
        db.mfa_verify(&other, &codes[0], true, &Cipher, 3030)
            .unwrap().0,
        id
    );
    let third = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3030)
        .unwrap();
    assert!(
        db2.mfa_verify(&third, &codes[0], true, &Cipher, 3030)
            .is_err()
    );
    let conn = db.conn();
    let hash: String = conn
        .query_row(
            "SELECT code_hash FROM mfa_recovery_codes LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(hash.starts_with("$argon2"));
    let factor: String = conn
        .query_row(
            "SELECT first_factor FROM mfa_challenges WHERE token_hash=?1",
            [token_hash(&other)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(factor, "channel_otp");
}
#[test]
fn mfa_ttl_attempt_cap_and_decrypt_failure() {
    let (_dir, db, id) = setup();
    let (_, codes) = enroll(&db, &id);
    let expired = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3000)
        .unwrap();
    assert!(
        db.mfa_verify(&expired, &codes[0], true, &Cipher, 3300)
            .is_err()
    );
    let capped = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3000)
        .unwrap();
    for _ in 0..5 {
        assert!(
            db.mfa_verify(&capped, "wrong", false, &Cipher, 3000)
                .is_err()
        );
    }
    assert!(
        db.mfa_verify(&capped, &codes[0], true, &Cipher, 3000)
            .is_err()
    );
    let broken = db
        .mfa_challenge(&id, FirstFactor::Password, false, 4000)
        .unwrap();
    assert!(
        db.mfa_verify(&broken, "000000", false, &Broken, 4000)
            .is_err()
    );
    assert!(db.mfa_active(&id).unwrap());
    assert!(
        db.mfa_verify(&broken, &codes[0], true, &Broken, 4000)
            .is_ok()
    );
}
#[test]
fn mfa_enrollment_bound_to_user_and_purpose() {
    let (_dir, db, id) = setup();
    let other = db
        .create_user(
            "other@example.com",
            "Other",
            "password-test",
            UserRole::Employee,
        )
        .unwrap();
    let token = db
        .mfa_challenge(&id, FirstFactor::Password, true, 3000)
        .unwrap();
    assert!(
        db.mfa_begin(&other.id, Some(&token), &Cipher, 3000)
            .is_err()
    );
    assert!(db.mfa_begin(&id,Some(&token),&Cipher,3000).is_err());
    db.mfa_prepare_enrollment(&token,true,3000).unwrap();
    let proof=db.mfa_issue_enrollment_code(None,&id,3000).unwrap();
    db.mfa_prove_enrollment(&token,&proof,3000).unwrap();
    let secret = db.mfa_begin(&id, Some(&token), &Cipher, 3000).unwrap();
    assert!(!db.mfa_active(&id).unwrap());
    let code = hotp(&decode_secret(&secret).unwrap(), 100, 6);
    assert!(db.mfa_verify(&token, &code, false, &Cipher, 3000).is_err());
    assert!(db.mfa_confirm(&id, None, &code, &Cipher, 3000).is_err());
    assert!(
        db.mfa_confirm(&id, Some(&token), &code, &Cipher, 3000)
            .is_ok()
    );
    assert!(db.mfa_enrollment_user(&token, 3000).is_err());
    assert!(
        db.mfa_confirm(&id, Some(&token), &code, &Cipher, 3000)
            .is_err()
    );
}
#[test]
fn mfa_reset_reads_fresh_admin_and_invalidates_challenges() {
    let (_dir, db, id) = setup();
    let (_, codes) = enroll(&db, &id);
    let employee = db
        .create_user(
            "employee@example.com",
            "Employee",
            "password-test",
            UserRole::Employee,
        )
        .unwrap();
    assert!(db.mfa_admin_reset(&employee.id, &id, &codes[0], &Cipher, 3000).is_err());
    let challenge = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3000)
        .unwrap();
    assert!(db.mfa_admin_reset(&id,&id,&codes[0],&Cipher,3000).is_err());
    let admin=db.create_user("second@example.com","Second","password-test",UserRole::Admin).unwrap();
    let (_,admin_codes)=enroll(&db,&admin.id);
    assert!(db.mfa_admin_reset(&admin.id,&id,"wrong",&Cipher,3000).is_err());
    db.mfa_admin_reset(&admin.id,&id,&admin_codes[0],&Cipher,3000).unwrap();
    assert!(!db.mfa_active(&id).unwrap());
    assert!(
        db.mfa_verify(&challenge, &codes[0], true, &Cipher, 3000)
            .is_err()
    );
    db.conn()
        .execute("UPDATE users SET role='employee' WHERE id=?1", [&id])
        .unwrap();
    assert!(db.mfa_admin_reset(&id, &employee.id, &codes[0], &Cipher, 3000).is_err());
}

#[test]
fn mfa_regenerate_replaces_all_codes_disable_requires_fresh_factor_and_audits() {
    let (_dir, db, id) = setup();
    let (_, old) = enroll(&db, &id);
    let new = db.mfa_change(&id, &old[0], false, &Cipher, 3030).unwrap();
    assert_eq!(new.len(), 10);
    assert!(db.mfa_change(&id, &old[1], true, &Cipher, 3030).is_err());
    assert!(db.mfa_active(&id).unwrap());
    db.mfa_change(&id, &new[0], true, &Cipher, 3030).unwrap();
    assert!(!db.mfa_active(&id).unwrap());
    let audit = db.query_audit_log(Some(&id), None, 100).unwrap();
    let text = serde_json::to_string(&audit).unwrap();
    for code in old.iter().chain(new.iter()) {
        assert!(!text.contains(code));
    }
    assert!(text.contains("mfa.recovery_regenerated"));
    assert!(text.contains("mfa.recovery_used"));
    assert!(text.contains("mfa.verification_failed"));
    assert!(text.contains("mfa.disabled"));
}

#[test]
fn mfa_cross_connection_recovery_race_has_one_winner() {
    let (dir, db, id) = setup();
    let (_, codes) = enroll(&db, &id);
    let token = db
        .mfa_challenge(&id, FirstFactor::Password, false, 3030)
        .unwrap();
    let second = UserDb::new(&dir.path().join("users.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let token2 = token.clone();
    let code = codes[0].clone();
    let task = std::thread::spawn(move || {
        gate.wait();
        second
            .mfa_verify(&token2, &code, true, &Cipher, 3030)
            .is_ok()
    });
    barrier.wait();
    let first = db
        .mfa_verify(&token, &codes[0], true, &Cipher, 3030)
        .is_ok();
    assert_ne!(first, task.join().unwrap());
}

#[test]
fn mfa_legacy_token_issuer_is_not_called_after_activation() {
    let (_dir, db, id) = setup();
    assert_eq!(
        db.mfa_issue_if_unenrolled(&id, || Ok("legacy")).unwrap(),
        "legacy"
    );
    enroll(&db, &id);
    let called = std::cell::Cell::new(false);
    assert!(
        db.mfa_issue_if_unenrolled(&id, || {
            called.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!called.get());
}

#[test]
fn mfa_password_step_up_session_binding_and_epoch_revocation() {
    let (_dir,db,id)=setup();
    assert!(db.mfa_password_challenge(&id,"wrong","session-a",3000).is_err());
    let token=db.mfa_password_challenge(&id,"password-test","session-a",3000).unwrap();
    db.mfa_prepare_enrollment(&token,false,3000).unwrap();
    assert!(db.mfa_check_session(&token,&id,"session-b",3000).is_err());
    assert!(db.mfa_check_session(&token,"other","session-a",3000).is_err());
    assert!(db.mfa_check_session(&token,&id,"session-a",3300).is_err());
    assert!(db.mfa_http_enrollment(&token,None,3000).is_err());
    db.mfa_check_session(&token,&id,"session-a",3000).unwrap();
    assert!(db.mfa_require_independent_proof(&token,3000).is_err());
    let secret=db.mfa_begin(&id,Some(&token),&Cipher,3000).unwrap();
    let codes=db.mfa_confirm(&id,Some(&token),&hotp(&decode_secret(&secret).unwrap(),100,6),&Cipher,3000).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,1);
    db.update_user(&id,None,None,Some("new-password")).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,2);
    db.mfa_change(&id,&codes[0],true,&Broken,3030).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,3);
    db.mfa_operator_reset("mfa@example.com").unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,4);
}
#[test]
fn mfa_proof_is_single_use_user_bound_attempt_capped_and_hashed() {
    let (_dir,db,id)=setup();
    let other=db.create_user("other@example.com","Other","password-test",UserRole::Admin).unwrap();
    assert!(db.mfa_issue_enrollment_code(Some(&id),&id,3000).is_err());
    let code=db.mfa_issue_enrollment_code(Some(&other.id),&id,3000).unwrap();
    let token=db.mfa_challenge(&id,FirstFactor::Password,true,3000).unwrap();
    db.mfa_prepare_enrollment(&token,true,3000).unwrap();
    assert!(db.mfa_begin(&id,Some(&token),&Cipher,3000).is_err());
    assert!(db.mfa_prove_enrollment(&token,"wrong",3000).is_err());
    db.mfa_prove_enrollment(&token,&code,3000).unwrap();
    let second=db.mfa_challenge(&id,FirstFactor::Password,true,3000).unwrap();
    db.mfa_prepare_enrollment(&second,true,3000).unwrap();
    assert!(db.mfa_prove_enrollment(&second,&code,3000).is_err());
    let code2=db.mfa_issue_enrollment_code(Some(&other.id),&id,3000).unwrap();
    let hash:String=db.conn().query_row("SELECT code_hash FROM mfa_enrollment_codes WHERE user_id=?1",[&id],|r|r.get(0)).unwrap();
    assert_ne!(hash,code2);
    for _ in 0..4 { assert!(db.mfa_prove_enrollment(&second,"wrong",3000).is_err()); }
    assert!(db.mfa_prove_enrollment(&second,&code2,3000).is_err());
    assert!(db.mfa_prove_enrollment(&token,&code,3300).is_err());
}
#[test]
fn mfa_channel_proof_cannot_be_used_as_login_otp_or_on_another_challenge() {
    let (_dir,db,id)=setup();
    db.bind_channel_identity(&id,"telegram","12345",true).unwrap();
    let token=db.mfa_challenge(&id,FirstFactor::Password,true,3000).unwrap();
    let otp=db.mfa_prepare_enrollment(&token,true,3000).unwrap().unwrap();
    assert!(db.verify_otp(&token,&otp.code).is_err());
    assert!(db.mfa_prepare_enrollment(&token,true,3000).is_err());
    let second=db.mfa_challenge(&id,FirstFactor::Password,true,3000).unwrap();
    db.mfa_prepare_enrollment(&second,true,3000).unwrap();
    db.mfa_prove_enrollment(&token,&otp.code,3000).unwrap();
    assert!(db.mfa_begin(&id,Some(&second),&Cipher,3000).is_err());
}

#[test]
fn mfa_never_enrolled_password_change_preserves_epoch_zero_but_cancels_pending() {
    let (_dir,db,id)=setup();
    let token=db.mfa_password_challenge(&id,"password-test","session",3000).unwrap();
    db.mfa_prepare_enrollment(&token,false,3000).unwrap();
    db.update_user(&id,None,None,Some("new-password")).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,0);
    assert!(db.mfa_enrollment_user(&token,3000).is_err());
}
