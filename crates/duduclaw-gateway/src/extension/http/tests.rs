use super::*;
use axum::routing::{get, post};
use tower::ServiceExt;

struct Routes {
    paths: Vec<(&'static str, UserRole)>,
    legacy: bool,
}

#[async_trait::async_trait]
impl GatewayExtension for Routes {
    fn extension_id(&self) -> &str { "test" }
    fn extra_routes(&self) -> Option<Router> {
        self.legacy.then(Router::new)
    }
    fn http_routes(&self) -> Vec<ExtensionRoute> {
        self.paths.iter().map(|(path, minimum_role)| ExtensionRoute {
            path: (*path).into(),
            methods: post(|| async { "extension" }),
            minimum_role: *minimum_role,
        }).collect()
    }
}

fn deny_auth() -> Authenticate {
    Arc::new(|_, _| Err(StatusCode::UNAUTHORIZED.into_response()))
}

#[test]
fn extension_http_refuses_core_paths_and_opaque_routers() {
    for path in ["/api/tts", "/api/login", "/ws", "/ext/other/path", "/ext/test/../api",
        "/ext/test/%2e%2e/api", "/ext/test/{*rest}", "/ext/test//status", "/ext/test/"] {
        let extension = Routes { paths: vec![(path, UserRole::Admin)], legacy: false };
        assert!(merge_routes(Router::new(), &extension, deny_auth()).is_err(), "{path}");
    }
    let legacy = Routes { paths: vec![], legacy: true };
    assert!(merge_routes(Router::new(), &legacy, deny_auth()).is_err());
}

#[test]
fn extension_http_refuses_overlap_even_with_different_verbs() {
    let extension = Routes { paths: vec![("/ext/test/status", UserRole::Admin)], legacy: false };
    let core = Router::new().route("/ext/test/status", get(|| async { "core" }));
    assert!(merge_routes(core, &extension, deny_auth()).is_err());
    let duplicate = Routes { paths: vec![
        ("/ext/test/status", UserRole::Admin), ("/ext/test/status", UserRole::Employee),
    ], legacy: false };
    assert!(merge_routes(Router::new(), &duplicate, deny_auth()).is_err());
}

#[tokio::test]
async fn extension_http_requires_auth_and_checks_origin_before_handler() {
    let extension = Routes { paths: vec![("/ext/test/status", UserRole::Admin)], legacy: false };
    let app = merge_routes(Router::new(), &extension, deny_auth()).unwrap();
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app.oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .header("origin", "https://localhost.evil.invalid").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn extension_http_authenticated_routes_keep_origin_and_raw_body_limit() {
    let extension = Routes { paths: vec![("/ext/test/status", UserRole::Employee)], legacy: false };
    let app = merge_routes(Router::new(), &extension, Arc::new(|_, _| Ok(UserContext::admin_fallback()))).unwrap();
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .header("origin", "https://evil.invalid").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app.oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .body(Body::from(vec![0; MAX_BODY_BYTES + 1])).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn extension_http_constructor_defaults_to_admin() {
    let route = ExtensionRoute::new("/ext/test/status", get(|| async { "extension" }));
    assert_eq!(route.minimum_role, UserRole::Admin);
}

#[tokio::test]
async fn extension_http_employee_route_still_requires_auth_without_origin() {
    let extension = Routes { paths: vec![("/ext/test/status", UserRole::Employee)], legacy: false };
    let app = merge_routes(Router::new(), &extension, deny_auth()).unwrap();
    let response = app.oneshot(Request::builder().method("POST").uri("/ext/test/status")
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
