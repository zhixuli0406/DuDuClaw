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
    assert!(db.bind_channel_identity_authorized(&id,"telegram","12345",true,Some(&id),true).is_ok());
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
