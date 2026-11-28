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
        other => panic!("expected success, got {other:?}"),
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
    assert_eq!(get["readable"], true);
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
    assert_eq!(payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await),
        json!({"readable":false,"message":UNREADABLE}));
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({"attachment_max_mb":10})).await);
}

#[tokio::test]
async fn unreadable_access_get_reports_recovery_and_set_preserves_config() {
    let f = fixture().await;
    for text in ["[odoo_access", "odoo_access='invalid'\n", "[odoo_access]\npermission_mode='invalid'\n"] {
        std::fs::write(f.home.path().join("config.toml"), text).unwrap();
        assert_eq!(payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await),
            json!({"readable":false,"message":UNREADABLE}));
        refused(rpc(&f, &f.member, "odoo.access_get", json!({})).await);
        refused(rpc(&f, &f.admin, "odoo.access_get", json!({"unknown":true})).await);
        refused(rpc(&f, &f.admin, "odoo.access_set", json!({"attachment_max_mb":10})).await);
        assert_eq!(raw(&f), text);
    }
    std::fs::remove_file(f.home.path().join("config.toml")).unwrap();
    assert_eq!(payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await),
        json!({"readable":false,"message":UNREADABLE}));
    std::fs::create_dir(f.home.path().join("config.toml")).unwrap();
    assert_eq!(payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await),
        json!({"readable":false,"message":UNREADABLE}));
    refused(rpc(&f, &f.admin, "odoo.access_set", json!({})).await);
    assert!(f.home.path().join("config.toml").is_dir());
}

#[tokio::test]
async fn follow_mode_impact_counts_active_avatars_and_literal_requester_flags() {
    let f = fixture().await;
    let conn = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    conn.execute("UPDATE users SET avatar_agent_id=?2 WHERE id=?1", rusqlite::params![f.admin.user_id, "a"]).unwrap();
    conn.execute("UPDATE users SET avatar_agent_id=?2 WHERE id=?1", rusqlite::params![f.member.user_id, "b"]).unwrap();
    for (email, status, avatar) in [
        ("suspended@test.invalid", "suspended", Some("suspended-avatar")),
        ("no-avatar@test.invalid", "active", None),
    ] {
        let user = f.db.create_user(email, "Excluded", "password", UserRole::Employee).unwrap();
        conn.execute("UPDATE users SET status=?2,avatar_agent_id=?3 WHERE id=?1",
            rusqlite::params![user.id, status, avatar]).unwrap();
        conn.execute("INSERT INTO member_odoo_credentials
            (user_id,login,odoo_uid,odoo_name,secret_ct,server_fp,rev,state,verified_at,updated_at)
            VALUES(?1,'login',1,'Name',X'00','server',1,'needs_reverify','now','now')", [&user.id]).unwrap();
    }
    // A stored row counts even when the credential needs re-verification.
    conn.execute("INSERT INTO member_odoo_credentials
        (user_id,login,odoo_uid,odoo_name,secret_ct,server_fp,rev,state,verified_at,updated_at)
        VALUES(?1,'login',1,'Name',X'00','server',1,'needs_reverify','now','now')", [&f.admin.user_id]).unwrap();
    std::fs::write(f.home.path().join("agents/a/agent.toml"), "[agent]\ndisplay_name='Agent A'\n[odoo]\nact_as_requester=true\n").unwrap();
    std::fs::create_dir_all(f.home.path().join("agents/b")).unwrap();
    std::fs::write(f.home.path().join("agents/b/agent.toml"), "[odoo]\nact_as_requester='true'\n").unwrap();
    std::fs::create_dir_all(f.home.path().join("agents/eph-ignored")).unwrap();
    std::fs::write(f.home.path().join("agents/eph-ignored/agent.toml"), "[odoo]\nact_as_requester=true\n").unwrap();
    for method in ["odoo.access_get", "odoo.access_set"] {
        let result = payload(rpc(&f, &f.admin, method, json!({})).await);
        assert_eq!(result["readable"], true);
        assert_eq!(result["follow_mode_impact"], json!({
            "avatars_with_personal_credentials":1,"agents_acting_as_requester":1,
        }));
        assert_eq!(result["employees"].as_array().unwrap().len(), 2);
    }
    // Reporting a malformed flag must not relax the migration's strict parser.
    assert!(employees(f.home.path()).is_err());
}

#[tokio::test]
async fn follow_mode_impact_read_errors_refuse_without_writing() {
    let f = fixture().await;
    std::fs::write(f.home.path().join("agents/a/agent.toml"), "[odoo").unwrap();
    refused(rpc(&f, &f.admin, "odoo.access_get", json!({})).await);
    assert_eq!(raw(&f), "# retained\n");
    std::fs::write(f.home.path().join("agents/a/agent.toml"), "[agent]\nname='A'\n").unwrap();
    let conn = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    conn.execute("DROP TABLE member_odoo_credentials", []).unwrap();
    for method in ["odoo.access_get", "odoo.access_set"] {
        refused(rpc(&f, &f.admin, method, json!({})).await);
        assert_eq!(raw(&f), "# retained\n");
    }
    // Invalid config is still reportable even when counts cannot be read.
    std::fs::write(f.home.path().join("config.toml"), "odoo_access=false").unwrap();
    assert_eq!(payload(rpc(&f, &f.admin, "odoo.access_get", json!({})).await),
        json!({"readable":false,"message":UNREADABLE}));
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

// Per-employee access uses the same live browser authority as company policy.
const AGENT_ACCESS_INITIAL: &str = "# 員工設定\n[agent]\nname = 'access-ai'\ndisplay_name = 'Access AI'\n\n[odoo] # connection stays untouched\nurl = 'https://example.invalid'\ndb = 'demo'\napi_key_enc = 'opaque-secret' # retain bytes\nact_as_requester = false # identity\npermission_override = 'read_only' # restriction\n\n[model]\npreferred = 'keep-me'\n";

async fn agent_fixture() -> Fixture {
    let mut f = fixture().await;
    f.admin.origin = duduclaw_auth::acl::ContextOrigin::DashboardSession;
    f.member.origin = duduclaw_auth::acl::ContextOrigin::DashboardSession;
    payload(f.handler.handle_agents_create_as(json!({"name":"access-ai"}), Some(&f.admin)).await);
    // Retain the scaffold's required fields: a successful write rescans the
    // registry, which drops configs that cannot deserialize as AgentConfig.
    let scaffold: toml::Table = agent_raw(&f).parse().unwrap();
    let mut table: toml::Table = AGENT_ACCESS_INITIAL.parse().unwrap();
    for (key, value) in scaffold {
        if let (Some(current), Some(defaults)) = (table.get_mut(&key).and_then(toml::Value::as_table_mut), value.as_table()) {
            for (field, default) in defaults {
                current.entry(field.clone()).or_insert_with(|| default.clone());
            }
        } else {
            table.entry(key).or_insert(value);
        }
    }
    let initial = super::super::config_commit::render_preserving(AGENT_ACCESS_INITIAL, &table).unwrap();
    toml::from_str::<duduclaw_core::types::AgentConfig>(&initial).expect("valid agent fixture");
    std::fs::write(agent_path(&f), initial).unwrap();
    f.handler.registry.write().await.scan().await.unwrap();
    assert!(f.handler.registry.read().await.get("access-ai").is_some());
    f
}
fn agent_path(f: &Fixture) -> PathBuf { f.home.path().join("agents/access-ai/agent.toml") }
fn agent_raw(f: &Fixture) -> String { std::fs::read_to_string(agent_path(f)).unwrap() }
async fn agent_set(f: &Fixture, mut params: Value) -> WsFrame {
    params["agent_id"] = json!("access-ai");
    rpc(f, &f.admin, "odoo.agent_access_set", params).await
}

#[tokio::test]
async fn agent_access_ack_widens_and_preserves_other_bytes() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let set = payload(agent_set(&f, json!({"act_as_requester":true,"permission_override":"engine","acknowledge_risk":true})).await);
    assert!(f.handler.registry.read().await.get("access-ai").is_some(), "successful save must retain the employee in the registry");
    assert_eq!(set["act_as_requester"], true);
    assert_eq!(set["permission_override"], "engine");
    assert_eq!(agent_raw(&f), initial
        .replace("act_as_requester = false", "act_as_requester = true")
        .replace("'read_only'", "\"engine\""));
    payload(agent_set(&f, json!({"permission_override":"inherit","acknowledge_risk":true})).await);
    let get = payload(rpc(&f, &f.admin, "odoo.agent_config_get", json!({"agent_id":"access-ai"})).await);
    assert_eq!(get["act_as_requester"], true);
    assert_eq!(get["permission_override"], "inherit");
    let events: Vec<Value> = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap()
        .lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event_type"] == "odoo_agent_access_changed").collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["details"]["actor_user_id"], f.admin.user_id);
    assert_eq!(events[0]["details"]["agent_id"], "access-ai");
    assert_eq!(events[0]["details"]["before"], json!({"act_as_requester":false,"permission_override":"read_only"}));
    assert_eq!(events[0]["details"]["after"], json!({"act_as_requester":true,"permission_override":"engine"}));
    assert_eq!(events[0]["details"]["acknowledged"], true);
    assert_eq!(events[0]["details"]["phase"], "before_commit");
}

#[tokio::test]
async fn agent_access_without_ack_only_narrows() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    for params in [json!({"act_as_requester":true}), json!({"permission_override":"engine"}), json!({"permission_override":"inherit"})] {
        refused(agent_set(&f, params).await);
        assert_eq!(agent_raw(&f), initial);
    }
    payload(agent_set(&f, json!({"act_as_requester":true,"permission_override":"inherit","acknowledge_risk":true})).await);
    payload(agent_set(&f, json!({"act_as_requester":false,"permission_override":"engine"})).await);
    payload(agent_set(&f, json!({"permission_override":"read_only"})).await);
}

#[tokio::test]
async fn agent_access_refuses_owners_keys_ai_and_stale_identity() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let params = json!({"agent_id":"access-ai","act_as_requester":true,"acknowledge_risk":true});
    f.db.bind_agent(&f.member.user_id, "access-ai", AccessLevel::Owner).unwrap();
    refused(rpc(&f, &f.member, "odoo.agent_access_set", params.clone()).await);
    let mut key = f.admin.clone();
    key.member_key_id = Some("member-key".into());
    refused(rpc(&f, &key, "odoo.agent_access_set", params.clone()).await);
    for origin in [duduclaw_auth::acl::ContextOrigin::Other, duduclaw_auth::acl::ContextOrigin::AdminToken] {
        let mut ai = f.admin.clone();
        ai.origin = origin;
        refused(rpc(&f, &ai, "odoo.agent_access_set", params.clone()).await);
    }
    refused(f.handler.handle_odoo_agent_access_set(params.clone(), &f.admin).await);
    let mut stale = f.admin.clone();
    stale.credential_epoch += 1;
    refused(rpc(&f, &stale, "odoo.agent_access_set", params.clone()).await);
    f.db.update_user(&f.admin.user_id, None, Some(UserRole::Employee), None).unwrap();
    refused(rpc(&f, &f.admin, "odoo.agent_access_set", params).await);
    assert_eq!(agent_raw(&f), initial);
}

#[tokio::test]
async fn agent_access_invalid_fields_or_missing_employee_never_write() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    for params in [
        json!({"permission_override":"unknown","acknowledge_risk":true}),
        json!({"permission_override":null}), json!({"permission_override":3}),
        json!({"act_as_requester":"true"}), json!({"act_as_requester":null}),
        json!({"acknowledge_risk":"true"}), json!({"acknowledge_risk":null}),
        json!({"url":"https://other.invalid","act_as_requester":false}),
        json!({"acknowledge_risk":true}),
    ] {
        refused(agent_set(&f, params).await);
        assert_eq!(agent_raw(&f), initial);
    }
    for id in ["missing", "../access-ai", ""] {
        refused(rpc(&f, &f.admin, "odoo.agent_access_set", json!({"agent_id":id,"act_as_requester":true,"acknowledge_risk":true})).await);
    }
    std::fs::remove_file(agent_path(&f)).unwrap();
    refused(agent_set(&f, json!({"act_as_requester":true,"acknowledge_risk":true})).await);
    assert!(!agent_path(&f).exists());
}

#[tokio::test]
async fn agent_access_audit_failure_prevents_publication() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let audit = f.home.path().join("security_audit.jsonl");
    std::fs::remove_file(&audit).unwrap();
    std::fs::create_dir(&audit).unwrap();
    refused(agent_set(&f, json!({"act_as_requester":true,"acknowledge_risk":true})).await);
    assert_eq!(agent_raw(&f), initial);
}

#[tokio::test]
async fn agent_access_adapter_checks_scope_hash_and_write_ahead_order() {
    use crate::agent_files::writer::{commit_odoo_agent_access, Commit, CommitMeta, Baseline};
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let hash = super::super::config_commit::content_hash(initial);
    let make_commit = || Commit {
        home:f.home.path(), agent_id:"access-ai", key:crate::agent_files::FileKey::AgentToml,
        expected_hash:Some(&hash), meta:CommitMeta::new(&f.admin.user_id, "odoo.agent_access_set"),
        baseline:Baseline::Accept,
    };
    let mut table: toml::Table = initial.parse().unwrap();
    table["odoo"].as_table_mut().unwrap().insert("act_as_requester".into(), toml::Value::Boolean(true));
    let mut unrelated = table.clone();
    unrelated["odoo"].as_table_mut().unwrap().insert("db".into(), toml::Value::String("changed".into()));
    assert!(commit_odoo_agent_access(&f.handler.registry, make_commit(), &unrelated, |_, _| panic!("scope must fail before audit"), || Ok(())).await.is_err());
    assert_eq!(agent_raw(&f), initial);
    // The callback is the write-ahead audit boundary: disk still has old bytes.
    let outcome = commit_odoo_agent_access(&f.handler.registry, make_commit(), &table, |before, after| {
        assert_eq!(agent_raw(&f), before);
        assert_ne!(before, after);
        Err("audit unavailable".into())
    }, || Ok(())).await;
    assert!(outcome.is_err());
    assert_eq!(agent_raw(&f), initial);
    std::fs::write(agent_path(&f), format!("{initial}# concurrent edit\n")).unwrap();
    assert!(commit_odoo_agent_access(&f.handler.registry, make_commit(), &table, |_, _| panic!("CAS must fail before audit"), || Ok(())).await.is_err());
    assert!(agent_raw(&f).ends_with("# concurrent edit\n"));
}

#[tokio::test]
async fn agent_access_final_guard_rechecks_live_admin() {
    use crate::agent_files::writer::{commit_odoo_agent_access, Commit, CommitMeta, Baseline};
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let hash = super::super::config_commit::content_hash(initial);
    let mut table: toml::Table = initial.parse().unwrap();
    table["odoo"].as_table_mut().unwrap().insert("act_as_requester".into(), toml::Value::Boolean(true));
    let c = Commit { home:f.home.path(), agent_id:"access-ai", key:crate::agent_files::FileKey::AgentToml,
        expected_hash:Some(&hash), meta:CommitMeta::new(&f.admin.user_id, "odoo.agent_access_set"), baseline:Baseline::Accept };
    f.db.update_user(&f.admin.user_id, None, Some(UserRole::Employee), None).unwrap();
    assert!(commit_odoo_agent_access(&f.handler.registry, c, &table, |_, _| {
        fresh(&f.db, &f.admin, DASHBOARD_SESSION_ID).map(|_| ())
    }, || Ok(())).await.is_err());
    assert_eq!(agent_raw(&f), initial);
}

#[test]
fn agent_access_scope_rejects_semantically_equal_unrelated_reformatting() {
    use crate::agent_files::writer::require_only_odoo_agent_access_edit;
    let after = AGENT_ACCESS_INITIAL.replace("act_as_requester = false", "act_as_requester = true");
    assert!(require_only_odoo_agent_access_edit(AGENT_ACCESS_INITIAL, &after).is_ok());
    for changed in [after.replace("db = 'demo'", "db=\"demo\""), after.replace("# 員工設定", "# changed"), after.replace("[model]", "[ model ]")] {
        assert!(require_only_odoo_agent_access_edit(AGENT_ACCESS_INITIAL, &changed).is_err());
    }
}

#[tokio::test]
async fn agent_access_defaults_unknown_stored_values_and_personal_edition() {
    let f = agent_fixture().await;
    assert!(!super::super::is_enterprise_only_method("odoo.agent_access_set"));
    let mut doc: toml_edit::DocumentMut = agent_raw(&f).parse().unwrap();
    doc.remove("odoo");
    let base = doc.to_string();
    toml::from_str::<duduclaw_core::types::AgentConfig>(&base).expect("valid agent fixture without Odoo");
    std::fs::write(agent_path(&f), &base).unwrap();
    let get = payload(rpc(&f, &f.admin, "odoo.agent_config_get", json!({"agent_id":"access-ai"})).await);
    assert_eq!(get["permission_override"], "inherit");
    assert_eq!(get["act_as_requester"], false);
    payload(agent_set(&f, json!({"permission_override":"read_only"})).await);
    assert!(agent_raw(&f).starts_with(&base));
    std::fs::write(agent_path(&f), format!("{base}[odoo]\npermission_override='legacy-unknown'\n")).unwrap();
    refused(agent_set(&f, json!({"permission_override":"engine"})).await);
    payload(agent_set(&f, json!({"permission_override":"engine","acknowledge_risk":true})).await);
    for fields in ["act_as_requester='true'", "permission_override=1"] {
        let raw = format!("{base}[odoo]\n{fields}\n");
        std::fs::write(agent_path(&f), &raw).unwrap();
        refused(agent_set(&f, json!({"act_as_requester":true,"acknowledge_risk":true})).await);
        assert_eq!(agent_raw(&f), raw);
    }
}

#[tokio::test]
async fn agent_access_consent_never_spills_into_generic_writers() {
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let widened = initial.replace("act_as_requester = false", "act_as_requester = true");
    let table: toml::Table = widened.parse().unwrap();
    let hash = super::super::config_commit::content_hash(initial);
    refused(rpc(&f, &f.admin, "config.raw.set", json!({"file":"agent:access-ai","content":widened,"base_hash":hash})).await);
    assert!(super::super::config_commit::commit_table_locked(&agent_path(&f), hash.clone(), &table).await.is_err());
    // A source string is metadata, not a capability to bypass the guard.
    let c = crate::agent_files::writer::Commit {
        home:f.home.path(), agent_id:"access-ai", key:crate::agent_files::FileKey::AgentToml,
        expected_hash:Some(&hash), meta:crate::agent_files::writer::CommitMeta::new(&f.admin.user_id, "odoo.agent_access_set"),
        baseline:crate::agent_files::writer::Baseline::Accept,
    };
    assert!(crate::agent_files::writer::commit_locked(&c, |_| Ok(widened), || Ok(())).is_err());
    for (key, kind, value) in [("act_as_requester", "boolean", json!(true)), ("permission_override", "string", json!("engine"))] {
        refused(rpc(&f, &f.admin, "agents.update", json!({"agent_id":"access-ai","advanced_kv":[{
            "section":"odoo","key":key,"type":kind,"value":value}]})).await);
    }
    assert_eq!(agent_raw(&f), initial);
}

#[tokio::test]
async fn agent_access_rechecks_authority_after_audit_before_replace() {
    use crate::agent_files::writer::{commit_odoo_agent_access, Commit, CommitMeta, Baseline};
    let f = agent_fixture().await;
    let initial_text = agent_raw(&f);
    let initial = initial_text.as_str();
    let hash = super::super::config_commit::content_hash(initial);
    let table: toml::Table = initial.replace("act_as_requester = false", "act_as_requester = true").parse().unwrap();
    let audited = std::sync::atomic::AtomicBool::new(false);
    let c = Commit { home:f.home.path(), agent_id:"access-ai", key:crate::agent_files::FileKey::AgentToml,
        expected_hash:Some(&hash), meta:CommitMeta::new(&f.admin.user_id, "odoo.agent_access_set"), baseline:Baseline::Accept };
    let result = commit_odoo_agent_access(&f.handler.registry, c, &table, |_, _| {
        audited.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }, || {
        assert!(audited.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(agent_raw(&f), initial);
        Err("session revoked before publication".into())
    }).await;
    assert!(result.is_err());
    assert_eq!(agent_raw(&f), initial);
}


#[test]
fn agent_widening_guards_direct_to_employee_edit_page() {
    let message = "這位 AI 員工的 Odoo 權限只能在該 AI 員工的編輯頁確認風險後變更。";
    for (before, after) in [
        ("", "[odoo]\nact_as_requester=true\n"),
        ("[odoo]\npermission_override='read_only'\n", "[odoo]\npermission_override='engine'\n"),
        ("", "odoo=false\n"),
        ("", "[odoo]\npermission_override=false\n"),
    ] {
        let before_table = before.parse::<toml::Table>().unwrap();
        let after_table = after.parse::<toml::Table>().unwrap();
        assert_eq!(super::super::config_raw_rpc::refuse_odoo_agent_widening_edit(
            &before_table, &after_table).unwrap_err(), message);
        assert_eq!(super::super::config_commit::refuse_odoo_agent_widening_text_edit(
            before, after).unwrap_err(), message);
    }
}
