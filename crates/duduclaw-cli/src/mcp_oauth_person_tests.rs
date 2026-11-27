use super::*;

#[tokio::test]
async fn member_key_consent_is_refused_without_storing_code_or_grant() {
    let home = tempfile::tempdir().unwrap();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("holder@test.invalid", "Holder", "test-password",
        duduclaw_auth::UserRole::Employee).unwrap();
    let key = db.member_key_create(&user.id, "MCP", 30, user.credential_epoch,
        false, &["tasks:read".into()]).unwrap();
    let resource = "https://example.test/mcp";
    let client = RegisteredClient {
        client_name: "Person app".into(), redirect_uris: vec!["https://example.test/cb".into()],
        created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
    };
    update_store(home.path(), |store| {
        store.clients.insert("person-app".into(), client.clone());
    }).unwrap();
    let before = std::fs::read(store_path(home.path())).unwrap();
    let form = DecisionForm {
        client_id: "person-app".into(), redirect_uri: "https://example.test/cb".into(),
        state: "consent-state".into(), code_challenge: "a".repeat(43),
        scope: "tasks:read".into(), operator_key: key.secret.clone(),
        action: "approve".into(), resource: Some(resource.into()),
    };
    let response = decision_response(home.path(), client, form, resource);
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get("location").is_none());
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("Personal keys cannot be used to authorize OAuth connectors yet; use the personal key directly as a bearer token."));
    assert!(!body.contains(&key.secret));
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before);
    let store = load_store(home.path()).unwrap();
    assert!(store.codes.is_empty() && store.grants.is_empty());
    assert!(store.tokens.is_empty() && store.refresh.is_empty());
}
