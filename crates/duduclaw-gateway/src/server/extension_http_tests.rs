use super::*;
use axum::{body::Body, http::{Request, StatusCode}, Extension};
use crate::extension::ExtensionRoute;
use duduclaw_auth::UserRole;
use tower::ServiceExt;

struct TestExtension;
#[async_trait::async_trait]
impl GatewayExtension for TestExtension {
    fn extension_id(&self) -> &str { "test" }
    fn http_routes(&self) -> Vec<ExtensionRoute> {
        [
            ("/ext/test/who", UserRole::Admin),
            ("/ext/test/manager", UserRole::Manager),
            ("/ext/test/employee", UserRole::Employee),
        ].into_iter().map(|(path, minimum_role)| {
            let mut route = ExtensionRoute::new(path, get(|Extension(ctx): Extension<UserContext>| async move {
                Json(serde_json::json!({"admin": ctx.is_admin()}))
            }));
            route.minimum_role = minimum_role;
            route
        }).collect()
    }
}

#[tokio::test]
async fn extension_http_real_jwt_live_role_password_and_suspension() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let user = user_db.create_user("ext@example.invalid", "extension test", "isolated-extension-test-password", duduclaw_auth::UserRole::Admin).unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"extension-test-isolated-jwt-secret-32b"));
    let token = jwt_config.issue_access_token(&user, &[]).unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(user_db.clone(), jwt_config.clone()).await;
    let (tx, _) = broadcast::channel(16);
    let (event_tx, _) = broadcast::channel(16);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None), handler, tx, event_tx,
        prepared_events: Default::default(), user_db: user_db.clone(), jwt_config,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
        home_dir: home.clone(),
    });
    let app = crate::extension::merge_routes(Router::new(), &TestExtension, Arc::new(move |headers, minimum_role| {
        extension_caller(&state, headers, minimum_role)
    })).unwrap();
    async fn call(app: &Router, path: &str, token: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri(path);
        if let Some(token) = token { request = request.header("authorization", format!("Bearer {token}")); }
        app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap()
    }
    assert_eq!(call(&app, "/ext/test/who", None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "/ext/test/who", Some("invalid")).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "/ext/test/who", Some(&token)).await.status(), StatusCode::OK);
    user_db.update_user(&user.id, None, Some(UserRole::Manager), None).unwrap();
    assert_eq!(call(&app, "/ext/test/who", Some(&token)).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "/ext/test/manager", Some(&token)).await.status(), StatusCode::OK);
    user_db.update_user(&user.id, None, Some(UserRole::Employee), None).unwrap();
    assert_eq!(call(&app, "/ext/test/who", Some(&token)).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "/ext/test/manager", Some(&token)).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "/ext/test/employee", None).await.status(), StatusCode::UNAUTHORIZED);
    let response = call(&app, "/ext/test/employee", Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["admin"], false, "JWT role is never authoritative");
    // Use an isolated DB edit to simulate a forced password-change policy.
    let conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
    conn.execute("UPDATE users SET must_change_password = 1 WHERE id = ?1", [&user.id]).unwrap();
    assert_eq!(call(&app, "/ext/test/employee", Some(&token)).await.status(), StatusCode::FORBIDDEN);
    conn.execute("UPDATE users SET must_change_password = 0 WHERE id = ?1", [&user.id]).unwrap();
    user_db.set_user_status(&user.id, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert_eq!(call(&app, "/ext/test/employee", Some(&token)).await.status(), StatusCode::UNAUTHORIZED);
}
