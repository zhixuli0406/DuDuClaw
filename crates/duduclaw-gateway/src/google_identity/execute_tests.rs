//! Actual signed route tests, using the same mock issuer/protection fixture as
//! person resolution. No Google bearer may enter a dedicated MCP response.
use super::*;
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request},
};
use duduclaw_core::google_identity::{CredentialKind, CredentialRef};
use sha2::Digest;
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

const INTERNAL: &str = "ddc_dev_11111111111111111111111111111111";
const DEDICATED: &str = "ddc_dev_22222222222222222222222222222222";

fn ready(agent: &str, subject: &str, requesters: &str) -> Home {
    let home = Home::new();
    home.ready(agent, subject, requesters);
    let mut config = std::fs::read_to_string(home.p().join("config.toml")).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    config.push_str(&format!(
        "\n[mcp_keys.{INTERNAL}]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{now}'\n\n[mcp_keys.{DEDICATED}]\nclient_id='{agent}'\nscopes=['google:read']\nis_external=true\ncreated_at='{now}'\n",
    ));
    std::fs::write(home.p().join("config.toml"), config).unwrap();
    duduclaw_core::ensure_identity_key(home.p()).unwrap();
    home
}
fn request(tool: &str) -> Value {
    json!({"tool":tool,"arguments":{"query":"fixture-native-execute"},"credential_ref":CredentialRef {
        kind:CredentialKind::Legacy, sha256:format!("{:x}",sha2::Sha256::digest(DEDICATED.as_bytes())),
    }})
}
fn router(home: &Home) -> Router {
    execute::router(Arc::new(execute::ExecuteState::new(
        home.p().to_path_buf(),
        None,
    )))
}
fn signed(home: &Home, agent: &str, bytes: &[u8], key: &str) -> HeaderMap {
    use crate::computer_use_sessions::auth;
    static NONCE: AtomicUsize = AtomicUsize::new(1);
    let nonce = format!("{:032x}", NONCE.fetch_add(1, Ordering::Relaxed));
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let token = duduclaw_core::mint_identity_token(
        &duduclaw_core::load_identity_key(home.p()).unwrap(),
        agent,
    );
    let signature = duduclaw_core::internal_request_signature(
        key.as_bytes(),
        agent,
        &token,
        &timestamp,
        &nonce,
        bytes,
    );
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (auth::AGENT_ID_HEADER, agent),
        (auth::TIMESTAMP_HEADER, &timestamp),
        (auth::NONCE_HEADER, &nonce),
        (auth::SIGNATURE_HEADER, &signature),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    headers
}
async fn post(app: Router, peer: &str, headers: HeaderMap, bytes: Vec<u8>) -> (u16, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(execute::ROUTE)
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .unwrap();
    req.headers_mut().extend(headers);
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
    let response = app.oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), execute::MAX_RESPONSE)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn call(app: Router, home: &Home, value: Value) -> (u16, Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    post(
        app,
        "127.0.0.1:5678",
        signed(home, "execute-bot", &bytes, INTERNAL),
        bytes,
    )
    .await
}

#[tokio::test]
async fn credential_wait_rechecks_original_authority_before_any_service_read() {
    for mutation in [
        "requesters",
        "none",
        "master",
        "domain",
        "person",
        "sealed-key",
        "home-sa",
        "home-oauth",
        "home-oauth-unchanged",
    ] {
        let subject = format!("execute-await-{mutation}@c.test");
        let home = ready("execute-bot", &subject, "requesters='anyone'");
        let users = Arc::new(duduclaw_auth::db::UserDb::new(&home.p().join("users.db")).unwrap());
        let person = users
            .create_user(
                &subject,
                "Synthetic unit person",
                "unit-password-only",
                duduclaw_auth::UserRole::Employee,
            )
            .unwrap();
        if mutation.starts_with("home-") {
            home.agent("execute-bot", "[google]\nidentity='home'\n");
            if mutation == "home-sa" {
                std::fs::write(
                    home.p().join("unit-home-sa.json"),
                    key_json(&format!("{}/token", mock())),
                )
                .unwrap();
                let raw = std::fs::read_to_string(home.p().join("config.toml")).unwrap();
                std::fs::write(home.p().join("config.toml"), format!("{raw}\n[integrations.google_service_account]\nkey_file='unit-home-sa.json'\nsubject='{subject}'\n")).unwrap();
            } else {
                std::fs::write(home.p().join(crate::mcp_oauth::TOKEN_FILE), serde_json::to_vec(&json!([{
                    "provider_id":"google", "access_token":"unit-expired", "refresh_token":format!("unit-refresh:{subject}"),
                    "expires_at":"2020-01-01T00:00:00Z", "scopes":["unit-fixture"],
                }])).unwrap()).unwrap();
                std::fs::write(home.p().join("mcp-oauth-configs.json"), serde_json::to_vec(&json!([{
                    "provider_id":"google", "client_id":"unit-client", "client_secret":"unit-secret-only",
                    "auth_url":format!("{}/authorize", mock()), "token_url":format!("{}/token", mock()),
                    "redirect_uri":"http://127.0.0.1/unit", "scopes":["unit-fixture"],
                }])).unwrap()).unwrap();
            }
        }
        let (entered, release) = pause_mint(&subject);
        let app = execute::router(Arc::new(execute::ExecuteState::new(
            home.p().to_path_buf(),
            Some(users.clone()),
        )));
        let response = call(app, &home, request("gmail_search"));
        tokio::pin!(response);
        tokio::select! {
            _ = entered.notified() => {},
            result = &mut response => panic!("{mutation}: route returned before the credential wait: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => panic!("{mutation}: no credential wait observed"),
        }
        match mutation {
            "requesters" => home.agent(
                "execute-bot",
                &format!("[google]\nidentity='delegate:{subject}'\nrequesters='owner_only'\n"),
            ),
            "none" => home.agent("execute-bot", "[google]\nidentity='none'\n"),
            "person" => users
                .set_user_status(&person.id, duduclaw_auth::UserStatus::Suspended)
                .unwrap(),
            "sealed-key" => std::fs::remove_file(delegation_key::path(home.p())).unwrap(),
            "home-oauth-unchanged" => {}
            _ => {
                let mut doc: toml::Value = std::fs::read_to_string(home.p().join("config.toml"))
                    .unwrap()
                    .parse()
                    .unwrap();
                if mutation == "domain" {
                    doc["integrations"]["google_identity"]["allowed_subject_domains"] =
                        toml::Value::Array(vec![toml::Value::String("other.test".into())]);
                } else {
                    doc["integrations"]["google_workspace"] = toml::Value::Boolean(false);
                }
                std::fs::write(home.p().join("config.toml"), toml::to_string(&doc).unwrap())
                    .unwrap();
            }
        }
        release.notify_one();
        let (status, value) = tokio::time::timeout(std::time::Duration::from_secs(15), response)
            .await
            .unwrap();
        if mutation == "home-oauth-unchanged" {
            assert_eq!(status, 200, "normal refresh must keep working: {value}");
            assert_ne!(value["result"]["isError"], true);
            assert!(
                service_reads()
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(account, _)| account == &subject)
            );
        } else {
            assert_eq!(status, 403, "{mutation}: {value}");
            let expected = match mutation {
                "none" | "master" | "home-sa" | "home-oauth" => "disabled",
                "sealed-key" => "delegate_unavailable",
                _ => "identity_changed",
            };
            assert_eq!(value["code"], expected, "{mutation}: {value}");
            assert!(
                !service_reads()
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(account, _)| account == &subject),
                "{mutation}: service read after revocation"
            );
        }
    }
}

#[tokio::test]
async fn unknown_requester_defaults_deny_then_explicit_anyone_executes_without_returning_bearer() {
    let home = ready("execute-bot", "execute-positive@c.test", "fallback='home'");
    let (status, value) = call(router(&home), &home, request("gmail_search")).await;
    assert_eq!(status, 403, "{value}");
    assert_eq!(value["code"], "requester_refused");
    assert!(seen_for("execute-positive@c.test").is_empty());
    home.agent(
        "execute-bot",
        "[google]\nidentity='delegate:execute-positive@c.test'\nrequesters='anyone'\n",
    );
    let (status, value) = call(router(&home), &home, request("gmail_search")).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["ok"], true);
    assert_ne!(value["result"]["isError"], true);
    assert!(!value.to_string().contains("tok:execute-positive@c.test"));
    assert!(
        service_reads()
            .lock()
            .unwrap()
            .iter()
            .any(|(account, path)| account == "execute-positive@c.test"
                && path.contains("/messages")
                && path.contains("fixture-native-execute"))
    );
    let (status, value) = call(router(&home), &home, request("gmail_create_draft")).await;
    assert_eq!(status, 403, "{value}");
    assert_eq!(value["code"], "credential_refused");
    let audit = std::fs::read_to_string(home.p().join("tool_calls.jsonl")).unwrap();
    assert!(audit.contains("google_execute"));
    assert!(!audit.contains("tok:execute-positive@c.test"));
    assert!(!audit.contains(DEDICATED));
}

#[tokio::test]
async fn raw_external_key_and_digest_cannot_replace_host_proof_or_choose_another_employee() {
    let home = ready("execute-bot", "execute-auth@c.test", "requesters='anyone'");
    let app = router(&home);
    let bytes = serde_json::to_vec(&request("google_status")).unwrap();
    let (status, _) = post(
        app.clone(),
        "127.0.0.1:5678",
        HeaderMap::new(),
        bytes.clone(),
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = post(
        app.clone(),
        "127.0.0.1:5678",
        signed(&home, "execute-bot", &bytes, DEDICATED),
        bytes.clone(),
    )
    .await;
    assert_eq!(status, 401);
    let headers = signed(&home, "execute-bot", &bytes, INTERNAL);
    let (status, _) = post(app.clone(), "192.0.2.2:5678", headers, bytes.clone()).await;
    assert_eq!(status, 401);
    let (status, value) = post(
        app.clone(),
        "127.0.0.1:5678",
        signed(&home, "other-bot", &bytes, INTERNAL),
        bytes.clone(),
    )
    .await;
    assert_eq!(status, 403, "{value}");
    let headers = signed(&home, "execute-bot", &bytes, INTERNAL);
    let (status, value) = post(
        app.clone(),
        "127.0.0.1:5678",
        headers.clone(),
        bytes.clone(),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert!(!value.to_string().contains("tok:execute-auth@c.test"));
    let (status, _) = post(app, "127.0.0.1:5678", headers, bytes).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn closed_request_refuses_authority_fields_unknown_tools_oversize_and_live_removal() {
    let home = ready(
        "execute-bot",
        "execute-closed@c.test",
        "requesters='anyone'",
    );
    for field in [
        "principal",
        "requester",
        "user_id",
        "turn_id",
        "run_id",
        "scopes",
        "access_token",
    ] {
        let mut value = request("google_status");
        value[field] = json!("forged");
        assert_eq!(call(router(&home), &home, value).await.0, 400, "{field}");
    }
    assert_eq!(
        call(router(&home), &home, request("mail_poll")).await.0,
        400
    );
    let mut value = request("gmail_search");
    value["arguments"] = json!([]);
    assert_eq!(call(router(&home), &home, value).await.0, 400);
    let bytes = vec![b' '; execute::MAX_BODY + 1];
    assert_eq!(
        post(
            router(&home),
            "127.0.0.1:5678",
            signed(&home, "execute-bot", &bytes, INTERNAL),
            bytes
        )
        .await
        .0,
        413
    );
    let config = std::fs::read_to_string(home.p().join("config.toml")).unwrap();
    let mut doc: toml::Value = config.parse().unwrap();
    doc["mcp_keys"].as_table_mut().unwrap().remove(DEDICATED);
    std::fs::write(home.p().join("config.toml"), toml::to_string(&doc).unwrap()).unwrap();
    let (status, value) = call(router(&home), &home, request("google_status")).await;
    assert_eq!(status, 403, "{value}");
    assert_eq!(value["code"], "credential_refused");
}

#[tokio::test]
async fn authenticated_requests_share_the_routes_rate_limit_even_when_metadata_is_refused() {
    let home = ready("execute-bot", "execute-rate@c.test", "requesters='anyone'");
    let app = router(&home);
    let mut value = request("google_status");
    value["credential_ref"]["sha256"] = json!("a".repeat(64));
    for _ in 0..120 {
        assert_eq!(call(app.clone(), &home, value.clone()).await.0, 403);
    }
    assert_eq!(call(app, &home, value).await.0, 429);
}

#[tokio::test]
async fn dedicated_writes_never_retry_transient_service_responses() {
    let mut observed = Vec::new();
    for code in [503u16, 429] {
        let subject = format!("execute-write-{code}@c.test");
        let home = ready("execute-bot", &subject, "requesters='anyone'");
        let raw = std::fs::read_to_string(home.p().join("config.toml")).unwrap();
        std::fs::write(
            home.p().join("config.toml"),
            raw.replace(
                "scopes=['google:read']",
                "scopes=['google:read','google:write']",
            ),
        )
        .unwrap();
        let mut req = request("docs_append");
        req["arguments"] =
            json!({"document_id":format!("unit-write-{code}"),"text":"synthetic unit write"});
        let (status, value) = call(router(&home), &home, req).await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(value["result"]["isError"], true, "{value}");
        let count = service_reads()
            .lock()
            .unwrap()
            .iter()
            .filter(|(account, path)| account == &subject && path.ends_with(":batchUpdate"))
            .count();
        observed.push((code, count));
    }
    assert_eq!(
        observed,
        vec![(503, 1), (429, 1)],
        "dedicated writes must not repeat"
    );
}
