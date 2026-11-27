//! End-to-end: two AI employees, each with its own Google Chat app, against a
//! local fake (JWKS, OAuth token endpoint, Chat REST API) with locally signed
//! request tokens. No real Google credential is involved.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::response::{IntoResponse, Response};
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
    K.get_or_init(generate_test_key)
}

fn generate_test_key() -> TestKey {
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
}

/// A request token as Google's Chat signer would mint it for `audience`.
pub(crate) fn chat_jwt(audience: &str) -> String {
    chat_jwt_with("chat@system.gserviceaccount.com", audience,
        chrono::Utc::now().timestamp() + 600, true)
}

/// The untrusted signer uses the same kid but a different RSA key: this tests
/// signature verification, not just the unknown-kid branch.
pub(crate) fn chat_jwt_with(issuer: &str, audience: &str, expires: i64, trusted: bool) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test-kid".into());
    let claims = json!({"iss":issuer, "aud":audience, "iat":now, "exp":expires});
    let untrusted;
    let signer = if trusted { test_key() } else {
        untrusted = generate_test_key();
        &untrusted
    };
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(signer.pem.as_bytes()).unwrap();
    jsonwebtoken::encode(&header, &claims, &key).unwrap()
}

#[derive(Default)]
struct FakeGoogle {
    /// `(method, path, bearer, body)` of every Chat API call.
    calls: Mutex<Vec<(String, String, String, Value)>>,
    queries: Mutex<Vec<(String, String)>>,
    first_message_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    first_message_started: tokio::sync::Notify,
    subscription_topic: Mutex<Option<String>>,
    subscription_exactly_once: Mutex<Option<Value>>,
    jwks_requests: std::sync::atomic::AtomicUsize,
    token_scopes: Mutex<Vec<String>>,
    patch_status: Mutex<Option<StatusCode>>,
    post_status: Mutex<Option<StatusCode>>,
    delete_status: Mutex<Option<StatusCode>>,
    post_delay: Mutex<Option<Duration>>,
    unparseable_post: std::sync::atomic::AtomicBool,
}

async fn fake_google(
    State(f): State<Arc<FakeGoogle>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    f.queries.lock().unwrap().push((uri.path().to_owned(), uri.query().unwrap_or("").to_owned()));
    let path = uri.path().to_string();
    if path == "/jwks" {
        f.jwks_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let k = test_key();
        return axum::Json(
            json!({"keys": [{"kty": "RSA", "kid": "test-kid", "alg": "RS256", "n": k.n, "e": k.e}]}),
        ).into_response();
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
        ).into_response();
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
        return axum::Json(subscription).into_response();
    }
    if method == Method::PATCH {
        if let Some(status) = *f.patch_status.lock().unwrap() { return status.into_response(); }
    }
    if method == Method::DELETE {
        if let Some(status) = *f.delete_status.lock().unwrap() { return status.into_response(); }
    }
    if method == Method::POST {
        let delay = *f.post_delay.lock().unwrap();
        if let Some(delay) = delay { tokio::time::sleep(delay).await; }
        if f.unparseable_post.load(std::sync::atomic::Ordering::SeqCst) { return "not json".into_response(); }
        if let Some(status) = *f.post_status.lock().unwrap() { return status.into_response(); }
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
    let client_id = uri.query().and_then(|q| q.split('&').find_map(|v| v.strip_prefix("messageId=")));
    let name = match client_id {
        Some(id) => format!("{}/{id}", path.trim_start_matches('/')),
        None => format!("{}/messages/m{n}", path.trim_end_matches("/messages").trim_start_matches('/')),
    };
    axum::Json(json!({"name": name})).into_response()
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
        "thread": {"name": format!("{space}/threads/T")},
        "sender": {"name": "users/42", "type": "HUMAN", "displayName": "Human"}}})
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
    let store = crate::googlechat_inbox::Inbox::shared(home.path()).unwrap();
    assert!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap().is_empty(),
        "queue-full rejects before creating a placeholder");
}

#[test]
fn only_a_human_sender_type_is_admitted() {
    let with = |sender: Value| json!({"sender": sender});
    assert!(crate::googlechat::is_human_message(&with(json!({"name": "users/42", "type": "HUMAN"}))));
    for sender in [
        json!({"name": "users/42", "type": "BOT"}),
        json!({"name": "users/42", "type": "human"}),
        json!({"name": "users/42", "type": "TYPE_UNSPECIFIED"}),
        json!({"name": "users/42"}),
        json!({"name": "users/42", "type": 1}),
    ] {
        assert!(!crate::googlechat::is_human_message(&with(sender.clone())), "{sender}");
    }
    assert!(!crate::googlechat::is_human_message(&json!({})));
}

/// L5: a `BOT` sender (another Chat app — possibly carrying a person's id)
/// on the HTTP MESSAGE path is acknowledged and dropped: no turn, no reply,
/// no identity mapping. The next human message is answered normally.
#[tokio::test]
async fn bot_sender_on_the_http_path_never_starts_a_turn() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("bot-{}@p0-test.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
        &googlechat_section(home.path(), "444444445", &sa));
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx);
    let mut bot = message_event("spaces/BOTS", "BOT_ECHO_TEXT");
    bot["message"]["sender"]["type"] = json!("BOT");
    bot["message"]["name"] = json!("spaces/BOTS/messages/bot");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("444444445")), &bot).await, StatusCode::OK);
    // Same sender id, same conversation queue: had the bot message been
    // admitted it would be answered first.
    let mut human = message_event("spaces/BOTS", "human text");
    human["message"]["name"] = json!("spaces/BOTS/messages/human");
    assert_eq!(post(&app, "sales", Some(&chat_jwt("444444445")), &human).await, StatusCode::OK);
    wait_text(&fake, &format!("Bearer tok-{email}"), "OWN_BOT_REPLY").await;
    let replies = texts_with_bearer(&fake, &format!("Bearer tok-{email}"))
        .iter().filter(|text| text.contains("OWN_BOT_REPLY")).count();
    assert_eq!(replies, 1, "the BOT message must not have been answered");
    let db = rusqlite::Connection::open(home.path().join("sessions.db")).unwrap();
    let echoed: i64 = db
        .query_row("SELECT COUNT(*) FROM session_messages WHERE content LIKE '%BOT_ECHO_TEXT%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(echoed, 0, "a BOT sender must not create a turn");
}

#[tokio::test]
async fn queue_keys_preserve_thread_order_and_app_independence() {
    use crate::googlechat_conversation::GoogleChatConversation;
    let home = tempfile::tempdir().unwrap();
    let event = message_event("spaces/QUEUED", "first");
    let conversation = GoogleChatConversation::from_event(&event, false, false).unwrap();
    let queue = crate::conversation_queue::ConversationQueue::default();
    let (group, key) = conversation.queue_keys(Some("sales"));
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let first = conversation.clone();
    let first_home = home.path().to_owned();
    queue.submit_in(&group, &key, async move {
        let _lease = first.turn_lease(&first_home, true).await.unwrap();
        entered.send(()).unwrap(); wait.await.unwrap();
    }).unwrap();
    started.await.unwrap();
    let (second, mut second_started) = tokio::sync::oneshot::channel();
    let second_home = home.path().to_owned();
    queue.submit_in(&group, &key, async move {
        let _lease = conversation.turn_lease(&second_home, true).await.unwrap();
        second.send(()).unwrap();
    }).unwrap();
    for (app, thread) in [("hr", "T"), ("sales", "OTHER")] {
        let mut independent = event.clone();
        independent["message"]["thread"]["name"] = json!(format!("spaces/QUEUED/threads/{thread}"));
        let conversation = GoogleChatConversation::from_event(&independent, false, false).unwrap();
        let (group, key) = conversation.queue_keys(Some(app));
        let (done, finished) = tokio::sync::oneshot::channel();
        let independent_home = home.path().to_owned();
        queue.submit_in(&group, &key, async move {
            let _lease = conversation.turn_lease(&independent_home, true).await.unwrap();
            done.send(()).unwrap();
        }).unwrap();
        tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap();
    }
    assert!(matches!(second_started.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), second_started).await.unwrap().unwrap();
}

#[tokio::test]
async fn thread_lease_preserves_shared_app_order() {
    use crate::googlechat_conversation::GoogleChatConversation;
    let home = tempfile::tempdir().unwrap();
    let event = message_event("spaces/ORDER", "first");
    let thread = GoogleChatConversation::from_event(&event, false, false).unwrap();
    let mut other = event.clone();
    other["message"]["thread"]["name"] = json!("spaces/ORDER/threads/OTHER");
    let other_thread = GoogleChatConversation::from_event(&other, false, false).unwrap();
    for own_app in [false, true] {
        let first = thread.turn_lease(home.path(), own_app).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(20), thread.turn_lease(home.path(), !own_app)).await.is_err());
        let independent = tokio::time::timeout(Duration::from_secs(2), other_thread.turn_lease(home.path(), false)).await.unwrap().unwrap();
        drop(independent);
        drop(first);
        tokio::time::timeout(Duration::from_secs(2), thread.turn_lease(home.path(), !own_app)).await.unwrap().unwrap();
    }
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
        as_a.contains("agent=sales session=googlechat:spaces/SHARED:thread:spaces/SHARED/threads/T#agent:sales"),
        "{as_a}"
    );
    assert!(
        as_b.contains("agent=hr session=googlechat:spaces/SHARED:thread:spaces/SHARED/threads/T#agent:hr"),
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

#[tokio::test]
async fn dm_reply_placement_uses_inbound_thread_reply_and_dashboard_setting() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("placement-{}@test.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    use crate::trigger_route::tests::write_agent;
    write_agent(&home.path().join("agents"), "sales", "業務", "", "specialist", &googlechat_section(home.path(), "987654321", &sa));
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx.clone());
    for (index, kind, legacy, thread_reply, always, expected) in [
        (0, "DIRECT_MESSAGE", "DM", false, false, false),
        (1, "DIRECT_MESSAGE", "DM", true, false, true),
        (2, "SPACE", "SPACE", false, false, true),
        (3, "DIRECT_MESSAGE", "DM", false, true, true),
        (4, "", "DM", false, false, false),
    ] {
        ctx.channel_settings.set("googlechat", "global", "dm_reply_placement", if always { "thread" } else { "main" }).await.unwrap();
        let space = format!("spaces/PLACEMENT{index}");
        let mut event = message_event(&space, "hello");
        let mut carried = json!({"name":space, "type":legacy, "singleUserBotDm":true});
        if !kind.is_empty() { carried["spaceType"] = json!(kind); }
        event["space"] = carried.clone(); event["message"]["space"] = carried;
        event["message"]["thread"] = json!({"name":format!("{space}/threads/t")});
        event["message"]["threadReply"] = json!(thread_reply);
        assert_eq!(post(&app, "sales", Some(&chat_jwt("987654321")), &event).await, StatusCode::OK);
        wait_text(&fake, &format!("Bearer tok-{email}"), "OWN_BOT_REPLY").await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let done = fake.state.calls.lock().unwrap().iter().any(|(_, path, _, body)| path.contains(&space) && body["text"].as_str().is_some_and(|t| t.contains("OWN_BOT_REPLY")));
                if done { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        let calls = fake.state.calls.lock().unwrap();
        let creates: Vec<_> = calls.iter().filter(|(method, path, _, _)| method == "POST" && path == &format!("/{space}/messages")).collect();
        assert!(!creates.is_empty());
        for (_, _, _, body) in creates { assert_eq!(body.get("thread").is_some(), expected); }
        for (_, query) in fake.state.queries.lock().unwrap().iter().filter(|(path, _)| path == &format!("/{space}/messages")) {
            assert_eq!(query.split('&').any(|p| p == "messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD"), expected);
        }
    }
    assert!(!crate::channel_settings::CONFIG_KEYS.contains(&"dm_reply_placement"));
    assert!(!crate::channel_settings::MCP_ACCESS_KEYS.contains(&"dm_reply_placement"));
}

#[cfg(unix)]
#[tokio::test]
async fn placeholder_registration_survives_inflight_and_retires_on_reply_exits() {
    for mode in ["final", "failure", "delivery_failure", "silence", "command"] {
        let home = tempfile::tempdir().unwrap();
        let fake = start_fake().await;
        let email = format!("lifecycle-{}@test.invalid", uuid::Uuid::new_v4().simple());
        let sa = service_account(&fake, &email);
        crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
            &googlechat_section(home.path(), "444444443", &sa));
        let ctx = super::e2e_tests::reply_ctx(home.path()).await;
        let mut waiting = super::e2e_seam::arm_waiting(home.path());
        waiting.bind_failure_sessions();
        if mode == "failure" {
            super::e2e_seam::failure_reply(home.path(), "GENERATION_FAILED".into(), crate::channel_reply::TurnFailureReason::Internal);
        }
        if mode == "silence" { super::e2e_seam::empty_or_panicking_reply(home.path(), false); }
        let creds = crate::googlechat_agents::agent_creds(home.path(), "sales").await.unwrap();
        let state = Arc::new(crate::googlechat::GoogleChatState { ctx: ctx.clone(), creds, agent: Some("sales".into()) });
        let event = message_event("spaces/LIFECYCLE", if mode == "command" { "/help" } else { "hello" });
        let count = || -> i64 {
            rusqlite::Connection::open(home.path().join("googlechat_inbox.db")).unwrap()
                .query_row("SELECT count(*) FROM pending_placeholders", [], |r| r.get(0)).unwrap()
        };
        if mode == "command" {
            crate::googlechat::process_pubsub_event(&state, &event).await;
        } else {
            let observe = async {
                let turn = tokio::time::timeout(Duration::from_secs(10), waiting.entered.recv()).await.unwrap().unwrap();
                assert_eq!(count(), 1, "placeholder must be durable while the model is running");
                let rows = crate::googlechat_inbox::Inbox::shared(home.path()).unwrap()
                    .stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
                assert_eq!(rows[0].app_agent.as_deref(), Some("sales"));
                assert_eq!(rows[0].app_email, email);
                assert_eq!(rows[0].employee_id, "sales");
                assert_eq!(rows[0].session_id, turn.session_id);
                if mode == "delivery_failure" {
                    *fake.state.patch_status.lock().unwrap() = Some(StatusCode::FORBIDDEN);
                    *fake.state.post_status.lock().unwrap() = Some(StatusCode::SERVICE_UNAVAILABLE);
                }
                turn.release.send(()).unwrap();
            };
            tokio::join!(crate::googlechat::process_pubsub_event(&state, &event), observe);
        }
        assert_eq!(count(), 0, "every reply exit must retire recovery evidence: {mode}");
        let calls = fake.state.calls.lock().unwrap();
        assert!(calls.iter().any(|(method, _, _, _)| method == "POST"));
        assert!(calls.iter().any(|(method, _, _, _)| method == "PATCH" || method == "DELETE"));

    }
}

#[cfg(unix)]
#[tokio::test]
async fn restart_sweep_patches_existing_message_with_original_employee_app() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("restart-{}@test.invalid", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
        &googlechat_section(home.path(), "444444443", &sa));
    let store = crate::googlechat_inbox::Inbox::shared(home.path()).unwrap();
    let row = crate::googlechat_inbox::PendingPlaceholder {
        message_name: format!("spaces/RESTART/messages/client-{}", "a".repeat(32)), space: "spaces/RESTART".into(),
        app_agent: Some("sales".into()), app_email: email.clone(), employee_id: "sales".into(),
        session_id: "googlechat:spaces/RESTART:sales".into(), thread_name: None,
        created_at: chrono::Utc::now().timestamp() - 10, state: "pending".into(),
    };
    store.insert_placeholder(&row).unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    crate::googlechat_recovery::startup(home.path()).await;
    assert!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap().is_empty());
    let calls = fake.state.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "PATCH");
    assert_eq!(calls[0].1, format!("/{}", row.message_name));
    assert_eq!(calls[0].2, format!("Bearer tok-{email}"));
    assert_eq!(calls[0].3["text"], "這次回覆中斷了，請再傳一次。");
    drop(calls);
    // A removed/rebound app is retained for retry; no shared-app fallback.
    let mut mismatched = row;
    mismatched.app_email = "previous-app@test.invalid".into();
    store.insert_placeholder(&mismatched).unwrap();
    crate::googlechat_recovery::startup(home.path()).await;
    assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap().len(), 1);
    assert_eq!(fake.state.calls.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn restart_sweep_handles_api_failures_without_posting_a_notice() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("status-{}@test.invalid", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
        &googlechat_section(home.path(), "444444443", &sa));
    let store = crate::googlechat_inbox::Inbox::shared(home.path()).unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    for (status, age, kept) in [(404, 10, false), (403, 10, false), (503, 10, true), (503, 86_401, false)] {
        let row = crate::googlechat_inbox::PendingPlaceholder {
            message_name: format!("spaces/STATUS/messages/client-{}", "a".repeat(32)), space: "spaces/STATUS".into(),
            app_agent: Some("sales".into()), app_email: email.clone(), employee_id: "sales".into(),
            session_id: "googlechat:spaces/STATUS:sales".into(), thread_name: None,
            created_at: chrono::Utc::now().timestamp() - age, state: "pending".into(),
        };
        // A retained transient row is aged for the next iteration.
        store.remove_placeholder(&row.message_name).unwrap();
        store.insert_placeholder(&row).unwrap();
        *fake.state.patch_status.lock().unwrap() = Some(StatusCode::from_u16(status).unwrap());
        crate::googlechat_recovery::startup(home.path()).await;
        assert_eq!(!store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap().is_empty(), kept);
    }
    assert_eq!(fake.state.calls.lock().unwrap().len(), 4);
    assert!(fake.state.calls.lock().unwrap().iter().all(|(method, _, bearer, _)| method == "PATCH" && bearer == &format!("Bearer tok-{email}")));
}

// Shared controls for lifecycle tests using this existing OAuth/Chat fake.
pub(crate) fn placeholder_statuses(fake: &Fake, patch: Option<StatusCode>, post: Option<StatusCode>, delete: Option<StatusCode>) {
    *fake.state.patch_status.lock().unwrap() = patch;
    *fake.state.post_status.lock().unwrap() = post;
    *fake.state.delete_status.lock().unwrap() = delete;
}

pub(crate) fn placeholder_post_behavior(fake: &Fake, delay: Option<Duration>, unparseable: bool) {
    *fake.state.post_delay.lock().unwrap() = delay;
    fake.state.unparseable_post.store(unparseable, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn gate_placeholder_post(fake: &Fake, gate: Arc<tokio::sync::Semaphore>) {
    *fake.state.first_message_gate.lock().unwrap() = Some(gate);
}

pub(crate) async fn placeholder_post_started(fake: &Fake) {
    tokio::time::timeout(Duration::from_secs(10), fake.state.first_message_started.notified()).await.unwrap();
}

pub(crate) fn chat_calls(fake: &Fake) -> Vec<(String, String, String, Value)> { fake.state.calls.lock().unwrap().clone() }
pub(crate) fn chat_queries(fake: &Fake) -> Vec<(String, String)> { fake.state.queries.lock().unwrap().clone() }

#[path = "e2e_gchat_shared_tests.rs"]
mod shared_tests;
