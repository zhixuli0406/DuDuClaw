use super::*;
use crate::computer_use_sessions::auth as a;
use axum::http::Request as HttpRequest;
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
fn home() -> tempfile::TempDir {
    let h = tempfile::tempdir().unwrap();
    duduclaw_core::ensure_identity_key(h.path()).unwrap();
    std::fs::write(h.path().join("config.toml"), format!(
        "[delegation]\nhuman_principal='enforce'\n[mcp_keys.\"{KEY}\"]\nclient_id='gateway-internal'\nis_external=false\ncreated_at='{}'\nscopes=['admin']\n", chrono::Utc::now().to_rfc3339())).unwrap();
    h
}
fn headers(home: &std::path::Path, agent: &str, body: &[u8], nonce: &str) -> HeaderMap {
    let token =
        duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), agent);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let sig =
        duduclaw_core::internal_request_signature(KEY.as_bytes(), agent, &token, &ts, nonce, body);
    let mut h = HeaderMap::new();
    for (k, v) in [
        (a::AGENT_ID_HEADER, agent),
        (a::TIMESTAMP_HEADER, &ts),
        (a::NONCE_HEADER, nonce),
        (a::SIGNATURE_HEADER, &sig),
    ] {
        h.insert(k, v.parse().unwrap());
    }
    h
}
async fn post(
    state: Arc<RouteState>,
    peer: &str,
    headers: HeaderMap,
    body: Vec<u8>,
) -> (u16, Value) {
    let mut req = HttpRequest::builder()
        .method("POST")
        .uri(ROUTE)
        .body(axum::body::Body::from(body))
        .unwrap();
    req.headers_mut().extend(headers);
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
    let response = router(state).oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn request(state: Arc<RouteState>, agent: &str, body: Value) -> (u16, Value) {
    let bytes = serde_json::to_vec(&body).unwrap();
    let headers = headers(
        &state.home,
        agent,
        &bytes,
        &uuid::Uuid::new_v4().simple().to_string(),
    );
    post(state, "127.0.0.1:1234", headers, bytes).await
}
#[tokio::test]
async fn signed_route_requires_live_agent_bound_context_even_for_background() {
    let home = home();
    let state = Arc::new(RouteState::new(home.path().into(), None));
    let guard = super::super::context::register(home.path(), "bot").unwrap();
    let body = json!({"context":guard.reference(),"target":"sales","operation":"assign"});
    let (status, response) = request(state.clone(), "bot", body.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(response["principal"], Value::Null);
    assert_eq!(request(state.clone(), "other", body.clone()).await.0, 403);
    assert_eq!(
        request(
            state.clone(),
            "bot",
            json!({"target":"sales","operation":"assign"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            state.clone(),
            "bot",
            json!({"context":"f".repeat(64),"target":"sales","operation":"assign"})
        )
        .await
        .0,
        403
    );
    let mut injected = body.clone();
    injected["principal"] = json!("user:admin");
    assert_eq!(request(state.clone(), "bot", injected).await.0, 400);
    let mut bad_op = body.clone();
    bad_op["operation"] = json!("anything");
    assert_eq!(request(state.clone(), "bot", bad_op).await.0, 400);
    drop(guard);
    assert_eq!(request(state.clone(), "bot", body).await.0, 403);
}
#[tokio::test]
async fn signed_route_refuses_nonloopback_bad_auth_replay_and_rate_limit() {
    let home = home();
    let state = Arc::new(RouteState::new(home.path().into(), None));
    let guard = super::super::context::register(home.path(), "bot").unwrap();
    let bytes = serde_json::to_vec(
        &json!({"context":guard.reference(),"target":"sales","operation":"call"}),
    )
    .unwrap();
    let h = headers(
        home.path(),
        "bot",
        &bytes,
        &uuid::Uuid::new_v4().simple().to_string(),
    );
    assert_eq!(
        post(state.clone(), "192.0.2.1:1234", h.clone(), bytes.clone())
            .await
            .0,
        401
    );
    assert_eq!(
        post(
            state.clone(),
            "127.0.0.1:1234",
            HeaderMap::new(),
            bytes.clone()
        )
        .await
        .0,
        401
    );
    let mut forged = h.clone();
    forged.insert(a::SIGNATURE_HEADER, "0".repeat(64).parse().unwrap());
    assert_eq!(
        post(state.clone(), "127.0.0.1:1234", forged, bytes.clone())
            .await
            .0,
        401
    );
    assert_eq!(
        post(state.clone(), "127.0.0.1:1234", h.clone(), bytes.clone())
            .await
            .0,
        200
    );
    assert_eq!(
        post(state.clone(), "127.0.0.1:1234", h, bytes.clone())
            .await
            .0,
        401
    );
    // The replay attempt consumed one authenticated rate-limit entry.
    for _ in 2..a::RATE_LIMIT_PER_MINUTE {
        let h = headers(
            home.path(),
            "bot",
            &bytes,
            &uuid::Uuid::new_v4().simple().to_string(),
        );
        assert_eq!(
            post(state.clone(), "127.0.0.1:1234", h, bytes.clone())
                .await
                .0,
            200
        );
    }
    let h = headers(
        home.path(),
        "bot",
        &bytes,
        &uuid::Uuid::new_v4().simple().to_string(),
    );
    assert_eq!(post(state.clone(), "127.0.0.1:1234", h, bytes).await.0, 429);
}
#[tokio::test]
async fn route_rechecks_user_rights_and_ignores_process_edition_records() {
    use duduclaw_auth::{AccessLevel, UserDb, UserRole};
    let home = home();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let mgr = db
        .create_user("mgr@test.local", "mgr", "TeamPass-12345", UserRole::Manager)
        .unwrap();
    let emp = db
        .create_user(
            "emp@test.local",
            "emp",
            "TeamPass-12345",
            UserRole::Employee,
        )
        .unwrap();
    db.set_manager(&emp.id, Some(&mgr.id)).unwrap();
    db.bind_agent(&emp.id, "sales", AccessLevel::Owner).unwrap();
    crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
    let state = Arc::new(RouteState::new(home.path().into(), None));
    super::super::scope_first(Some(format!("user:{}", mgr.id)), async {
        let guard = super::super::context::register(home.path(), "bot").unwrap();
        let body = json!({"context":guard.reference(),"target":"sales","operation":"assign"});
        // Unknown verified tier cannot use team grants, even though legacy
        // process edition says Enterprise.
        assert_eq!(request(state.clone(), "bot", body.clone()).await.0, 403);
        db.bind_agent(&mgr.id, "sales", AccessLevel::Viewer)
            .unwrap();
        assert_eq!(request(state.clone(), "bot", body.clone()).await.0, 403);
        let mut call = body.clone();
        call["operation"] = json!("call");
        assert_eq!(request(state.clone(), "bot", call.clone()).await.0, 200);
        db.unbind_agent(&mgr.id, "sales").unwrap();
        assert_eq!(request(state.clone(), "bot", call).await.0, 403);
        assert!(
            super::super::access::principal_permits_with_team(
                home.path(),
                &duduclaw_core::HumanPrincipal::user(&mgr.id).unwrap(),
                "sales",
                duduclaw_core::HumanRight::Assign,
                true
            )
            .unwrap()
        );
    })
    .await;
}

#[tokio::test]
async fn machine_tool_authority_checks_the_task_human_even_with_agent_admin_scope() {
    use duduclaw_auth::{AccessLevel, UserDb, UserRole};
    let home = home();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@test.local", "Admin", "TeamPass-12345", UserRole::Admin).unwrap();
    let employee = db.create_user("employee@test.local", "Employee", "TeamPass-12345", UserRole::Employee).unwrap();
    db.bind_agent(&employee.id, "bot", AccessLevel::Owner).unwrap();
    let state = Arc::new(RouteState::new(home.path().into(), None));
    // A task's propagated human identity, not its AI employee's Owner grant
    // or the signed MCP key's Admin scope, decides machine rights.
    for (user, expected) in [(&employee, 403), (&admin, 200)] {
        super::super::scope_first(Some(format!("user:{}", user.id)), async {
            let guard = super::super::context::register(home.path(), "bot").unwrap();
            let body = json!({"context":guard.reference(),"target":"bot","operation":"system_operation"});
            assert_eq!(request(state.clone(), "bot", body.clone()).await.0, expected);
            assert_eq!(request(state.clone(), "other", body.clone()).await.0, 403);
            let mut other_target = body.clone();
            other_target["target"] = json!("other");
            assert_eq!(request(state.clone(), "bot", other_target).await.0, 403);
            if expected == 200 {
                rusqlite::Connection::open(home.path().join("users.db")).unwrap()
                    .execute("UPDATE users SET role='employee' WHERE id=?1", [&admin.id]).unwrap();
                assert_eq!(request(state.clone(), "bot", body.clone()).await.0, 403, "role revocation is live");
            }
            drop(guard);
            assert_eq!(request(state.clone(), "bot", body).await.0, 403);
        }).await;
    }
    let guard = super::super::context::register(home.path(), "bot").unwrap();
    let body = json!({"context":guard.reference(),"target":"bot","operation":"system_operation"});
    assert_eq!(request(state.clone(), "bot", body.clone()).await.0, 200, "verified background remains allowed");
    drop(guard);
    assert_eq!(request(state.clone(), "bot", body).await.0, 403, "absence of a human is not proof of background work");
    let fake = json!({"context":"f".repeat(64),"target":"bot","operation":"system_operation"});
    assert_eq!(request(state, "bot", fake).await.0, 403);
}
