use super::*;
use crate::{test_support::test_db, UserRole};

fn user(db: &UserDb, name: &str) -> String {
    db.create_user(&format!("{name}@test.invalid"), name, "password123", UserRole::Employee).unwrap().id
}
fn link(user: &str, sub: &str, via: &str, at: i64) -> NewGoogleSigninLink {
    NewGoogleSigninLink { user_id: user.into(), google_sub: sub.into(), hd: "test.invalid".into(),
        email_at_link: "a@test.invalid".into(), linked_via: via.into(), linked_by: "admin".into(), linked_at: at }
}
fn insert(db: &UserDb, link: &NewGoogleSigninLink) -> Result<(), String> {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    UserDb::insert_google_signin_link_tx(&tx, link)?;
    tx.commit().map_err(error)
}
fn request(db: &UserDb, sub: &str, now: i64) -> Result<(), String> {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    UserDb::upsert_pending_signin_request_tx(&tx, &format!("req-{sub}"), sub, "a@test.invalid", "test.invalid", "A", now)?;
    tx.commit().map_err(error)
}
fn take(db: &UserDb, user: &str, email: &str, now: i64) -> bool {
    let mut conn = db.conn();
    let tx = conn.transaction().unwrap();
    let valid = UserDb::take_valid_google_signin_attestation_tx(&tx, user, email, now).unwrap();
    tx.commit().unwrap();
    valid
}
fn attestation_count(db: &UserDb) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM google_signin_attestations", [], |r| r.get(0)).unwrap()
}

#[test]
fn google_signin_link_unique_checks_and_primitives() {
    let (db, _path) = test_db();
    let a = user(&db, "a"); let b = user(&db, "b");
    insert(&db, &link(&a, "123", "jit_direct", 10)).unwrap();
    assert!(insert(&db, &link(&b, "123", "self_link", 10)).is_err());
    assert!(insert(&db, &link(&a, "456", "self_link", 10)).is_err());
    assert_eq!(db.google_signin_link_by_sub("123").unwrap().unwrap().user_id, a);
    db.touch_google_signin(&a, 42).unwrap();
    assert_eq!(db.google_signin_link_for_user(&a).unwrap().unwrap().last_signin_at, Some(42));
    assert!(insert(&db, &link(&b, "456", "invalid", 10)).is_err());
    for sub in ["".to_owned(), "1x".into(), "１２".into(), "1\0x".into(), "1".repeat(256)] {
        assert!(!valid_google_sub(&sub));
        assert!(insert(&db, &link(&b, &sub, "self_link", 10)).is_err());
    }
    // Bypass Rust to exercise the database CHECK constraints independently.
    for (sub, via) in [("456".to_owned(), "bad"), ("x1".into(), "self_link"),
        ("1x".into(), "self_link"), ("1\0x".into(), "self_link"), ("".into(), "self_link"), ("1".repeat(256), "self_link")] {
        assert!(db.conn().execute("INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
            VALUES(?1,?2,'test.invalid','b@test.invalid',?3,'admin',0)", params![b,sub,via]).is_err());
        if via == "self_link" {
            assert!(db.conn().execute("INSERT INTO google_signin_requests(id,google_sub,email,hd,display_name,status,requested_at,expires_at)
                VALUES('bad',?1,'b@test.invalid','test.invalid','B','pending',0,1)", [&sub]).is_err());
        }
    }
    assert!(valid_google_sub(&"1".repeat(255)));
    assert!(db.delete_google_signin_link(&a).unwrap());
    assert!(!db.delete_google_signin_link(&a).unwrap());
    assert!(db.google_signin_link_by_sub("123").unwrap().is_none());
}

#[test]
fn google_signin_pending_cap_and_request_lifecycle() {
    let (db, _path) = test_db();
    for n in 1..=SIGNIN_PENDING_MAX { request(&db, &n.to_string(), 100).unwrap(); }
    assert_eq!(request(&db, "51", 100).unwrap_err(), "signin_pending_full");
    request(&db, "1", 200).unwrap();
    let first = db.signin_request_by_sub("1").unwrap().unwrap();
    assert_eq!(first.requested_at, 100);
    assert_eq!(first.expires_at, 100 + SIGNIN_REQUEST_TTL_SECS);
    assert_eq!(db.list_pending_signin_requests().unwrap().len(), 50);
    let a = user(&db, "a");
    {
        let mut conn = db.conn(); let tx = conn.transaction().unwrap();
        UserDb::set_signin_request_status_tx(&tx, &first.id, SigninRequestStatus::Approved, "admin", 300, Some(&a), true).unwrap();
        tx.commit().unwrap();
    }
    let approved = db.signin_request_by_id(&first.id).unwrap().unwrap();
    assert_eq!(approved.status, SigninRequestStatus::Approved);
    assert_eq!(approved.decided_by.as_deref(), Some("admin"));
    assert_eq!(approved.decided_at, Some(300));
    assert_eq!(approved.user_id.as_deref(), Some(a.as_str()));
    assert!(approved.mfa_preauthorized);
    assert_eq!(db.expire_signin_requests(100 + SIGNIN_REQUEST_TTL_SECS - 1).unwrap(), 0);
    assert_eq!(db.expire_signin_requests(100 + SIGNIN_REQUEST_TTL_SECS).unwrap(), 49);
    assert_eq!(db.signin_request_by_sub("2").unwrap().unwrap().status, SigninRequestStatus::Expired);
    request(&db, "1", 400).unwrap();
    let pending = db.signin_request_by_sub("1").unwrap().unwrap();
    assert_eq!(pending.status, SigninRequestStatus::Pending);
    assert_eq!(pending.requested_at, 400);
    assert_eq!(pending.decided_at, None);
    assert_eq!(pending.user_id, None);
    assert!(!pending.mfa_preauthorized);
    assert!(SigninRequestStatus::parse("unknown").is_err());
    for status in [SigninRequestStatus::Pending, SigninRequestStatus::Approved, SigninRequestStatus::Rejected, SigninRequestStatus::Expired] {
        assert_eq!(SigninRequestStatus::parse(status.as_str()).unwrap(), status);
    }
}

#[test]
fn google_signin_attestation_void_after_email_change() {
    let (db, _path) = test_db(); let a = user(&db, "a");
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    // Even changing back to the original email must not restore the attestation.
    db.conn().execute("UPDATE users SET email='changed@test.invalid' WHERE id=?1", [&a]).unwrap();
    db.conn().execute("UPDATE users SET email='a@test.invalid' WHERE id=?1", [&a]).unwrap();
    assert!(!take(&db, &a, "a@test.invalid", 101));
    assert_eq!(attestation_count(&db), 0);
}
#[test]
fn google_signin_attestation_void_after_credential_seq_change() {
    let (db, _path) = test_db(); let a = user(&db, "a");
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    db.conn().execute("UPDATE users SET password_hash='changed' WHERE id=?1", [&a]).unwrap();
    assert!(!take(&db, &a, "a@test.invalid", 101));
    assert_eq!(attestation_count(&db), 0);
}
#[test]
fn google_signin_attestation_expiry_single_use_and_token_email() {
    let (db, _path) = test_db(); let a = user(&db, "a"); let b = user(&db, "b");
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    assert!(!take(&db, &a, "other@test.invalid", 101));
    assert_eq!(attestation_count(&db), 1);
    // Another account's change does not invalidate this user's attestation.
    db.conn().execute("UPDATE users SET password_hash='changed' WHERE id=?1", [&b]).unwrap();
    assert!(take(&db, &a, "A@TEST.INVALID", 101));
    assert!(!take(&db, &a, "a@test.invalid", 101));
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    assert!(!take(&db, &a, "a@test.invalid", 100 + SIGNIN_ATTESTATION_TTL_SECS));
    assert_eq!(attestation_count(&db), 0);
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    assert!(take(&db, &a, "a@test.invalid", 100 + SIGNIN_ATTESTATION_TTL_SECS - 1));
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    db.delete_google_signin_attestation(&a).unwrap();
    assert_eq!(attestation_count(&db), 0);
}
#[test]
fn google_signin_jit_count_includes_pending() {
    let (db, _path) = test_db();
    for (name, sub, via, at) in [("a","1","jit_direct",100), ("b","2","jit_approved",101),
        ("c","3","self_link",100), ("d","4","admin_attest",100), ("e","5","jit_direct",99)] {
        let id = user(&db, name); insert(&db, &link(&id, sub, via, at)).unwrap();
    }
    request(&db, "6", 100).unwrap(); request(&db, "7", 99).unwrap();
    assert_eq!(db.count_signin_jit_since(100).unwrap(), 3);
    assert_eq!(db.count_signin_jit_since(101).unwrap(), 1);
    assert_eq!(db.count_signin_jit_since(102).unwrap(), 0);
}
#[test]
fn google_signin_user_delete_cascades_and_nulls_request() {
    let (db, _path) = test_db(); let a = user(&db, "a");
    insert(&db, &link(&a, "1", "self_link", 100)).unwrap();
    db.upsert_google_signin_attestation(&a, "admin", 100).unwrap();
    request(&db, "2", 100).unwrap();
    {
        let mut conn = db.conn(); let tx = conn.transaction().unwrap();
        UserDb::set_signin_request_status_tx(&tx, "req-2", SigninRequestStatus::Approved, "admin", 100, Some(&a), false).unwrap();
        tx.commit().unwrap();
    }
    db.conn().execute("DELETE FROM users WHERE id=?1", [&a]).unwrap();
    assert!(db.google_signin_link_for_user(&a).unwrap().is_none());
    assert_eq!(attestation_count(&db), 0);
    assert_eq!(db.signin_request_by_sub("2").unwrap().unwrap().user_id, None);
}
#[test]
fn google_signin_legacy_onboarding_migration_is_idempotent() {
    let (db, path) = test_db(); let a = user(&db, "a");
    db.conn().execute_batch("DROP TABLE user_onboarding;
        CREATE TABLE user_onboarding(user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        dismissed INTEGER NOT NULL DEFAULT 0,channel TEXT NOT NULL DEFAULT 'pending',google TEXT NOT NULL DEFAULT 'pending',
        example TEXT NOT NULL DEFAULT 'pending',capabilities TEXT NOT NULL DEFAULT 'pending');").unwrap();
    db.conn().execute("INSERT INTO user_onboarding(user_id,channel) VALUES(?1,'skipped')", [&a]).unwrap();
    crate::onboarding::migrate(&db.conn()).unwrap();
    crate::onboarding::migrate(&db.conn()).unwrap();
    for column in ["source","avatar","google_chat","odoo","mfa","avatar_agent_name","avatar_next_at","avatar_error"] {
        let value: Option<String> = db.conn().query_row(&format!("SELECT {column} FROM user_onboarding WHERE user_id=?1"), [&a], |r| r.get(0)).unwrap();
        assert_eq!(value, None);
    }
    let attempts: i64 = db.conn().query_row("SELECT avatar_attempts FROM user_onboarding WHERE user_id=?1", [&a], |r| r.get(0)).unwrap();
    assert_eq!(attempts, 0);
    let progress = db.onboarding_progress(&a, 0).unwrap();
    assert!(progress.enabled);
    assert_eq!(progress.steps.channel, crate::onboarding::StepStatus::Skipped);
    drop(db);
    let reopened = UserDb::new(path.path()).unwrap();
    assert_eq!(reopened.onboarding_progress(&a, 0).unwrap().steps.channel, crate::onboarding::StepStatus::Skipped);
}

#[test]
fn signin_attest_batch_is_atomic_and_checks_live_admin() {
    let (db, _path) = test_db();
    let admin = db.create_user("admin@test.invalid", "Admin", "password123", UserRole::Admin).unwrap();
    let a = user(&db, "a");
    let b = user(&db, "b");
    for ids in [vec![a.clone(), "missing".into()], vec![a.clone(), a.clone()], vec![a.clone(); 101]] {
        assert_eq!(db.attest_google_signin_users(&admin.id, admin.credential_epoch, &ids, 100), Err("signin_attest_invalid_target"));
        assert_eq!(attestation_count(&db), 0);
        assert!(db.query_audit_log(None, Some("google_signin.attested"), 100).unwrap().is_empty());
    }
    db.set_user_status(&b, crate::UserStatus::Suspended).unwrap();
    assert_eq!(db.attest_google_signin_users(&admin.id, admin.credential_epoch, &[a.clone(), b.clone()], 100), Err("signin_attest_invalid_target"));
    db.set_user_status(&b, crate::UserStatus::Active).unwrap();
    insert(&db, &link(&b, "123", "self_link", 10)).unwrap();
    assert_eq!(db.attest_google_signin_users(&admin.id, admin.credential_epoch, &[a.clone(), b], 100), Err("signin_attest_invalid_target"));
    assert_eq!(db.attest_google_signin_users(&a, 0, &[a.clone()], 100), Err("signin_admin_required"));
    assert_eq!(db.attest_google_signin_users(&admin.id, admin.credential_epoch + 1, &[a.clone()], 100), Err("signin_admin_required"));
    assert_eq!(attestation_count(&db), 0);
    assert_eq!(db.attest_google_signin_users(&admin.id, admin.credential_epoch, &[a.clone(), admin.id.clone()], 100), Ok(2));
    assert_eq!(attestation_count(&db), 2);
    assert_eq!(db.query_audit_log(Some(&admin.id), Some("google_signin.attested"), 100).unwrap().len(), 2);
    assert!(take(&db, &a, "a@test.invalid", 101));
}

#[test]
fn google_signin_unlink_revokes_sessions() {
    let (db, _path) = test_db();
    let admin = db.create_user("admin@test.invalid", "Admin", "password123", UserRole::Admin).unwrap();
    let a = user(&db, "a");
    insert(&db, &link(&a, "123", "self_link", 10)).unwrap();
    db.upsert_google_signin_attestation(&a, &admin.id, 100).unwrap();
    let user = db.get_user(&a).unwrap().unwrap();
    let jwt = crate::JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
    let (_, token) = jwt.issue_session_tokens(&user, &[]).unwrap();
    let claims = jwt.verify_refresh_token(&token).unwrap();
    let refresh = || db.mfa_refresh_and_issue(&a, claims.credential_epoch, claims.jti.as_deref(), false, |_, _| Ok(()));
    assert!(refresh().is_ok());
    let generation = db.acl_generation().unwrap();
    assert_eq!(db.unlink_google_signin(&a, user.credential_epoch, &a, 101), Err("signin_admin_required"));
    assert_eq!(db.unlink_google_signin(&admin.id, admin.credential_epoch + 1, &a, 101), Err("signin_admin_required"));
    db.unlink_google_signin(&admin.id, admin.credential_epoch, &a, 101).unwrap();
    assert_eq!(db.get_user(&a).unwrap().unwrap().credential_epoch, user.credential_epoch + 1);
    assert!(db.acl_generation().unwrap() > generation);
    assert!(refresh().is_err());
    assert!(db.google_signin_link_for_user(&a).unwrap().is_none());
    assert_eq!(attestation_count(&db), 0);
    assert_eq!(db.query_audit_log(Some(&admin.id), Some("google_signin.unlinked"), 100).unwrap().len(), 1);
    assert_eq!(db.unlink_google_signin(&admin.id, admin.credential_epoch, &a, 102), Err("signin_not_linked"));
}

#[test]
fn google_signin_storage_errors_do_not_expose_sqlite_details() {
    let (db, _path) = test_db();
    let a = user(&db, "a");
    insert(&db, &link(&a, "123", "self_link", 10)).unwrap();
    assert_eq!(insert(&db, &link(&a, "456", "self_link", 10)), Err("signin_storage_unavailable".into()));
    db.conn().execute("DROP TABLE google_signin_requests", []).unwrap();
    assert_eq!(db.list_pending_signin_requests().unwrap_err(), "signin_storage_unavailable");
}

#[test]
fn link_requires_password() {
    let (db, _path) = test_db();
    let id = user(&db, "self");
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    assert_eq!(db.google_signin_verify_password(&id, epoch, "wrong"), Err("signin_password_invalid"));
    db.google_signin_verify_password(&id, epoch, "password123").unwrap();
    db.conn().execute("UPDATE users SET password_hash='' WHERE id=?1", [&id]).unwrap();
    assert_eq!(db.google_signin_verify_password(&id, epoch, "password123"), Err("signin_link_needs_password"));
}

#[test]
fn link_refuses_sub_linked_elsewhere() {
    let (db, _path) = test_db();
    let a = user(&db, "first");
    let b = user(&db, "second");
    insert(&db, &link(&a, "123", "jit_direct", 1)).unwrap();
    let identity = crate::signin_members::VerifiedGoogleIdentity {
        sub: "123".into(), email: "second@test.invalid".into(), hd: "test.invalid".into(), display_name: "Second".into(),
    };
    let epoch = db.get_user(&b).unwrap().unwrap().credential_epoch;
    assert_eq!(db.self_link_google_signin(&b, epoch, db.google_signin_credential_revision(&b).unwrap(), &identity, 2), Err("signin_sub_linked"));
    assert!(db.google_signin_link_for_user(&b).unwrap().is_none());
}

#[test]
fn unlink_passwordless_refused() {
    let (db, _path) = test_db();
    let id = user(&db, "self");
    insert(&db, &link(&id, "123", "jit_direct", 1)).unwrap();
    db.conn().execute("UPDATE users SET password_hash='' WHERE id=?1", [&id]).unwrap();
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    assert_eq!(db.self_unlink_google_signin(&id, epoch, "password123", 2), Err("signin_unlink_needs_password"));
    assert!(db.google_signin_link_for_user(&id).unwrap().is_some());
}

#[test]
fn unlink_bumps_epoch() {
    let (db, _path) = test_db();
    let id = user(&db, "self");
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    let identity = crate::signin_members::VerifiedGoogleIdentity {
        sub: "123".into(), email: "self@test.invalid".into(), hd: "test.invalid".into(), display_name: "Self".into(),
    };
    db.self_link_google_signin(&id, epoch, db.google_signin_credential_revision(&id).unwrap(), &identity, 1).unwrap();
    let row = db.google_signin_link_for_user(&id).unwrap().unwrap();
    assert_eq!(row.linked_via, "self_link");
    assert_eq!(row.linked_by, id);
    assert!(db.self_unlink_google_signin(&id, epoch+1, "password123", 2).is_err());
    assert!(db.self_unlink_google_signin(&id, epoch, "wrong", 2).is_err());
    db.self_unlink_google_signin(&id, epoch, "password123", 2).unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch, epoch+1);
    assert!(db.google_signin_link_for_user(&id).unwrap().is_none());
    let actions: Vec<String> = db.conn().prepare("SELECT action FROM auth_audit_log WHERE user_id=?1 ORDER BY id").unwrap()
        .query_map([&id], |r| r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    assert!(actions.iter().any(|a| a=="google_signin.self_linked"));
    assert!(actions.iter().any(|a| a=="google_signin.self_unlinked"));
}

#[test]
fn google_signin_link_password_change_invalidates_epoch_zero_proof() {
    let (db, _path) = test_db();
    let id = user(&db, "self");
    let revision = db.google_signin_credential_revision(&id).unwrap();
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    db.google_signin_verify_password(&id, epoch, "password123").unwrap();
    db.change_own_password(&id, "replacement123").unwrap();
    assert_eq!(db.get_user(&id).unwrap().unwrap().credential_epoch, epoch);
    let identity = crate::signin_members::VerifiedGoogleIdentity {
        sub: "123".into(), email: "self@test.invalid".into(), hd: "test.invalid".into(), display_name: "Self".into(),
    };
    assert_eq!(db.self_link_google_signin(&id, epoch, revision, &identity, 2), Err("signin_session_expired"));
}

#[test]
fn self_link_requires_matching_email() {
    let (db, _path) = test_db();
    let id = user(&db, "self");
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    let revision = db.google_signin_credential_revision(&id).unwrap();
    let mut identity = crate::signin_members::VerifiedGoogleIdentity {
        sub: "123".into(), email: "victim@test.invalid".into(), hd: "test.invalid".into(), display_name: "Victim".into(),
    };
    assert_eq!(db.self_link_google_signin(&id, epoch, revision, &identity, 1), Err("signin_link_email_mismatch"));
    assert!(db.google_signin_link_for_user(&id).unwrap().is_none());
    // Completion must compare the current email, not an earlier account snapshot.
    db.conn().execute("UPDATE users SET email='current@test.invalid' WHERE id=?1", [&id]).unwrap();
    let epoch = db.get_user(&id).unwrap().unwrap().credential_epoch;
    let revision = db.google_signin_credential_revision(&id).unwrap();
    identity.email = "self@test.invalid".into();
    assert_eq!(db.self_link_google_signin(&id, epoch, revision, &identity, 2), Err("signin_link_email_mismatch"));
    assert!(db.google_signin_link_for_user(&id).unwrap().is_none());
    identity.email = "CURRENT@TEST.INVALID".into();
    db.self_link_google_signin(&id, epoch, revision, &identity, 3).unwrap();
    assert_eq!(db.google_signin_link_for_user(&id).unwrap().unwrap().google_sub, "123");
}
