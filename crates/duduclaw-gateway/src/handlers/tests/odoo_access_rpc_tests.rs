use super::*;

struct Fixture {
    home: tempfile::TempDir,
    db: Arc<UserDb>,
    handler: MethodHandler,
    admin: UserContext,
    member: UserContext,
}
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "# retained\n").unwrap();
    let dir = home.path().join("agents/a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\ndisplay_name='Agent A'\n").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let admin = db.create_user("a@test.invalid", "Admin", "password", UserRole::Admin).unwrap();
    let member = db.create_user("m@test.invalid", "Member", "password", UserRole::Employee).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"odoo-access-rpc-tests-secret-32-bytes"))).await;
    Fixture { admin: db.live_user_context(&admin.id).unwrap().unwrap(),
        member: db.live_user_context(&member.id).unwrap().unwrap(), home, db, handler }
}
async fn rpc(f: &Fixture, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), "test-session".into())),
        f.handler.handle_conn(method, params, ctx,
            crate::power_local::RpcConnInfo::from_ws("127.0.0.1:12345".parse().unwrap(), false))).await
}
fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p,
        _ => panic!("expected success"),
    }
}
fn refused(frame: WsFrame) { assert!(matches!(frame, WsFrame::Response { ok: false, .. })); }
fn raw(f: &Fixture) -> String { std::fs::read_to_string(f.home.path().join("config.toml")).unwrap() }

#[tokio::test]
async fn admin_browser_only_and_live_authority() {
    let f = fixture().await;
    for method in ["odoo.access_get", "odoo.access_set"] {
        refused(rpc(&f, &f.member, method, json!({})).await);
        let mut key = f.admin.clone();
        key.member_key_id = Some("member-key".into());
        refused(rpc(&f, &key, method, json!({})).await);
        refused(f.handler.handle_odoo_access_rpc(method, json!({}), &f.admin).await);
    }
    f.db.update_user(&f.admin.user_id, None, Some(UserRole::Employee), None).unwrap();
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"attachment_max_mb":5})).await);
    assert_eq!(raw(&f), "# retained\n");
}

#[tokio::test]
async fn get_defaults_and_follow_ack_audit_then_engine_removes_ack() {
    let f = fixture().await;
    let get = payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await);
    assert_eq!(get["config"]["permission_mode"], "engine");
    assert_eq!(get["defaults"]["always_refuse"], json!(DEFAULT_ALWAYS_REFUSE));
    assert_eq!(get["fixed_refusals"].as_array().unwrap().len(), 5);
    assert_eq!(get["employees"][0], json!({"id":"a","display_name":"Agent A","has_own_account":false}));
    assert_eq!(get["upgrade_notice_pending"], false);
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"permission_mode":"follow_odoo_account"})).await);
    assert_eq!(raw(&f), "# retained\n");
    let set = payload(rpc(&f, &f.admin, "odoo.access_set", json!({"permission_mode":"follow_odoo_account","acknowledge_risk":true})).await);
    assert_eq!(set["config"]["permission_mode"], "follow_odoo_account");
    assert_eq!(set["config"]["permission_mode_ack"]["by"], f.admin.user_id);
    chrono::DateTime::parse_from_rfc3339(set["config"]["permission_mode_ack"]["at"].as_str().unwrap()).unwrap();
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    let event: Value = audit.lines().map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|e| e.to_string().contains("odoo_access_changed")).unwrap();
    assert!(event.to_string().contains(&f.admin.user_id));
    assert!(event.to_string().contains("\"acknowledged\":true"));
    payload(rpc(&f, &f.admin, "odoo.access_set", json!({"permission_mode":"engine"})).await);
    let table: toml::Table = raw(&f).parse().unwrap();
    assert!(table["odoo_access"].get("permission_mode_ack").is_none());
    assert!(raw(&f).contains("# retained"));
}

#[tokio::test]
async fn removing_refusals_and_adding_company_agents_require_ack() {
    let f = fixture().await;
    let rules: Vec<_> = DEFAULT_ALWAYS_REFUSE.iter().copied().filter(|r| *r != "unlink:*").collect();
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"always_refuse":rules})).await);
    payload(rpc(&f, &f.admin, "odoo.access_set", json!({"always_refuse":rules,"acknowledge_risk":true})).await);
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"company_key_agents":["a"]})).await);
    let set = payload(rpc(&f, &f.admin, "odoo.access_set", json!({"company_key_agents":["a","a"],"acknowledge_risk":true})).await);
    assert_eq!(set["config"]["company_key_agents"], json!(["a"]));
    payload(rpc(&f, &f.admin, "odoo.access_set", json!({"always_refuse":DEFAULT_ALWAYS_REFUSE,"company_key_agents":[]})).await);
}

#[tokio::test]
async fn invalid_inputs_and_invalid_config_fail_closed() {
    let f = fixture().await;
    for params in [
        json!({"always_refuse":["write:bad*middle"],"acknowledge_risk":true}),
        json!({"company_key_agents":["missing"],"acknowledge_risk":true}),
        json!({"company_key_agents":["../a"],"acknowledge_risk":true}),
        json!({"attachment_max_mb":0}), json!({"attachment_max_mb":26}),
        json!({"attachment_max_mb":"5"}), json!({"attachment_max_mb":null}),
        json!({"permission_mode":"typo"}), json!({"unknown":true}),
        json!({"permission_mode_ack":{"by":"forged","at":"2026-11-27T00:00:00Z"}}),
    ] {
        refused(rpc(&f, &f.admin, "odoo.access_set", params).await);
        assert_eq!(raw(&f), "# retained\n");
    }
    std::fs::write(f.home.path().join("config.toml"), "[odoo_access]\nattachment_max_mb=0\n").unwrap();
    let frame = rpc(&f, &f.admin, "odoo.access_get", json!({})).await;
    assert!(serde_json::to_string(&frame).unwrap().contains("[odoo_access]"));
    refused(frame);
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"attachment_max_mb":10})).await);
}

#[tokio::test]
async fn audit_failure_aborts_and_notice_can_be_dismissed() {
    let f = fixture().await;
    let initial = "[odoo_access]\nupgrade_notice_pending=true\n";
    std::fs::write(f.home.path().join("config.toml"), initial).unwrap();
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"permission_mode":"follow_odoo_account","acknowledge_risk":true})).await);
    assert_eq!(raw(&f), initial);
    std::fs::remove_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    let set = payload(rpc(&f, &f.admin, "odoo.access_set", json!({"dismiss_upgrade_notice":true})).await);
    assert_eq!(set["upgrade_notice_pending"], false);
    assert!(!OdooAccessConfig::parse_config_toml(&raw(&f)).unwrap().upgrade_notice_pending);
}
