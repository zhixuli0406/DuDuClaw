use super::*;
use crate::mfa::tests::{Cipher,setup};

fn enrolled(db:&UserDb,id:&str,now:i64)->Vec<String> {
    let token=db.mfa_factor_password_challenge(id,"password-test","session",now).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,now).unwrap();
    db.mfa_email_verify_and_issue(&token,&code,true,now,|_,_,codes|Ok(codes)).unwrap()
}

#[test]
fn email_enrollment_proves_address_and_keeps_recovery_and_legacy_totp() {
    let (_dir,db,id)=setup();
    let (_,old)=crate::mfa::tests::enroll(&db,&id);
    assert!(enrolled(&db,&id,3100).is_empty());
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,3200).unwrap();
    assert!(db.mfa_verify(&token,&old[0],true,&Cipher,3200).is_ok());
    let factors=db.mfa_factors(&id).unwrap();
    assert_eq!(factors["factors"]["email"],true);
    assert_eq!(factors["factors"]["totp"],true);
    assert_eq!(factors["last_used"],"email");
}

#[test]
fn email_code_is_hashed_bound_single_use_and_expires_at_ten_minutes() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    let a=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    let b=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    let (_,code)=db.mfa_email_send(&a,false,2000).unwrap();
    let stored:String=db.conn().query_row("SELECT code_hash FROM mfa_email_codes WHERE token_hash=?1",[token_hash(&a)],|r|r.get(0)).unwrap();
    assert_ne!(stored,code);
    assert_eq!(code.len(),6);
    assert!(db.mfa_email_verify_and_issue(&b,&code,false,2001,|_,_,_|Ok(())).is_err());
    db.mfa_email_verify_and_issue(&a,&code,false,2599,|_,_,_|Ok(())).unwrap();
    assert!(db.mfa_email_verify_and_issue(&a,&code,false,2599,|_,_,_|Ok(())).is_err());
    let c=db.mfa_challenge(&id,FirstFactor::Password,false,3000).unwrap();
    let (_,code)=db.mfa_email_send(&c,false,3000).unwrap();
    assert!(db.mfa_email_verify_and_issue(&c,&code,false,3600,|_,_,_|Ok(())).is_err());
}

#[test]
fn email_challenge_five_attempt_limit_is_shared_and_resend_never_resets_it() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    db.mfa_email_send(&token,false,2000).unwrap();
    for _ in 0..4 {assert!(db.mfa_email_verify_and_issue(&token,"wrong",false,2001,|_,_,_|Ok(())).is_err());}
    let (_,code)=db.mfa_email_send(&token,false,2060).unwrap();
    assert!(db.mfa_verify(&token,"wrong",true,&Cipher,2060).is_err());
    assert!(db.mfa_email_verify_and_issue(&token,&code,false,2061,|_,_,_|Ok(())).is_err());
    assert!(db.mfa_email_send(&token,false,2120).is_err());
}

#[test]
fn email_resend_limits_span_challenges_and_failed_delivery_invalidates_only_its_code() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,5000).unwrap();
    let (_,first)=db.mfa_email_send(&token,false,5000).unwrap();
    assert!(db.mfa_email_send(&token,false,5059).is_err());
    let (_,second)=db.mfa_email_send(&token,false,5060).unwrap();
    db.mfa_email_delivery_failed(&token,&first).unwrap();
    db.mfa_email_verify_and_issue(&token,&second,false,5061,|_,_,_|Ok(())).unwrap();
    for now in [5120,5180,5240] {
        let token=db.mfa_challenge(&id,FirstFactor::Password,false,now).unwrap();
        db.mfa_email_send(&token,false,now).unwrap();
    }
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,5300).unwrap();
    assert!(db.mfa_email_send(&token,false,5300).is_err());
}

#[test]
fn email_enrollment_rollback_recovery_without_totp_and_reset_all_factors() {
    let (_dir,db,id)=setup();
    let token=db.mfa_factor_password_challenge(&id,"password-test","session",1000).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,1000).unwrap();
    assert!(db.mfa_email_verify_and_issue(&token,&code,true,1000,|_,_,_|Err::<(),_>("issuer failed".into())).is_err());
    assert!(!db.mfa_active(&id).unwrap());
    let codes=db.mfa_email_verify_and_issue(&token,&code,true,1000,|_,_,codes|Ok(codes)).unwrap();
    assert_eq!(codes.len(),10);
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    db.mfa_verify(&token,&codes[0],true,&Cipher,2000).unwrap();
    db.mfa_operator_reset("mfa@example.com").unwrap();
    assert!(!db.mfa_active(&id).unwrap());
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM mfa_email_credentials",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn email_removal_needs_recent_session_bound_reauth_and_preserves_last_factor_policy() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    record_enforcement(&mut db.conn(),true,1500).unwrap();
    assert!(db.mfa_factor_password_challenge(&id,"wrong","session",2000).is_err());
    let token=db.mfa_factor_password_challenge(&id,"password-test","session",2000).unwrap();
    assert!(db.mfa_email_remove_and_issue(&id,&token,"other-session",2001,false,|_,_,_|Ok(())).is_err());
    assert!(db.mfa_email_remove_and_issue(&id,&token,"session",2001,false,|_,_,_|Ok(())).is_err());
    assert!(db.mfa_email_remove_and_issue(&id,&token,"session",2001,true,|_,_,_| -> Result<()> { panic!("issuer called") }).is_err());
    record_enforcement(&mut db.conn(),false,2002).unwrap();
    assert!(db.mfa_email_remove_and_issue(&id,&token,"session",2300,false,|_,_,_|Ok(())).is_err());
    db.mfa_email_remove_and_issue(&id,&token,"session",2003,false,|_,_,_|Ok(())).unwrap();
    assert!(!db.mfa_active(&id).unwrap());
}

#[test]
fn email_address_change_invalidates_verified_address_and_challenges() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    let (_,code)=db.mfa_email_send(&token,false,2000).unwrap();
    db.conn().execute("UPDATE users SET email='changed@example.com' WHERE id=?1",[&id]).unwrap();
    assert!(!db.mfa_active(&id).unwrap());
    assert!(db.mfa_email_verify_and_issue(&token,&code,false,2001,|_,_,_|Ok(())).is_err());
}

#[test]
fn email_then_totp_preserves_recovery_codes_and_last_factor_removal_is_refused() {
    let (_dir,db,id)=setup();
    let recovery=enrolled(&db,&id,1000);
    record_enforcement(&mut db.conn(),true,1001).unwrap();
    let token=db.mfa_password_challenge(&id,"password-test","session",2000).unwrap();
    assert!(db.mfa_prepare_enrollment(&token,true,2000).unwrap().is_none());
    db.mfa_require_independent_proof(&token,2000).unwrap();
    let secret=db.mfa_begin(&id,Some(&token),&Cipher,2000).unwrap();
    let code=hotp(&decode_secret(&secret).unwrap(),2000/30,6);
    assert!(db.mfa_confirm(&id,Some(&token),&code,&Cipher,2000).unwrap().is_empty());
    db.mfa_change(&id,&recovery[0],true,&Cipher,2001).unwrap();
    assert!(db.mfa_active(&id).unwrap());
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,2002).unwrap();
    db.mfa_verify(&token,&recovery[1],true,&Cipher,2002).unwrap();
    assert!(db.bind_channel_identity_authorized(&id,"telegram","12345",true,Some(&id)).is_ok());
}

#[test]
fn email_admin_reset_reauth_checks_live_role_target_and_consumes_grant() {
    let (_dir,db,id)=setup();
    let target=db.create_user("target@example.test","Target","password-test",UserRole::Employee).unwrap();
    enrolled(&db,&target.id,1000);
    let token=db.mfa_factor_password_challenge(&id,"password-test","session",2000).unwrap();
    assert!(db.mfa_admin_reset_reauth(&id,&id,&token,"session",2000).is_err());
    db.conn().execute("UPDATE users SET role='employee' WHERE id=?1",[&id]).unwrap();
    assert!(db.mfa_admin_reset_reauth(&id,&target.id,&token,"session",2000).is_err());
    db.conn().execute("UPDATE users SET role='admin' WHERE id=?1",[&id]).unwrap();
    db.mfa_admin_reset_reauth(&id,&target.id,&token,"session",2000).unwrap();
    assert!(!db.mfa_active(&target.id).unwrap());
    assert!(db.mfa_admin_reset_reauth(&id,&target.id,&token,"session",2000).is_err());
}

#[test]
fn email_http_capability_checks_are_read_only_and_preserve_scopes() {
    let (_dir,db,id)=setup();
    let token=db.mfa_challenge(&id,FirstFactor::Password,true,1000).unwrap();
    db.mfa_prepare_enrollment(&token,false,1000).unwrap();
    assert!(db.mfa_email_http_capability(&token,true,None,1000).is_ok());
    assert!(db.mfa_email_http_capability(&token,false,None,1000).is_err());
    assert!(db.mfa_email_http_capability(&"a".repeat(64),true,None,1000).is_err());
    assert!(db.mfa_email_http_capability(&token,true,None,1000+TTL).is_err());
    let counts:(i64,i64,i64)=db.conn().query_row("SELECT
        (SELECT COUNT(*) FROM mfa_email_codes),(SELECT COUNT(*) FROM mfa_email_sends),
        (SELECT SUM(attempts) FROM mfa_challenges)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(counts,(0,0,0));
    let stepup=db.mfa_factor_password_challenge(&id,"password-test","session",1000).unwrap();
    assert!(db.mfa_email_http_capability(&stepup,true,None,1000).is_err());
    let channel=db.mfa_challenge(&id,FirstFactor::ChannelOtp,true,1000).unwrap();
    db.mfa_prepare_enrollment(&channel,false,1000).unwrap();
    assert!(db.mfa_email_http_capability(&channel,true,None,1000).is_err());
    assert!(db.mfa_email_http_capability(&channel,true,Some("wrong"),1000).is_err());
    assert!(db.mfa_email_http_capability(&channel,true,Some("password-test"),1000).is_ok());
    db.conn().execute("UPDATE mfa_challenges SET attempts=5 WHERE token_hash=?1",[token_hash(&token)]).unwrap();
    assert!(db.mfa_email_http_capability(&token,true,None,1000).is_err());
}

#[test]
fn email_http_capability_refuses_consumed_login_and_inactive_user() {
    let (_dir,db,id)=setup();
    enrolled(&db,&id,1000);
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,2000).unwrap();
    assert!(db.mfa_email_http_capability(&token,false,None,2000).is_ok());
    let (_,code)=db.mfa_email_send(&token,false,2000).unwrap();
    db.mfa_email_verify_and_issue(&token,&code,false,2000,|_,_,_|Ok(())).unwrap();
    assert!(db.mfa_email_http_capability(&token,false,None,2000).is_err());
    let token=db.mfa_challenge(&id,FirstFactor::Password,false,3000).unwrap();
    db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1",[&id]).unwrap();
    assert!(db.mfa_email_http_capability(&token,false,None,3000).is_err());
}

fn pending_email(db: &UserDb, user: &str, now: i64) -> String {
    let token=db.mfa_challenge(user,FirstFactor::Password,true,now).unwrap();
    db.mfa_prepare_enrollment(&token,true,now).unwrap();
    token
}

fn prove_email(db: &UserDb, user: &str, token: &str, now: i64) -> String {
    let code=db.mfa_issue_enrollment_code(None,user,&enrollment_challenge_ref(token),now).unwrap();
    db.mfa_prove_enrollment(token,&code,now).unwrap();
    code
}

fn assert_email_not_written(db: &UserDb, user: &str) {
    let counts:(i64,i64)=db.conn().query_row("SELECT
        (SELECT COUNT(*) FROM mfa_email_credentials WHERE user_id=?1),
        (SELECT COUNT(*) FROM mfa_recovery_codes WHERE user_id=?1)",
        [user],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counts,(0,0));
}

#[test]
fn email_enforced_first_enrollment_requires_proof_before_send_and_write() {
    for proof in ["missing", "admin", "password", "unknown"] {
        let (_dir,db,id)=setup();
        record_enforcement(&mut db.conn(),true,1000).unwrap();
        let token=pending_email(&db,&id,1001);
        match proof {
            "missing" => { db.conn().execute("DELETE FROM mfa_enrollment_proofs WHERE token_hash=?1",[token_hash(&token)]).unwrap(); },
            "admin" => {}, // The normal pending, unverified Admin proof.
            kind => { db.conn().execute("UPDATE mfa_enrollment_proofs SET kind=?2,verified=1 WHERE token_hash=?1",params![token_hash(&token),kind]).unwrap(); },
        }
        assert!(db.mfa_email_http_capability(&token,true,None,1001).is_err(),"{proof}");
        assert!(db.mfa_email_send(&token,true,1001).is_err(),"{proof}");
        let counts:(i64,i64)=db.conn().query_row("SELECT (SELECT COUNT(*) FROM mfa_email_codes),
            (SELECT COUNT(*) FROM mfa_email_sends)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(counts,(0,0));
        // A valid delivery record must not bypass the authoritative write gate.
        db.conn().execute("INSERT INTO mfa_email_codes(token_hash,user_id,email,code_hash,expires_at)
            SELECT ?1,id,email,?3,?4 FROM users WHERE id=?2",
            params![token_hash(&token),id,email_hash(&token,"123456"),1601]).unwrap();
        assert!(db.mfa_email_verify_and_issue(&token,"123456",true,1001,|_,_,_| -> Result<()> {
            panic!("refused enrollment issued tokens")
        }).is_err(),"{proof}");
        assert_email_not_written(&db,&id);
    }
}

#[test]
fn email_admin_proof_is_single_use_and_enrollment_issues_recovery_and_tokens() {
    let (_dir,db,id)=setup();
    record_enforcement(&mut db.conn(),true,1000).unwrap();
    let token=pending_email(&db,&id,1001);
    let proof=prove_email(&db,&id,&token,1001);
    let other=pending_email(&db,&id,1001);
    assert!(db.mfa_prove_enrollment(&other,&proof,1001).is_err());
    assert!(db.mfa_email_send(&other,true,1001).is_err());
    db.mfa_email_http_capability(&token,true,None,1001).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,1001).unwrap();
    let codes=db.mfa_email_verify_and_issue(&token,&code,true,1001,|account,_,codes| {
        assert_eq!(account.id,id);
        assert_eq!(account.credential_epoch,1);
        Ok(codes)
    }).unwrap();
    assert_eq!(codes.len(),10);
    assert!(db.mfa_active(&id).unwrap());
    assert!(db.mfa_email_verify_and_issue(&token,&code,true,1001,|_,_,_| -> Result<()> {
        panic!("reused proof issued tokens")
    }).is_err());
    db.mfa_operator_reset("mfa@example.com").unwrap();
    let next=pending_email(&db,&id,1062);
    assert!(db.mfa_prove_enrollment(&next,&proof,1062).is_err());
    assert!(db.mfa_email_send(&next,true,1062).is_err());
}

#[test]
fn email_prepared_proof_rechecks_epoch_factor_status_and_kind() {
    for after_send in [false,true] {
        for change in ["epoch", "factor", "disabled", "unknown", "unverified", "missing_epoch"] {
            let (_dir,db,id)=setup();
            record_enforcement(&mut db.conn(),true,1000).unwrap();
            let token=pending_email(&db,&id,1001);
            prove_email(&db,&id,&token,1001);
            let code=if after_send { db.mfa_email_send(&token,true,1001).unwrap().1 } else { "123456".into() };
            match change {
                "epoch" => { db.conn().execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1",[&id]).unwrap(); },
                // Keep the epoch unchanged to exercise the public capability guard.
                "factor" => { db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)",[&id]).unwrap(); },
                "disabled" => { db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1",[&id]).unwrap(); },
                "unknown" => { db.conn().execute("UPDATE mfa_enrollment_proofs SET kind='unknown' WHERE token_hash=?1",[token_hash(&token)]).unwrap(); },
                "unverified" => { db.conn().execute("UPDATE mfa_enrollment_proofs SET verified=0 WHERE token_hash=?1",[token_hash(&token)]).unwrap(); },
                "missing_epoch" => { db.conn().execute("UPDATE mfa_enrollment_proofs SET credential_epoch=NULL WHERE token_hash=?1",[token_hash(&token)]).unwrap(); },
                _ => unreachable!(),
            }
            assert!(db.mfa_email_http_capability(&token,true,None,1061).is_err(),"{change}");
            assert!(db.mfa_email_send(&token,true,1061).is_err(),"{change}");
            assert!(db.mfa_email_verify_and_issue(&token,&code,true,1061,|_,_,_| -> Result<()> {
                panic!("stale proof issued tokens")
            }).is_err(),"{change}");
            assert_email_not_written(&db,&id);
        }
    }
}

#[test]
fn email_policy_enabled_after_send_requires_independent_proof_at_write() {
    let (_dir,db,id)=setup();
    let token=db.mfa_factor_password_challenge(&id,"password-test","session",1000).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,1000).unwrap();
    record_enforcement(&mut db.conn(),true,1001).unwrap();
    assert!(db.mfa_email_verify_and_issue(&token,&code,true,1001,|_,_,_| -> Result<()> {
        panic!("policy toggle bypass issued tokens")
    }).is_err());
    assert_email_not_written(&db,&id);
}

#[test]
fn email_additional_factor_under_enforcement_needs_no_independent_code() {
    let (_dir,db,id)=setup();
    crate::mfa::tests::enroll(&db,&id);
    record_enforcement(&mut db.conn(),true,3001).unwrap();
    let token=db.mfa_password_challenge(&id,"password-test","session",3100).unwrap();
    assert!(db.mfa_prepare_enrollment(&token,true,3100).unwrap().is_none());
    db.mfa_prove_enrollment(&token,"",3100).unwrap();
    db.mfa_check_session(&token,&id,"session",3100).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,3100).unwrap();
    let codes=db.mfa_email_verify_and_issue(&token,&code,true,3100,|_,_,codes|Ok(codes)).unwrap();
    assert!(codes.is_empty());
    assert_eq!(db.mfa_factors(&id).unwrap()["factors"]["email"],true);
}

#[test]
fn email_non_enforced_public_enrollment_needs_no_independent_code() {
    let (_dir,db,id)=setup();
    let token=db.mfa_challenge(&id,FirstFactor::Password,true,1000).unwrap();
    db.mfa_prepare_enrollment(&token,false,1000).unwrap();
    db.mfa_prove_enrollment(&token,"",1000).unwrap();
    db.mfa_email_http_capability(&token,true,None,1000).unwrap();
    let (_,code)=db.mfa_email_send(&token,true,1000).unwrap();
    let recovery=db.mfa_email_verify_and_issue(&token,&code,true,1000,|_,_,codes|Ok(codes)).unwrap();
    assert_eq!(recovery.len(),10);
}
