use super::*;
use duduclaw_auth::UserRole;

fn identity(auth_time: Option<i64>) -> oidc::VerifiedIdToken {
    oidc::VerifiedIdToken { sub: "123".into(), email: "member@example.test".into(), hd: "example.test".into(), name: "Member".into(), auth_time }
}
fn binding() -> SessionBinding {
    SessionBinding { user_id: uuid::Uuid::new_v4().to_string(), session_id: uuid::Uuid::new_v4().to_string(), epoch: 0 }
}
fn fixture() -> (tempfile::TempDir, UserDb, SessionBinding) {
    let home = crate::fs_safe::private_test_home().unwrap();
    std::fs::write(home.path().join("config.toml"), "[dashboard]\npublic_url='https://company.test'\n[google_signin]\nenabled=true\nallowed_hd=['example.test']\napproval='admin'\ndaily_new_accounts=20\n[google_signin.enabled_ack]\nby='admin'\nat='2026-11-27T00:00:00Z'\n").unwrap();
    crate::mcp_oauth::upsert_client_config(home.path(), crate::mcp_oauth::McpOAuthClientConfig {
        provider_id: "google".into(), client_id: "client".into(), client_secret: "test-secret".into(),
        auth_url: oauth::AUTHORIZE.into(), token_url: oauth::TOKEN.into(), scopes: vec![], redirect_uri: String::new(),
    }).unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("member@example.test", "Member", "password123", UserRole::Employee).unwrap();
    let b = SessionBinding { user_id: user.id, epoch: user.credential_epoch, ..binding() };
    (home, db, b)
}
fn add_ticket(home: &Path, b: &SessionBinding, reauth: bool, auth_time: Option<i64>) -> String {
    let value = oidc::random_state().unwrap();
    TICKETS.lock().unwrap().insert(hash(&value), Ticket {
        credential_revision: Some(0),
        purpose: if reauth { Purpose::Reauth(b.clone()) } else { Purpose::Link(b.clone()) },
        home: home.into(), cookie_sha256: [0;32], secure: true,
        result: LoginResult::Identity(identity(auth_time)), created: Instant::now(),
    });
    value
}
fn make_passwordless(home: &Path, db: &UserDb, b: &mut SessionBinding) {
    let conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
    conn.execute("UPDATE users SET password_hash='' WHERE id=?1", [&b.user_id]).unwrap();
    conn.execute("INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
        VALUES(?1,'123','example.test','member@example.test','jit_direct',?1,1)", [&b.user_id]).unwrap();
    b.epoch = db.get_user(&b.user_id).unwrap().unwrap().credential_epoch;
}
#[test]
fn link_refuses_different_personal_connection_sub() {
    assert_eq!(same_personal_sub(Some("456"), "123"), Err("signin_personal_sub_mismatch"));
    assert!(same_personal_sub(Some("123"), "123").is_ok());
    assert!(same_personal_sub(None, "123").is_ok());
}
#[test]
fn link_ticket_bound_to_session() {
    let (home, db, b) = fixture();
    for other in [SessionBinding { session_id: uuid::Uuid::new_v4().to_string(), ..b.clone() },
        SessionBinding { epoch: b.epoch+1, ..b.clone() }, SessionBinding { user_id: "other".into(), ..b.clone() }] {
        let value = add_ticket(home.path(), &b, false, None);
        assert!(claim_ticket(home.path(), &other, &value, false).is_err());
        assert!(claim_ticket(home.path(), &b, &value, false).is_err());
    }
    let value = add_ticket(home.path(), &b, false, None);
    assert!(claim_ticket(home.path(), &b, &value, true).is_err());
    let value = add_ticket(home.path(), &b, false, None);
    rusqlite::Connection::open(home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&b.user_id]).unwrap();
    assert_eq!(complete(home.path(), &db, &b, &value, false, 1000), Err("signin_session_expired"));
    assert!(db.google_signin_link_for_user(&b.user_id).unwrap().is_none());
}
#[test]
fn reauth_only_for_passwordless() {
    let (home, db, mut b) = fixture();
    assert_eq!(start(home.path(), &db, b.clone(), true, ""), Err("signin_reauth_password_account"));
    make_passwordless(home.path(), &db, &mut b);
    let url = start(home.path(), &db, b.clone(), true, "").unwrap();
    let url = url::Url::parse(&url).unwrap();
    let query: HashMap<_,_> = url.query_pairs().into_owned().collect();
    assert_eq!(query["claims"], r#"{"id_token":{"auth_time":{"essential":true}}}"#);
    assert_eq!(query["login_hint"], "member@example.test");
    assert_eq!(take_pending(&query["state"]).unwrap().purpose, Purpose::Reauth(b));
}
#[test]
fn reauth_missing_auth_time_refused() {
    assert_eq!(reauth_identity("123", &identity(None), 1000), Err("signin_reauth_unavailable"));
}
#[test]
fn reauth_stale_auth_time_refused() {
    for at in [699, 1061] {
        assert_eq!(reauth_identity("123", &identity(Some(at)), 1000), Err("signin_reauth_unavailable"));
    }
    for at in [700, 1000, 1060] { reauth_identity("123", &identity(Some(at)), 1000).unwrap(); }
}
#[test]
fn reauth_sub_mismatch_refused() {
    assert_eq!(reauth_identity("456", &identity(Some(1000)), 1000), Err("signin_reauth_sub_mismatch"));
}
#[test]
fn reauth_grant_single_use_and_session_bound() {
    let (home, db, mut b) = fixture();
    make_passwordless(home.path(), &db, &mut b);
    let ticket = add_ticket(home.path(), &b, true, Some(1000));
    complete(home.path(), &db, &b, &ticket, true, 1000).unwrap();
    for other in [SessionBinding { session_id: "other".into(), ..b.clone() }, SessionBinding { epoch: b.epoch+1, ..b.clone() }] {
        assert!(!take_grant(home.path(), &other));
    }
    assert!(!take_grant(&home.path().join("different-home"), &b));
    let token = challenge(home.path(), &db, &b.user_id, &b.session_id, b.epoch, "", 1000, false).unwrap();
    db.mfa_check_session(&token, &b.user_id, &b.session_id, 1000).unwrap();
    assert!(challenge(home.path(), &db, &b.user_id, &b.session_id, b.epoch, "", 1000, false).is_err());
    assert!(complete(home.path(), &db, &b, &ticket, true, 1000).is_err());
    put_grant(home.path(), &b).unwrap();
    GRANTS.lock().unwrap().insert(grant_key(home.path(), &b), Instant::now()-GRANT_TTL);
    assert!(!take_grant(home.path(), &b));
}
#[test]
fn password_account_cannot_use_grant() {
    let (home, db, b) = fixture();
    put_grant(home.path(), &b).unwrap();
    assert!(challenge(home.path(), &db, &b.user_id, &b.session_id, b.epoch, "", 1000, false).is_err());
    assert!(take_grant(home.path(), &b));
}

#[test]
fn reauth_completion_refuses_untrusted_identity_without_grant() {
    let (home, db, mut b) = fixture();
    make_passwordless(home.path(), &db, &mut b);
    for auth_time in [None, Some(699), Some(1061)] {
        let ticket = add_ticket(home.path(), &b, true, auth_time);
        assert_eq!(complete(home.path(), &db, &b, &ticket, true, 1000), Err("signin_reauth_unavailable"));
        assert!(!take_grant(home.path(), &b));
    }
    rusqlite::Connection::open(home.path().join("users.db")).unwrap()
        .execute("UPDATE google_signin_links SET google_sub='456' WHERE user_id=?1", [&b.user_id]).unwrap();
    let ticket = add_ticket(home.path(), &b, true, Some(1000));
    assert_eq!(complete(home.path(), &db, &b, &ticket, true, 1000), Err("signin_reauth_sub_mismatch"));
    assert!(!take_grant(home.path(), &b));
}

#[test]
fn link_requires_password_and_creates_only_link_pending() {
    let (home, db, b) = fixture();
    assert_eq!(start(home.path(), &db, b.clone(), false, ""), Err("signin_password_invalid"));
    assert_eq!(start(home.path(), &db, b.clone(), false, "wrong"), Err("signin_password_invalid"));
    let url = start(home.path(), &db, b.clone(), false, "password123").unwrap();
    let query: HashMap<_,_> = url::Url::parse(&url).unwrap().query_pairs().into_owned().collect();
    assert_eq!(take_pending(&query["state"]).unwrap().purpose, Purpose::Link(b));
}

#[test]
fn link_csrf_other_users_google_refused() {
    let (home, db, b) = fixture();
    let url = start(home.path(), &db, b.clone(), false, "password123").unwrap();
    let query: HashMap<_,_> = url::Url::parse(&url).unwrap().query_pairs().into_owned().collect();
    let pending = take_pending(&query["state"]).unwrap();
    let value = oidc::random_state().unwrap();
    let mut victim = identity(None);
    victim.email = "victim@example.test".into();
    TICKETS.lock().unwrap().insert(hash(&value), Ticket {
        credential_revision: pending.credential_revision,
        purpose: pending.purpose.clone(), home: home.path().into(), cookie_sha256: [0;32], secure: true,
        result: LoginResult::Identity(victim), created: Instant::now(),
    });
    assert_eq!(complete(home.path(), &db, &b, &value, false, 1000), Err("signin_link_email_mismatch"));
    assert!(db.google_signin_link_for_user(&b.user_id).unwrap().is_none());
    assert_eq!(complete(home.path(), &db, &b, &value, false, 1000), Err("signin_session_expired"));
}
