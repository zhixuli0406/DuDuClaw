use super::*;
use super::tests::{setup, Cipher};
use crate::signin_members::{SigninApproval, SigninOutcome, SigninPolicy, VerifiedGoogleIdentity};

const NOW: i64 = 1_800_000_000;

fn approved(preauthorize: bool) -> (tempfile::TempDir, UserDb, String, String, String) {
    let (dir, db, admin) = setup();
    record_enforcement(&mut db.conn(),true,NOW*1_000_000).unwrap();
    let identity = VerifiedGoogleIdentity {
        sub: "214".into(), email: "colleague@company.test".into(),
        hd: "company.test".into(), display_name: "新同仁".into(),
    };
    let policy = SigninPolicy {
        approval: SigninApproval::Admin, daily_cap: 100,
        settings_version: "test".into(), ack_by: admin.clone(), ack_at: "test".into(),
    };
    let request = match db.resolve_google_signin(&identity,&policy,NOW, &|_| Ok(None)).unwrap() {
        SigninOutcome::PendingApproval { request_id } => request_id,
        other => panic!("unexpected outcome: {other:?}"),
    };
    let epoch = db.get_user(&admin).unwrap().unwrap().credential_epoch;
    let user = db.approve_signin_request(&admin,epoch,&request,preauthorize,NOW).unwrap();
    assert_eq!(db.get_user(&user).unwrap().unwrap().credential_epoch,0);
    (dir, db, admin, user, request)
}

fn prepare(db: &UserDb, user: &str, first: FirstFactor, now: i64) -> String {
    let token = db.mfa_challenge(user,first,true,now).unwrap();
    assert!(db.mfa_prepare_enrollment(&token,true,now).unwrap().is_none());
    token
}

fn proof(db: &UserDb, token: &str) -> (String,bool) {
    db.conn().query_row("SELECT kind,verified FROM mfa_enrollment_proofs WHERE token_hash=?1",
        [token_hash(token)], |r|Ok((r.get(0)?,r.get(1)?))).unwrap()
}

fn bump(db: &UserDb, user: &str) {
    db.conn().execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1",[user]).unwrap();
}

#[test]
fn google_oidc_name_roundtrip() {
    let (_dir, db, _admin, user, _request) = approved(false);
    let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,true,NOW).unwrap();
    let first: String = db.conn().query_row("SELECT first_factor FROM mfa_challenges WHERE token_hash=?1",
        [token_hash(&token)], |r|r.get(0)).unwrap();
    assert_eq!(first,"google_oidc");
    assert_eq!(db.mfa_enrollment_user(&token,NOW).unwrap(),user);
}

#[test]
fn google_login_requires_enrollment_when_enforced() {
    let (_dir, db, admin, user, _request) = approved(false);
    assert!(db.mfa_issue_if_unenrolled(&user,0,false,|_,_|Ok(())).is_err());
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&token),("admin".into(),false));
    assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
    assert!(db.mfa_begin(&user,Some(&token),&Cipher,NOW).is_err());
    let code = db.mfa_issue_enrollment_code(Some(&admin),&user,&enrollment_challenge_ref(&token),NOW).unwrap();
    db.mfa_prove_enrollment(&token,&code,NOW).unwrap();
    db.mfa_require_independent_proof(&token,NOW).unwrap();
    db.mfa_begin(&user,Some(&token),&Cipher,NOW).unwrap();
}

#[test]
fn google_oidc_enrollment_needs_no_password() {
    let (_dir, db, admin, user, _request) = approved(false);
    let google = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    db.mfa_http_enrollment(&google,None,NOW).unwrap();
    let password = prepare(&db,&admin,FirstFactor::Password,NOW);
    db.mfa_http_enrollment(&password,None,NOW).unwrap();
    let channel = prepare(&db,&admin,FirstFactor::ChannelOtp,NOW);
    assert!(db.mfa_http_enrollment(&channel,None,NOW).is_err());
    db.mfa_http_enrollment(&channel,Some("password-test"),NOW).unwrap();
}

#[test]
fn unknown_first_factor_denied() {
    let (_dir, db, _admin, user, request) = approved(true);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    db.conn().execute("UPDATE mfa_challenges SET first_factor='bogus' WHERE token_hash=?1",
        [token_hash(&token)]).unwrap();
    assert!(db.mfa_http_enrollment(&token,None,NOW).is_err());
    assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
    assert!(db.mfa_prepare_enrollment(&token,true,NOW).is_err());
    assert!(!db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized);
}

#[test]
fn preauth_single_use_epoch_bound() {
    let (_dir, db, _admin, user, request) = approved(true);
    let first = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&first),("signin_approval".into(),true));
    db.mfa_require_independent_proof(&first,NOW).unwrap();
    assert!(!db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized);
    assert!(db.mfa_prepare_enrollment(&first,true,NOW).is_err());
    let second = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&second),("admin".into(),false));
    assert!(db.mfa_require_independent_proof(&second,NOW).is_err());
    let secret = db.mfa_begin(&user,Some(&first),&Cipher,NOW).unwrap();
    bump(&db,&user);
    assert!(db.mfa_require_independent_proof(&first,NOW).is_err());
    let code = test_totp_code(&secret,NOW/30);
    assert!(db.mfa_confirm_and_issue(&user,Some(&first),&code,&Cipher,NOW,|_,_,_|Ok(())).is_err());
    assert!(!db.mfa_active(&user).unwrap());

    let (_dir2, db2, _admin2, user2, request2) = approved(true);
    let token = db2.mfa_challenge(&user2,FirstFactor::GoogleOidc,true,NOW).unwrap();
    bump(&db2,&user2);
    db2.mfa_prepare_enrollment(&token,true,NOW).unwrap();
    assert_eq!(proof(&db2,&token),("admin".into(),false));
    assert!(db2.signin_request_by_id(&request2).unwrap().unwrap().mfa_preauthorized);
}

#[test]
fn preauth_expires_after_72h() {
    for age in [72*3600-1,72*3600,72*3600+1,-1] {
        let (_dir, db, _admin, user, request) = approved(true);
        let now = NOW+age;
        let token = prepare(&db,&user,FirstFactor::GoogleOidc,now);
        let allowed = (0..72*3600).contains(&age);
        assert_eq!(proof(&db,&token),
            (if allowed { "signin_approval" } else { "admin" }.into(),allowed));
        assert_eq!(db.mfa_require_independent_proof(&token,now).is_ok(),allowed);
        assert_eq!(db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized,!allowed);
    }
}

#[test]
fn preauth_requires_google_first_factor() {
    for first in [FirstFactor::Password,FirstFactor::ChannelOtp] {
        let (_dir, db, _admin, user, request) = approved(true);
        let token = prepare(&db,&user,first,NOW);
        assert_eq!(proof(&db,&token),("admin".into(),false));
        assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
        assert!(db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized);
    }
}

#[test]
fn preauth_not_for_users_with_factor() {
    for after_prepare in [false,true] {
        let (_dir, db, _admin, user, request) = approved(true);
        let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,true,NOW).unwrap();
        if after_prepare { db.mfa_prepare_enrollment(&token,true,NOW).unwrap(); }
        // Keep epoch zero to exercise the separate live-factor guard.
        db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)",
            [&user]).unwrap();
        if !after_prepare {
            db.mfa_prepare_enrollment(&token,true,NOW).unwrap();
            assert_eq!(proof(&db,&token),("admin".into(),false));
        }
        assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
        assert_eq!(db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized,!after_prepare);
    }
}

#[test]
fn preauth_not_without_flag() {
    let (_dir, db, _admin, user, request) = approved(false);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&token),("admin".into(),false));
    assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
    assert!(!db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized);
}

#[test]
fn preauth_rechecks_active_user() {
    for after_prepare in [false,true] {
        let (_dir, db, _admin, user, _request) = approved(true);
        let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,true,NOW).unwrap();
        if after_prepare { db.mfa_prepare_enrollment(&token,true,NOW).unwrap(); }
        db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1",[&user]).unwrap();
        if !after_prepare {
            db.mfa_prepare_enrollment(&token,true,NOW).unwrap();
            assert_eq!(proof(&db,&token),("admin".into(),false));
        }
        assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
    }
}

#[test]
fn preauth_consumption_rolls_back_with_proof_write() {
    let (_dir, db, _admin, user, request) = approved(true);
    let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,true,NOW).unwrap();
    db.conn().execute_batch("CREATE TEMP TRIGGER fail_proof BEFORE INSERT ON mfa_enrollment_proofs
        BEGIN SELECT RAISE(ABORT,'test proof failure'); END;").unwrap();
    assert!(db.mfa_prepare_enrollment(&token,true,NOW).is_err());
    assert!(db.signin_request_by_id(&request).unwrap().unwrap().mfa_preauthorized);
    let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM mfa_enrollment_proofs WHERE token_hash=?1",
        [token_hash(&token)], |r|r.get(0)).unwrap();
    assert_eq!(count,0);
}

#[test]
fn preauth_completes_first_native_factor() {
    let (_dir, db, _admin, user, _request) = approved(true);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    let secret = db.mfa_begin(&user,Some(&token),&Cipher,NOW).unwrap();
    let code = test_totp_code(&secret,NOW/30);
    db.mfa_confirm_and_issue(&user,Some(&token),&code,&Cipher,NOW,|account,_,_| {
        assert_eq!(account.credential_epoch,1);
        Ok(())
    }).unwrap();
    assert!(db.mfa_active(&user).unwrap());
    assert!(db.mfa_issue_if_unenrolled(&user,1,false,|_,_|Ok(())).is_err());
    assert!(db.mfa_challenge(&user,FirstFactor::GoogleOidc,true,NOW).is_err());
    db.mfa_challenge(&user,FirstFactor::GoogleOidc,false,NOW).unwrap();
}

#[test]
fn unknown_proof_kind_denied_even_with_factor() {
    let (_dir, db, _admin, user, _request) = approved(true);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    db.conn().execute("UPDATE mfa_enrollment_proofs SET kind='bogus' WHERE token_hash=?1",
        [token_hash(&token)]).unwrap();
    db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)",
        [&user]).unwrap();
    assert!(db.mfa_require_independent_proof(&token,NOW).is_err());
}

fn assert_email_refused(db: &UserDb, user: &str, token: &str, enrollment: bool) {
    assert_eq!(db.mfa_email_http_capability(token,enrollment,None,NOW).unwrap_err(),DENIED);
    assert_eq!(db.mfa_email_send(token,enrollment,NOW).unwrap_err(),DENIED);
    let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM mfa_email_codes WHERE token_hash=?1",
        [token_hash(token)], |r|r.get(0)).unwrap();
    assert_eq!(count,0);
    // Seed a valid code so verification must reject the first factor, not
    // merely fail because sending was refused or the code was incorrect.
    let code = "123456";
    db.conn().execute("INSERT INTO mfa_email_codes(token_hash,user_id,email,code_hash,expires_at)
        SELECT ?1,id,email,?3,?4 FROM users WHERE id=?2",
        params![token_hash(token),user,token_hash(&format!("email:{token}:{code}")),NOW+email::EMAIL_TTL]).unwrap();
    let mut issued = false;
    let result = db.mfa_email_verify_and_issue(token,code,enrollment,NOW,|_,_,_| {
        issued = true;
        Ok(())
    });
    assert_eq!(result.unwrap_err(),DENIED);
    assert!(!issued);
    let state: (i64,bool) = db.conn().query_row("SELECT attempts,consumed FROM mfa_challenges WHERE token_hash=?1",
        [token_hash(token)], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(state,(0,false));
}

fn email_factor_count(db: &UserDb, user: &str) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM mfa_email_credentials WHERE user_id=?1",
        [user], |r|r.get(0)).unwrap()
}

#[test]
fn google_oidc_email_enrollment_refused() {
    let (_dir, db, _admin, user, _request) = approved(false);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&token),("admin".into(),false));
    assert_email_refused(&db,&user,&token,true);
    assert_eq!(email_factor_count(&db,&user),0);
    assert!(!db.mfa_active(&user).unwrap());
}

#[test]
fn google_oidc_email_refused_even_with_preauth() {
    let (_dir, db, _admin, user, _request) = approved(true);
    let token = prepare(&db,&user,FirstFactor::GoogleOidc,NOW);
    assert_eq!(proof(&db,&token),("signin_approval".into(),true));
    assert_email_refused(&db,&user,&token,true);
    assert_eq!(email_factor_count(&db,&user),0);
    let secret = db.mfa_begin(&user,Some(&token),&Cipher,NOW).unwrap();
    let code = test_totp_code(&secret,NOW/30);
    let codes = db.mfa_confirm_and_issue(&user,Some(&token),&code,&Cipher,NOW,|account,_,codes| {
        assert_eq!(account.credential_epoch,1);
        Ok(codes)
    }).unwrap();
    assert_eq!(codes.len(),10);
    assert_eq!(db.mfa_factors(&user).unwrap()["factors"]["totp"],true);
}

#[test]
fn google_oidc_email_login_refused() {
    let (_dir, db, _admin, user, _request) = approved(false);
    db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at)
        SELECT id,email,?2 FROM users WHERE id=?1",params![user,NOW]).unwrap();
    let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,false,NOW).unwrap();
    assert_email_refused(&db,&user,&token,false);
    assert_eq!(email_factor_count(&db,&user),1);
}

#[test]
fn password_email_unchanged() {
    let (_dir, db, admin, _user, _request) = approved(false);
    let token = prepare(&db,&admin,FirstFactor::Password,NOW);
    assert_eq!(proof(&db,&token),("admin".into(),false));
    db.mfa_email_http_capability(&token,true,None,NOW).unwrap();
    let (address,code) = db.mfa_email_send(&token,true,NOW).unwrap();
    assert_eq!(address,db.get_user(&admin).unwrap().unwrap().email);
    let mut issued = false;
    let codes = db.mfa_email_verify_and_issue(&token,&code,true,NOW,|account,_,codes| {
        issued = true;
        assert_eq!(account.id,admin);
        Ok(codes)
    }).unwrap();
    assert!(issued);
    assert_eq!(codes.len(),10);
    assert_eq!(email_factor_count(&db,&admin),1);
    assert_eq!(db.mfa_factors(&admin).unwrap()["factors"]["email"],true);
}

#[test]
fn unknown_first_factor_email_refused() {
    for enrollment in [true,false] {
        let (_dir, db, _admin, user, _request) = approved(false);
        if !enrollment {
            db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at)
                SELECT id,email,?2 FROM users WHERE id=?1",params![user,NOW]).unwrap();
        }
        let token = db.mfa_challenge(&user,FirstFactor::GoogleOidc,enrollment,NOW).unwrap();
        if enrollment { db.mfa_prepare_enrollment(&token,true,NOW).unwrap(); }
        db.conn().execute("UPDATE mfa_challenges SET first_factor='bogus' WHERE token_hash=?1",
            [token_hash(&token)]).unwrap();
        assert_email_refused(&db,&user,&token,enrollment);
        assert_eq!(email_factor_count(&db,&user),if enrollment {0} else {1});
    }
}
