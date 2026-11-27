//! Real signed route plus the owned OAuth fixture; no live Google traffic.
use super::*;

async fn ready() -> Fixture {
    let f = Fixture::new();
    f.connect().await;
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id).unwrap();
    std::fs::write(f.home.join("agents/bot/agent.toml"), "[agent]\nname='bot'\n[google]\nidentity='owner'\nfallback='home'\n").unwrap();
    f
}

async fn request(f: &Fixture) -> (u16, Value) {
    use crate::computer_use_sessions::auth;
    use crate::google_identity::route;
    use tower::ServiceExt;
    let internal = crate::google_identity::owner_test_support::INTERNAL;
    let config = std::fs::read_to_string(f.home.join("config.toml")).unwrap();
    std::fs::write(f.home.join("config.toml"), format!("{config}\n[mcp_keys.{internal}]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n", chrono::Utc::now().to_rfc3339())).unwrap();
    duduclaw_core::ensure_identity_key(&f.home).unwrap();
    let turn = uuid::Uuid::new_v4().to_string();
    let _turn = crate::knowledge::turn_requesters::register("bot", &turn,
        RequesterIdentity::DashboardUser { user_id: f.alice.id.clone() }).unwrap();
    let body = serde_json::to_vec(&json!({"tool":"gmail_read", "turn_id":turn})).unwrap();
    let key = duduclaw_core::load_identity_key(&f.home).unwrap();
    let token = duduclaw_core::mint_identity_token(&key, "bot");
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let timestamp = authority::now().to_string();
    let signature = duduclaw_core::internal_request_signature(internal.as_bytes(), "bot", &token, &timestamp, &nonce, &body);
    let mut req = axum::http::Request::builder().method("POST").uri(route::ROUTE)
        .header("content-type", "application/json")
        .header(auth::AGENT_ID_HEADER, "bot").header(auth::TIMESTAMP_HEADER, timestamp)
        .header(auth::NONCE_HEADER, nonce).header(auth::SIGNATURE_HEADER, signature)
        .body(axum::body::Body::from(body)).unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo("127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap()));
    let state = Arc::new(route::GoogleTokenRouteState::new(f.home.clone(), Some(f.db.clone())));
    let response = route::router(state).oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn expire(f: &Fixture, mode: &str) -> vault::Record {
    let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
    let mut secret = vault::unseal(&key, &record).unwrap();
    secret.expires_at = Some(1);
    secret.refresh_token = Some(format!("scope-isolation-{mode}"));
    assert!(vault::finish_refresh(&f.home, &key, &record.metadata, &secret).unwrap());
    vault::get(&f.home, &f.alice.id).unwrap().unwrap()
}

async fn assert_isolated(f: &Fixture, before: &vault::Record) {
    let (status, response) = request(f).await;
    assert_eq!(status, 403);
    assert_eq!(response["code"], "write_scope_isolated");
    assert!(response.get("access_token").is_none());
    assert!(!response.to_string().contains("isolation-fresh-secret"));
    let current = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    assert_eq!(current.metadata.status, Status::NeedsReconnect);
    assert_eq!(current.metadata.revision, before.metadata.revision + 1);
    let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
    // The rejected refresh secret was never persisted.
    let secret = vault::unseal(&key, &current).unwrap();
    assert_ne!(secret.access_token, "isolation-fresh-secret");
    tokio::task::yield_now().await;
    let notices = crate::notify_push::person_notice_test::take(&f.home);
    assert!(notices.iter().any(|(user, text)| user == &f.alice.id && text.contains("我的連線")));
    let audit = std::fs::read_to_string(f.home.join("tool_calls.jsonl")).unwrap();
    assert!(audit.lines().filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .any(|row| row["reason"] == "write_scope_isolated" && row["success"] == false));
    assert!(!audit.contains("isolation-fresh-secret"));
    assert!(!audit.contains("access-alice"));
}

#[tokio::test]
async fn google_token_refresh_drive_isolated_marks_reconnect_and_notifies() {
    let f = ready().await;
    let before = expire(&f, "drive").await;
    assert_isolated(&f, &before).await;
}

#[tokio::test]
async fn google_token_refresh_without_scope_evidence_fails_closed() {
    let f = ready().await;
    let before = expire(&f, "missing").await;
    crate::notify_push::person_notice_test::take(&f.home);
    let (status, response) = request(&f).await;
    assert_eq!(status, 403);
    assert_eq!(response["code"], "scope_unverified");
    assert!(response.get("access_token").is_none());
    let current = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    assert_eq!(current.metadata.status, before.metadata.status);
    assert_eq!(current.metadata.revision, before.metadata.revision);
    let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
    let old_secret = vault::unseal(&key, &before).unwrap();
    let secret = vault::unseal(&key, &current).unwrap();
    assert_eq!(secret.access_token, old_secret.access_token);
    assert_eq!(secret.refresh_token, old_secret.refresh_token);
    assert_eq!(secret.expires_at, old_secret.expires_at);
    tokio::task::yield_now().await;
    assert!(crate::notify_push::person_notice_test::take(&f.home).is_empty());
    let audit = std::fs::read_to_string(f.home.join("tool_calls.jsonl")).unwrap();
    assert!(audit.lines().filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .any(|row| row["reason"] == "scope_unverified" && row["success"] == false));
    assert!(!audit.contains("isolation-fresh-secret"));
}

#[tokio::test]
async fn google_token_refresh_readonly_still_succeeds() {
    let f = ready().await;
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
    let secret = vault::unseal(&key, &old).unwrap();
    let mut scopes = old.metadata.granted_scopes.clone();
    scopes.push(oauth::DRIVE_TOOLS_SCOPE.into());
    assert!(vault::finish_refresh_with_scopes(&f.home, &key, &old.metadata, &secret, Some(&scopes)).unwrap());
    expire(&f, "readonly").await;
    let (status, response) = request(&f).await;
    assert_eq!(status, 200);
    assert_eq!(response["access_token"], "isolation-fresh-secret");
    assert_eq!(vault::get(&f.home, &f.alice.id).unwrap().unwrap().metadata.status, Status::Connected);
}

#[tokio::test]
async fn google_token_cached_drive_isolated_without_refresh_or_home_fallback() {
    let f = ready().await;
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
    let secret = vault::unseal(&key, &old).unwrap();
    let mut scopes = old.metadata.granted_scopes.clone();
    scopes.push(oauth::DRIVE_WRITE_SCOPE.into());
    assert!(vault::finish_refresh_with_scopes(&f.home, &key, &old.metadata, &secret, Some(&scopes)).unwrap());
    let before = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    assert_isolated(&f, &before).await;
}
