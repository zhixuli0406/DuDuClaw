use super::*;

struct Fixture { home: tempfile::TempDir, db: Arc<UserDb>, handler: MethodHandler, member: UserContext, admin: UserContext }
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[member_api_keys]\nenabled=true\nrate_per_minute=8").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let member = db.create_user(&format!("member-{}@test.invalid", uuid::Uuid::new_v4()), "Member", "test-password", UserRole::Employee).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"member-rpc-tests-secret-at-least-32-bytes"))).await;
    Fixture { member: db.live_user_context(&member.id).unwrap().unwrap(), admin: db.live_user_context(&admin.id).unwrap().unwrap(), home, db, handler }
}
async fn browser(f: &Fixture, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), uuid::Uuid::new_v4().to_string())),
        f.handler.handle_conn(method, params, ctx, crate::power_local::RpcConnInfo::from_ws("127.0.0.1:12345".parse().unwrap(), false))).await
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
    let args = json!({"name":"service", "expires_in_days":90, "password":"test-password"});
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
    let key = f.db.member_key_create(&f.member.user_id, "x", 1, f.member.credential_epoch, false, &[]).unwrap();
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

#[tokio::test]
async fn stolen_access_and_refreshed_access_cannot_create_without_fresh_proof() {
    let f = fixture().await;
    let jwt = JwtConfig::new(b"member-rpc-tests-secret-at-least-32-bytes");
    let user = f.db.get_user(&f.member.user_id).unwrap().unwrap();
    let (access, refresh) = jwt.issue_session_tokens(&user, &[]).unwrap();
    let stolen = jwt.verify_refresh_token(&refresh).unwrap();
    let refreshed = jwt.issue_access_token_in_session(&user, &[], stolen.jti.as_deref()).unwrap();
    for access in [access, refreshed] {
        let claims = jwt.verify_access_token(&access).unwrap();
        let result = crate::google_personal::DASHBOARD_SESSION.scope(Some((claims.sub, claims.jti.unwrap())),
            f.handler.handle_conn("me.member_keys.create", json!({"name":"stolen", "expires_in_days":365}), &f.member,
                crate::power_local::RpcConnInfo::from_ws("127.0.0.1:12345".parse().unwrap(), false))).await;
        assert_eq!(code(result), "member_key_step_up_required");
    }
    assert!(f.db.member_api_keys_for_user(&f.member.user_id).unwrap().is_empty());
    payload(browser(&f, &f.member, "me.member_keys.create", json!({"name":"legitimate", "expires_in_days":365, "password":"test-password"})).await);
}

#[tokio::test]
async fn wrong_password_uses_the_login_lockout_and_cannot_be_bypassed_by_reconnect() {
    let f = fixture().await;
    for _ in 0..5 {
        assert_eq!(code(browser(&f, &f.member, "me.member_keys.create",
            json!({"name":"x", "expires_in_days":30, "password":"wrong"})).await), "member_key_step_up_required");
    }
    assert!(!crate::server::check_login_rate_limit("127.0.0.1".parse().unwrap(), &f.member.email));
    assert_eq!(code(browser(&f, &f.member, "me.member_keys.create",
        json!({"name":"x", "expires_in_days":30, "password":"test-password"})).await), "member_key_reauth_rate_limited");
    assert!(f.db.member_api_keys_for_user(&f.member.user_id).unwrap().is_empty());
    let g = fixture().await;
    for _ in 0..5 { assert!(crate::server::check_login_rate_limit("127.0.0.1".parse().unwrap(), &g.member.email)); }
    assert_eq!(code(browser(&g, &g.member, "me.member_keys.create",
        json!({"name":"x", "expires_in_days":30, "password":"test-password"})).await), "member_key_reauth_rate_limited");
}

#[tokio::test]
async fn key_updates_reject_all_credential_fields_and_legacy_aliases_without_writes() {
    let f = fixture().await;
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=true\nrate_per_minute=60000").unwrap();
    let key = f.db.member_key_create(&f.member.user_id, "test", 1, 0, false, &[]).unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    let agent = f.home.path().join("agents/test");
    std::fs::create_dir_all(&agent).unwrap();
    let original = "[agent]\nname='test'\ndisplay_name='Unchanged'\n[channels.telegram]\nbot_token='original'\n[odoo]\napi_key='original'\npassword='original'\n";
    std::fs::write(agent.join("agent.toml"), original).unwrap();
    let fields = ["telegram_bot_token", "discord_bot_token", "slack_app_token", "slack_bot_token",
        "telegram_token", "discord_token", "slack_token", "bot_token", "app_token", "api_key", "password",
        "token", "secret", "key", "credential", "accessToken", "apiKey", "client_secret",
        "line_channel_access_token", "line_channel_secret", "line_access_token", "line_secret",
        "whatsapp_access_token", "whatsapp_verify_token", "feishu_app_secret", "feishu_encrypt_key",
        "feishu_verification_token", "wecom_corp_secret", "wecom_token", "wecom_encoding_aes_key",
        "dingtalk_app_secret", "odoo_api_key", "odoo_password", "odoo.api_key", "odoo.password",
        "telegram_bot_token_enc", "discord_bot_token_enc", "slack_app_token_enc", "slack_bot_token_enc",
        "future_secret", "unknown_field"];
    let mut cases = Vec::new();
    for field in fields {
        for value in [json!("replacement"), json!(""), Value::Null, json!({})] {
            let mut args = json!({"agent_id":"test", "display_name":"Must not change"});
            args[field] = value;
            cases.push(args);
        }
    }
    for field in ["api_key", "password", "api_key_enc", "password_enc"] {
        for value in [json!("replacement"), json!(""), Value::Null] {
            let mut args = json!({"agent_id":"test", "display_name":"Must not change", "odoo":{}});
            args["odoo"][field] = value;
            cases.push(args);
        }
    }
    cases.extend([
        json!({"agent_id":"test", "odoo":null}),
        json!({"agent_id":"test", "channels":{"telegram":{"bot_token":""}}}),
        json!({"agent_id":"test", "prompt":{"api_key":"secret"}}),
        json!({"agent_id":"test", "memory":{"credential":"secret"}}),
        json!({"agent_id":"test", "advanced_kv":[{"section":"odoo", "key":"password", "value":"", "type":"string"}]}),
    ]);
    for args in cases {
        assert_eq!(code(f.handler.handle("agents.update", args.clone(), &ctx).await), "member_key_forbidden_field", "{args}");
        assert_eq!(code(f.handler.handle_agents_update_as(args, Some(&ctx)).await), "member_key_forbidden_field");
        assert_eq!(std::fs::read_to_string(agent.join("agent.toml")).unwrap(), original);
    }
}

#[tokio::test]
async fn member_key_owner_cannot_enable_unlimited_skill_content() {
    let f = fixture().await;
    payload(f.handler.handle_agents_create_as(
        json!({"name": "skill-owner", "display_name": "Skill Owner"}), Some(&f.admin),
    ).await);
    f.db.bind_agent(&f.member.user_id, "skill-owner", AccessLevel::Owner).unwrap();
    let key = f.db.member_key_create(
        &f.member.user_id, "skills", 1, f.member.credential_epoch, false, &[],
    ).unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    let path = f.home.path().join("agents/skill-owner/agent.toml");
    let before = std::fs::read(&path).unwrap();
    let frame = f.handler.handle("agents.update", json!({
        "agent_id": "skill-owner", "skill_budget_unlimited": true,
    }), &ctx).await;
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => {
            let message = e.as_str().unwrap();
            assert!(message.contains("administrator"), "{message}");
            assert!(message.contains("evolution.skill_budget_unlimited"), "{message}");
        }
        other => panic!("expected authority refusal, got {other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|row| row["event_type"] == "agent_authority_refused"
            && row["details"]["user_id"] == ctx.user_id
            && row["details"]["keys"] == json!(["evolution.skill_budget_unlimited"])));
}

#[tokio::test]
async fn lists_paginate_and_reject_invalid_limits_and_cursors() {
    let f = fixture().await;
    for _ in 0..3 { f.db.member_key_create(&f.member.user_id, "x", 1, 0, false, &[]).unwrap(); }
    for (ctx, method) in [(&f.member, "me.member_keys.list"), (&f.admin, "member_keys.admin_list")] {
        let first = payload(browser(&f, ctx, method, json!({"limit":2})).await);
        let second = payload(browser(&f, ctx, method, json!({"limit":2, "cursor":first["next_cursor"]})).await);
        assert_eq!(first["keys"].as_array().unwrap().len(), 2);
        assert_eq!(second["keys"].as_array().unwrap().len(), 1);
        assert!(second["next_cursor"].is_null());
        for args in [json!({"limit":0}), json!({"limit":101}), json!({"limit":"25"}), json!({"cursor":42}), json!({"cursor":"bad"})] {
            assert_eq!(code(browser(&f, ctx, method, args).await), "member_key_invalid_page");
        }
    }
}

include!("member_key_scope_tests.rs");
