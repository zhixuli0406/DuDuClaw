use super::*;
use crate::UserRole;
fn setup() -> (tempfile::TempDir,UserDb,String) {
    let dir=tempfile::tempdir().unwrap();
    let db=UserDb::new(&dir.path().join("users.db")).unwrap();
    let user=db.create_user("remember@example.com","Remember","test-password",UserRole::Admin).unwrap();
    (dir,db,user.id)
}

#[test]
fn remembered_device_revocation_fences_pending_delivery_across_connections() {
    for action in ["revoke", "logout", "reuse", "shorten", "delete"] {
        let (dir, db, user) = setup();
        let (family, secret, _) = create(&db, &user);
        let writer = UserDb::new(&dir.path().join("users.db")).unwrap();
        let generation = db.acl_generation().unwrap();
        assert!(db.with_acl_generation(generation, || ()).is_ok());
        match action {
            "revoke" => writer.remember_revoke(&user, Some(&family)).unwrap(),
            "logout" => writer.remember_logout(&secret).unwrap(),
            "reuse" => {
                writer.remember_rotate(&secret, 1001, false, |_, _, _| Ok(())).unwrap();
                assert!(writer.remember_rotate(&secret, 1002, false, |_, _, _| Ok(())).is_err());
            }
            "shorten" => { writer.conn().execute("UPDATE remembered_families SET expires_at=1001 WHERE id=?1", [&family]).unwrap(); }
            "delete" => {
                writer.conn().execute("DELETE FROM remembered_tokens WHERE family=?1", [&family]).unwrap();
                writer.conn().execute("DELETE FROM remembered_families WHERE id=?1", [&family]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(db.with_acl_generation(generation, || panic!("revoked device delivered")).is_err(), "{action}");
    }
}

#[test]
fn sliding_expiry_uses_current_durable_policy_and_clamps_existing_families() {
    let (dir,db,id)=setup();
    assert_eq!(db.remember_days(),180);
    let (family,secret,original)=create(&db,&id);
    db.remember_admin_policy(&id,0,Some(365)).unwrap();
    assert_eq!(db.remember_devices(&id,1000).unwrap()[0].expires_at,original);
    let (_,next,extended)=db.remember_rotate(&secret,2000,false,|_,_,_|Ok(())).unwrap();
    assert_eq!(extended,2000+365*86400);
    db.remember_admin_policy(&id,0,Some(1)).unwrap();
    let second=UserDb::new(&dir.path().join("users.db")).unwrap();
    assert_eq!(second.remember_days(),1);
    assert_eq!(db.remember_devices(&id,2001).unwrap()[0].expires_at,extended);
    let (_,next,shortened)=second.remember_rotate(&next,3000,false,|_,_,_|Ok(())).unwrap();
    assert_eq!(shortened,3000+86400);
    assert!(!db.remember_session_valid(Some(&family),&id,shortened).unwrap());
    assert!(db.remember_rotate(&next,shortened,false,|_,_,_|Ok(())).is_err());
    let (_,_,created)=db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"New",None,365,4000,false,|_,_,_|Ok(())).unwrap();
    assert_eq!(created,4000+86400);
}

#[test]
fn policy_rejects_out_of_bounds_and_non_admin_or_stale_actor() {
    let (_dir,db,id)=setup();
    for days in [0,366,u16::MAX] { assert!(db.remember_admin_policy(&id,0,Some(days)).is_err()); }
    assert_eq!(db.remember_days(),180);
    let employee=db.create_user("employee@example.com","Employee","test-password",UserRole::Employee).unwrap();
    assert!(db.remember_admin_policy(&employee.id,0,None).is_err());
    assert!(db.remember_admin_policy(&employee.id,0,Some(365)).is_err());
    assert!(db.remember_admin_policy(&id,1,Some(365)).is_err());
    for days in [1,180,365] { assert_eq!(db.remember_admin_policy(&id,0,Some(days)).unwrap(),days); }
}

#[test]
fn corrupt_policy_always_falls_back_to_bounded_default() {
    for sql in ["DELETE FROM remembered_policy_state", "DROP TABLE remembered_policy_state",
        "PRAGMA ignore_check_constraints=ON; UPDATE remembered_policy_state SET days=0",
        "PRAGMA ignore_check_constraints=ON; UPDATE remembered_policy_state SET days=366",
        "PRAGMA ignore_check_constraints=ON; UPDATE remembered_policy_state SET days='invalid'",
        "PRAGMA ignore_check_constraints=ON; INSERT INTO remembered_policy_state VALUES(2,365)"] {
        let (_dir,db,id)=setup(); let (_,secret,_)=create(&db,&id);
        db.conn().execute_batch(sql).unwrap();
        assert_eq!(db.remember_days(),180);
        assert_eq!(db.remember_rotate(&secret,2000,false,|_,_,_|Ok(())).unwrap().2,2000+180*86400);
    }
}

#[test]
fn policy_write_rolls_back_if_strict_readback_or_audit_fails() {
    let (_dir,db,id)=setup();
    db.conn().execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO remembered_policy_state VALUES(2,365)").unwrap();
    assert!(db.remember_admin_policy(&id,0,Some(30)).is_err());
    assert_eq!(db.conn().query_row("SELECT days FROM remembered_policy_state WHERE id=1",[],|r|r.get::<_,i64>(0)).unwrap(),180);
    db.conn().execute_batch("DELETE FROM remembered_policy_state WHERE id=2; DROP TABLE auth_audit_log").unwrap();
    assert!(db.remember_admin_policy(&id,0,Some(30)).is_err());
    assert_eq!(db.remember_days(),180);
    assert!(db.remember_admin_revoke_all(&id,0,&id).is_err());
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,0);
}

#[test]
fn failed_renewal_does_not_extend_or_consume_and_overflow_is_denied() {
    let (_dir,db,id)=setup(); let (family,secret,expires)=create(&db,&id);
    assert!(db.remember_rotate::<()>(&secret,2000,false,|_,_,_|Err("issuer failed".into())).is_err());
    let device=db.remember_devices(&id,2000).unwrap().remove(0);
    assert_eq!((device.expires_at,device.last_used_at),(expires,1000));
    db.conn().execute("UPDATE remembered_families SET expires_at=?1 WHERE id=?2",params![i64::MAX,family]).unwrap();
    assert!(db.remember_rotate(&secret,i64::MAX-1,false,|_,_,_|Ok(())).is_err());
    assert!(db.remember_rotate(&secret,2001,false,|_,_,_|Ok(())).is_ok());
}

#[test]
fn admin_revoke_all_validates_actor_target_and_audits_without_tokens() {
    let (_dir,db,admin)=setup();
    let target=db.create_user("target@example.com","Target","test-password",UserRole::Employee).unwrap();
    let (one,first,_)=create(&db,&target.id);let (two,second,_)=create(&db,&target.id);
    let (_,admin_secret,_)=create(&db,&admin);
    assert!(db.remember_admin_revoke_all(&target.id,0,&admin).is_err());
    assert!(db.remember_admin_revoke_all(&admin,1,&target.id).is_err());
    assert!(db.remember_admin_revoke_all(&admin,0,"missing").is_err());
    assert_eq!(db.get_user(&target.id).unwrap().unwrap().credential_epoch,0);
    let generation=db.acl_generation().unwrap();
    db.remember_admin_revoke_all(&admin,0,&target.id).unwrap();
    assert_eq!(db.get_user(&target.id).unwrap().unwrap().credential_epoch,1);
    assert!(db.acl_generation().unwrap()>generation);
    for (family,token) in [(one,first.clone()),(two,second.clone())] {
        assert!(!db.remember_session_valid(Some(&family),&target.id,1001).unwrap());
        assert!(db.remember_rotate(&token,1001,false,|_,_,_|Ok(())).is_err());
    }
    assert!(db.remember_rotate(&admin_secret,1001,false,|_,_,_|Ok(())).is_ok());
    let log=serde_json::to_string(&db.query_audit_log(Some(&admin),None,100).unwrap()).unwrap();
    assert!(log.contains("session.admin_revoked"));assert!(log.contains(&target.id));
    assert!(log.contains("session.admin_revoke_refused"));
    for token in [first,second,admin_secret] { assert!(!log.contains(&token)); }
}

#[test]
fn disabling_and_offboarding_revoke_credentials_even_after_reactivation() {
    for status in [crate::UserStatus::Suspended,crate::UserStatus::Offboarded] {
        let (_dir,db,id)=setup();let (family,secret,_)=create(&db,&id);
        db.set_user_status(&id,status).unwrap();
        assert!(db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).is_err());
        assert!(!db.remember_session_valid(Some(&family),&id,1001).unwrap());
        db.set_user_status(&id,crate::UserStatus::Active).unwrap();
        assert!(!crate::mfa::credential_epoch_matches(0,db.get_user(&id).unwrap().unwrap().credential_epoch));
        assert!(db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).is_err());
    }
}
fn create(db:&UserDb,id:&str)->(String,String,i64) {
    db.remember_create(id,0,&uuid::Uuid::new_v4().to_string(),"Browser",Some("127.0.0.1"),180,1000,false,|_,_,family|Ok(family.to_owned())).unwrap()
}
#[test]
fn remembered_rotation_reuse_revokes_family_and_never_stores_secrets() {
    let (_dir,db,id)=setup();
    let (family,secret,expires)=create(&db,&id);
    assert_eq!(expires,1000+180*86400);
    assert!(db.remember_session_valid(Some(&family),&id,1001).unwrap());
    let (_,next,expires2)=db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).unwrap();
    assert_ne!(secret,next);assert_eq!(expires+1,expires2);
    let hashes:Vec<String>=db.conn().prepare("SELECT hash FROM remembered_tokens").unwrap().query_map([],|r|r.get(0)).unwrap().map(|r|r.unwrap()).collect();
    assert!(!hashes.contains(&secret));assert!(!hashes.contains(&next));
    assert!(db.remember_rotate(&secret,1002,false,|_,_,_|Ok(())).is_err());
    assert!(db.remember_rotate(&next,1002,false,|_,_,_|Ok(())).is_err());
    assert!(!db.remember_session_valid(Some(&family),&id,1002).unwrap());
    let audit=serde_json::to_string(&db.query_audit_log(Some(&id),None,100).unwrap()).unwrap();
    assert!(audit.contains("session.reuse_detected"));assert!(!audit.contains(&secret));
}
#[test]
fn remembered_revokes_on_password_epoch_role_status_and_delete() {
    for sql in ["UPDATE users SET password_hash='changed' WHERE id=?1","UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1","UPDATE users SET role='employee' WHERE id=?1","UPDATE users SET status='suspended' WHERE id=?1","DELETE FROM users WHERE id=?1"] {
        let (_dir,db,id)=setup();let (family,secret,_)=create(&db,&id);
        let generation=db.acl_generation().unwrap();
        db.conn().execute(sql,[&id]).unwrap();
        if sql.starts_with("DELETE") { assert!(db.acl_generation().unwrap()>generation); }
        assert!(!db.remember_session_valid(Some(&family),&id,1001).unwrap());
        assert!(db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).is_err());
    }
}
#[test]
fn remembered_revoke_is_owner_bound_and_logout_revokes_rotated_family() {
    let (_dir,db,id)=setup();let (family,secret,_)=create(&db,&id);
    db.remember_revoke("other",Some(&family)).unwrap();
    assert!(db.remember_session_valid(Some(&family),&id,1001).unwrap());
    let (_,next,_)=db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).unwrap();
    db.remember_logout(&secret).unwrap();
    assert!(db.remember_rotate(&next,1002,false,|_,_,_|Ok(())).is_err());
    let (_,secret,_)=create(&db,&id);
    let epoch=db.get_user(&id).unwrap().unwrap().credential_epoch;
    let generation=db.acl_generation().unwrap();
    db.remember_revoke(&id,None).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch,epoch+1);
    assert!(db.acl_generation().unwrap()>generation);
    assert!(db.remember_rotate(&secret,1002,false,|_,_,_|Ok(())).is_err());
}
#[test]
fn remembered_expiry_and_issuer_failure_are_atomic() {
    let (_dir,db,id)=setup();let (_,secret,_)=create(&db,&id);
    assert!(db.remember_rotate::<()>(&secret,1001,false,|_,_,_|Err("issuer failed".into())).is_err());
    let (_,next,expires)=db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).unwrap();
    assert!(db.remember_rotate(&next,expires,false,|_,_,_|Ok(())).is_err());
    let count=db.remember_devices(&id,1001).unwrap().len();
    assert!(db.remember_create::<()>(&id,0,&uuid::Uuid::new_v4().to_string(),"Browser",None,180,1001,false,|_,_,_|Err("issuer failed".into())).is_err());
    assert_eq!(db.remember_devices(&id,1001).unwrap().len(),count);
    for days in [0,366] {assert!(db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"Browser",None,days,1001,false,|_,_,_|Ok(())).is_err());}
}
#[test]
fn remembered_cross_connection_rotation_has_one_winner_then_family_is_dead() {
    let (dir,db,id)=setup();let (family,secret,_)=create(&db,&id);
    let second=UserDb::new(&dir.path().join("users.db")).unwrap();
    let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));let gate=barrier.clone();let other=secret.clone();
    let thread=std::thread::spawn(move||{gate.wait();second.remember_rotate(&other,1001,false,|_,_,_|Ok(())).is_ok()});
    barrier.wait();let first=db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).is_ok();
    assert_ne!(first,thread.join().unwrap());assert!(!db.remember_session_valid(Some(&family),&id,1002).unwrap());
}

#[test]
fn remembered_login_exchange_is_single_use_and_blocks_legacy_refresh() {
    let (_dir,db,id)=setup();let session=uuid::Uuid::new_v4().to_string();
    assert!(!db.remember_login_exchanged(Some(&session)).unwrap());
    db.remember_create(&id,0,&session,"Browser",None,180,1000,false,|_,_,_|Ok(())).unwrap();
    assert!(db.remember_login_exchanged(Some(&session)).unwrap());
    assert!(db.remember_create(&id,0,&session,"Browser",None,180,1000,false,|_,_,_|Ok(())).is_err());
}

#[test]
fn remembered_cleanup_prunes_only_after_expiry_and_refresh_safety_window() {
    let (_dir,db,id)=setup();let (family,_,expires)=create(&db,&id);
    for now in [expires, expires+7*86400-1] {
        db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"New",None,180,now,false,|_,_,_|Ok(())).unwrap();
        assert!(!db.remember_session_valid(Some(&family),&id,now).unwrap());
    }
    db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"New",None,180,expires+7*86400,false,|_,_,_|Ok(())).unwrap();
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM remembered_families WHERE id=?1",[family],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn remembered_generation_storage_is_constant_and_old_replay_still_revokes() {
    let (_dir,db,id)=setup();let (family,first,_)=create(&db,&id);
    let mut token=first.clone();
    for i in 0..2000 {
        token=db.remember_rotate(&token,1001+i,false,|_,_,_|Ok(())).unwrap().1;
    }
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM remembered_tokens WHERE family=?1",[&family],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(db.conn().query_row("SELECT generation FROM remembered_families WHERE id=?1",[&family],|r|r.get::<_,i64>(0)).unwrap(),2000);
    assert!(db.remember_rotate(&first,4000,false,|_,_,_|Ok(())).is_err());
    assert!(!db.remember_session_valid(Some(&family),&id,4000).unwrap());
}

#[test]
fn remembered_forged_generation_or_mac_cannot_revoke_or_rotate() {
    let (_dir,db,id)=setup();let (family,token,_)=create(&db,&id);
    let (_,next,_)=db.remember_rotate(&token,1001,false,|_,_,_|Ok(())).unwrap();
    for forged in [next.replace(".1.",".0."), next.replace(".1.",".2."),format!("{family}.1.{}","a".repeat(64))] {
        assert!(db.remember_rotate(&forged,1002,false,|_,_,_|Ok(())).is_err());
        assert!(db.remember_logout(&forged).is_err());
        assert!(db.remember_session_valid(Some(&family),&id,1002).unwrap());
    }
    assert!(db.remember_rotate(&next,1002,false,|_,_,_|Ok(())).is_ok());
}

#[test]
fn remembered_revoked_families_count_toward_retained_storage_cap() {
    let (_dir,db,id)=setup();
    for _ in 0..100 {let (family,_,_)=create(&db,&id);db.remember_revoke(&id,Some(&family)).unwrap();}
    assert!(db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"New",None,180,1001,false,|_,_,_|Ok(())).is_err());
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM remembered_families",[],|r|r.get::<_,i64>(0)).unwrap(),100);
}

#[test]
fn remembered_signing_key_survives_reopen_and_missing_or_corrupt_key_fails_closed() {
    let (dir,db,id)=setup();let (_,token,_)=create(&db,&id);
    let path=dir.path().join("users.db");
    let second=UserDb::new(&path).unwrap();
    assert!(second.remember_rotate(&token,1001,false,|_,_,_|Ok(())).is_ok());
    std::fs::write(path.with_extension("remember-key"),b"corrupt").unwrap();
    assert!(UserDb::new(&path).is_err());
    std::fs::remove_file(path.with_extension("remember-key")).unwrap();
    assert!(UserDb::new(&path).is_err());
}

#[test]
fn remembered_socket_age_is_bounded_before_tombstone_cleanup() {
    assert!(socket_age_valid(0));
    assert!(socket_age_valid(86399));
    assert!(!socket_age_valid(86400));
    assert!(!socket_age_valid(u64::MAX));
}

#[test]
fn remembered_prototype_database_migrates_and_revokes_old_credentials() {
    let (dir,db,id)=setup();let (family,_,_)=create(&db,&id);
    let path=dir.path().join("users.db");
    db.conn().execute_batch("ALTER TABLE remembered_families DROP COLUMN generation;").unwrap();
    drop(db);
    std::fs::remove_file(path.with_extension("remember-key")).unwrap();
    let db=UserDb::new(&path).unwrap();
    assert!(!db.remember_session_valid(Some(&family),&id,1001).unwrap());
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM remembered_tokens",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert!(db.remember_create(&id,0,&uuid::Uuid::new_v4().to_string(),"New",None,180,1001,false,|_,_,_|Ok(())).is_ok());
}

#[test]
fn remembered_signing_key_creation_is_shared_and_owner_only() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("users.db");
    let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));
    let other_path=path.clone();let other_barrier=barrier.clone();
    let worker=std::thread::spawn(move|| {
        let conn=Connection::open_in_memory().unwrap();other_barrier.wait();
        let key=load_key(&other_path,&conn).unwrap();credential(&key,"test-family",0)
    });
    let conn=Connection::open_in_memory().unwrap();barrier.wait();
    let key=load_key(&path,&conn).unwrap();
    assert_eq!(credential(&key,"test-family",0),worker.join().unwrap());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path.with_extension("remember-key")).unwrap().permissions().mode() & 0o777,0o600);
    }
}

#[test]
fn admin_can_revoke_existing_legacy_non_uuid_user() {
    let (_dir,db,admin)=setup();
    let target=db.create_user("legacy@example.com","Legacy","test-password",UserRole::Employee).unwrap();
    db.conn().execute("UPDATE users SET id='legacy-user' WHERE id=?1",[target.id]).unwrap();
    let (_,secret,_)=create(&db,"legacy-user");
    db.remember_admin_revoke_all(&admin,0,"legacy-user").unwrap();
    assert!(db.remember_rotate(&secret,1001,false,|_,_,_|Ok(())).is_err());
}
