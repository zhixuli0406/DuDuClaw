//! Public invitation endpoints. They consume a capability but never create a session.
use super::*;
use axum::http::{HeaderValue, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// Only the TCP peer is trusted. Reverse-proxy headers never identify callers.
// None is the coarse peer budget; Some contains 16 hex chars of a token digest.
type InviteBuckets = HashMap<(IpAddr, Option<String>), (Instant, u32)>;
static INVITE_RATE_LIMITER: std::sync::LazyLock<Mutex<InviteBuckets>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn within_budget(map: &mut InviteBuckets, key: (IpAddr, Option<String>), now: Instant, limit: u32) -> bool {
    let entry = map.entry(key).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() >= 900 { *entry = (now, 0); }
    entry.1 = entry.1.saturating_add(1);
    entry.1 <= limit
}

fn rate_limit_at(map: &mut InviteBuckets, ip: IpAddr, token: &str, now: Instant) -> bool {
    if map.len() > 10000 {
        map.retain(|_, (start, _)| now.duration_since(*start).as_secs() < 900);
    }
    // Refused attempts also spend the coarse budget, without allocating more
    // token buckets once the peer has exhausted its allowance.
    if !within_budget(map, (ip, None), now, 300) { return false; }
    let digest = Sha256::digest(token.as_bytes());
    let prefix: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    within_budget(map, (ip, Some(prefix)), now, 10)
}

fn check_invite_rate_limit(ip: IpAddr, token: &str) -> bool {
    let mut map = INVITE_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    rate_limit_at(&mut map, ip, token, Instant::now())
}

fn response(status: StatusCode, body: Value) -> axum::response::Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

fn invalid() -> axum::response::Response {
    response(StatusCode::OK, json!({"valid": false}))
}

fn limited() -> axum::response::Response {
    response(StatusCode::TOO_MANY_REQUESTS, json!({"valid": false}))
}

fn masked_email(email: &str) -> String {
    // Do not reveal the local part (even a one-character mailbox).
    match email.rsplit_once('@') {
        Some((_, domain)) => format!("***@{domain}"),
        None => "***".to_string(),
    }
}

async fn inspect(db: Arc<UserDb>, token: String) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || db.user_invite_email(&token)).await {
        Ok(Ok(Some(email))) => response(StatusCode::OK, json!({"valid": true, "email_masked": masked_email(&email)})),
        _ => invalid(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct InviteToken {
    token: String,
}

pub(super) async fn check_invite(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Result<Json<InviteToken>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    let token = body.as_ref().map(|Json(body)| body.token.as_str()).unwrap_or("");
    if !check_invite_rate_limit(addr.ip(), token) { return limited(); }
    let Ok(Json(body)) = body else { return invalid(); };
    inspect(state.user_db.clone(), body.token).await
}

#[derive(serde::Deserialize)]
pub(super) struct InvitePassword {
    token: String,
    password: String,
}

async fn redeem(db: Arc<UserDb>, token: String, password: String) -> axum::response::Response {
    // Check validity before returning policy errors. A bad capability has one
    // response regardless of account state, lifetime, or password contents.
    let probe_db = db.clone();
    let probe_token = token.clone();
    if !matches!(tokio::task::spawn_blocking(move || probe_db.user_invite_email(&probe_token)).await, Ok(Ok(Some(_)))) {
        return invalid();
    }
    // Match the normal REST password-change minimum and the hashing maximum.
    if password.chars().count() < 8 || password.len() > 1024 {
        return response(StatusCode::BAD_REQUEST, json!({"valid": false, "code": "password_policy"}));
    }
    // The store rechecks the account and claims the invite inside the same
    // transaction as the password update, including the affected-row check.
    match tokio::task::spawn_blocking(move || db.consume_user_invite(&token, &password)).await {
        Ok(Ok(Some(_))) => response(StatusCode::OK, json!({"valid": true})),
        _ => invalid(),
    }
}

pub(super) async fn post_invite(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Result<Json<InvitePassword>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    let token = body.as_ref().map(|Json(body)| body.token.as_str()).unwrap_or("");
    if !check_invite_rate_limit(addr.ip(), token) { return limited(); }
    let Ok(Json(body)) = body else { return invalid(); };
    redeem(state.user_db.clone(), body.token, body.password).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{NewUser, UserRole, UserStatus};

    fn fixture() -> (tempfile::TempDir, Arc<UserDb>, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&dir.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@example.test", "Admin", "long password", UserRole::Admin).unwrap();
        let user = db.create_invited_user_with(&NewUser { email: "person@example.test", display_name: "Person", password: "", role: UserRole::Employee, department: None, manager: None }).unwrap();
        (dir, db, admin.id, user.id)
    }

    async fn read(response: axum::response::Response) -> (StatusCode, Value) {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn token_and_peer_budgets_expire_and_store_only_digest_prefixes() {
        let mut map = InviteBuckets::new();
        let ip = "192.0.2.101".parse().unwrap();
        let other = "192.0.2.102".parse().unwrap();
        let now = Instant::now();
        for _ in 0..10 { assert!(rate_limit_at(&mut map, ip, "secret-token", now)); }
        assert!(!rate_limit_at(&mut map, ip, "secret-token", now));
        assert!(map.contains_key(&(ip, Some("930bbdc51b6aed5c".into()))));
        assert!(map.keys().all(|(_, key)| key.as_ref().is_none_or(|key| key.len() == 16 && !key.contains("secret-token"))));
        assert!(rate_limit_at(&mut map, other, "secret-token", now));
        for index in 11..300 { assert!(rate_limit_at(&mut map, ip, &format!("token-{index}"), now)); }
        assert!(!rate_limit_at(&mut map, ip, "fresh-token", now));
        assert!(rate_limit_at(&mut map, ip, "secret-token", now + std::time::Duration::from_secs(900)));
    }

    #[tokio::test]
    async fn public_validity_masking_policy_and_no_session() {
        let (_dir, db, admin, user) = fixture();
        let invite = db.issue_user_invite(&user, &admin).unwrap();
        let valid = read(inspect(db.clone(), invite.token.clone()).await).await;
        assert_eq!(valid, (StatusCode::OK, json!({"valid":true, "email_masked":"***@example.test"})));
        let policy = read(redeem(db.clone(), invite.token.clone(), "short".into()).await).await;
        assert_eq!(policy, (StatusCode::BAD_REQUEST, json!({"valid":false,"code":"password_policy"})));
        assert_eq!(read(redeem(db.clone(), invite.token.clone(), "long password".into()).await).await, (StatusCode::OK, json!({"valid":true})));
        assert_eq!(read(inspect(db.clone(), invite.token.clone()).await).await, (StatusCode::OK, json!({"valid":false})));
        assert_eq!(read(redeem(db.clone(), invite.token, "long password".into()).await).await, (StatusCode::OK, json!({"valid":false})));
    }

    #[tokio::test]
    async fn unknown_revoked_used_and_disabled_have_identical_responses() {
        let (dir, db, admin, user) = fixture();
        let old = db.issue_user_invite(&user, &admin).unwrap();
        let current = db.issue_user_invite(&user, &admin).unwrap();
        let expected = read(inspect(db.clone(), "unknown".into()).await).await;
        assert_eq!(read(inspect(db.clone(), old.token.clone()).await).await, expected);
        assert_eq!(read(redeem(db.clone(), old.token, "short".into()).await).await, expected);
        // Expiry has the same HTTP body and status as unknown/revoked.
        let conn = rusqlite::Connection::open(dir.path().join("users.db")).unwrap();
        conn.execute("UPDATE user_invites SET expires_at=0 WHERE user_id=?1", [&user]).unwrap();
        assert_eq!(read(inspect(db.clone(), current.token.clone()).await).await, expected);
        assert_eq!(read(redeem(db.clone(), current.token, "long password".into()).await).await, expected);
        let current = db.issue_user_invite(&user, &admin).unwrap();
        db.set_user_status(&user, UserStatus::Suspended).unwrap();
        assert_eq!(read(inspect(db.clone(), current.token.clone()).await).await, expected);
        assert_eq!(read(redeem(db.clone(), current.token, "long password".into()).await).await, expected);
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn public_posts_share_token_budget_but_other_tokens_remain_usable() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let admin = db.create_user("admin@routes.test", "Admin", "long password", duduclaw_auth::UserRole::Admin).unwrap();
        let user = db.create_invited_user_with(&duduclaw_auth::NewUser {
            email: "new@routes.test", display_name: "New", password: "",
            role: duduclaw_auth::UserRole::Employee, department: None, manager: None,
        }).unwrap();
        let invite = db.issue_user_invite(&user.id, &admin.id).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None), handler: MethodHandler::new(home.clone()).await,
            tx, event_tx, prepared_events: Default::default(), user_db: db,
            jwt_config: Arc::new(JwtConfig::new(b"invitation-route-tests-at-least-32-bytes")),
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
            home_dir: home,
        });
        let app = Router::new().route("/api/invite/check", post(check_invite))
            .route("/api/invite/redeem", post(post_invite)).with_state(state);
        let peer: SocketAddr = "192.0.2.218:1234".parse().unwrap();
        // No credentials or sessions: the capability is supplied only in JSON.
        for (path, expected) in [
            ("/api/invite/check", json!({"valid":true,"email_masked":"***@routes.test"})),
            ("/api/invite/redeem", json!({"valid":true})),
            ("/api/invite/check", json!({"valid":false})),
        ] {
            let mut request = axum::http::Request::builder().method("POST").uri(path)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({"token":invite.token,"password":"new strong password"}).to_string())).unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            let result = app.clone().oneshot(request).await.unwrap();
            assert_eq!(result.status(), StatusCode::OK);
            assert!(result.headers().get(header::SET_COOKIE).is_none());
            let bytes = axum::body::to_bytes(result.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
        }
        for (method, path, expected) in [
            ("GET", "/api/invite/check", StatusCode::METHOD_NOT_ALLOWED),
            ("GET", "/api/invite/redeem", StatusCode::METHOD_NOT_ALLOWED),
            ("GET", "/api/invite/old-token", StatusCode::NOT_FOUND),
            ("POST", "/api/invite/old-token", StatusCode::NOT_FOUND),
        ] {
            let request = axum::http::Request::builder().method(method).uri(path)
                .body(axum::body::Body::empty()).unwrap();
            assert_eq!(app.clone().oneshot(request).await.unwrap().status(), expected);
        }

        for index in 0..12 {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri(if index % 2 == 0 { "/api/invite/check" } else { "/api/invite/redeem" })
                .header("host", format!("forged-{index}.example.test"))
                .header("x-forwarded-for", format!("192.0.2.{index}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({"token": if index == 11 { "another-token" } else { "same-token" }, "password":"long password"}).to_string()))
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            let result = app.clone().oneshot(request).await.unwrap();
            assert_eq!(result.status(), if index == 10 { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK });
            let body = axum::body::to_bytes(result.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({"valid":false}));
        }
    }
}
