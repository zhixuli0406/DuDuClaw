//! Public invitation endpoints. They consume a capability but never create a session.
use super::*;
use axum::http::{HeaderValue, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// Only the TCP peer is trusted. Reverse-proxy headers never identify callers.
// Tokens have 256 bits of entropy; a shared proxy peer needs no coarse budget.
use std::collections::{BTreeMap, BTreeSet};

const INVITE_BUCKET_CAPACITY: usize = 10_000;
type InviteBucketKey = (IpAddr, String);

struct InviteBucket {
    start: Instant,
    attempts: u32,
    order: u64,
}

#[derive(Default)]
struct InviteBuckets {
    entries: HashMap<InviteBucketKey, InviteBucket>,
    insertion_order: BTreeMap<u64, InviteBucketKey>,
    expiry_order: BTreeSet<(Instant, u64)>,
    next_order: u64,
}

static INVITE_RATE_LIMITER: std::sync::LazyLock<Mutex<InviteBuckets>> =
    std::sync::LazyLock::new(|| Mutex::new(InviteBuckets::default()));

fn rate_limit_at(map: &mut InviteBuckets, ip: IpAddr, token: &str, now: Instant) -> bool {
    let digest = Sha256::digest(token.as_bytes());
    let prefix: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    let key = (ip, prefix);
    if !map.entries.contains_key(&key) {
        if map.entries.len() == INVITE_BUCKET_CAPACITY {
            // Prefer an expired bucket, even if an older insertion was renewed.
            // One eviction makes room; ordered indexes avoid a whole-map scan.
            let expired = map.expiry_order.first()
                .filter(|(start, _)| now.duration_since(*start).as_secs() >= 900)
                .map(|(_, order)| *order);
            let oldest = expired.or_else(|| map.insertion_order.first_key_value().map(|(order, _)| *order));
            if let Some(order) = oldest {
                if let Some(oldest_key) = map.insertion_order.remove(&order) {
                    if let Some(bucket) = map.entries.remove(&oldest_key) {
                        map.expiry_order.remove(&(bucket.start, order));
                    }
                }
            }
        }
        let order = map.next_order;
        map.next_order += 1;
        map.insertion_order.insert(order, key.clone());
        map.expiry_order.insert((now, order));
        map.entries.insert(key.clone(), InviteBucket { start: now, attempts: 0, order });
    }
    let entry = map.entries.get_mut(&key).expect("invite bucket just inserted");
    if now.duration_since(entry.start).as_secs() >= 900 {
        map.expiry_order.remove(&(entry.start, entry.order));
        entry.start = now;
        entry.attempts = 0;
        map.expiry_order.insert((now, entry.order));
    }
    entry.attempts = entry.attempts.saturating_add(1);
    entry.attempts <= 10
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
    fn token_budgets_expire_and_store_only_digest_prefixes() {
        let mut map = InviteBuckets::default();
        let ip = "192.0.2.101".parse().unwrap();
        let other = "192.0.2.102".parse().unwrap();
        let now = Instant::now();
        for _ in 0..10 { assert!(rate_limit_at(&mut map, ip, "secret-token", now)); }
        assert!(!rate_limit_at(&mut map, ip, "secret-token", now));
        assert!(map.entries.contains_key(&(ip, "930bbdc51b6aed5c".into())));
        assert!(map.entries.keys().all(|(_, key)| key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit())));
        assert!(rate_limit_at(&mut map, other, "secret-token", now));
        assert!(!rate_limit_at(&mut map, ip, "secret-token", now + std::time::Duration::from_secs(899)));
        assert!(rate_limit_at(&mut map, ip, "secret-token", now + std::time::Duration::from_secs(900)));
        assert_eq!(map.entries.len(), 2);
        assert_eq!(map.insertion_order.len(), 2);
        assert_eq!(map.expiry_order.len(), 2);
    }

    #[tokio::test]
    async fn bogus_tokens_from_one_proxy_peer_do_not_block_a_real_invite() {
        let (_dir, db, admin, user) = fixture();
        let invite = db.issue_user_invite(&user, &admin).unwrap();
        let mut map = InviteBuckets::default();
        let ip = "192.0.2.101".parse().unwrap();
        let now = Instant::now();
        for index in 0..1000 {
            assert!(rate_limit_at(&mut map, ip, &format!("bogus-{index}"), now));
        }
        assert!(rate_limit_at(&mut map, ip, &invite.token, now));
        assert_eq!(read(inspect(db, invite.token).await).await.1["valid"], true);
    }

    #[test]
    fn buckets_and_order_indexes_stay_bounded_and_evict_oldest() {
        let mut map = InviteBuckets::default();
        let ip = "192.0.2.101".parse().unwrap();
        let now = Instant::now();
        for index in 0..INVITE_BUCKET_CAPACITY * 2 {
            // Exercise both live and expired capacity eviction.
            let at = now + std::time::Duration::from_secs(if index > INVITE_BUCKET_CAPACITY { 900 } else { 0 });
            assert!(rate_limit_at(&mut map, ip, &format!("bogus-{index}"), at));
            assert!(map.entries.len() <= INVITE_BUCKET_CAPACITY);
            assert_eq!(map.insertion_order.len(), map.entries.len());
            assert_eq!(map.expiry_order.len(), map.entries.len());
        }
        assert_eq!(map.entries.len(), INVITE_BUCKET_CAPACITY);
        let oldest = format!("{:x}", Sha256::digest(b"bogus-0"));
        assert!(!map.entries.contains_key(&(ip, oldest[..16].into())));
        let newest = format!("{:x}", Sha256::digest(format!("bogus-{}", INVITE_BUCKET_CAPACITY * 2 - 1).as_bytes()));
        assert!(map.entries.contains_key(&(ip, newest[..16].into())));
    }

    #[test]
    fn capacity_evicts_expired_buckets_before_renewed_oldest_insertion() {
        let mut map = InviteBuckets::default();
        let ip = "192.0.2.101".parse().unwrap();
        let now = Instant::now();
        for index in 0..INVITE_BUCKET_CAPACITY {
            assert!(rate_limit_at(&mut map, ip, &format!("token-{index}"), now));
        }
        let later = now + std::time::Duration::from_secs(900);
        for _ in 0..10 { assert!(rate_limit_at(&mut map, ip, "token-0", later)); }
        assert!(!rate_limit_at(&mut map, ip, "token-0", later));
        assert!(rate_limit_at(&mut map, ip, "fresh-token", later));
        // Renewing the oldest insertion must not let capacity eviction reset it.
        assert!(!rate_limit_at(&mut map, ip, "token-0", later));
        let expired = format!("{:x}", Sha256::digest(b"token-1"));
        assert!(!map.entries.contains_key(&(ip, expired[..16].into())));
        assert_eq!(map.entries.len(), INVITE_BUCKET_CAPACITY);
        assert_eq!(map.insertion_order.len(), INVITE_BUCKET_CAPACITY);
        assert_eq!(map.expiry_order.len(), INVITE_BUCKET_CAPACITY);
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
