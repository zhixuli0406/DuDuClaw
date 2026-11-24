use super::*;

struct Fixture { home: tempfile::TempDir, db: Arc<UserDb>, handler: MethodHandler, member: UserContext, admin: UserContext }
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[member_api_keys]\nenabled=true\nrate_per_minute=8").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let member = db.create_user("member@test.invalid", "Member", "test-password", UserRole::Employee).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"member-rpc-tests-secret-at-least-32-bytes"))).await;
    Fixture { member: db.live_user_context(&member.id).unwrap().unwrap(), admin: db.live_user_context(&admin.id).unwrap().unwrap(), home, db, handler }
}
async fn browser(f: &Fixture, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), uuid::Uuid::new_v4().to_string())),
        f.handler.handle(method, params, ctx)).await
}
fn code(frame: WsFrame) -> String {
    match frame { WsFrame::Response { ok: false, error: Some(e), .. } => e["code"].as_str().unwrap_or("").to_owned(), _ => panic!("expected refusal") }
}
fn payload(frame: WsFrame) -> Value {
    match frame { WsFrame::Response { ok: true, payload: Some(p), .. } => p, _ => panic!("expected success") }
}
#[tokio::test]
async fn browser_and_admin_creation_gates_and_disabled_management() {
    let f = fixture().await;
    let args = json!({"name":"service", "expires_in_days":90});
    assert_eq!(code(f.handler.handle("me.member_keys.create", args.clone(), &f.member).await), "member_keys_browser_required");
    assert_eq!(code(browser(&f, &f.admin, "me.member_keys.create", args.clone()).await), "member_key_admin_forbidden");
    let created = payload(browser(&f, &f.member, "me.member_keys.create", args).await);
    let listed = payload(browser(&f, &f.member, "me.member_keys.list", json!({})).await);
    assert!(!listed.to_string().contains(created["secret"].as_str().unwrap()));
    for config in ["[member_api_keys]\nenabled=false", "[member_api_keys]\nenabled='true'", "bad ["] {
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        for method in ["me.member_keys.list", "me.member_keys.create", "me.member_keys.revoke", "member_keys.admin_list", "member_keys.admin_revoke"] {
            assert_eq!(code(browser(&f, &f.admin, method, json!({})).await), "member_keys_disabled");
        }
        assert!(crate::member_keys::authenticate(f.home.path(), &f.db, created["secret"].as_str().unwrap()).is_err());
    }
}
#[tokio::test]
async fn mfa_enabled_creation_requires_fresh_factor() {
    let f = fixture().await;
    let raw = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'unusable-test-secret',0)", [&f.member.user_id]).unwrap();
    let result = browser(&f, &f.member, "me.member_keys.create", json!({"name":"x","expires_in_days":30})).await;
    assert_eq!(code(result), "member_key_step_up_required");
    assert!(f.db.member_api_keys_for_user(&f.member.user_id).unwrap().is_empty());
}
#[tokio::test]
async fn contract_rate_and_open_context_revalidation() {
    let f = fixture().await;
    let key = f.db.member_key_create(&f.member.user_id, "x", 1, f.member.credential_epoch).unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    for method in ["me.member_keys.list", "me.status_tokens.create", "users.change_password", "mfa.status", "system.config", "not.in.contract"] {
        assert_eq!(code(f.handler.handle(method, json!({}), &ctx).await), "member_key_forbidden");
    }
    for _ in 0..2 { payload(f.handler.handle("users.me", json!({}), &ctx).await); }
    assert_eq!(code(f.handler.handle("users.me", json!({}), &ctx).await), "member_key_rate_limited");
    assert!(f.handler.socket_context(&ctx, false).await.is_ok());
    f.db.member_key_revoke_own(&f.member.user_id, &key.info.id).unwrap();
    assert!(f.handler.socket_context(&ctx, false).await.is_err());
    assert!(f.handler.socket_context(&ctx, true).await.is_err());
    assert!(matches!(f.handler.handle("users.me", json!({}), &ctx).await, WsFrame::Response { ok: false, .. }));
}
