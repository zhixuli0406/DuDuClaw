use super::*;
use axum::http::Request;
use serde_json::json;
use tower::ServiceExt;

struct Fixture {
    _home: tempfile::TempDir,
    state: Arc<AppState>,
    token: String,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    std::fs::write(home.join("config.toml"), "[a2a]\nexposed_agents=['sales']\npublic_url='https://gateway.example/a2a'\n").unwrap();
    crate::trigger_route::tests::write_agent(&home.join("agents"), "sales", "Sales", "@sales", "specialist", "");
    std::fs::create_dir_all(home.join("agents/sales/SKILLS")).unwrap();
    std::fs::write(home.join("agents/sales/SKILLS/report.md"), "---\nname: report\ndescription: Prepare reports\n---\nInstructions\n").unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let user = db.create_user("a2a@example.invalid", "A2A", "isolated-a2a-test-password", duduclaw_auth::UserRole::Admin).unwrap();
    let jwt = Arc::new(JwtConfig::new(b"a2a-isolated-test-secret-at-least-32-bytes"));
    let token = jwt.issue_access_token(&user, &[]).unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(db.clone(), jwt.clone()).await;
    let (tx, _) = broadcast::channel(4);
    let (event_tx, _) = broadcast::channel(4);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None), handler, tx, event_tx, prepared_events: Default::default(),
        user_db: db, jwt_config: jwt,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
        home_dir: home,
    });
    Fixture { _home: directory, state, token }
}

async fn call(f: &Fixture, token: Option<&str>, version: Option<&str>, body: axum::body::Body) -> (StatusCode, Value) {
    let router = Router::new().route("/a2a", post(rpc)).with_state(f.state.clone());
    let mut request = Request::builder().method("POST").uri("/a2a").header("content-type", "application/json");
    if let Some(token) = token { request = request.header("authorization", format!("Bearer {token}")); }
    if let Some(version) = version { request = request.header("a2a-version", version); }
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(ConnectInfo("127.0.0.81:1234".parse::<SocketAddr>().unwrap()));
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.headers()["a2a-version"], "1.0");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a2a_http_auth_precedes_body_and_never_uses_authless_fallback() {
    let f = fixture().await;
    for token in [None, Some("legacy-admin-token"), Some("ddc_prod_00000000000000000000000000000000")] {
        let body = axum::body::Body::from_stream(futures_util::stream::once(async {
            Err::<axum::body::Bytes, std::io::Error>(std::io::Error::other("must not read"))
        }));
        let (status, value) = call(&f, token, Some("1.0"), body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(value["error"]["data"][0]["reason"], "UNAUTHENTICATED");
    }
}

#[tokio::test]
async fn a2a_http_version_parse_and_method_mapping() {
    let f = fixture().await;
    let request = json!({"jsonrpc":"2.0", "id":7, "method":"SendStreamingMessage", "params":{}}).to_string();
    for version in [None, Some(""), Some("0.3"), Some("2.0")] {
        let (status, value) = call(&f, Some(&f.token), version, request.clone().into()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"]["code"], -32009);
    }
    let (status, value) = call(&f, Some(&f.token), Some("1.0"), request.into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["id"], 7);
    assert_eq!(value["error"]["code"], -32004);
    let (_, value) = call(&f, Some(&f.token), Some("1.0"), "not json".into()).await;
    assert_eq!(value["error"]["code"], -32700);
    for (method, code) in [("message/send", -32602), ("tasks/get", -32602), ("tasks/cancel", -32601)] {
        let body = json!({"jsonrpc":"2.0", "id":8, "method":method, "params":{}}).to_string();
        let (_, value) = call(&f, Some(&f.token), Some("1.0"), body.into()).await;
        assert_eq!(value["error"]["code"], code, "{method}: {value}");
    }
}

#[tokio::test]
async fn a2a_http_rejects_batches_notifications_and_oversized_body() {
    let f = fixture().await;
    for body in ["[]", r#"{"jsonrpc":"2.0","method":"SendMessage"}"#] {
        let (_, value) = call(&f, Some(&f.token), Some("1.0"), body.into()).await;
        assert_eq!(value["error"]["code"], -32600);
    }
    let (status, _) = call(&f, Some(&f.token), Some("1.0"), "x".repeat(65537).into()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a2a_card_http_response_requires_a_valid_signature() {
    let f = fixture().await;
    let card = f.state.handler.a2a_card(None).await.unwrap();
    assert_eq!(signed_card_response(card.clone(), None).status(), StatusCode::SERVICE_UNAVAILABLE);
    let signer = crate::a2a_signing::A2aSigner::from_secret_bytes([18; 32]);
    let response = signed_card_response(card, Some(&signer));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let card = serde_json::from_slice(&bytes).unwrap();
    assert!(crate::a2a_signing::verify_card(&card, &signer.verifying_key()));
}


#[tokio::test]
async fn a2a_http_anonymous_buckets_cannot_exhaust_authenticated_rpc() {
    let limits = HttpLimits::new();
    let proxy = "192.0.2.4".parse().unwrap();
    for _ in 0..120 {
        assert!(limits.allow(Traffic::Discovery, proxy).await);
        assert!(limits.allow(Traffic::Refused, proxy).await);
    }
    assert!(!limits.allow(Traffic::Discovery, proxy).await);
    assert!(!limits.allow(Traffic::Refused, proxy).await);
    for _ in 0..120 { assert!(limits.allow(Traffic::Rpc, proxy).await); }
    assert!(!limits.allow(Traffic::Rpc, proxy).await);
}

#[tokio::test]
async fn a2a_http_global_discovery_limit_is_separate_from_rpc() {
    let limits = HttpLimits::new();
    for n in 0..600 {
        let ip = std::net::IpAddr::V6(std::net::Ipv6Addr::from(n as u128 + 1));
        assert!(limits.allow(Traffic::Discovery, ip).await);
    }
    let ip = "2001:db8::ffff".parse().unwrap();
    assert!(!limits.allow(Traffic::Discovery, ip).await);
    assert!(limits.allow(Traffic::Rpc, ip).await);
    for n in 0..1199 {
        let ip = std::net::IpAddr::V6(std::net::Ipv6Addr::from(n as u128 + 1));
        assert!(limits.allow(Traffic::Rpc, ip).await);
    }
    assert!(!limits.allow(Traffic::Rpc, ip).await);
}

#[tokio::test]
async fn a2a_http_rejected_peer_does_not_spend_global_capacity() {
    for (kind, capacity) in [(Traffic::Discovery, 600), (Traffic::Rpc, 1200), (Traffic::Refused, 600)] {
        let limits = HttpLimits::new();
        let peer = "192.0.2.4".parse().unwrap();
        // Use a fresh local limiter for every class; no shared test counters.
        for _ in 0..120 { assert!(limits.allow(kind, peer).await); }
        for _ in 0..capacity {
            assert!(!limits.allow(kind, peer).await);
        }
        for n in 0..capacity - 120 {
            let other = std::net::IpAddr::V6(std::net::Ipv6Addr::from(n as u128 + 1));
            assert!(limits.allow(kind, other).await);
        }
        assert!(!limits.allow(kind, "2001:db8::ffff".parse().unwrap()).await);
    }
}

#[tokio::test]
async fn a2a_http_saturated_card_bucket_still_admits_rpc_from_same_proxy() {
    let f = fixture().await;
    let peer: SocketAddr = "127.0.0.92:1234".parse().unwrap();
    for _ in 0..120 { assert!(LIMITS.allow(Traffic::Discovery, peer.ip()).await); }
    // Stops before signing, so the test never reads a production signing key.
    let limited = card(State(f.state.clone()), ConnectInfo(peer), HeaderMap::new()).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {}", f.token).parse().unwrap());
    headers.insert("a2a-version", "1.0".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    let body = json!({"jsonrpc":"2.0", "id":1, "method":"SendStreamingMessage", "params":{}}).to_string();
    let response = rpc(State(f.state), ConnectInfo(peer), headers, body.into()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["error"]["code"], -32004);
}
