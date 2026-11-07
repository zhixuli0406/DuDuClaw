//! End-to-end: two AI employees, each with its own Google Chat app, against a
//! local fake (JWKS, OAuth token endpoint, Chat REST API) with locally signed
//! request tokens. No real Google credential is involved.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use base64::Engine;
use serde_json::{Value, json};
use tower::ServiceExt;

const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// One RSA key per test process (generation is the slow part).
struct TestKey {
    pem: String,
    n: String,
    e: String,
}

fn test_key() -> &'static TestKey {
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(|| {
        use aws_lc_rs::encoding::AsDer;
        use aws_lc_rs::signature::KeyPair as _;
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&kp).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
        let body: Vec<String> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            body.join("\n")
        );
        let public = kp.public_key();
        TestKey {
            pem,
            n: B64URL.encode(public.modulus().big_endian_without_leading_zero()),
            e: B64URL.encode(public.exponent().big_endian_without_leading_zero()),
        }
    })
}

/// A request token as Google's Chat signer would mint it for `audience`.
pub(crate) fn chat_jwt(audience: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test-kid".into());
    let claims = json!({"iss": "chat@system.gserviceaccount.com", "aud": audience, "iat": now, "exp": now + 600});
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(test_key().pem.as_bytes()).unwrap();
    jsonwebtoken::encode(&header, &claims, &key).unwrap()
}

#[derive(Default)]
struct FakeGoogle {
    /// `(method, path, bearer, body)` of every Chat API call.
    calls: Mutex<Vec<(String, String, String, Value)>>,
    first_message_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    first_message_started: tokio::sync::Notify,
    subscription_topic: Mutex<Option<String>>,
    subscription_exactly_once: Mutex<Option<Value>>,
    jwks_requests: std::sync::atomic::AtomicUsize,
    token_scopes: Mutex<Vec<String>>,
}

async fn fake_google(
    State(f): State<Arc<FakeGoogle>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::Json<Value> {
    let path = uri.path().to_string();
    if path == "/jwks" {
        f.jwks_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let k = test_key();
        return axum::Json(
            json!({"keys": [{"kty": "RSA", "kid": "test-kid", "alg": "RS256", "n": k.n, "e": k.e}]}),
        );
    }
    if path == "/token" {
        // The access token names the service account that asked for it, so
        // the test can tell which app's credentials a send used.
        let form = String::from_utf8_lossy(&body).to_string();
        let assertion = form
            .split('&')
            .find_map(|kv| kv.strip_prefix("assertion="))
            .unwrap_or("");
        let payload = assertion.split('.').nth(1).unwrap_or("");
        let claims: Value =
            serde_json::from_slice(&B64URL.decode(payload).unwrap_or_default()).unwrap_or_default();
        f.token_scopes.lock().unwrap().push(claims["scope"].as_str().unwrap_or("").to_string());
        let iss = claims["iss"].as_str().unwrap_or("?");
        return axum::Json(
            json!({"access_token": format!("tok-{iss}"), "expires_in": 3600, "token_type": "Bearer"}),
        );
    }
    let bearer = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let n = {
        let mut calls = f.calls.lock().unwrap();
        calls.push((method.to_string(), path.clone(), bearer, body));
        calls.len()
    };
    if method == Method::GET && path.starts_with("/v1/projects/") && path.contains("/subscriptions/") {
        let topic = f.subscription_topic.lock().unwrap().clone()
            .unwrap_or_else(|| "projects/test-project/topics/duduclaw-chat-events".into());
        let mut subscription = json!({"name": path.trim_start_matches("/v1/"), "topic": topic});
        if let Some(value) = f.subscription_exactly_once.lock().unwrap().clone() {
            subscription["enableExactlyOnceDelivery"] = value;
        }
        return axum::Json(subscription);
    }
    let gate = if method == Method::POST && path.ends_with("/messages") {
        f.first_message_gate.lock().unwrap().take()
    } else {
        None
    };
    if let Some(gate) = gate {
        f.first_message_started.notify_one();
        gate.acquire().await.unwrap().forget();
    }
    axum::Json(
        json!({"name": format!("{}/m{n}", path.trim_end_matches("/messages").trim_start_matches('/'))}),
    )
}

pub(crate) struct Fake {
    state: Arc<FakeGoogle>,
    origin: String,
}

pub(crate) async fn start_fake() -> Fake {
    let state = Arc::new(FakeGoogle::default());
    let app = Router::new()
        .fallback(fake_google)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fake { state, origin }
}

pub(crate) fn set_subscription_topic(fake: &Fake, topic: &str) {
    *fake.state.subscription_topic.lock().unwrap() = Some(topic.to_string());
}

pub(crate) fn set_subscription_exactly_once(fake: &Fake, value: Option<Value>) {
    *fake.state.subscription_exactly_once.lock().unwrap() = value;
}

pub(crate) fn service_account(fake: &Fake, email: &str) -> String {
    crate::googlechat::test_routes::set(email, &fake.origin, &format!("{}/jwks", fake.origin));
    json!({
        "type": "service_account",
        "client_email": email,
        "private_key": test_key().pem,
        "token_uri": format!("{}/token", fake.origin),
    })
    .to_string()
}

pub(crate) fn googlechat_section(home: &Path, project: &str, sa_json: &str) -> String {
    let enc = crate::config_crypto::encrypt_value(sa_json, home).unwrap();
    format!(
        "\n[channels.googlechat]\nproject_number = \"{project}\"\nservice_account_json_enc = \"{enc}\"\n"
    )
}

pub(crate) fn message_event(space: &str, text: &str) -> Value {
    json!({"type": "MESSAGE", "space": {"name": space, "type": "SPACE"}, "message": {
        "text": text, "argumentText": text, "space": {"name": space, "type": "SPACE"},
        "sender": {"name": "users/42", "displayName": "Human"}}})
}

pub(crate) async fn post(app: &Router, agent: &str, bearer: Option<&str>, event: &Value) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/webhook/googlechat/agents/{agent}"))
        .header("content-type", "application/json");
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(event.to_string())).unwrap())
        .await
        .unwrap();
    resp.status()
}

pub(crate) fn texts_with_bearer(fake: &Fake, bearer: &str) -> Vec<String> {
    fake.state
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, _, b, _)| b == bearer)
        .filter_map(|(_, _, _, body)| body["text"].as_str().map(str::to_string))
        .collect()
}

pub(crate) async fn wait_text(fake: &Fake, bearer: &str, needle: &str) -> String {
    for _ in 0..150 {
        let all = texts_with_bearer(fake, bearer).join("\n");
        if all.contains(needle) {
            return all;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "no message with {needle} sent as {bearer}: {:?}",
        fake.state.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn googlechat_full_queue_acks_without_creating_a_turn() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("full-{}@p0-test.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
        &googlechat_section(home.path(), "444444443", &sa));
    let mut ctx = super::e2e_tests::reply_ctx(home.path()).await;
    Arc::get_mut(&mut ctx).unwrap().googlechat_queue = crate::conversation_queue::ConversationQueue::with_caps(1, 1);
    let group = json!(["sales", "spaces/FULL"]).to_string();
    let (release, blocked) = tokio::sync::oneshot::channel::<()>();
    ctx.googlechat_queue.submit_in(&group, "occupied", async { let _ = blocked.await; }).unwrap();
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx.clone());
    let mut event = message_event("spaces/FULL", "must be dropped");
    event["message"]["name"] = json!("spaces/FULL/messages/rejected");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("444444443")), &event).await, StatusCode::OK);
    let notice = wait_text(&fake, &format!("Bearer tok-{email}"), "這則訊息沒有受理，請稍後再傳一次").await;
    assert!(!notice.contains("OWN_BOT_REPLY"));
    release.send(()).unwrap();
    let (drained, done) = tokio::sync::oneshot::channel();
    ctx.googlechat_queue.submit_in("barrier", "occupied", async { let _ = drained.send(()); }).unwrap();
    done.await.unwrap();
    let db = rusqlite::Connection::open(home.path().join("sessions.db")).unwrap();
    let turns: i64 = db.query_row("SELECT COUNT(*) FROM session_messages", [], |r| r.get(0)).unwrap();
    assert_eq!(turns, 0, "a rejected event must not create a turn");
    assert_eq!(texts_with_bearer(&fake, &format!("Bearer tok-{email}")).len(), 1);
}

#[tokio::test]
async fn own_app_requests_queue_in_order_while_other_apps_keep_running() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let fake = start_fake().await;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let email_a = format!("queued-a-{id}@p0-test.iam.gserviceaccount.com");
    let email_b = format!("queued-b-{id}@p0-test.iam.gserviceaccount.com");
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    for (agent, email, project) in [
        ("sales", &email_a, "444444441"),
        ("hr", &email_b, "444444442"),
    ] {
        let sa = service_account(&fake, email);
        write_agent(&agents, agent, agent, "", "specialist", &googlechat_section(home.path(), project, &sa));
    }
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *fake.state.first_message_gate.lock().unwrap() = Some(gate.clone());
    let first = message_event("spaces/QUEUED", "first");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("444444441")), &first).await, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(10), fake.state.first_message_started.notified()).await.unwrap();

    let second = message_event("spaces/QUEUED", "second");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("444444441")), &second).await, StatusCode::OK);
    assert_eq!(post(&app, "hr", Some(&chat_jwt("444444442")), &second).await, StatusCode::OK);
    wait_text(&fake, &format!("Bearer tok-{email_b}"), "OWN_BOT_REPLY").await;
    // A slow first turn in sales must prevent a second placeholder/reply,
    // while hr can finish in the very same space with the same human sender.
    let before_release = texts_with_bearer(&fake, &format!("Bearer tok-{email_a}"));
    gate.add_permits(1);
    assert_eq!(before_release.len(), 1, "another request bypassed the own-app queue: {before_release:?}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let replies = texts_with_bearer(&fake, &format!("Bearer tok-{email_a}"))
                .iter().filter(|text| text.contains("OWN_BOT_REPLY")).count();
            if replies == 2 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn each_app_reaches_only_its_employee_and_wrong_audiences_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let fake = start_fake().await;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let email_a = format!("app-a-{id}@p0-test.iam.gserviceaccount.com");
    let email_b = format!("app-b-{id}@p0-test.iam.gserviceaccount.com");
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    let sa_a = service_account(&fake, &email_a);
    let sa_b = service_account(&fake, &email_b);
    write_agent(
        &agents,
        "sales",
        "業務",
        "@sales",
        "specialist",
        &googlechat_section(home.path(), "111111111", &sa_a),
    );
    write_agent(
        &agents,
        "hr",
        "人資",
        "@hr",
        "specialist",
        &googlechat_section(home.path(), "222222222", &sa_b),
    );
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx.clone());

    // A's token on B's endpoint, a wrong audience, no token, a forged token,
    // an employee without an app and an unknown id: all refused, nothing sent.
    let event = message_event("spaces/SHARED", "hello");
    assert_eq!(
        post(&app, "hr", Some(&chat_jwt("111111111")), &event).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(&app, "sales", Some(&chat_jwt("999999999")), &event).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(&app, "sales", None, &event).await,
        StatusCode::UNAUTHORIZED
    );
    let forged = {
        let t = chat_jwt("111111111");
        let mut parts: Vec<&str> = t.split('.').collect();
        parts[2] = "AAAA";
        parts.join(".")
    };
    assert_eq!(
        post(&app, "sales", Some(&forged), &event).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(&app, "dudu", Some(&chat_jwt("111111111")), &event).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(&app, "nobody", Some(&chat_jwt("111111111")), &event).await,
        StatusCode::UNAUTHORIZED
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        fake.state.calls.lock().unwrap().is_empty(),
        "refused requests send nothing"
    );

    // Both apps live in the same space; each answers as its own employee
    // with its own credentials.
    assert_eq!(
        post(&app, "sales", Some(&chat_jwt("111111111")), &event).await,
        StatusCode::OK
    );
    assert_eq!(
        post(&app, "hr", Some(&chat_jwt("222222222")), &event).await,
        StatusCode::OK
    );
    let as_a = wait_text(&fake, &format!("Bearer tok-{email_a}"), "OWN_BOT_REPLY").await;
    let as_b = wait_text(&fake, &format!("Bearer tok-{email_b}"), "OWN_BOT_REPLY").await;
    assert!(
        as_a.contains("agent=sales session=googlechat:spaces/SHARED#agent:sales"),
        "{as_a}"
    );
    assert!(
        as_b.contains("agent=hr session=googlechat:spaces/SHARED#agent:hr"),
        "{as_b}"
    );
    assert!(
        !as_a.contains("agent=hr") && !as_b.contains("agent=sales"),
        "apps never cross"
    );
    assert!(!as_a.contains("SESSION_CLASH") && !as_b.contains("SESSION_CLASH"));
    let status = ctx.channel_status.read().await;
    assert!(status.get("googlechat:sales").is_some_and(|s| s.connected));

    // Outbound (delegation forward / proactive) uses the employee's own app.
    drop(status);
    crate::googlechat::send_text_to_space_as(
        home.path(),
        Some("hr"),
        "spaces/SHARED",
        "proactive note",
    )
    .await
    .unwrap();
    wait_text(&fake, &format!("Bearer tok-{email_b}"), "proactive note").await;
}

#[tokio::test]
async fn removing_an_app_closes_its_endpoint() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let fake = start_fake().await;
    let email = format!(
        "app-c-{}@p0-test.iam.gserviceaccount.com",
        uuid::Uuid::new_v4().simple()
    );
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    let sa = service_account(&fake, &email);
    write_agent(
        &agents,
        "ops",
        "營運",
        "@ops",
        "specialist",
        &googlechat_section(home.path(), "333333333", &sa),
    );
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    let app = crate::googlechat_agents::router(ctx.clone());
    assert!(
        crate::googlechat_agents::probe(&ctx, "ops").await.is_ok(),
        "credentials verify against the fake"
    );
    write_agent(&agents, "ops", "營運", "@ops", "specialist", "");
    crate::googlechat_agents::forget("ops");
    let event = message_event("spaces/X", "hi");
    assert_eq!(
        post(&app, "ops", Some(&chat_jwt("333333333")), &event).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        crate::googlechat::send_text_to_space_as(home.path(), Some("ops"), "spaces/X", "x")
            .await
            .is_err(),
        "no own app and no shared app: nothing is sent"
    );
}

#[tokio::test]
async fn googlechat_pubsub_switch_disables_verified_own_webhook() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("switch-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let mut key: Value = serde_json::from_str(&service_account(&fake, &email)).unwrap();
    key["project_id"] = json!("test-project");
    let http = googlechat_section(home.path(), "123456789", &key.to_string());
    let agents = home.path().join("agents");
    crate::trigger_route::tests::write_agent(&agents, "sales", "業務", "@sales", "specialist", &http);
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    let app = crate::googlechat_agents::router(ctx);
    let added = json!({"type":"ADDED_TO_SPACE", "space":{"name":"spaces/AAA"}, "user":{"name":"users/42"}});
    assert_eq!(post(&app, "sales", Some(&chat_jwt("123456789")), &added).await, StatusCode::OK);
    let section = format!("{http}connection = \"pubsub\"\npubsub_subscription = \"projects/test-project/subscriptions/chat-events\"\n");
    crate::trigger_route::tests::write_agent(&agents, "sales", "業務", "@sales", "specialist", &section);
    let event = message_event("spaces/AAA", "hello");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("123456789")), &event).await, StatusCode::UNAUTHORIZED);
    assert_eq!(post(&app, "sales", Some(&chat_jwt("999999999")), &event).await, StatusCode::UNAUTHORIZED);
    assert!(texts_with_bearer(&fake, &format!("Bearer tok-{email}")).is_empty());
}

#[tokio::test]
async fn googlechat_pubsub_own_webhook_refuses_before_jwks_and_matches_unknown_agent() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("early-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let mut key: Value = serde_json::from_str(&service_account(&fake, &email)).unwrap();
    key["project_id"] = json!("test-project");
    let section = format!("{}connection = \"pubsub\"\npubsub_subscription = \"projects/test-project/subscriptions/chat-events\"\n", googlechat_section(home.path(), "123456789", &key.to_string()));
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "業務", "@sales", "specialist", &section);
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    let headers = HeaderMap::from_iter([(axum::http::header::AUTHORIZATION, format!("Bearer {}", chat_jwt("123456789")).parse().unwrap())]);
    let event = Bytes::from(message_event("spaces/AAA", "hello").to_string());
    let known = crate::googlechat_agents::handle(ctx.clone(), "sales", &headers, event.clone()).await;
    let unknown = crate::googlechat_agents::handle(ctx, "missing", &headers, event).await;
    assert_eq!(known.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(known.status(), unknown.status());
    assert_eq!(axum::body::to_bytes(known.into_body(), 1024).await.unwrap(), axum::body::to_bytes(unknown.into_body(), 1024).await.unwrap());
    assert_eq!(fake.state.jwks_requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(fake.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn googlechat_chat_and_pubsub_tokens_use_separate_minimal_scopes() {
    let fake = start_fake().await;
    let email = format!("scopes-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let raw = service_account(&fake, &email);
    let creds = crate::googlechat::GoogleChatCreds::candidate(&raw).unwrap();
    creds.token().await.unwrap();
    creds.pubsub_token().await.unwrap();
    assert_eq!(*fake.state.token_scopes.lock().unwrap(), vec![
        "https://www.googleapis.com/auth/chat.bot",
        "https://www.googleapis.com/auth/pubsub",
    ]);
    // Independent caches retain the correct grants instead of reusing the
    // other transport's token or requesting a combined scope.
    creds.token().await.unwrap();
    creds.pubsub_token().await.unwrap();
    assert_eq!(fake.state.token_scopes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn googlechat_http_addon_shapes_report_actionable_channel_status() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("agents")).unwrap();
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    let raw = json!({"client_email":"addon@test-project.iam.gserviceaccount.com", "private_key":test_key().pem}).to_string();
    let creds = Arc::new(crate::googlechat::GoogleChatCreds::candidate(&raw).unwrap());
    for agent in [None, Some("sales".to_string())] {
        let label = agent.as_deref().map(|id| super::label("googlechat", id)).unwrap_or_else(|| "googlechat".into());
        let state = Arc::new(crate::googlechat::GoogleChatState { ctx: ctx.clone(), creds: creds.clone(), agent });
        for event in [json!({"commonEventObject":{}}), json!({"chat":{"messagePayload":{"message":{"text":"hello"}}}})] {
            ctx.channel_status.write().await.remove(&label);
            let response = crate::googlechat::dispatch_event(state.clone(), Bytes::from(event.to_string())).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(body.as_ref(), crate::googlechat::ADDON_DIAGNOSTIC.as_bytes());
            let statuses = ctx.channel_status.read().await;
            let status = statuses.get(&label).expect("unsupported shape must not be silently discarded");
            assert!(!status.connected);
            assert_eq!(status.error.as_deref(), Some(crate::googlechat::ADDON_DIAGNOSTIC));
            assert!(status.error.as_ref().unwrap().contains("Build this Chat app as a Google Workspace add-on"));
        }
    }
}
