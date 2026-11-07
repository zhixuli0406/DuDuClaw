use super::*;

#[tokio::test]
async fn mcp_oauth_grants_rpc_is_admin_only() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let mut caller = UserContext::admin_fallback();
    for role in [UserRole::Employee, UserRole::Manager] {
        caller.role = role;
        for method in ["mcp_oauth_grants.list", "mcp_oauth_grants.revoke"] {
            let frame = handler.handle(method, json!({"id": uuid::Uuid::new_v4().to_string()}), &caller).await;
            assert!(matches!(frame, WsFrame::Response { error: Some(_), .. }));
        }
    }
    caller.role = UserRole::Admin;
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    assert!(matches!(handler.handle("mcp_oauth_grants.list", json!({}), &caller).await,
        WsFrame::Response { error: None, .. }));
    assert!(matches!(handler.handle("mcp_oauth_grants.revoke", json!({"id": "invalid"}), &caller).await,
        WsFrame::Response { error: Some(_), .. }));
    assert!(matches!(handler.handle("mcp_oauth_grants.revoke", json!({"id": uuid::Uuid::new_v4().to_string()}), &caller).await,
        WsFrame::Response { error: None, .. }));
}

#[tokio::test]
async fn mcp_oauth_grants_rpc_fails_on_corrupt_store() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    std::fs::write(duduclaw_core::mcp_oauth_store::store_path(home.path()), "broken").unwrap();
    let caller = UserContext::admin_fallback();
    for method in ["mcp_oauth_grants.list", "mcp_oauth_grants.revoke"] {
        let frame = handler.handle(method, json!({"id": uuid::Uuid::new_v4().to_string()}), &caller).await;
        assert!(matches!(frame, WsFrame::Response { error: Some(_), .. }));
    }
}

#[tokio::test]
async fn mcp_oauth_grants_rpc_lists_and_revokes_family_without_secrets() {
    use duduclaw_core::mcp_oauth_store::{self as store, Consent, Grant, IssuedToken};
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let key = "ddc_dev_0123456789abcdef0123456789abcdef";
    std::fs::write(home.path().join("config.toml"), format!(
        "[mcp_keys.{key}]\nclient_id='operator'\nscopes=['memory:read']\nis_external=false\ncreated_at='{}'\n",
        chrono::Utc::now().to_rfc3339(),
    )).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store::update_store(home.path(), |s| {
        s.grants.insert(id.clone(), Grant {
            resource: "https://mcp.example.com/mcp".into(),            client_id: "client".into(), client_name: "Example".into(), scope: "memory:read".into(),
            issued_unix: 123, last_used_unix: Some(456),
            consent: Consent::capture(home.path(), key, "memory:read").unwrap(),
        });
        let token = IssuedToken {
            resource: "https://mcp.example.com/mcp".into(),            client_id: "client".into(), client_name: "Example".into(), scope: "memory:read".into(),
            expires_unix: chrono::Utc::now().timestamp() + 3600, family_id: id.clone(), used: false,
        };
        s.tokens.insert("access-digest".into(), token.clone());
        s.refresh.insert("refresh-digest".into(), token);
    }).unwrap();
    let caller = UserContext::admin_fallback();
    let frame = handler.handle("mcp_oauth_grants.list", json!({}), &caller).await;
    let WsFrame::Response { payload: Some(payload), error: None, .. } = frame else { panic!("list failed"); };
    assert_eq!(payload["grants"][0]["client_name"], "Example");
    assert_eq!(payload["grants"][0]["issued_unix"], 123);
    assert_eq!(payload["grants"][0]["last_used_unix"], 456);
    let rendered = payload.to_string();
    for secret in [key, "access-digest", "refresh-digest", "key_hash", "entry_hash"] {
        assert!(!rendered.contains(secret));
    }
    assert!(matches!(handler.handle("mcp_oauth_grants.revoke", json!({"id": id}), &caller).await,
        WsFrame::Response { error: None, .. }));
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    let event: Value = audit.lines().map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["event_type"] == "mcp_oauth_grant_revoke").unwrap();
    assert_eq!(event["details"]["actor"], caller.user_id);
    assert_eq!(event["details"]["family_id"], id);
    assert_eq!(event["details"]["client_name"], "Example");
    assert_eq!(event["details"]["scopes"], json!(["memory:read"]));
    assert_eq!(event["details"]["outcome"], "revoked");
    for secret in [key, "access-digest", "refresh-digest", "key_hash", "entry_hash"] { assert!(!audit.contains(secret)); }
    let stored = store::load_store(home.path()).unwrap();
    assert!(stored.tokens.is_empty() && stored.refresh.is_empty() && stored.grants.is_empty());
}

#[tokio::test]
async fn mcp_oauth_grants_rpc_reports_config_failure_and_audits_all_outcomes() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let mut caller = UserContext::admin_fallback();
    caller.user_id = "oauth-admin-test".into();
    for raw in ["[broken", "mcp_keys=7"] {
        std::fs::write(home.path().join("config.toml"), raw).unwrap();
        assert!(matches!(handler.handle("mcp_oauth_grants.list", json!({}), &caller).await,
            WsFrame::Response { error: Some(_), .. }));
    }
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    assert!(matches!(handler.handle("mcp_oauth_grants.list", json!({}), &caller).await,
        WsFrame::Response { error: Some(_), .. }));
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    assert!(matches!(handler.handle("mcp_oauth_grants.list", json!({}), &caller).await,
        WsFrame::Response { error: None, .. }));
    let id = uuid::Uuid::new_v4().to_string();
    handler.handle("mcp_oauth_grants.revoke", json!({"id": "invalid-secret"}), &caller).await;
    handler.handle("mcp_oauth_grants.revoke", json!({"id": id}), &caller).await;
    std::fs::write(duduclaw_core::mcp_oauth_store::store_path(home.path()), "broken").unwrap();
    handler.handle("mcp_oauth_grants.revoke", json!({"id": id}), &caller).await;
    caller.role = UserRole::Manager;
    handler.handle("mcp_oauth_grants.revoke", json!({"id": id}), &caller).await;
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!raw.contains("invalid-secret"));
    let events: Vec<Value> = raw.lines().map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["event_type"] == "mcp_oauth_grant_revoke").collect();
    assert_eq!(events.len(), 4);
    for (event, outcome) in events.iter().zip(["invalid_request", "not_found", "store_unavailable", "denied"]) {
        assert_eq!(event["details"]["actor"], "oauth-admin-test");
        assert_eq!(event["details"]["outcome"], outcome);
        assert!(event["timestamp"].as_str().is_some());
    }
}
