use super::*;

const DASHBOARD_SESSION_ID: &str = "18600000-0000-4000-8000-000000000001";

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
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), DASHBOARD_SESSION_ID.into())),
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
    let audit = f.home.path().join("security_audit.jsonl");
    // set_user_db may already have emitted an edition-watcher audit record.
    match std::fs::remove_file(&audit) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => panic!("remove fixture audit: {error}"),
    }
    std::fs::create_dir(&audit).unwrap();
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"permission_mode":"follow_odoo_account","acknowledge_risk":true})).await);
    assert_eq!(raw(&f), initial);
    std::fs::remove_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    let set = payload(rpc(&f, &f.admin, "odoo.access_set", json!({"dismiss_upgrade_notice":true})).await);
    assert_eq!(set["upgrade_notice_pending"], false);
    assert!(!OdooAccessConfig::parse_config_toml(&raw(&f)).unwrap().upgrade_notice_pending);
}


const RAW_ODOO_REFUSAL: &str = "這項 Odoo 權限設定只能在 Odoo 設定頁確認風險後變更。";

fn access_text(value: Value) -> String {
    let table: toml::Table = serde_json::from_value(json!({"odoo_access":value})).unwrap();
    toml::to_string(&table).unwrap()
}

fn widening_cases() -> Vec<(String, String)> {
    let defaults = json!(DEFAULT_ALWAYS_REFUSE);
    let reduced: Vec<_> = DEFAULT_ALWAYS_REFUSE.iter().copied().filter(|r| *r != "unlink:*").collect();
    vec![
        (String::new(), access_text(json!({"permission_mode":"follow_odoo_account"}))),
        (String::new(), access_text(json!({"permission_mode_ack":{"by":"forged","at":"2026-11-27T00:00:00Z"}}))),
        (String::new(), access_text(json!({"always_refuse":reduced}))),
        (access_text(json!({"always_refuse":["write:crm.lead"]})), String::new()),
        (access_text(json!({"always_refuse":defaults})), access_text(json!({"always_refuse":[]}))),
        (String::new(), "[odoo]\nmember_credentials=true\n".into()),
        ("[odoo]\nmember_credentials=false\n".into(), "[odoo]\nmember_credentials=true\n".into()),
    ]
}

fn narrowing_cases() -> Vec<(String, String)> {
    let follow = json!({"permission_mode":"follow_odoo_account",
        "permission_mode_ack":{"by":"admin","at":"2026-11-27T00:00:00Z"}});
    let mut added: Vec<_> = DEFAULT_ALWAYS_REFUSE.to_vec();
    added.push("write:crm.lead");
    vec![
        (access_text(follow.clone()), access_text(json!({"permission_mode":"engine"}))),
        (access_text(follow.clone()), access_text(json!({"permission_mode":"follow_odoo_account"}))),
        (access_text(follow.clone()), access_text(follow)),
        (String::new(), access_text(json!({"always_refuse":added}))),
        (access_text(json!({"always_refuse":DEFAULT_ALWAYS_REFUSE})), String::new()),
        (String::new(), access_text(json!({"company_key_agents":["a"],"company_key_agents_migrated":"done",
            "upgrade_notice_pending":true,"attachment_max_mb":25}))),
        (access_text(json!({"company_key_agents":["a"]})), access_text(json!({"company_key_agents":[]}))),
        ("[odoo]\nmember_credentials=true\n".into(), "[odoo]\nmember_credentials=false\n".into()),
        ("[odoo]\nmember_credentials=true\n".into(), "[odoo]\nmember_credentials=true\n".into()),
    ]
}

#[test]
fn odoo_guard_compares_effective_refusals_and_raw_acknowledgements() {
    use super::super::config_commit::refuse_odoo_widening_text_edit as guard;
    for (before, after) in widening_cases() {
        assert_eq!(guard(&before, &after).unwrap_err(), RAW_ODOO_REFUSAL, "{before} -> {after}");
    }
    for (before, after) in narrowing_cases() {
        assert!(guard(&before, &after).is_ok(), "{before} -> {after}");
    }
    let before = access_text(json!({"permission_mode":"follow_odoo_account",
        "permission_mode_ack":{"by":"admin","at":"2026-11-27T00:00:00Z"}}));
    let after = before.replace("admin", "forged");
    assert!(guard(&before, &after).is_err());
    for invalid in ["broken [", "odoo_access=[]", "[odoo_access]\nalways_refuse=1", "odoo=[]"] {
        assert!(guard("", invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn raw_editor_and_shared_commit_refuse_odoo_widening() {
    let f = fixture().await;
    let path = f.home.path().join("config.toml");
    for (before, after) in widening_cases() {
        std::fs::write(&path, &before).unwrap();
        let hash = super::super::config_commit::content_hash(&before);
        let table: toml::Table = after.parse().unwrap();
        let error = super::super::config_commit::commit_table_locked(&path, hash.clone(), &table).await.unwrap_err();
        assert!(error.contains(RAW_ODOO_REFUSAL), "{error}");
        let frame = rpc(&f, &f.admin, "config.raw.set", json!({"file":"config","content":after,"base_hash":hash})).await;
        match frame {
            WsFrame::Response { ok:false, error:Some(error), .. } => assert_eq!(error.as_str(), Some(RAW_ODOO_REFUSAL)),
            other => panic!("expected Odoo refusal: {other:?}"),
        }
        assert_eq!(raw(&f), before);
    }
}

#[tokio::test]
async fn raw_editor_allows_odoo_narrowing_and_admin_bookkeeping() {
    let f = fixture().await;
    let path = f.home.path().join("config.toml");
    for (before, after) in narrowing_cases() {
        std::fs::write(&path, &before).unwrap();
        payload(rpc(&f, &f.admin, "config.raw.set", json!({"file":"config","content":after,
            "base_hash":super::super::config_commit::content_hash(&before)})).await);
        assert_eq!(raw(&f).parse::<toml::Table>().unwrap(), after.parse::<toml::Table>().unwrap());
    }
}

#[tokio::test]
async fn generic_settings_refuse_nested_and_flat_odoo_widening() {
    let f = fixture().await;
    let reduced: Vec<_> = DEFAULT_ALWAYS_REFUSE.iter().copied().filter(|r| *r != "unlink:*").collect();
    for method in ["system.update_config", "google.integration.set"] {
        for edit in [
            json!({"odoo_access":{"permission_mode":"follow_odoo_account"}}),
            json!({"odoo_access.permission_mode":"follow_odoo_account"}),
            json!({"odoo_access":{"permission_mode_ack":{"by":"forged","at":"2026-11-27T00:00:00Z"}}}),
            json!({"odoo_access.permission_mode_ack.by":"forged"}),
            json!({"odoo_access":{"always_refuse":reduced}}),
            json!({"odoo_access.always_refuse":[]}),
            json!({"odoo":{"member_credentials":true}}),
            json!({"odoo.member_credentials":true}),
        ] {
            let mut params = json!({"log_level":"debug","enabled":true});
            params.as_object_mut().unwrap().extend(edit.as_object().unwrap().clone());
            let before = raw(&f);
            let frame = rpc(&f, &f.admin, method, params).await;
            match frame {
                WsFrame::Response { ok:false, error:Some(error), .. } => assert_eq!(error.as_str(), Some(RAW_ODOO_REFUSAL)),
                other => panic!("expected Odoo refusal: {other:?}"),
            }
            assert_eq!(raw(&f), before);
        }
    }
    // Removing a custom refusal key must not restore weaker defaults.
    std::fs::write(f.home.path().join("config.toml"), access_text(json!({"always_refuse":["write:crm.lead"]}))).unwrap();
    refused(rpc(&f, &f.admin, "system.update_config", json!({"log_level":"debug","odoo_access.always_refuse":null})).await);
}

#[tokio::test]
async fn generic_settings_allow_odoo_narrowing_params_without_writing_unsupported_keys() {
    let f = fixture().await;
    let mut added = DEFAULT_ALWAYS_REFUSE.to_vec();
    added.push("write:crm.lead");
    for params in [
        json!({"odoo_access":{"permission_mode":"engine"}}),
        json!({"odoo_access.permission_mode_ack":null}),
        json!({"odoo_access.always_refuse":added}),
        json!({"odoo_access.company_key_agents":["a"]}),
        json!({"odoo.member_credentials":false}),
    ] {
        let mut params = params;
        params["log_level"] = json!("debug");
        payload(rpc(&f, &f.admin, "system.update_config", params).await);
    }
}

#[test]
fn dedicated_odoo_commit_cannot_change_unrelated_fields() {
    use super::super::config_commit::{commit_odoo_table, content_hash, OdooWrite};
    let dir = tempfile::tempdir().unwrap();
    let before = "[general]\nlog_level='info'\n";
    std::fs::write(dir.path().join("config.toml"), before).unwrap();
    for scope in [OdooWrite::Access, OdooWrite::MemberCredentials] {
        let after: toml::Table = "[general]\nlog_level='debug'\n".parse().unwrap();
        assert!(commit_odoo_table(dir.path(), &content_hash(before), &after, scope, || Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), before);
    }
}
