use super::*;
use duduclaw_auth::JwtConfig;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<UserDb>, UserContext) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),
        "# keep this comment\n[approval]\nskill_ttl_days = 7 # sibling\n[channels]\ntelegram_bot_token = 'secret-canary'\n").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let user = db.create_user("admin@example.test", "管理員", "password-test", UserRole::Admin).unwrap();
    let ctx = db.live_user_context(&user.id).unwrap().unwrap();
    (home, db, ctx)
}

fn params(enabled: bool) -> Value {
    json!({"enabled": enabled, "channels": ["telegram", "googlechat"],
        "step_up_methods": ["web"], "card_valid_hours": 168, "step_up_exempt_kinds": []})
}

fn write(home: &Path, db: &UserDb, ctx: &UserContext, value: Value) -> Result<Value, String> {
    set(home, db, ctx, request(value)?, || {})
}

fn ok(frame: &WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: true, .. }) }
fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        _ => panic!("expected successful settings response"),
    }
}

async fn handler(home: &Path, db: Arc<UserDb>) -> MethodHandler {
    let handler = MethodHandler::new(home.to_path_buf()).await;
    handler.set_user_db(db, Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
    handler
}

async fn call(handler: &MethodHandler, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(
        Some((ctx.user_id.clone(), "00000000-0000-4000-8000-000000000001".into())),
        handler.handle_chat_admin_settings_rpc(method, params, ctx),
    ).await
}

fn audits(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap().lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event_type"] == "approval_chat_admin_settings_changed").collect()
}

#[tokio::test]
async fn enabling_requires_ack_and_jwt_but_no_password_or_totp() {
    let (home, db, ctx) = fixture();
    let handler = handler(home.path(), db.clone()).await;
    let original = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!ok(&call(&handler, &ctx, "approvals.chat_admin.set", params(true)).await));
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    assert_eq!(db.mfa_factors(&ctx.user_id).unwrap()["factors"]["totp"], false);
    let mut value = params(true);
    value["acknowledge_risk"] = json!(true);
    let response = payload(call(&handler, &ctx, "approvals.chat_admin.set", value).await);
    assert_eq!(response["success"], true);
    let (text, _, cfg) = read(home.path()).unwrap();
    let ack = cfg.enabled_ack.unwrap();
    assert_eq!(ack.by.to_string(), ctx.user_id);
    assert!((chrono::Utc::now() - ack.at.with_timezone(&chrono::Utc)).num_seconds().abs() < 60);
    assert_eq!(response["hash"], config_commit::content_hash(&text));
    let view = payload(call(&handler, &ctx, "approvals.chat_admin.get", json!({})).await);
    assert_eq!(view["enabled"], true);
    assert_eq!(view["enabled_ack"]["by_name"], "管理員");
    assert_eq!(view["hash"], response["hash"]);
    assert!(!view.to_string().contains("secret-canary"));
}

#[test]
fn exemptions_need_exact_ack_and_rechecking_is_audited() {
    let (home, db, ctx) = fixture();
    let mut value = params(false);
    value["step_up_exempt_kinds"] = json!(["cli_egress", "org_change"]);
    let original = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(write(home.path(), &db, &ctx, value.clone()).is_err());
    for ack in [json!(["cli_egress"]), json!(["cli_egress", "cli_egress"]),
        json!(["cli_egress", "org_change", "egress_guard"])] {
        value["acknowledge_exempt_risk"] = ack;
        assert!(write(home.path(), &db, &ctx, value.clone()).is_err());
    }
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    value["acknowledge_exempt_risk"] = json!(["org_change", "cli_egress"]);
    write(home.path(), &db, &ctx, value).unwrap();
    let rows = audits(home.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["details"]["by"], ctx.user_id);
    assert_eq!(rows[0]["details"]["newly_exempted"], json!(["cli_egress", "org_change"]));
    assert_eq!(rows[0]["details"]["before"]["step_up_exempt_kinds"], json!([]));
    assert_eq!(rows[0]["details"]["after"]["step_up_exempt_kinds"], json!(["cli_egress", "org_change"]));
    assert!(chrono::DateTime::parse_from_rfc3339(rows[0]["details"]["at"].as_str().unwrap()).is_ok());
    write(home.path(), &db, &ctx, params(false)).unwrap();
    let rows = audits(home.path());
    assert_eq!(rows[1]["details"]["newly_required"], json!(["cli_egress", "org_change"]));
    assert_eq!(rows[1]["details"]["newly_exempted"], json!([]));
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("config_changed"));
    assert!(!audit.contains("secret-canary"));
}

#[test]
fn strict_fields_channels_methods_and_t2_values() {
    let (home, db, ctx) = fixture();
    for (key, invalid) in [
        ("unknown", json!(true)), ("enabled_ack", json!({"by": ctx.user_id})),
        ("password", json!("password-test")), ("enabled", json!("true")),
        ("channels", json!(["line"])), ("channels", json!(["slack"])),
        ("channels", json!(["discord"])), ("channels", json!(["telegram", "telegram"])),
        ("step_up_methods", json!([])), ("step_up_methods", json!(["passkey"])),
        ("card_valid_hours", json!(0)), ("card_valid_hours", json!(721)),
        ("card_valid_hours", json!(1.5)), ("step_up_exempt_kinds", json!(["knowledge_quarantine"])),
        ("step_up_exempt_kinds", json!(["unknown"])), ("acknowledge_risk", json!("true")),
        ("acknowledge_risk", Value::Null), ("acknowledge_exempt_risk", json!(true)),
        ("expected_hash", Value::Null),
    ] {
        let mut value = params(false);
        value[key] = invalid;
        assert!(write(home.path(), &db, &ctx, value).is_err(), "{key}");
    }
    let mut value = params(true);
    value["acknowledge_risk"] = json!(true);
    value["channels"] = json!([]);
    assert!(write(home.path(), &db, &ctx, value).is_err());
    let mut valid = params(false);
    valid["step_up_methods"] = json!(["web", "dm_totp"]);
    write(home.path(), &db, &ctx, valid).unwrap();
}

#[test]
fn unreadable_config_get_is_off_all_required_and_set_never_overwrites() {
    let (home, db, ctx) = fixture();
    let path = home.path().join("config.toml");
    for broken in ["[bad", "approval = false", "[approval.chat_admin]\nunknown = true",
        "[approval.chat_admin]\nstep_up_exempt_kinds = ['knowledge_quarantine']"] {
        std::fs::write(&path, broken).unwrap();
        let view = get(home.path(), &db, &[]).unwrap();
        assert_eq!(view["readable"], false);
        assert_eq!(view["enabled"], false);
        assert_eq!(view["enabled_ack"], Value::Null);
        assert_eq!(view["hash"], Value::Null);
        assert_eq!(view["kinds"].as_array().unwrap().len(), STEP_UP_KIND_KEYS.len());
        assert!(view["kinds"].as_array().unwrap().iter().all(|k| k["step_up_required"] == true));
        assert!(write(home.path(), &db, &ctx, params(false)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }
    std::fs::remove_file(&path).unwrap();
    assert_eq!(get(home.path(), &db, &[]).unwrap()["readable"], false);
    assert!(write(home.path(), &db, &ctx, params(false)).is_err());
    assert!(!path.exists());
    std::fs::create_dir(&path).unwrap();
    assert_eq!(get(home.path(), &db, &[]).unwrap()["readable"], false);
    assert!(write(home.path(), &db, &ctx, params(false)).is_err());
}

#[test]
fn disabling_sweeps_after_durable_write_removes_ack_and_preserves_siblings() {
    let (home, db, ctx) = fixture();
    let mut value = params(true);
    value["acknowledge_risk"] = json!(true);
    write(home.path(), &db, &ctx, value).unwrap();
    let ack = read(home.path()).unwrap().2.enabled_ack;
    write(home.path(), &db, &ctx, params(true)).unwrap();
    assert_eq!(read(home.path()).unwrap().2.enabled_ack, ack);
    let swept = std::cell::Cell::new(false);
    set(home.path(), &db, &ctx, request(params(false)).unwrap(), || {
        let (_, table, cfg) = read(home.path()).unwrap();
        assert!(!cfg.enabled);
        assert!(cfg.enabled_ack.is_none());
        assert!(config_commit::toml_at(&table, "approval.chat_admin.enabled_ack").is_none());
        swept.set(true);
    }).unwrap();
    assert!(swept.get());
    let (text, table, _) = read(home.path()).unwrap();
    assert_eq!(table["approval"]["skill_ttl_days"].as_integer(), Some(7));
    assert!(text.contains("# keep this comment"));
    assert!(text.contains("# sibling"));
    assert_eq!(table["channels"]["telegram_bot_token"].as_str(), Some("secret-canary"));
}

#[test]
fn stale_hash_refused_without_audit_or_sweep() {
    let (home, db, ctx) = fixture();
    let original = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let mut value = params(false);
    value["expected_hash"] = json!("stale");
    assert!(set(home.path(), &db, &ctx, request(value).unwrap(), || panic!("must not sweep")).is_err());
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    assert!(!home.path().join("security_audit.jsonl").exists());
}

#[tokio::test]
async fn only_live_admin_jwt_session_can_get_or_set() {
    let (home, db, ctx) = fixture();
    let handler = handler(home.path(), db.clone()).await;
    for (method, value) in [("approvals.chat_admin.get", json!({})), ("approvals.chat_admin.set", params(false))] {
        assert_eq!(minimum_role(method), Some(UserRole::Admin));
        assert!(!ok(&handler.handle_chat_admin_settings_rpc(method, value.clone(), &ctx).await));
        for other in [UserContext::admin_fallback(),
            UserContext { member_key_id: Some("member-key".into()), ..ctx.clone() },
            UserContext { role: UserRole::Manager, ..ctx.clone() },
            UserContext { must_change_password: true, ..ctx.clone() }] {
            assert!(!ok(&call(&handler, &other, method, value.clone()).await));
        }
        crate::google_personal::DASHBOARD_SESSION.scope(Some(("other".into(), "00000000-0000-4000-8000-000000000001".into())), async {
            assert!(!ok(&handler.handle_chat_admin_settings_rpc(method, value.clone(), &ctx).await));
        }).await;
    }
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), "00000000-0000-4000-8000-000000000001".into())), async {
        assert!(ok(&handler.handle("approvals.chat_admin.get", json!({}), &ctx).await));
        assert!(!ok(&handler.handle("approvals.chat_admin.future", json!({}), &ctx).await));
    }).await;
    db.update_user(&ctx.user_id, None, Some(UserRole::Manager), None).unwrap();
    assert!(!ok(&call(&handler, &ctx, "approvals.chat_admin.get", json!({})).await));
    assert!(!ok(&call(&handler, &ctx, "approvals.chat_admin.set", params(false)).await));
}

#[test]
fn status_and_get_do_not_count_removed_bot_proofs_and_report_unsupported_channels() {
    let (home, db, ctx) = fixture();
    let started = duduclaw_auth::chat_link::start(&db, &ctx.user_id, "telegram", "session-hash").unwrap();
    let code = duduclaw_auth::chat_link::parse_code(&started.code).unwrap();
    assert_eq!(duduclaw_auth::chat_link::match_code(&db, "telegram", &code,
        "12344821", "telegram", "12344821", "管理員", true).unwrap(),
        duduclaw_auth::chat_link::MatchOutcome::Matched);
    duduclaw_auth::chat_link::confirm(&db, &ctx.user_id, &started.handshake_id, "session-hash").unwrap();
    assert_eq!(get(home.path(), &db, &[]).unwrap()["admins"][0]["proven_channels"], json!(["telegram"]));
    assert_eq!(status(home.path(), Some(&db), &[])["proven_admins"], 1);
    std::fs::write(home.path().join("config.toml"),
        "[dashboard]\npublic_url='https://dashboard.example.com'\n[approval.chat_admin]\nchannels=['telegram','line']\n").unwrap();
    let view = get(home.path(), &db, &[]).unwrap();
    assert_eq!(view["unsupported_channels_ignored"], true);
    assert_eq!(view["channels"], json!(["telegram"]));
    assert_eq!(view["web_link_available"], true);
    assert_eq!(view["admins"][0]["user_id"], ctx.user_id);
    assert_eq!(view["admins"][0]["proven_channels"], json!([]));
    assert_eq!(view["t1_kinds"].as_array().unwrap().len(), 2);
    assert!(view["kinds"].as_array().unwrap().iter().all(|kind| {
        kind["label"]["zh-TW"].is_string() && kind["label"]["en"].is_string()
            && kind["label"]["ja"].is_string() && kind["impact"].is_string()
    }));
    let status = status(home.path(), Some(&db), &[]);
    assert_eq!(status["enabled"], false);
    assert_eq!(status["proven_admins"], 0);
    assert_eq!(status["web_link_available"], true);
}

#[tokio::test]
async fn generic_update_refused_in_all_representations_before_other_writes() {
    let (home, db, ctx) = fixture();
    write(home.path(), &db, &ctx, params(false)).unwrap();
    let handler = handler(home.path(), db.clone()).await;
    let original = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    for value in [json!({"approval": {"chat_admin": {"enabled": true}}, "log_level": "debug"}),
        json!({"approval.chat_admin.enabled": true}), json!({"approval.chat_admin": null}),
        json!({"approval": null}), json!({"approval": {"chat_admin.enabled": true}})] {
        assert!(!ok(&handler.handle_system_update_config(value, &ctx).await));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    }
    let (_, table, _) = read(home.path()).unwrap();
    assert!(refuse_generic_change(&table, &json!({"approval.chat_admin.enabled": false})).is_ok());
    assert!(refuse_generic_change(&table, &json!({"approval": {"skill_ttl_days": 8}})).is_ok());
    assert!(refuse_generic_change(&table, &json!({"approval.chat_admin":
        config_commit::toml_at_json(&table, "approval.chat_admin")})).is_ok());
}

#[test]
fn ineffective_ack_requires_fresh_enable_acknowledgement() {
    let (home, db, ctx) = fixture();
    for ack in [String::new(), format!("enabled_ack = {{ by = '{}', at = '2026-11-27T00:00:00Z' }}",
        uuid::Uuid::new_v4())] {
        std::fs::write(home.path().join("config.toml"), format!("[approval.chat_admin]\nenabled=true\n{ack}\n")).unwrap();
        assert_eq!(get(home.path(), &db, &[]).unwrap()["enabled"], false);
        assert!(write(home.path(), &db, &ctx, params(true)).is_err());
        let mut value = params(true);
        value["acknowledge_risk"] = json!(true);
        write(home.path(), &db, &ctx, value).unwrap();
        assert!(config::effective(home.path(), Some(&db)).is_some());
    }
}
