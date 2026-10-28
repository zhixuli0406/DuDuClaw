//! `POST /api/tasks/rpc` — task RPCs that carry files. Isolated home, no
//! production runtime.
use super::*;
use axum::http::{header, Request, StatusCode};
use base64::Engine;
use duduclaw_auth::UserRole;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    home: std::path::PathBuf,
    state: Arc<AppState>,
    admin_token: String,
    viewer_token: String,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    let system = duduclaw_auth::UserContext::admin_fallback();
    handler
        .handle(
            "agents.create",
            json!({ "name": "a1", "display_name": "Alice" }),
            &system,
        )
        .await;
    let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let password = "isolated-task-rpc-test-password";
    let admin = user_db
        .create_user("admin@example.invalid", "admin", password, UserRole::Admin)
        .unwrap();
    let viewer = user_db
        .create_user(
            "viewer@example.invalid",
            "viewer",
            password,
            UserRole::Employee,
        )
        .unwrap();
    user_db
        .bind_agent(&viewer.id, "a1", duduclaw_auth::models::AccessLevel::Viewer)
        .unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"task-rpc-http-isolated-test-secret-32b"));
    let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
    let viewer_token = jwt_config.issue_access_token(&viewer, &[]).unwrap();
    handler
        .set_user_db(user_db.clone(), jwt_config.clone())
        .await;
    handler
        .set_task_store(Arc::new(crate::task_store::TaskStore::open(&home).unwrap()))
        .await;
    let (tx, _) = broadcast::channel(16);
    let (event_tx, _) = broadcast::channel(16);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None),
        handler,
        tx,
        event_tx,
        user_db,
        jwt_config,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
            home.clone(),
            reqwest::Client::new(),
        )),
        home_dir: home.clone(),
    });
    Fixture {
        _directory: directory,
        home,
        state,
        admin_token,
        viewer_token,
    }
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/api/tasks/rpc",
            post(handle_task_rpc_with_files).layer(DefaultBodyLimit::max(
                crate::handlers::task_attachments::MAX_TASK_RPC_BODY_BYTES,
            )),
        )
        .with_state(state)
}

async fn post_rpc(router: &Router, token: Option<&str>, body: String) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/tasks/rpc")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(axum::body::Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn goal_with_file(bytes: &[u8]) -> String {
    json!({
        "method": "tasks.goal_create",
        "params": {
            "agent_id": "a1",
            "description": "整理附件",
            "attachments": [{
                "name": "名單.csv",
                "mime": "text/csv",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            }],
        },
    })
    .to_string()
}

fn saved_files(home: &std::path::Path) -> usize {
    std::fs::read_dir(home.join("agents/a1/attachments"))
        .map(|rd| rd.count())
        .unwrap_or(0)
}

#[tokio::test]
async fn needs_a_bearer_token_and_only_forwards_the_two_task_methods() {
    let f = fixture().await;
    let r = router(f.state.clone());
    let (status, _) = post_rpc(&r, None, goal_with_file(b"1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post_rpc(&r, Some("not-a-token"), goal_with_file(b"1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for method in [
        "agents.list",
        "tasks.goal_create ",
        "TASKS.GOAL_CREATE",
        "users.list",
    ] {
        let body = json!({ "method": method, "params": {} }).to_string();
        let (status, _) = post_rpc(&r, Some(&f.admin_token), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method:?}");
    }
    let (status, _) = post_rpc(&r, Some(&f.admin_token), "not json".to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = json!({ "method": "tasks.goal_create", "params": [] }).to_string();
    let (status, _) = post_rpc(&r, Some(&f.admin_token), body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(saved_files(&f.home), 0);
}

#[tokio::test]
async fn runs_the_same_dispatch_as_the_socket() {
    let f = fixture().await;
    let r = router(f.state.clone());
    // Bigger than the 1 MiB socket frame and axum's 2 MiB default body cap.
    let file = vec![7u8; 3 * 1024 * 1024];
    let (status, frame) = post_rpc(&r, Some(&f.admin_token), goal_with_file(&file)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(frame["type"], "res");
    assert_eq!(frame["ok"], true, "{frame}");
    assert_eq!(frame["payload"]["attachments_saved"], 1);
    assert_eq!(saved_files(&f.home), 1);
    // A Viewer is refused by the handler's own check, exactly as on the socket.
    let (status, frame) = post_rpc(&r, Some(&f.viewer_token), goal_with_file(b"1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(frame["ok"], false);
    assert_eq!(frame["error"], "permission denied");
    assert_eq!(saved_files(&f.home), 1);
}
