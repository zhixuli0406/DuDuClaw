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
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"member-policy-rpc-tests-secret-32-bytes"))).await;
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
async fn policy_requires_live_admin_browser_session() {
    let f = fixture().await;
    for method in ["member_keys.policy_get", "member_keys.policy_set"] {
        refused(rpc(&f, &f.member, method, json!({})).await);
        let mut key = f.admin.clone();
        key.member_key_id = Some("member-key".into());
        refused(rpc(&f, &key, method, json!({})).await);
        refused(f.handler.handle_member_keys_policy_rpc(method, json!({}), &key).await);
        refused(f.handler.handle_member_keys_policy_rpc(method, json!({}), &UserContext::admin_fallback()).await);
        refused(f.handler.handle_member_keys_policy_rpc(method, json!({}), &f.admin).await);
        let mut stale = f.admin.clone();
        stale.credential_epoch += 1;
        refused(rpc(&f, &stale, method, json!({})).await);
    }
    f.db.update_user(&f.admin.user_id, None, Some(UserRole::Employee), None).unwrap();
    refused(rpc(&f, &f.admin, "member_keys.policy_set", json!({"allow_conversation_content":true,"acknowledge_risk":true})).await);
    assert_eq!(raw(&f), "# retained\n");
}

#[tokio::test]
async fn policy_defaults_and_acknowledged_widening_work_while_disabled() {
    let f = fixture().await;
    let initial = "# retained\n[member_api_keys]\nenabled=false # keep\nrate_per_minute=321\n";
    std::fs::write(f.home.path().join("config.toml"), initial).unwrap();
    let get = payload(rpc(&f, &f.admin, "member_keys.policy_get", json!({})).await);
    assert_eq!(get, json!({"readable":true,"enabled":false,"allow_conversation_content":false,
        "allow_mcp_scopes":[],"available_mcp_scopes":PERSON_KEY_MCP_SCOPE_STRINGS,
        "risk":{"conversation":CONVERSATION_RISK,"mcp_scopes":SCOPES_RISK}}));
    for params in [json!({"allow_conversation_content":true}), json!({"allow_mcp_scopes":["tasks:read"]})] {
        refused(rpc(&f, &f.admin, "member_keys.policy_set", params).await);
        assert_eq!(raw(&f), initial);
    }
    let after = payload(rpc(&f, &f.admin, "member_keys.policy_set", json!({
        "allow_conversation_content":true,"allow_mcp_scopes":["tasks:read","odoo:read","tasks:read"],"acknowledge_risk":true
    })).await);
    assert_eq!(after["allow_conversation_content"], true);
    assert_eq!(after["allow_mcp_scopes"], json!(["odoo:read","tasks:read"]));
    assert_eq!(after["enabled"], false);
    assert_eq!(after, payload(rpc(&f, &f.admin, "member_keys.policy_get", json!({})).await));
    let stored: toml::Table = raw(&f).parse().unwrap();
    assert_eq!(stored["member_api_keys"]["rate_per_minute"].as_integer(), Some(321));
    assert!(raw(&f).contains("enabled=false # keep"));
    let events: Vec<Value> = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap()
        .lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event_type"] == "member_keys_policy_changed").collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["details"], json!({"actor_user_id":f.admin.user_id,
        "before":{"allow_conversation_content":false,"allow_mcp_scopes":[]},
        "after":{"allow_conversation_content":true,"allow_mcp_scopes":["odoo:read","tasks:read"]},
        "acknowledged":true,"phase":"before_commit"}));
    let narrowed = payload(rpc(&f, &f.admin, "member_keys.policy_set",
        json!({"allow_conversation_content":false,"allow_mcp_scopes":["tasks:read"]})).await);
    assert_eq!(narrowed["allow_conversation_content"], false);
    assert_eq!(narrowed["allow_mcp_scopes"], json!(["tasks:read"]));
    payload(rpc(&f, &f.admin, "member_keys.policy_set", json!({"allow_mcp_scopes":["tasks:read"]})).await);
}

#[tokio::test]
async fn policy_rejects_unknown_null_and_invalid_params() {
    let f = fixture().await;
    for params in [json!({"allow_mcp_scopes":["admin"]}), json!({"unknown":true}),
        json!({"allow_conversation_content":null}), json!({"allow_mcp_scopes":null}),
        json!({"acknowledge_risk":null}), json!({"enabled":true}), json!({"rate_per_minute":9}),
        json!({"allow_mcp_scopes":[1]}), json!({"allow_conversation_content":"true"}), json!(null), json!([])] {
        refused(rpc(&f, &f.admin, "member_keys.policy_set", params).await);
        assert_eq!(raw(&f), "# retained\n");
    }
    refused(rpc(&f, &f.admin, "member_keys.policy_get", json!({"unknown":true})).await);
    refused(rpc(&f, &f.admin, "member_keys.policy_get", json!(null)).await);
}

#[tokio::test]
async fn unreadable_policy_get_reports_state_and_set_fails_closed() {
    let f = fixture().await;
    let path = f.home.path().join("config.toml");
    for invalid in ["broken [", "member_api_keys=[]", "[member_api_keys]\nrate_per_minute=0",
        "[member_api_keys]\nunknown=true", "[member_api_keys]\nallow_mcp_scopes=['admin']"] {
        std::fs::write(&path, invalid).unwrap();
        assert_eq!(payload(rpc(&f, &f.admin, "member_keys.policy_get", json!({})).await),
            json!({"readable":false,"message":INVALID}));
        refused(rpc(&f, &f.admin, "member_keys.policy_set", json!({"allow_mcp_scopes":[]})).await);
        assert_eq!(raw(&f), invalid);
    }
    std::fs::remove_file(&path).unwrap();
    assert_eq!(payload(rpc(&f, &f.admin, "member_keys.policy_get", json!({})).await)["readable"], false);
    refused(rpc(&f, &f.admin, "member_keys.policy_set", json!({})).await);
    assert!(!path.exists());
    std::fs::create_dir(&path).unwrap();
    assert_eq!(payload(rpc(&f, &f.admin, "member_keys.policy_get", json!({})).await)["readable"], false);
    refused(rpc(&f, &f.admin, "member_keys.policy_set", json!({})).await);
}

#[tokio::test]
async fn policy_audit_failure_aborts_publication() {
    let f = fixture().await;
    let audit = f.home.path().join("security_audit.jsonl");
    if audit.exists() { std::fs::remove_file(&audit).unwrap(); }
    std::fs::create_dir(&audit).unwrap();
    refused(rpc(&f, &f.admin, "member_keys.policy_set",
        json!({"allow_conversation_content":true,"acknowledge_risk":true})).await);
    assert_eq!(raw(&f), "# retained\n");
}

#[tokio::test]
async fn generic_writers_refuse_member_policy_widening_but_allow_narrowing() {
    let f = fixture().await;
    let path = f.home.path().join("config.toml");
    for field in ["allow_conversation_content=true", "allow_mcp_scopes=['tasks:read']"] {
        let before = raw(&f);
        let after = format!("[member_api_keys]\n{field}\n");
        refused(rpc(&f, &f.admin, "config.raw.set", json!({"file":"config","content":after,
            "base_hash":super::super::config_commit::content_hash(&before)})).await);
        let table: toml::Table = after.parse().unwrap();
        assert!(super::super::config_commit::commit_table_locked(&path,
            super::super::config_commit::content_hash(&before), &table).await.is_err());
        assert_eq!(raw(&f), before);
    }
    for method in ["system.update_config", "google.integration.set"] {
        for edit in [json!({"member_api_keys.allow_conversation_content":true}),
            json!({"member_api_keys":{"allow_conversation_content":true}}),
            json!({"member_api_keys.allow_mcp_scopes":["tasks:read"]}),
            json!({"member_api_keys":{"allow_mcp_scopes":["tasks:read"]}})] {
            let mut params = edit;
            params["log_level"] = json!("debug");
            refused(rpc(&f, &f.admin, method, params).await);
            assert_eq!(raw(&f), "# retained\n");
        }
    }
    let before = "[member_api_keys]\nallow_conversation_content=true\nallow_mcp_scopes=['tasks:read','odoo:read']\n";
    let after = "[member_api_keys]\nallow_conversation_content=false\nallow_mcp_scopes=['tasks:read']\n";
    std::fs::write(&path, before).unwrap();
    payload(rpc(&f, &f.admin, "config.raw.set", json!({"file":"config","content":after,
        "base_hash":super::super::config_commit::content_hash(before)})).await);
    assert_eq!(raw(&f).parse::<toml::Table>().unwrap(), after.parse::<toml::Table>().unwrap());
}

#[tokio::test]
async fn member_list_includes_creation_policy() {
    let f = fixture().await;
    std::fs::write(f.home.path().join("config.toml"),
        "[member_api_keys]\nenabled=true\nallow_conversation_content=true\nallow_mcp_scopes=['tasks:read']\n").unwrap();
    let result = payload(rpc(&f, &f.member, "me.member_keys.list", json!({})).await);
    assert_eq!(result["policy"], json!({"allow_conversation_content":true,"allow_mcp_scopes":["tasks:read"]}));
}

#[test]
fn policy_matches_admin_list_edition_and_member_contract_classification() {
    for method in ["member_keys.policy_get", "member_keys.policy_set"] {
        assert_eq!(super::super::gates::is_enterprise_only_method(method),
            super::super::gates::is_enterprise_only_method("member_keys.admin_list"));
        assert!(super::super::member_contract::member_key_method(method).is_none());
        assert_eq!(super::super::personal_p1g_gate::metadata_allowlisted(method),
            super::super::personal_p1g_gate::metadata_allowlisted("member_keys.admin_list"));
    }
}

#[tokio::test]
async fn policy_narrowing_blocks_new_keys_without_changing_issued_keys() {
    let f = fixture().await;
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=true\n").unwrap();
    payload(rpc(&f, &f.admin, "member_keys.policy_set", json!({"allow_conversation_content":true,
        "allow_mcp_scopes":["tasks:read"],"acknowledge_risk":true})).await);
    let created = payload(rpc(&f, &f.member, "me.member_keys.create", json!({"name":"existing",
        "expires_in_days":30,"password":"password","include_conversations":true,"mcp_scopes":["tasks:read"]})).await);
    payload(rpc(&f, &f.admin, "member_keys.policy_set",
        json!({"allow_conversation_content":false,"allow_mcp_scopes":[]})).await);
    let listed = payload(rpc(&f, &f.member, "me.member_keys.list", json!({})).await);
    assert_eq!(listed["policy"], json!({"allow_conversation_content":false,"allow_mcp_scopes":[]}));
    assert_eq!(listed["keys"][0]["id"], created["key"]["id"]);
    assert_eq!(listed["keys"][0]["include_conversations"], true);
    assert_eq!(listed["keys"][0]["mcp_scopes"], json!(["tasks:read"]));
    for extra in [json!({"include_conversations":true}), json!({"mcp_scopes":["tasks:read"]})] {
        let mut params = json!({"name":"new","expires_in_days":30,"password":"password"});
        params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        refused(rpc(&f, &f.member, "me.member_keys.create", params).await);
    }
    assert_eq!(f.db.member_api_keys_for_user(&f.member.user_id).unwrap().len(), 1);
}
