use super::*;
use duduclaw_auth::{JwtConfig, chat_link::HandshakeStatus};
use std::sync::Arc;

const PRINCIPAL: &str = "12344821";
const SESSION_A: &str = "00000000-0000-4000-8000-000000000001";
const SESSION_B: &str = "00000000-0000-4000-8000-000000000002";
const METHODS: &[&str] = &[
    "me.chat_link.start", "me.chat_link.status", "me.chat_link.confirm", "me.chat_link.cancel",
    "me.chat_link.reject", "me.chat_link.list", "me.chat_link.remove",
];

fn ok(frame: &WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: true, .. }) }
fn assert_login_refused(frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => assert_eq!(error, json!(LOGIN_REQUIRED)),
        _ => panic!("expected dashboard-session refusal"),
    }
}
fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        _ => panic!("expected successful chat-link response"),
    }
}

async fn fixture() -> (tempfile::TempDir, Arc<UserDb>, MethodHandler, UserContext) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),
        "[channels]\ntelegram_bot_token = 'fixture-token'\n").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let admin = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
    let ctx = db.live_user_context(&admin.id).unwrap().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
    (home, db, handler, ctx)
}

async fn call(handler: &MethodHandler, ctx: &UserContext, session: &str, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(
        Some((ctx.user_id.clone(), session.to_owned())),
        handler.handle_chat_link_rpc(method, params, ctx),
    ).await
}

async fn begin(handler: &MethodHandler, ctx: &UserContext) -> Value {
    payload(call(handler, ctx, SESSION_A, "me.chat_link.start", json!({"channel": "telegram"})).await)
}

fn receive(db: &UserDb, started: &Value) {
    assert_eq!(chat_link::match_code(db, "telegram",
        &chat_link::parse_code(started["code"].as_str().unwrap()).unwrap(),
        PRINCIPAL, "telegram", PRINCIPAL, "<b>王小明</b>", true).unwrap(), chat_link::MatchOutcome::Matched);
}

#[tokio::test]
async fn chat_link_rpc_refuses_member_and_mcp_keys() {
    let (_home, _db, handler, ctx) = fixture().await;
    for method in METHODS {
        let mut member = ctx.clone();
        member.member_key_id = Some("member-key".into());
        assert_login_refused(call(&handler, &member, SESSION_A, method, json!({})).await);
        // Legacy/internal contexts stay forbidden even with an artificial scope.
        assert_login_refused(call(&handler, &UserContext::admin_fallback(), SESSION_A, method, json!({})).await);
        // An individually named context from an in-process/MCP caller has no JWT session.
        assert_login_refused(handler.handle_chat_link_rpc(method, json!({}), &ctx).await);
        assert_eq!(super::super::member_contract::member_key_method(method), None);
    }
    crate::google_personal::DASHBOARD_SESSION.scope(
        Some(("other-user".into(), SESSION_A.into())), async {
            assert!(session_hash(&ctx).is_err());
        },
    ).await;
    assert!(!ok(&handler.handle("me.chat_link.list", json!({}), &UserContext::admin_fallback()).await));
}

#[tokio::test]
async fn chat_link_dispatch_routes_known_and_refuses_unknown_methods() {
    let (_home, _db, handler, ctx) = fixture().await;
    crate::google_personal::DASHBOARD_SESSION.scope(
        Some((ctx.user_id.clone(), SESSION_A.into())), async {
            assert!(ok(&handler.handle("me.chat_link.list", json!({}), &ctx).await));
            assert!(!ok(&handler.handle("me.chat_link.future", json!({}), &ctx).await));
        },
    ).await;
}

#[tokio::test]
async fn chat_link_rpc_refuses_non_admin_and_live_demotion() {
    let (_home, db, handler, ctx) = fixture().await;
    for role in [UserRole::Manager, UserRole::Employee] {
        let mut non_admin = ctx.clone();
        non_admin.role = role;
        assert!(!ok(&call(&handler, &non_admin, SESSION_A, "me.chat_link.list", json!({})).await));
    }
    db.update_user(&ctx.user_id, None, Some(UserRole::Employee), None).unwrap();
    assert!(!ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.list", json!({})).await));
}

#[tokio::test]
async fn chat_link_confirm_from_another_session_refused() {
    let (home, db, handler, ctx) = fixture().await;
    let started = begin(&handler, &ctx).await;
    receive(&db, &started);
    let params = json!({"handshake_id": started["handshake_id"]});
    assert!(!ok(&call(&handler, &ctx, SESSION_B, "me.chat_link.confirm", params.clone()).await));
    assert!(db.approval_proofs(&ctx.user_id).unwrap().is_empty());
    assert!(ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.confirm", params).await));
    assert_eq!(db.approval_proofs(&ctx.user_id).unwrap().len(), 1);
    let records = db.query_audit_log(Some(&ctx.user_id), Some("channel_identity_proven"), 10).unwrap();
    assert_eq!(records.len(), 1);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("channel_identity_proven"));
    assert!(audit.contains("••••4821"));
    assert!(!audit.contains(PRINCIPAL));
    assert!(!audit.contains(SESSION_A));
    assert!(!audit.contains(started["code"].as_str().unwrap()));
}

#[tokio::test]
async fn rejected_match_writes_audit() {
    let (home, db, handler, ctx) = fixture().await;
    let started = begin(&handler, &ctx).await;
    receive(&db, &started);
    let params = json!({"handshake_id": started["handshake_id"]});
    assert!(ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.reject", params.clone()).await));
    assert!(!ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.reject", params.clone()).await));
    assert!(!ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.confirm", params).await));
    assert_eq!(chat_link::status(&db, &ctx.user_id, started["handshake_id"].as_str().unwrap())
        .unwrap().unwrap().status, HandshakeStatus::Rejected);
    let records = db.query_audit_log(Some(&ctx.user_id), Some("channel_link_rejected"), 10).unwrap();
    assert_eq!(records.len(), 1);
    let detail: Value = serde_json::from_str(records[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["user_id"], ctx.user_id);
    assert_eq!(detail["channel"], "telegram");
    assert_eq!(detail["masked_principal"], "••••4821");
    assert_eq!(detail["account"], "telegram");
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert_eq!(audit.lines().filter(|line| line.contains("channel_link_rejected")).count(), 1);
    assert!(!audit.contains(PRINCIPAL));
    assert!(!audit.contains(started["code"].as_str().unwrap()));
    assert!(db.approval_proofs(&ctx.user_id).unwrap().is_empty());
}

#[tokio::test]
async fn chat_link_start_refuses_without_configured_bot() {
    let (home, db, handler, ctx) = fixture().await;
    std::fs::write(home.path().join("config.toml"), "[channels]\n").unwrap();
    for channel in chat_link::CHANNELS {
        assert!(!ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.start", json!({"channel": channel})).await));
    }
    assert!(chat_link::pending_ids(&db, &ctx.user_id).unwrap().is_empty());
}

#[tokio::test]
async fn another_users_handshake_is_not_visible() {
    let (_home, db, handler, ctx) = fixture().await;
    let other = db.create_user("other@example.test", "Other", "password-test", UserRole::Admin).unwrap();
    let other = db.live_user_context(&other.id).unwrap().unwrap();
    let started = begin(&handler, &ctx).await;
    for method in ["me.chat_link.status", "me.chat_link.confirm", "me.chat_link.cancel", "me.chat_link.reject"] {
        assert!(!ok(&call(&handler, &other, SESSION_A, method,
            json!({"handshake_id": started["handshake_id"]})).await));
    }
    let own = payload(call(&handler, &other, SESSION_A, "me.chat_link.list", json!({})).await);
    assert_eq!(own["handshakes"], json!([]));
    assert_eq!(chat_link::status(&db, &ctx.user_id, started["handshake_id"].as_str().unwrap())
        .unwrap().unwrap().status, HandshakeStatus::Waiting);
}

#[tokio::test]
async fn chat_link_status_list_and_remove_keep_owner_scope() {
    let (_home, db, handler, ctx) = fixture().await;
    let started = begin(&handler, &ctx).await;
    receive(&db, &started);
    let params = json!({"handshake_id": started["handshake_id"]});
    let status = payload(call(&handler, &ctx, SESSION_A, "me.chat_link.status", params.clone()).await);
    assert_eq!(status["status"], "matched");
    assert_eq!(status["display_name"], "<b>王小明</b>");
    assert_eq!(status["masked_target"], "••••4821");
    for key in ["code", "code_digest", "session_hash", "matched_principal", "matched_dm", "matched_account"] {
        assert!(status.get(key).is_none(), "{key}");
    }
    let listing = payload(call(&handler, &ctx, SESSION_A, "me.chat_link.list", json!({})).await);
    assert_eq!(listing["handshakes"], json!([status]));
    assert!(ok(&call(&handler, &ctx, SESSION_A, "me.chat_link.confirm", params).await));
    db.bind_channel_identity(&ctx.user_id, "googlechat", "users/12", false).unwrap();
    let listing = payload(call(&handler, &ctx, SESSION_A, "me.chat_link.list", json!({})).await);
    let identities = listing["identities"].as_array().unwrap();
    assert!(identities.iter().any(|row| row["channel"] == "telegram" && row["proof_status"] == "self_confirmed"));
    assert!(identities.iter().any(|row| row["channel"] == "googlechat" && row["proof_status"] == "admin_prefilled"));
    let other = db.create_user("other@example.test", "Other", "password-test", UserRole::Admin).unwrap();
    let other = db.live_user_context(&other.id).unwrap().unwrap();
    let remove = json!({"channel": "telegram", "channel_user_id": PRINCIPAL});
    assert!(!ok(&call(&handler, &other, SESSION_A, "me.chat_link.remove", remove.clone()).await));
    let generation = db.live_user_context(&ctx.user_id).unwrap().unwrap().acl_generation;
    assert_eq!(payload(call(&handler, &ctx, SESSION_A, "me.chat_link.remove", remove.clone()).await),
        json!({"revoked": true}));
    let repeat = call(&handler, &ctx, SESSION_A, "me.chat_link.remove", remove).await;
    assert!(!ok(&repeat));
    assert!(serde_json::to_string(&repeat).unwrap().contains(NOT_FOUND));
    let rows = db.channel_identities_for_user(&ctx.user_id).unwrap();
    assert!(rows.iter().any(|row| row.channel == "telegram" && row.channel_user_id == PRINCIPAL && row.verified));
    assert_eq!(db.find_verified_user_id_by_channel("telegram", PRINCIPAL).unwrap(), Some(ctx.user_id.clone()));
    let unproven = call(&handler, &ctx, SESSION_A, "me.chat_link.remove",
        json!({"channel": "googlechat", "channel_user_id": "users/12"})).await;
    assert!(!ok(&unproven));
    assert!(serde_json::to_string(&unproven).unwrap().contains(NOT_FOUND));
    assert!(db.approval_proofs(&ctx.user_id).unwrap().is_empty());
    assert!(db.live_user_context(&ctx.user_id).unwrap().unwrap().acl_generation > generation);
    let audit = db.query_audit_log(Some(&ctx.user_id), Some("channel_identity_proof_revoked"), 10).unwrap();
    assert_eq!(audit.len(), 1);
    let detail: Value = serde_json::from_str(audit[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["user_id"], ctx.user_id);
    assert_eq!(detail["channel"], "telegram");
    assert_eq!(detail["masked_principal"], otp::mask_target(PRINCIPAL));
    assert_eq!(detail["account"], "telegram");
    assert!(!audit[0].detail.as_deref().unwrap().contains(PRINCIPAL));
    assert!(db.query_audit_log(Some(&ctx.user_id), Some("channel_identity_unbound"), 10).unwrap().is_empty());
}

#[tokio::test]
async fn chat_link_cancel_and_unmatched_reject_do_not_report_stolen_match() {
    let (_home, db, handler, ctx) = fixture().await;
    for method in ["me.chat_link.cancel", "me.chat_link.reject"] {
        let started = begin(&handler, &ctx).await;
        assert!(ok(&call(&handler, &ctx, SESSION_A, method,
            json!({"handshake_id": started["handshake_id"]})).await));
    }
    assert!(db.query_audit_log(Some(&ctx.user_id), Some("channel_link_rejected"), 10).unwrap().is_empty());
}

#[test]
fn chat_link_methods_and_inputs_are_explicit() {
    for method in METHODS {
        assert_eq!(minimum_role(method), Some(UserRole::Admin));
        assert!(!super::super::is_enterprise_only_method(method));
    }
    assert_eq!(minimum_role("me.chat_link.future"), None);
    for (method, params) in [
        ("me.chat_link.future", json!({})),
        ("me.chat_link.start", json!({"channel": "Telegram"})),
        ("me.chat_link.start", json!({"channel": "telegram", "user_id": "other"})),
        ("me.chat_link.confirm", json!({"handshake_id": SESSION_A, "session_hash": "forged"})),
        ("me.chat_link.confirm", json!({"handshake_id": "not-a-uuid"})),
        ("me.chat_link.status", json!({"handshake_id": "x".repeat(500)})),
        ("me.chat_link.list", json!({"user_id": "other"})),
        ("me.chat_link.list", json!([])),
        ("me.chat_link.remove", json!({"channel": "telegram", "channel_user_id": "x".repeat(513)})),
        ("me.chat_link.remove", json!({"channel": "telegram", "channel_user_id": "123\n456"})),
    ] {
        assert!(request(method, &params).is_err());
    }
}

#[test]
fn chat_link_targets_only_list_configured_http_apps_and_cached_names() {
    let home = tempfile::tempdir().unwrap();
    let shared = "[channels]\ngooglechat_project_number = '123456789'\ngooglechat_service_account_json_enc = 'encrypted'\n";
    std::fs::write(home.path().join("config.toml"), shared).unwrap();
    assert_eq!(targets(home.path(), "googlechat", &[]).unwrap().len(), 1);
    std::fs::write(home.path().join("config.toml"), format!("{shared}googlechat_connection = 'pubsub'\n")).unwrap();
    assert!(targets(home.path(), "googlechat", &[]).is_err());
    let agent = "p9-chat-link-fixture";
    let dir = home.path().join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    let config = "[channels.telegram]\nbot_token_enc = 'encrypted'\n[channels.googlechat]\nproject_number = '987654321'\nservice_account_json_enc = 'encrypted'\n";
    std::fs::write(dir.join("agent.toml"), config).unwrap();
    let label = crate::agent_channels::label("telegram", agent);
    crate::agent_channels::identity::set(&label, "fixture_bot");
    let agents = vec![agent.to_owned()];
    let tg = targets(home.path(), "telegram", &agents).unwrap();
    assert_eq!(tg[0]["bot_username"], "fixture_bot");
    assert_eq!(tg[0]["account"], label);
    assert_eq!(tg[0]["agent_id"], agent);
    assert_eq!(targets(home.path(), "googlechat", &agents).unwrap()[0]["account"],
        crate::agent_channels::label("googlechat", agent));
    std::fs::write(dir.join("agent.toml"), format!("{config}connection = 'pubsub'\n")).unwrap();
    assert!(targets(home.path(), "googlechat", &agents).is_err());
    std::fs::write(dir.join("agent.toml"), format!("{config}connection = true\n")).unwrap();
    assert!(targets(home.path(), "googlechat", &agents).is_err());
    crate::agent_channels::identity::clear(&label);
}

#[test]
fn status_projection_normalizes_existing_display_names() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
    let started = chat_link::start(&db, &user.id, "telegram", "session").unwrap();
    let state = chat_link::status(&db, &user.id, &started.handshake_id).unwrap().unwrap();
    assert!(status_value(state)["display_name"].is_null());
    let name = format!("王\n\r\t\u{1b}\u{202E}\u{200B}{}", "明".repeat(250));
    let mut state = chat_link::status(&db, &user.id, &started.handshake_id).unwrap().unwrap();
    state.matched_display = Some(name);
    assert_eq!(status_value(state)["display_name"], json!(format!("王{}", "明".repeat(199))));
}
