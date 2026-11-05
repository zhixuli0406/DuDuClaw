//! In-process route tests; never bind a listener or contact a provider.
use super::*;
use axum::{body::{Body, Bytes}, http::Request};
use duduclaw_auth::UserRole;
use std::{io::Cursor, sync::atomic::{AtomicUsize, Ordering}};
use tower::ServiceExt;

struct Fixture {
    _serial: tokio::sync::MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    state: Arc<AppState>,
    admin: duduclaw_auth::User,
    token: String,
    employee_token: String,
    addr: SocketAddr,
}

impl Fixture {
    async fn new() -> Self {
        // Route tests share production upload slots. Keep unrelated test jobs
        // from consuming one another's capacity and causing spurious 429s.
        static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let serial = SERIAL.lock().await;
        let home = tempfile::tempdir().unwrap();
        let user_db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = user_db.create_user("admin@example.test", "Admin", "test-password", UserRole::Admin).unwrap();
        let employee = user_db.create_user("employee@example.test", "Employee", "test-password", UserRole::Employee).unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"company-api-test-secret-32-bytes!!!!"));
        let token = jwt_config.issue_access_token(&admin, &[]).unwrap();
        let employee_token = jwt_config.issue_access_token(&employee, &[]).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None), handler: MethodHandler::new(home.path().to_path_buf()).await,
            tx, event_tx, prepared_events: Default::default(), user_db, jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.path().to_path_buf(), reqwest::Client::new())),
            home_dir: home.path().to_path_buf(),
        });
        static NEXT_PEER: AtomicUsize = AtomicUsize::new(1);
        let octet = NEXT_PEER.fetch_add(1, Ordering::Relaxed) as u8;
        Self { _serial: serial, _home: home, state, admin, token, employee_token,
            addr: SocketAddr::from(([198, 51, 100, octet], 4000)) }
    }

    async fn send(&self, request: Request<Body>) -> axum::response::Response {
        Router::new().route("/api/company/logo", upload_route())
            .layer(axum::Extension(ConnectInfo(self.addr))).with_state(self.state.clone())
            .oneshot(request).await.unwrap()
    }

    fn request(&self, token: Option<&str>, content_type: &str, body: Body) -> Request<Body> {
        let mut request = Request::post("/api/company/logo").header(header::CONTENT_TYPE, content_type);
        if let Some(token) = token { request = request.header(header::AUTHORIZATION, format!("Bearer {token}")); }
        request.body(body).unwrap()
    }
}

fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(600, 300).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn unpolled_body() -> Body {
    Body::from_stream(futures_util::stream::poll_fn(|_| -> std::task::Poll<Option<Result<Bytes, std::io::Error>>> {
        panic!("unauthorized/rejected request polled the body")
    }))
}

#[tokio::test]
async fn company_upload_authentication_and_live_admin_precede_body_poll() {
    let fixture = Fixture::new().await;
    let refresh = fixture.state.jwt_config.issue_refresh_token(&fixture.admin.id).unwrap();
    for token in [None, Some("invalid-jwt"), Some(refresh.as_str())] {
        let mut request = fixture.request(token, "image/png", unpolled_body());
        // Neither cookies nor query credentials authorize uploads. Oversize
        // headers also must not mask the authentication response.
        request.headers_mut().insert(header::COOKIE, format!("access_token={}", fixture.token).parse().unwrap());
        request.headers_mut().insert(header::CONTENT_LENGTH, (MAX_LOGO_BYTES + 1).to_string().parse().unwrap());
        *request.uri_mut() = format!("/api/company/logo?token={}", fixture.token).parse().unwrap();
        assert_eq!(fixture.send(request).await.status(), StatusCode::UNAUTHORIZED);
    }
    let request = fixture.request(Some(&fixture.employee_token), "image/png", unpolled_body());
    assert_eq!(fixture.send(request).await.status(), StatusCode::FORBIDDEN);
    // The JWT still says Admin. The database no longer does.
    fixture.state.user_db.update_user(&fixture.admin.id, None, Some(UserRole::Employee), None).unwrap();
    let request = fixture.request(Some(&fixture.token), "image/png", unpolled_body());
    assert_eq!(fixture.send(request).await.status(), StatusCode::FORBIDDEN);
    assert!(!fixture.state.home_dir.join("company_logos").exists());
    let audit = std::fs::read_to_string(fixture.state.home_dir.join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("company.logo.upload"));
    assert!(audit.contains("refused"));
    assert!(!audit.contains(&fixture.token));
}

#[tokio::test]
async fn company_upload_raw_png_persists_name_and_audits_success() {
    let fixture = Fixture::new().await;
    crate::company_info::update(&fixture.state.home_dir,
        crate::company_info::CompanyUpdate { name: "Acme".into(), remove_logo: false }, || Ok(())).unwrap();
    let response = fixture.send(fixture.request(Some(&fixture.token), "image/png", Body::from(png()))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let info: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(info["name"], "Acme");
    let filename = info["logo_url"].as_str().unwrap().rsplit('/').next().unwrap();
    let saved = crate::company_info::read_logo(&fixture.state.home_dir, filename).unwrap();
    let decoded = image::load_from_memory(&saved).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 128));
    assert_eq!(crate::company_info::read(&fixture.state.home_dir).unwrap().logo_url.as_deref(), info["logo_url"].as_str());
    let audit = std::fs::read_to_string(fixture.state.home_dir.join("security_audit.jsonl")).unwrap();
    let row = audit.lines().filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .find(|v| v["details"]["method"] == "company.logo.upload").unwrap();
    assert_eq!(row["details"]["status"], "ok");
    assert_eq!(row["details"]["actor_user_id"], fixture.admin.id);
}

#[tokio::test]
async fn company_upload_rejects_content_types_mismatch_encoding_and_invalid_images() {
    let fixture = Fixture::new().await;
    for content_type in ["image/svg+xml", "application/octet-stream"] {
        assert_eq!(fixture.send(fixture.request(Some(&fixture.token), content_type, unpolled_body())).await.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let mut encoded = fixture.request(Some(&fixture.token), "image/png", unpolled_body());
    encoded.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    assert_eq!(fixture.send(encoded).await.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/jpeg", Body::from(png()))).await.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let mut broken = png();
    broken.truncate(broken.len() - 1);
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", Body::from(broken))).await.status(), StatusCode::BAD_REQUEST);
    assert!(!fixture.state.home_dir.join("company_logos").exists());
}

#[tokio::test]
async fn company_upload_declared_and_chunked_overflow_are_bounded() {
    let fixture = Fixture::new().await;
    let mut request = fixture.request(Some(&fixture.token), "image/png", unpolled_body());
    request.headers_mut().insert(header::CONTENT_LENGTH, (MAX_LOGO_BYTES + 1).to_string().parse().unwrap());
    assert_eq!(fixture.send(request).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let chunks = || Body::from_stream(futures_util::stream::iter([
        Ok::<_, std::io::Error>(Bytes::from(vec![0; MAX_LOGO_BYTES])), Ok(Bytes::from_static(b"x")),
    ]));
    // Exercise the production DefaultBodyLimit and the independent hard cap.
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", chunks())).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(read_upload(chunks()).await.unwrap_err(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(read_upload(Body::from(vec![0; MAX_LOGO_BYTES])).await.unwrap().len(), MAX_LOGO_BYTES);
    assert!(!fixture.state.home_dir.join("company_logos").exists());
}

#[tokio::test]
async fn company_upload_rechecks_admin_after_body_before_persisting() {
    let fixture = Fixture::new().await;
    let db = fixture.state.user_db.clone();
    let id = fixture.admin.id.clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        db.update_user(&id, None, Some(UserRole::Employee), None).unwrap();
        Ok::<_, std::io::Error>(Bytes::from(png()))
    }));
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", body)).await.status(), StatusCode::FORBIDDEN);
    assert!(!fixture.state.home_dir.join("company_logos").exists());
}

#[tokio::test]
async fn company_upload_rate_limits_account_and_peer_independently() {
    use duduclaw_security::rate_limiter::RateLimiter;
    let limiter = RateLimiter::new(6, std::time::Duration::from_secs(60));
    for _ in 1..=6 { assert!(upload_account_admitted(&limiter, "one-admin").await); }
    assert!(!upload_account_admitted(&limiter, "one-admin").await);
    let limiter = RateLimiter::new(6, std::time::Duration::from_secs(60));
    for _ in 1..=6 { assert!(upload_ip_admitted(&limiter, IpAddr::from([192, 0, 2, 8])).await); }
    assert!(!upload_ip_admitted(&limiter, IpAddr::from([192, 0, 2, 8])).await);
    let fixture = Fixture::new().await;
    for _ in 0..6 {
        assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "bad/type", unpolled_body())).await.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let response = fixture.send(fixture.request(Some(&fixture.token), "image/png", unpolled_body())).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "60");
}

#[tokio::test]
async fn company_upload_account_limit_survives_peer_changes() {
    let mut fixture = Fixture::new().await;
    for i in 1..=6 {
        fixture.addr.set_ip(IpAddr::from([203, 0, 113, i]));
        assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "bad/type", unpolled_body())).await.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    fixture.addr.set_ip(IpAddr::from([203, 0, 113, 7]));
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", unpolled_body())).await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn company_upload_limits_unauthenticated_audit_work_without_body_poll() {
    let fixture = Fixture::new().await;
    for _ in 0..6 {
        assert_eq!(fixture.send(fixture.request(None, "image/png", unpolled_body())).await.status(), StatusCode::UNAUTHORIZED);
    }
    let path = fixture.state.home_dir.join("security_audit.jsonl");
    let before = std::fs::read(&path).unwrap();
    assert_eq!(fixture.send(fixture.request(None, "image/png", unpolled_body())).await.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test(start_paused = true)]
async fn company_upload_busy_and_slow_bodies_release_capacity() {
    let fixture = Fixture::new().await;
    let first = UPLOAD_SLOTS.clone().try_acquire_owned().unwrap();
    let second = UPLOAD_SLOTS.clone().try_acquire_owned().unwrap();
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", unpolled_body())).await.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    drop(second);
    let pending = Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    assert_eq!(fixture.send(fixture.request(Some(&fixture.token), "image/png", pending)).await.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(UPLOAD_SLOTS.available_permits(), 2);
    assert!(!fixture.state.home_dir.join("company_logos").exists());
}
