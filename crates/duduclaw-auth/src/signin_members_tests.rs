use super::*;
use crate::google_signin::{SIGNIN_PENDING_MAX, SIGNIN_REQUEST_TTL_SECS};
use crate::test_support::test_db;
use crate::{User, UserRole, UserStatus};

const NOW: i64 = 1_800_000_000;

fn identity(sub: &str) -> VerifiedGoogleIdentity {
    VerifiedGoogleIdentity { sub: sub.into(), email: format!("member{sub}@company.test"),
        hd: "company.test".into(), display_name: "新同仁".into() }
}
fn policy(approval: SigninApproval, daily_cap: u32) -> SigninPolicy {
    SigninPolicy { approval, daily_cap, settings_version: "v1".into(), ack_by: "owner".into(), ack_at: "ack-time".into() }
}
fn password_user(db: &UserDb, email: &str, role: UserRole) -> User {
    db.create_user(email, "Existing", "test-password", role).unwrap()
}
fn admin(db: &UserDb) -> User { password_user(db, "admin@company.test", UserRole::Admin) }
fn direct(db: &UserDb, id: &VerifiedGoogleIdentity) -> String {
    match db.resolve_google_signin(id, &policy(SigninApproval::Direct, 100), NOW).unwrap() {
        SigninOutcome::Ok { user_id, credential_epoch, created: true, linked_via: Some("jit_direct") } => {
            assert_eq!(credential_epoch, db.get_user(&user_id).unwrap().unwrap().credential_epoch);
            user_id
        },
        other => panic!("unexpected outcome: {other:?}"),
    }
}
fn request(db: &UserDb, id: &VerifiedGoogleIdentity, at: i64) -> String {
    match db.resolve_google_signin(id, &policy(SigninApproval::Admin, 100), at).unwrap() {
        SigninOutcome::PendingApproval { request_id } => request_id,
        other => panic!("unexpected outcome: {other:?}"),
    }
}
fn count(db: &UserDb, table: &str) -> i64 {
    db.conn().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}
fn assert_passwordless_employee(db: &UserDb, user_id: &str) {
    let user = db.get_user(user_id).unwrap().unwrap();
    assert_eq!(user.role, UserRole::Employee);
    assert_eq!(user.status, UserStatus::Active);
    assert!(!user.must_change_password);
    assert_eq!(user.avatar_agent_id, None);
    assert_eq!(db.configured_primary_agent(user_id).unwrap(), None);
    assert!(db.get_user_agents(user_id).unwrap().is_empty());
    let hash: String = db.conn().query_row("SELECT password_hash FROM users WHERE id=?1", [user_id], |r| r.get(0)).unwrap();
    assert!(hash.is_empty());
    assert!(db.verify_password(&user.email, "").is_err());
    assert!(db.verify_password(&user.email, "test-password").is_err());
}
fn assert_onboarding(db: &UserDb, user_id: &str) {
    let valid: bool = db.conn().query_row("SELECT source='google_signin' AND avatar='pending' AND google='pending'
        AND google_chat='pending' AND odoo='pending' AND mfa='pending' FROM user_onboarding WHERE user_id=?1",
        [user_id], |r| r.get(0)).unwrap();
    assert!(valid);
}
fn audit_row(db: &UserDb, action: &str) -> (String, String, Value) {
    let (actor, target, detail): (String, String, String) = db.conn().query_row(
        "SELECT user_id,target,detail FROM auth_audit_log WHERE action=?1", [action],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    (actor, target, serde_json::from_str(&detail).unwrap())
}

#[test]
fn existing_email_never_links() {
    let (db, _path) = test_db();
    password_user(&db, "MEMBER1@company.test", UserRole::Employee);
    assert_eq!(db.resolve_google_signin(&identity("1"), &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::ExistingAccount);
    assert_eq!(count(&db, "users"), 1);
    assert_eq!(count(&db, "google_signin_links"), 0);
}

#[test]
fn recycled_email_new_sub_refused() {
    let (db, _path) = test_db();
    let first = identity("1");
    let user_id = direct(&db, &first);
    let recycled = VerifiedGoogleIdentity { sub: "2".into(), ..first };
    assert_eq!(db.resolve_google_signin(&recycled, &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::ExistingAccount);
    assert_eq!(count(&db, "users"), 1);
    assert_eq!(count(&db, "google_signin_links"), 1);
    assert_eq!(db.google_signin_link_for_user(&user_id).unwrap().unwrap().google_sub, "1");
}

#[test]
fn suspended_never_revived() {
    let (db, _path) = test_db();
    let id = identity("1");
    let user_id = direct(&db, &id);
    db.set_user_status(&user_id, UserStatus::Suspended).unwrap();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 100), NOW + 1).unwrap(), SigninOutcome::AccountDisabled);
    assert_eq!(db.get_user(&user_id).unwrap().unwrap().status, UserStatus::Suspended);
    assert_eq!(db.google_signin_link_for_user(&user_id).unwrap().unwrap().last_signin_at, None);
}

#[test]
fn offboarded_email_not_revived() {
    let (db, _path) = test_db();
    let user = password_user(&db, &identity("1").email, UserRole::Employee);
    db.set_user_status(&user.id, UserStatus::Offboarded).unwrap();
    assert_eq!(db.resolve_google_signin(&identity("1"), &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::ExistingAccount);
    assert_eq!(db.get_user(&user.id).unwrap().unwrap().status, UserStatus::Offboarded);
    assert_eq!(count(&db, "users"), 1);
    assert_eq!(count(&db, "google_signin_links"), 0);
}

#[test]
fn role_always_employee() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let before = db.acl_generation().unwrap();
    let user_id = direct(&db, &identity("1"));
    assert_passwordless_employee(&db, &user_id);
    assert_onboarding(&db, &user_id);
    assert_eq!(db.acl_generation().unwrap(), before + 1);
    let (actor, target, detail) = audit_row(&db, "user.create_member_signin");
    assert_eq!(actor, SYSTEM_ACTOR);
    assert_eq!(target, user_id);
    assert_eq!(detail, json!({"linked_via":"jit_direct", "hd":"company.test", "settings_version":"v1", "ack_by":"owner", "ack_at":"ack-time"}));
    let request_id = request(&db, &identity("2"), NOW);
    let approved = db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW).unwrap();
    assert_passwordless_employee(&db, &approved);
    assert_eq!(db.acl_generation().unwrap(), before + 2);
}

#[test]
fn daily_cap_counts_pending() {
    let (db, _path) = test_db();
    request(&db, &identity("1"), NOW);
    let direct_policy = policy(SigninApproval::Direct, 2);
    assert!(matches!(db.resolve_google_signin(&identity("2"), &direct_policy, NOW).unwrap(), SigninOutcome::Ok { created: true, .. }));
    for mode in [SigninApproval::Admin, SigninApproval::Direct] {
        assert_eq!(db.resolve_google_signin(&identity("3"), &policy(mode, 2), NOW).unwrap(), SigninOutcome::DailyLimit);
        assert_eq!(db.resolve_google_signin(&identity("3"), &policy(mode, 0), NOW).unwrap(), SigninOutcome::DailyLimit);
    }
    assert_eq!(db.count_signin_jit_since(NOW - 86400).unwrap(), 2);
    // Direct mode must not use the pending-refresh exemption.
    assert_eq!(db.resolve_google_signin(&identity("1"), &direct_policy, NOW).unwrap(), SigninOutcome::DailyLimit);
    assert!(matches!(db.resolve_google_signin(&identity("3"), &policy(SigninApproval::Admin, 2), NOW + 86401).unwrap(), SigninOutcome::PendingApproval { .. }));
}

#[test]
fn pending_refresh_not_capped() {
    let (db, _path) = test_db();
    let mut id = identity("1");
    let request_id = request(&db, &id, NOW);
    id.email = "changed@company.test".into();
    id.display_name = "更新名稱".into();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Admin, 0), NOW + 10).unwrap(),
        SigninOutcome::PendingApproval { request_id: request_id.clone() });
    let refreshed = db.signin_request_by_id(&request_id).unwrap().unwrap();
    assert_eq!(refreshed.email, id.email);
    assert_eq!(refreshed.display_name, id.display_name);
    assert_eq!(refreshed.requested_at, NOW);
    assert_eq!(refreshed.expires_at, NOW + SIGNIN_REQUEST_TTL_SECS);
    assert_eq!(count(&db, "google_signin_requests"), 1);
}

#[test]
fn pending_full_maps_to_outcome() {
    let (db, _path) = test_db();
    for sub in 1..=SIGNIN_PENDING_MAX { request(&db, &identity(&sub.to_string()), NOW); }
    assert_eq!(db.resolve_google_signin(&identity("999"), &policy(SigninApproval::Admin, 100), NOW).unwrap(), SigninOutcome::PendingFull);
    assert!(matches!(db.resolve_google_signin(&identity("1"), &policy(SigninApproval::Admin, 0), NOW + 1).unwrap(), SigninOutcome::PendingApproval { .. }));
    assert_eq!(count(&db, "google_signin_requests"), i64::from(SIGNIN_PENDING_MAX));
}

#[test]
fn rejected_cooldown() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let id = identity("1");
    let request_id = request(&db, &id, NOW - 9 * 86400);
    db.reject_signin_request(&admin.id, admin.credential_epoch, &request_id, NOW - 8 * 86400).unwrap();
    let rejected = db.signin_request_by_id(&request_id).unwrap().unwrap();
    assert_eq!(rejected.status, SigninRequestStatus::Rejected);
    assert_eq!(rejected.decided_by.as_deref(), Some(admin.id.as_str()));
    let (actor, target, _) = audit_row(&db, "user.signin_request_rejected");
    assert_eq!(actor, admin.id);
    assert_eq!(target, request_id);
    // At NOW - 2 days the rejection is six days old.
    for mode in [SigninApproval::Admin, SigninApproval::Direct] {
        assert_eq!(db.resolve_google_signin(&id, &policy(mode, 100), NOW - 2 * 86400).unwrap(), SigninOutcome::Rejected);
    }
    let new_request = request(&db, &id, NOW);
    assert_ne!(request_id, new_request);
    assert_eq!(count(&db, "users"), 1);
}

#[test]
fn attestation_links_once() {
    let (db, _path) = test_db();
    let id = identity("1");
    let user = password_user(&db, &id.email.to_ascii_uppercase(), UserRole::Employee);
    db.conn().execute("UPDATE users SET credential_epoch=7 WHERE id=?1", [&user.id]).unwrap();
    db.upsert_google_signin_attestation(&user.id, "attesting-admin", NOW).unwrap();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 0), NOW).unwrap(),
        SigninOutcome::Ok { user_id: user.id.clone(), credential_epoch: 7, created: false, linked_via: Some("admin_attest") });
    assert_eq!(count(&db, "google_signin_attestations"), 0);
    let link = db.google_signin_link_for_user(&user.id).unwrap().unwrap();
    assert_eq!(link.linked_by, "attesting-admin");
    assert_eq!(link.linked_via, "admin_attest");
    let (actor, target, detail) = audit_row(&db, "user.google_signin_linked");
    assert_eq!(actor, SYSTEM_ACTOR);
    assert_eq!(target, user.id);
    assert_eq!(detail, json!({"linked_via":"admin_attest", "hd":"company.test"}));
    assert_eq!(db.resolve_google_signin(&VerifiedGoogleIdentity { sub: "2".into(), ..id }, &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::ExistingAccount);
    assert_eq!(count(&db, "users"), 1);
}

#[test]
fn attestation_void_after_email_change() {
    let (db, _path) = test_db();
    let mut id = identity("1");
    let user = password_user(&db, &id.email, UserRole::Employee);
    db.upsert_google_signin_attestation(&user.id, "admin", NOW).unwrap();
    id.email = "changed@company.test".into();
    db.conn().execute("UPDATE users SET email=?2 WHERE id=?1", params![user.id, id.email]).unwrap();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::ExistingAccount);
    assert_eq!(count(&db, "google_signin_links"), 0);
    assert_eq!(count(&db, "google_signin_attestations"), 0);
}

#[test]
fn approve_requires_live_admin() {
    for change in ["role", "epoch", "status", "password"] {
        let (db, _path) = test_db();
        let admin = admin(&db);
        let request_id = request(&db, &identity("1"), NOW);
        let sql = match change {
            "role" => "UPDATE users SET role='employee' WHERE id=?1",
            "epoch" => "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1",
            "status" => "UPDATE users SET status='suspended' WHERE id=?1",
            "password" => "UPDATE users SET must_change_password=1 WHERE id=?1",
            _ => unreachable!(),
        };
        db.conn().execute(sql, [&admin.id]).unwrap();
        assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW), Err("signin_admin_required"));
        assert_eq!(db.reject_signin_request(&admin.id, admin.credential_epoch, &request_id, NOW), Err("signin_admin_required"));
        assert_eq!(db.signin_request_by_id(&request_id).unwrap().unwrap().status, SigninRequestStatus::Pending);
        assert_eq!(count(&db, "google_signin_links"), 0);
        assert_eq!(count(&db, "users"), 1);
    }
}

#[test]
fn approve_creates_account_and_onboarding_row() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let id = identity("1");
    let request_id = request(&db, &id, NOW);
    let user_id = db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, true, NOW + 10).unwrap();
    assert_onboarding(&db, &user_id);
    let approved = db.signin_request_by_id(&request_id).unwrap().unwrap();
    assert_eq!(approved.status, SigninRequestStatus::Approved);
    assert_eq!(approved.user_id.as_deref(), Some(user_id.as_str()));
    assert_eq!(approved.decided_by.as_deref(), Some(admin.id.as_str()));
    assert_eq!(approved.decided_at, Some(NOW + 10));
    assert!(approved.mfa_preauthorized);
    let link = db.google_signin_link_for_user(&user_id).unwrap().unwrap();
    assert_eq!(link.linked_by, admin.id);
    assert_eq!(link.linked_via, "jit_approved");
    let (actor, target, detail) = audit_row(&db, "user.create_member_signin");
    assert_eq!(actor, admin.id);
    assert_eq!(target, user_id);
    assert_eq!(detail, json!({"linked_via":"jit_approved", "hd":"company.test", "request_id":request_id, "mfa_preauthorized":true}));
    assert_eq!(db.count_signin_jit_since(NOW - 86400).unwrap(), 1);
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Admin, 0), NOW + 20).unwrap(),
        SigninOutcome::Ok { credential_epoch: db.get_user(&user_id).unwrap().unwrap().credential_epoch, user_id, created: false, linked_via: None });
    assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW + 20), Err("signin_request_not_pending"));
}

#[test]
fn approve_refuses_when_email_now_exists() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let id = identity("1");
    let request_id = request(&db, &id, NOW);
    password_user(&db, &id.email.to_ascii_uppercase(), UserRole::Employee);
    assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW), Err("signin_email_exists"));
    assert_eq!(db.signin_request_by_id(&request_id).unwrap().unwrap().status, SigninRequestStatus::Pending);
    assert_eq!(count(&db, "users"), 2);
    assert_eq!(count(&db, "google_signin_links"), 0);
}

#[test]
fn jit_transaction_atomic() {
    for target in ["user_onboarding", "auth_audit_log"] {
        for approval in [false, true] {
            let (db, _path) = test_db();
            let admin = admin(&db);
            let id = identity("1");
            let request_id = if approval { Some(request(&db, &id, NOW)) } else { None };
            let before = db.acl_generation().unwrap();
            db.conn().execute_batch(&format!("CREATE TEMP TRIGGER fail_signin BEFORE INSERT ON {target}
                BEGIN SELECT RAISE(ABORT, 'sensitive test SQL failure'); END;")).unwrap();
            if let Some(request_id) = request_id {
                assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, true, NOW), Err(STORAGE));
                let pending = db.signin_request_by_id(&request_id).unwrap().unwrap();
                assert_eq!(pending.status, SigninRequestStatus::Pending);
                assert_eq!(pending.user_id, None);
                assert!(!pending.mfa_preauthorized);
            } else {
                assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 100), NOW), Err(STORAGE.into()));
            }
            assert_eq!(count(&db, "users"), 1);
            assert_eq!(count(&db, "google_signin_links"), 0);
            assert_eq!(count(&db, "user_onboarding"), 0);
            assert_eq!(db.acl_generation().unwrap(), before);
            assert!(db.query_audit_log(None, Some("user.create_member_signin"), 10).unwrap().is_empty());
        }
    }
}

#[test]
fn invalid_identity_denied() {
    let (db, _path) = test_db();
    let valid = identity("1");
    for invalid in [
        VerifiedGoogleIdentity { sub: "not-a-sub".into(), ..valid.clone() },
        VerifiedGoogleIdentity { sub: "1\0".into(), ..valid.clone() },
        VerifiedGoogleIdentity { sub: "1".repeat(256), ..valid.clone() },
        VerifiedGoogleIdentity { hd: "".into(), ..valid.clone() },
        VerifiedGoogleIdentity { hd: "Company.test".into(), ..valid.clone() },
        VerifiedGoogleIdentity { hd: "company.test/".into(), ..valid.clone() },
        VerifiedGoogleIdentity { email: "bad-email".into(), ..valid.clone() },
        VerifiedGoogleIdentity { email: "bad\r\n@company.test".into(), ..valid.clone() },
        VerifiedGoogleIdentity { email: " member@company.test ".into(), ..valid.clone() },
    ] {
        let code = db.resolve_google_signin(&invalid, &policy(SigninApproval::Direct, 100), NOW).unwrap_err();
        assert!(["signin_invalid_sub", "signin_invalid_email", "signin_invalid_hd"].contains(&code.as_str()));
    }
    assert_eq!(count(&db, "users"), 0);
    assert_eq!(count(&db, "google_signin_links"), 0);
    assert_eq!(count(&db, "google_signin_requests"), 0);
}

#[test]
fn signin_existing_link_tracks_sub_and_touches_last_signin() {
    let (db, _path) = test_db();
    let mut id = identity("1");
    let user_id = direct(&db, &id);
    db.conn().execute("UPDATE users SET credential_epoch=9 WHERE id=?1", [&user_id]).unwrap();
    id.email = "renamed@company.test".into();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Admin, 0), NOW + 42).unwrap(),
        SigninOutcome::Ok { user_id: user_id.clone(), credential_epoch: 9, created: false, linked_via: None });
    let link = db.google_signin_link_for_user(&user_id).unwrap().unwrap();
    assert_eq!(link.last_signin_at, Some(NOW + 42));
    assert_eq!(link.email_at_link, identity("1").email);
    assert_eq!(count(&db, "users"), 1);
}

#[test]
fn signin_display_name_sanitized_and_falls_back() {
    let (db, _path) = test_db();
    let id = VerifiedGoogleIdentity { display_name: format!("\n{}\0", "同🦀".repeat(150)), ..identity("1") };
    let user_id = direct(&db, &id);
    assert_eq!(db.get_user(&user_id).unwrap().unwrap().display_name, "同🦀".repeat(100));
    let id = VerifiedGoogleIdentity { display_name: "\r\n\0".into(), ..identity("2") };
    let request_id = request(&db, &id, NOW);
    assert_eq!(db.signin_request_by_id(&request_id).unwrap().unwrap().display_name, "member2");
}

#[test]
fn signin_approve_refuses_expired_missing_and_linked_requests() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let id = identity("1");
    let request_id = request(&db, &id, NOW);
    assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, "missing", false, NOW), Err("signin_request_not_found"));
    assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW + SIGNIN_REQUEST_TTL_SECS), Err("signin_request_expired"));
    direct(&db, &id);
    assert_eq!(db.approve_signin_request(&admin.id, admin.credential_epoch, &request_id, false, NOW), Err("signin_sub_linked"));
    assert_eq!(db.signin_request_by_id(&request_id).unwrap().unwrap().status, SigninRequestStatus::Pending);
}

#[test]
fn signin_attestation_and_rejection_roll_back_with_audit() {
    let (db, _path) = test_db();
    let admin = admin(&db);
    let id = identity("1");
    let user = password_user(&db, &id.email, UserRole::Employee);
    db.upsert_google_signin_attestation(&user.id, &admin.id, NOW).unwrap();
    let request_id = request(&db, &identity("2"), NOW);
    db.conn().execute_batch("CREATE TEMP TRIGGER fail_signin_audit BEFORE INSERT ON auth_audit_log
        BEGIN SELECT RAISE(ABORT, 'sensitive test SQL failure'); END;").unwrap();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 100), NOW), Err(STORAGE.into()));
    assert_eq!(count(&db, "google_signin_attestations"), 1);
    assert_eq!(count(&db, "google_signin_links"), 0);
    assert_eq!(db.reject_signin_request(&admin.id, admin.credential_epoch, &request_id, NOW), Err(STORAGE));
    assert_eq!(db.signin_request_by_id(&request_id).unwrap().unwrap().status, SigninRequestStatus::Pending);
}

#[test]
fn signin_attested_disabled_user_never_links() {
    let (db, _path) = test_db();
    let id = identity("1");
    let user = password_user(&db, &id.email, UserRole::Employee);
    db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
    db.upsert_google_signin_attestation(&user.id, "admin", NOW).unwrap();
    assert_eq!(db.resolve_google_signin(&id, &policy(SigninApproval::Direct, 100), NOW).unwrap(), SigninOutcome::AccountDisabled);
    assert_eq!(count(&db, "google_signin_links"), 0);
    assert_eq!(db.get_user(&user.id).unwrap().unwrap().status, UserStatus::Suspended);
}

#[test]
fn user_update_google_only_account_cannot_be_admin() {
    let (db, _path) = test_db();
    let id = direct(&db, &identity("123"));
    for role in [UserRole::Admin, UserRole::Manager] {
        let update = crate::UserUpdate { role: Some(role), display_name: Some("Changed"), password: Some("new-password"), ..Default::default() };
        assert_eq!(db.apply_user_update(&id, &update).unwrap_err().to_string(), "role_requires_local_password");
        assert_eq!(db.update_user(&id, None, Some(role), None).unwrap_err(), "role_requires_local_password");
        assert_eq!(db.get_user(&id).unwrap().unwrap().display_name, "新同仁");
        assert_passwordless_employee(&db, &id);
    }
    db.update_user(&id, None, Some(UserRole::Employee), None).unwrap();
    let password = password_user(&db, "password@company.test", UserRole::Employee);
    for role in [UserRole::Manager, UserRole::Admin, UserRole::Employee] {
        db.update_user(&password.id, None, Some(role), None).unwrap();
        assert_eq!(db.get_user(&password.id).unwrap().unwrap().role, role);
    }
}
