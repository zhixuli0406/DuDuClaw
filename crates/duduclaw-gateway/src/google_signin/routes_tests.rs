//! Public HTTP contract tests with isolated homes and a local signed Google provider.
use super::*;
use axum::{body::Body, http::Request, routing::{get, post}, Form, Router};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use duduclaw_auth::{JwtConfig, UserRole};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::{RsaPrivateKey, RsaPublicKey, pkcs8::{EncodePrivateKey, LineEnding}, traits::PublicKeyParts};
use serde_json::Value;
use std::sync::{OnceLock, atomic::{AtomicUsize, Ordering}};
use tower::ServiceExt;

const BASE: &str = "https://company.example/team";
const CLIENT: &str = "signin-route-client";
const EMAIL: &str = "new-person@example.com";

struct Fixture {
    _home: tempfile::TempDir,
    state: Arc<AppState>,
    peer: SocketAddr,
}
impl Fixture {
    async fn new(personal: bool, enabled: bool, base: Option<&str>, enforce: bool, approval: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let public = base.map(|base| format!("[dashboard]\npublic_url={base:?}\n")).unwrap_or_default();
        std::fs::write(home.join("config.toml"), format!("{public}[auth]\nmfa_enforced={enforce}\n\
            [google_signin]\nenabled={enabled}\nallowed_hd=[\"example.com\"]\napproval={approval:?}\n\
            daily_new_accounts=20\n[google_signin.enabled_ack]\nby=\"admin\"\nat=\"2026-11-27T00:00:00Z\"\n")).unwrap();
        crate::mcp_oauth::upsert_client_config(&home, crate::mcp_oauth::McpOAuthClientConfig {
            provider_id: "google".into(), client_id: CLIENT.into(), client_secret: "test-secret".into(),
            auth_url: oauth::AUTHORIZE.into(), token_url: oauth::TOKEN.into(), scopes: vec![],
            redirect_uri: format!("{}/api/mcp/oauth/callback", base.unwrap_or(BASE)),
        }).unwrap();
        let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let jwt = Arc::new(JwtConfig::new(b"google-login-http-test-at-least-32-bytes"));
        let handler = crate::handlers::MethodHandler::new(home.clone()).await;
        handler.set_user_db(db.clone(), jwt.clone()).await;
        handler.set_test_edition(Some(if personal { duduclaw_core::EditionProfile::Personal }
            else { duduclaw_core::EditionProfile::Enterprise })).await;
        let (tx, _) = tokio::sync::broadcast::channel(4);
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: crate::auth::AuthManager::new(None), handler: Arc::new(handler), tx, event_tx, prepared_events: Default::default(),
            user_db: db, jwt_config: jwt,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
            home_dir: home,
        });
        static NEXT_PEER: AtomicUsize = AtomicUsize::new(1);
        let n = NEXT_PEER.fetch_add(1, Ordering::SeqCst);
        let peer = SocketAddr::from(([198, 18, (n / 250) as u8, (n % 250 + 1) as u8], 12000));
        Self { _home: dir, state, peer }
    }
    async fn ready() -> Self { Self::new(false, true, Some(BASE), false, "direct").await }
    async fn request(&self, method: &str, path: &str, body: &str, cookie: Option<&str>) -> Response {
        let app = Router::new()
            .route("/api/login/methods", get(methods))
            .route("/api/login/google/start", post(start).layer(axum::extract::DefaultBodyLimit::max(4096)))
            .route("/api/login/google/callback", get(callback))
            .route("/api/login/google/complete", post(complete).layer(axum::extract::DefaultBodyLimit::max(4096)))
            .with_state(self.state.clone());
        let mut request = Request::builder().method(method).uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            // Neither host nor forwarding headers may influence OAuth URLs.
            .header(header::HOST, "evil.example")
            .header("x-forwarded-host", "evil.example")
            .header("forwarded", "host=evil.example;proto=http");
        if let Some(cookie) = cookie { request = request.header(header::COOKIE, cookie); }
        let mut request = request.body(Body::from(body.to_owned())).unwrap();
        request.extensions_mut().insert(ConnectInfo(self.peer));
        app.oneshot(request).await.unwrap()
    }
    async fn start(&self) -> Started {
        let response = self.request("POST", "/api/login/google/start", "{}", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().to_owned();
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        let body = body(response).await;
        let url = url::Url::parse(body["authorization_url"].as_str().unwrap()).unwrap();
        let query = url.query_pairs().into_owned().collect::<HashMap<_, _>>();
        assert_eq!(query["scope"], "openid email profile");
        assert_eq!(query["code_challenge_method"], "S256");
        Started { cookie, set_cookie, query }
    }
    async fn callback(&self, provider: &Provider, flow: &Started, cookie: Option<&str>, suffix: &str) -> Response {
        let path = format!("/api/login/google/callback?code=test-code&state={}{}", flow.query["state"], suffix);
        oauth::TEST_ENDPOINT_BASE.scope(provider.base.clone(), self.request("GET", &path, "", cookie)).await
    }
    async fn complete(&self, ticket: &str, cookie: &str) -> Value {
        let response = self.request("POST", "/api/login/google/complete",
            &json!({"ticket":ticket}).to_string(), Some(cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let cookies: Vec<_> = response.headers().get_all(header::SET_COOKIE).iter()
            .map(|h| h.to_str().unwrap()).collect();
        assert_eq!(cookies.len(), 2);
        assert!(cookies.iter().all(|cookie| cookie.contains("Max-Age=0")));
        body(response).await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        PENDING.lock().unwrap().retain(|_, p| p.home != self.state.home_dir);
        TICKETS.lock().unwrap().retain(|_, t| t.home != self.state.home_dir);
        NOTIFIED.lock().unwrap().retain(|(home, _), _| home != &self.state.home_dir);
    }
}
struct Started { cookie: String, set_cookie: String, query: HashMap<String, String> }
async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn ticket(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    let location = response.headers()[header::LOCATION].to_str().unwrap();
    assert!(location.starts_with(&format!("{BASE}/login#gsi=")), "{location}");
    location.split_once("#gsi=").unwrap().1.to_owned()
}
fn assert_expired(response: &Response) {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], format!("{BASE}/login#gsi_error=expired"));
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
}

struct Keys { pem: String, jwks: Value }
fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let public = RsaPublicKey::from(&private);
        Keys { pem: private.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
            jwks: json!({"keys":[{"kty":"RSA","alg":"RS256","use":"sig","kid":"route-key",
                "n":URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
                "e":URL_SAFE_NO_PAD.encode(public.e().to_bytes_be())}]}) }
    })
}
struct Provider {
    base: String,
    task: tokio::task::JoinHandle<()>,
    token: Arc<Mutex<Option<String>>>,
    token_hits: Arc<AtomicUsize>,
    jwks_hits: Arc<AtomicUsize>,
}
impl Drop for Provider { fn drop(&mut self) { self.task.abort(); } }
impl Provider {
    async fn new() -> Self {
        let token = Arc::new(Mutex::new(None::<String>));
        let token_hits = Arc::new(AtomicUsize::new(0));
        let jwks_hits = Arc::new(AtomicUsize::new(0));
        let served = token.clone();
        let tokens = token_hits.clone();
        let jwks = jwks_hits.clone();
        let app = Router::new()
            .route("/oauth2.googleapis.com/token", post(move |Form(form): Form<HashMap<String, String>>| {
                tokens.fetch_add(1, Ordering::SeqCst);
                assert_eq!(form["client_id"], CLIENT);
                assert_eq!(form["client_secret"], "test-secret");
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["redirect_uri"], format!("{BASE}{CALLBACK_PATH}"));
                assert!(random_shape(&form["code_verifier"]));
                let value = served.lock().unwrap().clone().unwrap_or_default();
                async move { Json(json!({"id_token":value,"access_token":"discarded"})) }
            }))
            .route("/www.googleapis.com/oauth2/v3/certs", get(move || {
                jwks.fetch_add(1, Ordering::SeqCst);
                async { Json(keys().jwks.clone()) }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        Self { base, task, token, token_hits, jwks_hits }
    }
    fn prepare(&self, flow: &Started, change: impl FnOnce(&mut Value)) {
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({"iss":"https://accounts.google.com","aud":CLIENT,"azp":CLIENT,
            "sub":"123456789","email":EMAIL,"email_verified":true,"hd":"example.com",
            "name":"New Person","iat":now,"exp":now+3600,"nonce":flow.query["nonce"]});
        change(&mut claims);
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("route-key".into());
        *self.token.lock().unwrap() = Some(encode(&header, &claims,
            &EncodingKey::from_rsa_pem(keys().pem.as_bytes()).unwrap()).unwrap());
    }
    fn assert_no_outbound(&self) {
        assert_eq!(self.token_hits.load(Ordering::SeqCst), 0);
        assert_eq!(self.jwks_hits.load(Ordering::SeqCst), 0);
    }
}
async fn successful_callback(f: &Fixture, provider: &Provider) -> (Started, String) {
    let flow = f.start().await;
    provider.prepare(&flow, |_| {});
    let ticket = ticket(f.callback(provider, &flow, Some(&flow.cookie), "").await);
    (flow, ticket)
}

#[tokio::test]
async fn methods_off_by_default() {
    let f = Fixture::new(false, false, None, false, "admin").await;
    std::fs::write(f.state.home_dir.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
    let response = f.request("GET", "/api/login/methods", "", None).await;
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(body(response).await, json!({"google":"off"}));
}
#[tokio::test]
async fn methods_needs_public_url() {
    for base in [None, Some("http://public.example"), Some("https://user@company.example"), Some("https://company.example?next=x")] {
        let f = Fixture::new(false, true, base, false, "admin").await;
        assert_eq!(body(f.request("GET", "/api/login/methods", "", None).await).await,
            json!({"google":"needs_public_url"}));
    }
}
#[tokio::test]
async fn start_refused_when_off() {
    let f = Fixture::new(false, false, Some(BASE), false, "admin").await;
    let response = f.request("POST", "/api/login/google/start", "{}", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(body(response).await, json!({"error":"登入沒有完成，請稍後再試。"}));
}
#[tokio::test]
async fn start_refused_personal_edition() {
    let f = Fixture::new(true, true, Some(BASE), false, "direct").await;
    assert_eq!(f.request("POST", "/api/login/google/start", "{}", None).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(f.request("GET", "/api/login/methods", "", None).await).await, json!({"google":"off"}));
}
#[tokio::test]
async fn callback_without_cookie_refused() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    provider.prepare(&flow, |_| {});
    assert_expired(&f.callback(&provider, &flow, None, "").await);
    provider.assert_no_outbound();
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "").await);
}
#[tokio::test]
async fn ticket_other_cookie_refused() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    let other_cookie = format!("__Host-dd_gsi={}", oidc::random_state().unwrap());
    assert_eq!(f.complete(&ticket, &other_cookie).await, json!({"result":"expired"}));
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"expired"}));
}
#[tokio::test]
async fn state_replay() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, _) = successful_callback(&f, &provider).await;
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    assert_eq!(provider.token_hits.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn no_state_no_outbound() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    for query in ["code=test-code".to_owned(), format!("code=test-code&state={}", oidc::random_state().unwrap())] {
        let response = oauth::TEST_ENDPOINT_BASE.scope(provider.base.clone(), f.request("GET",
            &format!("/api/login/google/callback?{query}"), "", None)).await;
        assert_expired(&response);
    }
    provider.assert_no_outbound();
}
#[tokio::test]
async fn callback_redirect_is_fixed() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    assert_eq!(flow.query["redirect_uri"], format!("{BASE}{CALLBACK_PATH}"));
    provider.prepare(&flow, |_| {});
    let response = f.callback(&provider, &flow, Some(&flow.cookie),
        "&redirect=https%3A%2F%2Fevil.example&next=%2F%2Fevil.example").await;
    ticket(response);
}
#[tokio::test]
async fn nonce_mismatch() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    provider.prepare(&flow, |claims| { claims["nonce"] = json!("wrong-nonce"); });
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    assert!(f.state.user_db.list_users().unwrap().is_empty());
}
#[tokio::test]
async fn personal_account_code() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    provider.prepare(&flow, |claims| { claims.as_object_mut().unwrap().remove("hd"); });
    let ticket = ticket(f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"personal_account"}));
}
#[tokio::test]
async fn domain_not_allowed_code() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    provider.prepare(&flow, |claims| { claims["hd"] = json!("other.example"); });
    let ticket = ticket(f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"domain_not_allowed"}));
}
#[tokio::test]
async fn ticket_replay() {
    let f = Fixture::new(false, true, Some(BASE), false, "admin").await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"pending_approval"}));
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"expired"}));
}
#[tokio::test]
async fn login_without_attestation_does_not_take_grant_lease() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, first) = successful_callback(&f, &provider).await;
    let first_login = f.complete(&first, &flow.cookie).await;
    assert!(first_login["access_token"].is_string());
    let held = crate::google_personal::grants::acquire(&f.state.home_dir).await.unwrap();
    let (flow, ticket) = tokio::time::timeout(Duration::from_secs(2),
        successful_callback(&f, &provider)).await.expect("linked login must not wait for grant work");
    let login = f.complete(&ticket, &flow.cookie).await;
    assert!(login["access_token"].is_string());
    assert_eq!(login["user"]["id"], first_login["user"]["id"]);
    drop(held);
}

#[tokio::test]
async fn complete_issues_tokens_when_mfa_not_enforced() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    let value = f.complete(&ticket, &flow.cookie).await;
    let token = value["access_token"].as_str().unwrap();
    assert!(f.state.jwt_config.verify_access_token(token).is_ok());
    assert!(value["refresh_token"].is_string());
    assert_eq!(value["user"]["role"], "employee");
    assert!(!value["user"]["must_change_password"].as_bool().unwrap());
    let db = rusqlite::Connection::open(f.state.home_dir.join("users.db")).unwrap();
    let detail: String = db.query_row("SELECT detail FROM auth_audit_log WHERE action='login'", [], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&detail).unwrap(), json!({"method":"google"}));
}
#[tokio::test]
async fn google_login_requires_enrollment_when_enforced() {
    let f = Fixture::new(false, true, Some(BASE), true, "direct").await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    let value = f.complete(&ticket, &flow.cookie).await;
    assert_eq!(value["mfa_enrollment_required"], true);
    assert!(value["enrollment_token"].is_string());
    assert!(value.get("access_token").is_none());
    assert!(value.get("refresh_token").is_none());
    assert_eq!(value["email_available"], false);
    assert!(value["factors"].get("email").is_none());
}
#[tokio::test]
async fn pending_approval_code() {
    let f = Fixture::new(false, true, Some(BASE), false, "admin").await;
    let provider = Provider::new().await;
    for _ in 0..2 {
        let (flow, ticket) = successful_callback(&f, &provider).await;
        assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"pending_approval"}));
    }
    assert!(f.state.user_db.list_users().unwrap().is_empty());
    let requests = f.state.user_db.list_pending_signin_requests().unwrap();
    assert_eq!(requests.len(), 1);
    let key = (f.state.home_dir.clone(), format!("request:{}", requests[0].id));
    assert!(NOTIFIED.lock().unwrap().contains_key(&key));
}
#[tokio::test]
async fn existing_account_code() {
    let f = Fixture::ready().await;
    let user = f.state.user_db.create_user(EMAIL, "Existing", "test-password-long", UserRole::Employee).unwrap();
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"existing_account"}));
    assert!(f.state.user_db.google_signin_link_for_user(&user.id).unwrap().is_none());
}
#[tokio::test]
async fn login_attestation_refused_when_personal_connection_differs() {
    use crate::person_credentials::{self as vault, Metadata, Secret, Status};

    let f = Fixture::ready().await;
    let db = &f.state.user_db;
    let home = &f.state.home_dir;
    let user = db.create_user(EMAIL, "Existing", "test-password-long", UserRole::Employee).unwrap();
    let admin = db.create_user("admin@example.com", "Admin", "test-password-long", UserRole::Admin).unwrap();
    db.attest_google_signin_users(&admin.id, admin.credential_epoch, std::slice::from_ref(&user.id),
        chrono::Utc::now().timestamp()).unwrap();
    duduclaw_core::gateway_instance::acquire(home).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(home).enable(false).unwrap();
    let key = crate::google_identity::protection_gate(home).unwrap();
    let epoch = vault::begin_connection(home, &user.id).unwrap();
    vault::connect(home, &key, Metadata {
        owner_kind: "user".into(), owner_id: user.id.clone(), provider: "google".into(),
        credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
        account_email: EMAIL.into(), google_sub: "987654321".into(),
        hosted_domain: Some("example.com".into()), granted_scopes: vec!["openid".into(), "email".into()],
        local_purposes: vec!["base".into()], status: Status::Connected, status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None, oauth_client_id: CLIENT.into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue(&user.id).unwrap()),
    }, &Secret {
        access_token: "fixture-access-token".into(), refresh_token: Some("fixture-refresh-token".into()),
        expires_at: Some(4_000_000_000),
    }, epoch).unwrap();
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"existing_account"}));
    assert!(db.google_signin_link_for_user(&user.id).unwrap().is_none());
    assert_eq!(me::personal_sub(home, &user.id).unwrap().as_deref(), Some("987654321"));
    let conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
    let attestations: i64 = conn.query_row("SELECT COUNT(*) FROM google_signin_attestations WHERE user_id=?1",
        [&user.id], |row| row.get(0)).unwrap();
    assert_eq!(attestations, 1);
}

#[tokio::test]
async fn suspended_user_ticket_expired() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    let user = f.state.user_db.get_user_by_email(EMAIL).unwrap().unwrap();
    f.state.user_db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"expired"}));
}
#[tokio::test]
async fn cookie_flags() {
    for (base, secure) in [(BASE, true), ("http://localhost:18889", false)] {
        let f = Fixture::new(false, true, Some(base), false, "direct").await;
        let flow = f.start().await;
        assert!(flow.set_cookie.starts_with(&format!("{}=", cookie_name(secure))));
        assert_eq!(flow.set_cookie.contains("; Secure"), secure);
        for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=600"] {
            assert!(flow.set_cookie.split(';').any(|part| part.trim() == flag));
        }
        assert_eq!(flow.query["redirect_uri"], format!("{base}{CALLBACK_PATH}"));
    }
}

#[tokio::test]
async fn provider_error_and_changed_settings_do_not_exchange() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "&error=access_denied").await);
    let flow = f.start().await;
    let config_path = f.state.home_dir.join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap().replace("daily_new_accounts=20", "daily_new_accounts=19");
    std::fs::write(config_path, config).unwrap();
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    provider.assert_no_outbound();
}
#[tokio::test]
async fn json_unknown_fields_rejected_and_complete_always_clears_cookie() {
    let f = Fixture::ready().await;
    assert_eq!(f.request("POST", "/api/login/google/start", r#"{"remember":true}"#, None).await.status(), StatusCode::BAD_REQUEST);
    for payload in [r#"{"ticket":"unused","next":"https://evil.example"}"#, "not json"] {
        let response = f.request("POST", "/api/login/google/complete", payload, None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers().get_all(header::SET_COOKIE).iter().count(), 2);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}
#[tokio::test]
async fn google_active_email_factor_is_not_offered() {
    let f = Fixture::ready().await;
    let user = f.state.user_db.create_user(EMAIL, "Existing", "test-password-long", UserRole::Employee).unwrap();
    let now = chrono::Utc::now().timestamp();
    let challenge = f.state.user_db.mfa_factor_password_challenge(&user.id, "test-password-long", "session", now).unwrap();
    let (_, code) = f.state.user_db.mfa_email_send(&challenge, true, now).unwrap();
    f.state.user_db.mfa_email_verify_and_issue(&challenge, &code, true, now, |_, _, _| Ok(())).unwrap();
    let password = body(server_mfa::after_first_factor(&f.state, &user, FirstFactor::Password).await.unwrap()).await;
    assert_eq!(password["factors"]["email"], true);
    let google = body(server_mfa::after_first_factor(&f.state, &user, FirstFactor::GoogleOidc).await.unwrap()).await;
    assert_eq!(google["mfa_required"], true);
    assert_eq!(google["email_available"], false);
    assert!(google["factors"].get("email").is_none());
    assert_ne!(google["last_used"], "email");
    assert!(google.get("access_token").is_none());
}

#[tokio::test]
async fn credential_epoch_changed_ticket_expired() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let (flow, ticket) = successful_callback(&f, &provider).await;
    let user = f.state.user_db.get_user_by_email(EMAIL).unwrap().unwrap();
    // Simulate a credential revocation after the callback's account decision.
    let conn = rusqlite::Connection::open(f.state.home_dir.join("users.db")).unwrap();
    conn.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&user.id]).unwrap();
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"expired"}));
}
#[tokio::test]
async fn daily_limit_notified_once_per_calendar_day() {
    let f = Fixture::ready().await;
    let path = f.state.home_dir.join("config.toml");
    let config = std::fs::read_to_string(&path).unwrap().replace("daily_new_accounts=20", "daily_new_accounts=1");
    std::fs::write(path, config).unwrap();
    let provider = Provider::new().await;
    let (flow, first) = successful_callback(&f, &provider).await;
    assert!(f.complete(&first, &flow.cookie).await["access_token"].is_string());
    let mut sent_at = None;
    for sub in ["987654321", "987654322"] {
        let flow = f.start().await;
        provider.prepare(&flow, |claims| {
            claims["sub"] = json!(sub);
            claims["email"] = json!(format!("new-{sub}@example.com"));
        });
        let ticket = ticket(f.callback(&provider, &flow, Some(&flow.cookie), "").await);
        assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"daily_limit"}));
        let key = (f.state.home_dir.clone(), format!("daily:{}", chrono::Utc::now().date_naive()));
        let at = *NOTIFIED.lock().unwrap().get(&key).unwrap();
        if let Some(first) = sent_at { assert_eq!(first, at); } else { sent_at = Some(at); }
    }
}
#[tokio::test]
async fn state_and_ticket_expiry() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let flow = f.start().await;
    PENDING.lock().unwrap().get_mut(&hash(&flow.query["state"])).unwrap().created = Instant::now() - STATE_TTL;
    assert_expired(&f.callback(&provider, &flow, Some(&flow.cookie), "").await);
    provider.assert_no_outbound();
    let (flow, ticket) = successful_callback(&f, &provider).await;
    TICKETS.lock().unwrap().get_mut(&hash(&ticket)).unwrap().created = Instant::now() - TICKET_TTL;
    assert_eq!(f.complete(&ticket, &flow.cookie).await, json!({"result":"expired"}));
}
#[tokio::test]
async fn public_handlers_rate_limit() {
    let f = Fixture::new(false, false, Some(BASE), false, "admin").await;
    for _ in 0..30 {
        assert_eq!(f.request("POST", "/api/login/google/start", "{}", None).await.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(f.request("POST", "/api/login/google/start", "{}", None).await.status(), StatusCode::TOO_MANY_REQUESTS);
    for _ in 0..30 {
        assert_eq!(f.request("POST", "/api/login/google/complete", r#"{"ticket":"unknown"}"#, None).await.status(), StatusCode::OK);
    }
    let response = f.request("POST", "/api/login/google/complete", r#"{"ticket":"unknown"}"#, None).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers().get_all(header::SET_COOKIE).iter().count(), 2);
    let provider = Provider::new().await;
    for _ in 0..31 {
        let response = oauth::TEST_ENDPOINT_BASE.scope(provider.base.clone(),
            f.request("GET", "/api/login/google/callback?state=unknown&code=unused", "", None)).await;
        assert_expired(&response);
    }
    provider.assert_no_outbound();
}

#[tokio::test]
async fn start_limiter_allows_thirty_then_refuses() {
    let f = Fixture::ready().await;
    for _ in 0..30 {
        let flow = f.start().await;
        // Release pending capacity so this test isolates the request budget.
        assert!(take_pending(&flow.query["state"]).is_some());
    }
    assert_eq!(f.request("POST", "/api/login/google/start", "{}", None).await.status(),
        StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn pending_per_ip_cap() {
    let mut f = Fixture::ready().await;
    for _ in 0..8 { f.start().await; }
    assert_eq!(f.request("POST", "/api/login/google/start", "{}", None).await.status(),
        StatusCode::SERVICE_UNAVAILABLE);
    // Keep the same home and pending states, changing only the connected peer.
    f.peer = SocketAddr::from(([203, 0, 113, 214], 12000));
    f.start().await;
}

#[tokio::test]
async fn google_limiter_independent_of_password_limiter() {
    let f = Fixture::ready().await;
    for key in [EMAIL, "google-start"] {
        for _ in 0..5 { assert!(crate::server::check_login_rate_limit(f.peer.ip(), key)); }
        assert!(!crate::server::check_login_rate_limit(f.peer.ip(), key));
    }
    f.start().await;
    for key in [EMAIL, "google-start"] {
        crate::server::reset_login_rate_limit(f.peer.ip(), key);
    }
}

#[tokio::test]
async fn login_start_cannot_create_link_or_reauth_pending() {
    let f = Fixture::ready().await;
    for purpose in ["link", "reauth"] {
        let response = f.request("POST", "/api/login/google/start", &json!({"purpose":purpose}).to_string(), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let flow = f.start().await;
    assert_eq!(take_pending(&flow.query["state"]).unwrap().purpose, Purpose::Login);
}

#[tokio::test]
async fn self_link_callback_is_cookie_free_and_never_logs_in_or_provisions() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let user = f.state.user_db.create_user(EMAIL, "Existing", "password123", UserRole::Employee).unwrap();
    let binding = SessionBinding { user_id: user.id.clone(), session_id: uuid::Uuid::new_v4().to_string(), epoch: user.credential_epoch };
    let url = me::start(&f.state.home_dir, &f.state.user_db, binding.clone(), false, "password123").unwrap();
    let flow = Started { cookie: String::new(), set_cookie: String::new(),
        query: url::Url::parse(&url).unwrap().query_pairs().into_owned().collect() };
    provider.prepare(&flow, |_| {});
    let user_count = f.state.user_db.list_users().unwrap().len();
    let response = f.callback(&provider, &flow, None, "").await;
    let location = response.headers()[header::LOCATION].to_str().unwrap();
    assert!(location.starts_with(&format!("{BASE}/me/connections#gsi_link=")));
    let ticket = location.split_once("#gsi_link=").unwrap().1.to_owned();
    assert!(f.state.user_db.google_signin_link_for_user(&user.id).unwrap().is_none());
    assert_eq!(f.state.user_db.list_users().unwrap().len(), user_count);
    assert!(f.state.user_db.signin_request_by_sub("123456789").unwrap().is_none());
    me::complete(&f.state.home_dir, &f.state.user_db, &binding, &ticket, false, chrono::Utc::now().timestamp()).unwrap();
    assert_eq!(f.state.user_db.google_signin_link_for_user(&user.id).unwrap().unwrap().google_sub, "123456789");
    assert!(me::complete(&f.state.home_dir, &f.state.user_db, &binding, &ticket, false, chrono::Utc::now().timestamp()).is_err());
    let calls = provider.token_hits.load(Ordering::SeqCst);
    assert_expired(&f.callback(&provider, &flow, None, "").await);
    assert_eq!(provider.token_hits.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn self_link_refuses_google_email_different_from_account() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let user = f.state.user_db.create_user("existing@example.com", "Existing", "password123", UserRole::Employee).unwrap();
    let binding = SessionBinding { user_id: user.id.clone(), session_id: uuid::Uuid::new_v4().to_string(), epoch: user.credential_epoch };
    let url = me::start(&f.state.home_dir, &f.state.user_db, binding.clone(), false, "password123").unwrap();
    let flow = Started { cookie: String::new(), set_cookie: String::new(),
        query: url::Url::parse(&url).unwrap().query_pairs().into_owned().collect() };
    provider.prepare(&flow, |_| {});
    let response = f.callback(&provider, &flow, None, "").await;
    let location = response.headers()[header::LOCATION].to_str().unwrap();
    let ticket = location.split_once("#gsi_link=").unwrap().1.to_owned();

    assert_eq!(
        me::complete(&f.state.home_dir, &f.state.user_db, &binding, &ticket, false, chrono::Utc::now().timestamp()).unwrap_err(),
        "signin_link_email_mismatch"
    );
    assert!(f.state.user_db.google_signin_link_for_user(&user.id).unwrap().is_none());
}

#[tokio::test]
async fn self_link_callback_stale_session_makes_no_outbound_request() {
    let f = Fixture::ready().await;
    let provider = Provider::new().await;
    let user = f.state.user_db.create_user("existing@example.com", "Existing", "password123", UserRole::Employee).unwrap();
    let binding = SessionBinding { user_id: user.id.clone(), session_id: uuid::Uuid::new_v4().to_string(), epoch: user.credential_epoch };
    let url = me::start(&f.state.home_dir, &f.state.user_db, binding, false, "password123").unwrap();
    let flow = Started { cookie: String::new(), set_cookie: String::new(),
        query: url::Url::parse(&url).unwrap().query_pairs().into_owned().collect() };
    provider.prepare(&flow, |_| {});
    rusqlite::Connection::open(f.state.home_dir.join("users.db")).unwrap()
        .execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&user.id]).unwrap();
    let response = f.callback(&provider, &flow, None, "").await;
    assert_eq!(response.headers()[header::LOCATION], format!("{BASE}/me/connections#gsi_error=expired"));
    provider.assert_no_outbound();
}
