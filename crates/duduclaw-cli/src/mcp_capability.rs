// mcp_capability.rs — ADR-002: Capability Negotiation HTTP Middleware (W22-P0)
//
// Provides two Axum middleware functions that together implement the x-duduclaw
// header protocol (ADR-002 §3.3):
//
//   1. inject_capability_headers — appends x-duduclaw-version + x-duduclaw-capabilities
//      to every HTTP response (both success and error paths)
//
//   2. negotiate_capabilities — validates the optional x-duduclaw-capabilities request
//      header; returns 422 Unprocessable Entity if the server cannot satisfy the client's
//      stated capability requirements
//
// Recommended layer order in the router (outermost wraps innermost):
//
//   router
//       .layer(middleware::from_fn(negotiate_capabilities))   // inner: checked first
//       .layer(middleware::from_fn(inject_capability_headers)) // outer: headers added last
//
// With this ordering, inject_capability_headers runs on ALL responses — including 422s
// returned by negotiate_capabilities — so every response carries the standard DuDuClaw
// headers (ADR-002 §3.1).

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::mcp_headers::{
    build_capabilities_header, build_missing_capabilities_header, validate_client_capabilities,
    API_VERSION,
};

// ── Header name constants ─────────────────────────────────────────────────────

const HDR_VERSION: &str = "x-duduclaw-version";
const HDR_CAPABILITIES: &str = "x-duduclaw-capabilities";
const HDR_MISSING_CAPABILITIES: &str = "x-duduclaw-missing-capabilities";

// ── Middleware 1: response header injection ───────────────────────────────────

/// Axum middleware that appends `x-duduclaw-version` and `x-duduclaw-capabilities`
/// to **every** HTTP response (ADR-002 §3.1).
///
/// Must be the **outermost** layer so it runs on all responses, including 422s produced
/// by [`negotiate_capabilities`].
///
/// ```no_run
/// use axum::{Router, middleware};
/// use duduclaw_cli::mcp_capability::inject_capability_headers;
///
/// let router: Router = Router::new()
///     /* ... routes ... */
///     .layer(middleware::from_fn(inject_capability_headers));
/// ```
pub async fn inject_capability_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    // x-duduclaw-version: 1.2
    headers.insert(
        HeaderName::from_static(HDR_VERSION),
        HeaderValue::from_static(API_VERSION),
    );

    // x-duduclaw-capabilities: memory/3,audit/2,...
    // build_capabilities_header() returns valid ASCII — the expect is unreachable in practice.
    if let Ok(hv) = HeaderValue::from_str(&build_capabilities_header()) {
        headers.insert(HeaderName::from_static(HDR_CAPABILITIES), hv);
    }

    response
}

// ── Middleware 2: capability negotiation ──────────────────────────────────────

/// Axum middleware that validates the optional `x-duduclaw-capabilities` request header
/// (ADR-002 §3.3).
///
/// | Scenario | Server behaviour |
/// |----------|-----------------|
/// | No header present | Permissive — pass through (200) |
/// | Empty header value | Permissive — pass through (200) |
/// | Malformed header | Permissive — pass through (200) |
/// | All requirements met | Pass through (200) |
/// | Any requirement unmet | 422 Unprocessable Entity + `x-duduclaw-missing-capabilities` |
///
/// The 422 body follows ADR-002 §3.3.3:
/// ```json
/// {
///   "error": "capability_mismatch",
///   "message": "Required capabilities not available on this server",
///   "missing": [
///     { "capability": "a2a", "required_version": 1, "server_version": null }
///   ]
/// }
/// ```
///
/// Must be an **inner** layer (closer to the handler) so that `inject_capability_headers`
/// (outer) can append standard DuDuClaw headers to the 422 response as well.
pub async fn negotiate_capabilities(request: Request<Body>, next: Next) -> Response {
    // Clone the header value before consuming the request
    let client_cap_header = request
        .headers()
        .get(HDR_CAPABILITIES)
        .cloned();

    match validate_client_capabilities(client_cap_header.as_ref()) {
        Ok(()) => {
            // Requirements satisfied (or absent) — pass through to handler
            next.run(request).await
        }
        Err(mismatch) => {
            // Build the missing-capabilities header value
            let missing_header_val = build_missing_capabilities_header(&mismatch.missing);

            // Build structured JSON body (ADR-002 §3.3.3)
            let body = serde_json::json!({
                "error": "capability_mismatch",
                "message": "Required capabilities not available on this server",
                "missing": mismatch.missing.iter().map(|m| serde_json::json!({
                    "capability": m.capability,
                    "required_version": m.required_version,
                    "server_version": m.server_version,
                })).collect::<Vec<_>>(),
            });

            let mut response =
                (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response();

            // Attach x-duduclaw-missing-capabilities to the 422 response.
            // The standard x-duduclaw-version + x-duduclaw-capabilities headers will be
            // added by the outer inject_capability_headers middleware.
            if let Ok(hv) = HeaderValue::from_str(&missing_header_val) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static(HDR_MISSING_CAPABILITIES), hv);
            }

            response
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt; // provides Router::oneshot

    // ── Test router helpers ───────────────────────────────────────────────────

    /// Build a minimal router with both middleware layers attached.
    ///
    /// Layer order: inject_capability_headers (outer) → negotiate_capabilities (inner) → handler
    /// This means inject_capability_headers runs on ALL responses including 422s from negotiate.
    fn test_router() -> Router {
        async fn ok_handler() -> impl IntoResponse {
            (StatusCode::OK, "ok")
        }

        Router::new()
            .route("/", get(ok_handler))
            .layer(middleware::from_fn(negotiate_capabilities))   // inner
            .layer(middleware::from_fn(inject_capability_headers)) // outer
    }

    /// Build a minimal request to GET /
    fn get_root() -> Request<Body> {
        Request::builder().uri("/").body(Body::empty()).unwrap()
    }

    /// Build a GET / request with a specific x-duduclaw-capabilities header
    fn get_with_caps(caps: &'static str) -> Request<Body> {
        Request::builder()
            .uri("/")
            .header(HDR_CAPABILITIES, caps)
            .body(Body::empty())
            .unwrap()
    }

    // ── inject_capability_headers integration tests (≥ 3 required) ───────────

    #[tokio::test]
    async fn response_always_has_version_header() {
        let resp = test_router().oneshot(get_root()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let version = resp.headers().get(HDR_VERSION).expect("x-duduclaw-version must be present");
        assert_eq!(version.to_str().unwrap(), "1.2");
    }

    #[tokio::test]
    async fn response_always_has_capabilities_header() {
        let resp = test_router().oneshot(get_root()).await.unwrap();
        let caps = resp.headers().get(HDR_CAPABILITIES)
            .expect("x-duduclaw-capabilities must be present");
        let caps_str = caps.to_str().unwrap();
        assert!(caps_str.starts_with("memory/"), "memory must be first cap: {caps_str}");
    }

    #[tokio::test]
    async fn response_capabilities_header_reflects_registry() {
        let resp = test_router().oneshot(get_root()).await.unwrap();
        let caps = resp.headers()[HDR_CAPABILITIES].to_str().unwrap();
        assert!(caps.contains("mcp/2"),    "mcp/2 must be in capabilities: {caps}");
        assert!(caps.contains("wiki/1"),   "wiki/1 must be in capabilities: {caps}");
        assert!(caps.contains("audit/2"),  "audit/2 must be in capabilities: {caps}");
        assert!(!caps.contains("a2a/"),    "disabled a2a must not appear: {caps}");
        assert!(!caps.contains("secret-manager/"), "disabled secret-manager must not appear: {caps}");
    }

    // ── negotiate_capabilities pass-through tests ─────────────────────────────

    #[tokio::test]
    async fn no_capability_header_returns_200() {
        // Permissive mode: no header → always pass through
        let resp = test_router().oneshot(get_root()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn satisfied_capability_header_returns_200() {
        // mcp/2 and memory/3 are both enabled at the exact required versions
        let resp = test_router().oneshot(get_with_caps("mcp/2,memory/3")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn lower_version_than_required_is_still_ok() {
        // Client requests mcp/1 but server has mcp/2 — server version ≥ required → OK
        let resp = test_router().oneshot(get_with_caps("mcp/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── negotiate_capabilities 422 tests (≥ 3 required) ──────────────────────

    #[tokio::test]
    async fn disabled_capability_returns_422() {
        // a2a is in registry but enabled: false → 422
        let resp = test_router().oneshot(get_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn version_too_high_returns_422() {
        // mcp is at /2; client requires /99 → 422
        let resp = test_router().oneshot(get_with_caps("mcp/99")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn missing_capability_response_includes_missing_header() {
        // Both a2a and secret-manager are disabled
        let resp = test_router()
            .oneshot(get_with_caps("a2a/1,secret-manager/1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let missing_hdr = resp
            .headers()
            .get(HDR_MISSING_CAPABILITIES)
            .expect("x-duduclaw-missing-capabilities must be present on 422");
        let missing_str = missing_hdr.to_str().unwrap();
        assert!(missing_str.contains("a2a/1"),           "a2a/1 must be in missing: {missing_str}");
        assert!(missing_str.contains("secret-manager/1"), "secret-manager/1 must be in missing: {missing_str}");
    }

    #[tokio::test]
    async fn _422_response_also_carries_standard_duduclaw_headers() {
        // Even error responses must include standard x-duduclaw-* headers (ADR-002 §3.3.3)
        let resp = test_router().oneshot(get_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            resp.headers().contains_key(HDR_VERSION),
            "422 must carry x-duduclaw-version (added by inject_capability_headers)"
        );
        assert!(
            resp.headers().contains_key(HDR_CAPABILITIES),
            "422 must carry x-duduclaw-capabilities (added by inject_capability_headers)"
        );
    }

    #[tokio::test]
    async fn partial_mismatch_returns_422_with_only_missing_caps() {
        // memory/3 satisfied, a2a/1 not available → 422
        let resp = test_router()
            .oneshot(get_with_caps("memory/3,a2a/1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let missing_str = resp.headers()[HDR_MISSING_CAPABILITIES].to_str().unwrap();
        assert!(missing_str.contains("a2a/1"), "only a2a should be missing: {missing_str}");
        assert!(!missing_str.contains("memory"), "satisfied memory must not appear in missing: {missing_str}");
    }
}
