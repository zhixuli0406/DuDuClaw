//! P15 route authentication/JSON tests (no listener or provider).
use super::*;
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use crate::approval::{decision_service::tests::Fixture, chat_admin::{receipts, step_up}};

struct HttpFixture { service: Fixture, state: Arc<AppState>, token: String, raw: String }
impl HttpFixture {
    async fn new() -> Self {
        let service = Fixture::new();
        service.config(true, "telegram", &[]);
        let config_path = service.env.home.join("config.toml");
        let mut cfg = std::fs::read_to_string(&config_path).unwrap();
        cfg.push_str("[dashboard]\npublic_url='https://dashboard.example.test/'\n");
        std::fs::write(config_path, cfg).unwrap();
        rusqlite::Connection::open(service.env.home.join("users.db")).unwrap().execute(
            "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)", [&service.user]).unwrap();
        let id = service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
        let proof = service.proof(&id).await;
        let receipt = receipts::by_card_id(service.env.broker.store_for_tests(), &proof.card_id).await.unwrap().unwrap();
        let row = service.env.broker.get(&id).await.unwrap().unwrap();
        let outcome = step_up::begin(&service.env, &row, &receipt, crate::trigger_route::texts::Lang::ZhTw).await;
        let raw = outcome.card_link.unwrap().url.split_once("#ticket=").unwrap().1.to_owned();
        let user_db = service.env.users.clone().unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"approval-step-up-http-test-secret!!"));
        let user = user_db.get_user(&service.user).unwrap().unwrap();
        let token = jwt_config.issue_access_token(&user, &[]).unwrap();
        let home = service.env.home.clone();
        let handler = MethodHandler::new(home.clone()).await;
        handler.set_user_db(user_db.clone(), jwt_config.clone()).await;
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState { auth:AuthManager::new(Some("fixture-legacy-system-token".into())),
            handler: Arc::new(handler), tx, event_tx, prepared_events:Default::default(), user_db, jwt_config,
            otp_delivery:Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
            home_dir:home });
        Self { service, state, token, raw }
    }
    async fn request(&self, method: &str, token: &str, raw: &str, body: &str) -> axum::response::Response {
        let request = Request::builder().method(method).uri("/api/me/approvals/step-up")
            .header("authorization", format!("Bearer {token}"))
            .header("x-approval-ticket", raw).header("content-type", "application/json")
            .header("origin", "https://dashboard.example.test")
            .body(Body::from(body.to_owned())).unwrap();
        Router::new().route("/api/me/approvals/step-up", get(handle_approval_step_up_preview)
            .post(handle_approval_step_up_confirm).layer(DefaultBodyLimit::max(4096)))
            .with_state(self.state.clone()).oneshot(request).await.unwrap()
    }
}

#[tokio::test]
async fn member_key_system_mcp_and_sessionless_contexts_refused() {
    let f = HttpFixture::new().await;
    let user = f.state.user_db.get_user(&f.service.user).unwrap().unwrap();
    let member = f.state.user_db.create_user("member@test.invalid", "Member", "fixture-password-only",
        duduclaw_auth::UserRole::Employee).unwrap();
    let key = f.state.user_db.member_key_create(&member.id, "test", 1, member.credential_epoch, false, &[]).unwrap();
    let sessionless = f.state.jwt_config.issue_access_token_in_session(&user, &[], None).unwrap();
    let mut system = user.clone();
    system.id = "system".into();
    let system_jwt = f.state.jwt_config.issue_access_token(&system, &[]).unwrap();
    for token in [key.secret.as_str(), "fixture-legacy-system-token", "ddc_mcp_invalid",
        sessionless.as_str(), system_jwt.as_str()] {
        for method in ["GET", "POST"] {
            let response = f.request(method, token, &f.raw,
                r#"{"decision":"reject","expected_digest":"ignored"}"#).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["status"], "unauthenticated");
        }
    }
}

#[tokio::test]
async fn get_expired_shape_and_unknown_ticket_privacy() {
    let f = HttpFixture::new().await;
    let response = f.request("GET", &f.token, &f.raw, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    rusqlite::Connection::open(f.state.home_dir.join("protected/approval_step_up.db")).unwrap()
        .execute("UPDATE tickets SET expires=strftime('%s','now')-1", []).unwrap();
    let response = f.request("GET", &f.token, &f.raw, "").await;
    assert_eq!(response.status(), StatusCode::GONE);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"], "expired");
    assert_eq!(f.request("GET", &f.token, &"0".repeat(64), "").await.status(), StatusCode::NOT_FOUND);
    f.state.user_db.update_user(&f.service.user, None, Some(duduclaw_auth::UserRole::Employee), None).unwrap();
    assert_eq!(f.request("GET", &f.token, &f.raw, "").await.status(), StatusCode::FORBIDDEN);
}
