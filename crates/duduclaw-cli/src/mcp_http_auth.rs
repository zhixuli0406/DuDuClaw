// mcp_http_auth.rs — Axum request extractor for MCP HTTP/SSE authentication (W20-P1)
//
// Implements `AuthedPrincipal` as an Axum `FromRequestParts` extractor.
// Supports two authentication modes:
//
//   HeaderOnly     — `Authorization: Bearer <key>` only (used for POST /mcp/v1/call)
//   HeaderOrQuery  — also accepts `?api_key=<key>` (used for GET /mcp/v1/stream,
//                    because browser EventSource cannot set custom headers)
//
// Security note: `api_key` query-param is explicitly disallowed on the HTTP
// call endpoint to prevent keys from appearing in server access logs.

use std::path::PathBuf;

use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::mcp_auth::{authenticate_with_key, Principal};
use crate::mcp_namespace::{resolve, NamespaceContext};

// ── AuthMode ──────────────────────────────────────────────────────────────────

/// Controls how API keys may be submitted for a given endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuthMode {
    /// Only `Authorization: Bearer <key>` is accepted.
    /// Use for POST /mcp/v1/call (prevents key leakage in access logs).
    HeaderOnly,
    /// `Authorization: Bearer <key>` OR `?api_key=<key>` query parameter.
    /// Use for GET /mcp/v1/stream (browser EventSource cannot set headers).
    HeaderOrQuery,
}

// ── AuthedPrincipal ───────────────────────────────────────────────────────────

/// Authenticated principal with resolved namespace context.
/// Inserted as an Axum extractor into handler function signatures.
#[derive(Clone, Debug)]
pub struct AuthedPrincipal {
    pub principal: Principal,
    pub ns_ctx: NamespaceContext,
}

/// Rejection returned when authentication fails.
/// Maps to the appropriate HTTP status + JSON-RPC error body.
#[derive(Debug)]
pub struct AuthRejection {
    pub status: StatusCode,
    pub body: Value,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn auth_error(status: StatusCode, message: &str) -> AuthRejection {
    AuthRejection {
        status,
        body: serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32003, "message": message }
        }),
    }
}

// ── Shared state passed via Axum extensions ───────────────────────────────────

/// HTTP server config made available to extractors via Axum extensions.
#[derive(Clone)]
pub struct HttpAuthConfig {
    pub home_dir: PathBuf,
    pub auth_mode: AuthMode,
}

// ── FromRequestParts impl ─────────────────────────────────────────────────────

impl<S> FromRequestParts<S> for AuthedPrincipal
where
    S: Send + Sync,
{
    type Rejection = AuthRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Retrieve auth config from extensions
        let cfg = parts
            .extensions
            .get::<HttpAuthConfig>()
            .ok_or_else(|| auth_error(StatusCode::INTERNAL_SERVER_ERROR, "Missing auth config"))?
            .clone();

        // Extract raw API key
        let raw_key = extract_key(parts, cfg.auth_mode)?;

        // Validate key and resolve principal
        let principal =
            authenticate_with_key(&raw_key, &cfg.home_dir).map_err(|e| {
                auth_error(StatusCode::UNAUTHORIZED, &format!("Authentication failed: {e}"))
            })?;

        // Resolve namespace context
        let ns_ctx = resolve(&principal).map_err(|e| {
            auth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Namespace resolution failed: {e}"),
            )
        })?;

        Ok(AuthedPrincipal { principal, ns_ctx })
    }
}

/// Extract the raw API key from the request, respecting `AuthMode`.
fn extract_key(parts: &Parts, mode: AuthMode) -> Result<String, AuthRejection> {
    // 1. Try Authorization header first
    if let Some(auth_header) = parts.headers.get("Authorization") {
        let value = auth_header
            .to_str()
            .map_err(|_| auth_error(StatusCode::UNAUTHORIZED, "Invalid Authorization header"))?;

        if let Some(token) = value.strip_prefix("Bearer ") {
            if token.is_empty() {
                return Err(auth_error(StatusCode::UNAUTHORIZED, "Bearer token is empty"));
            }
            return Ok(token.to_string());
        }
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "Authorization header must use Bearer scheme",
        ));
    }

    // 2. Try query param (only if mode allows it)
    if mode == AuthMode::HeaderOrQuery {
        if let Some(query) = parts.uri.query() {
            for pair in query.split('&') {
                if let Some(val) = pair.strip_prefix("api_key=") {
                    if !val.is_empty() {
                        return Ok(val.to_string());
                    }
                }
            }
        }
    }

    Err(auth_error(
        StatusCode::UNAUTHORIZED,
        "Missing API key: provide Authorization: Bearer <key> header",
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, Uri};

    fn make_parts_with_header(auth_value: &str) -> Parts {
        let req = Request::builder()
            .uri("http://localhost/mcp/v1/call")
            .header("Authorization", auth_value)
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        parts
    }

    fn make_parts_with_query(query: &str) -> Parts {
        let uri: Uri = format!("http://localhost/mcp/v1/stream?{query}").parse().unwrap();
        let req = Request::builder()
            .uri(uri)
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        parts
    }

    fn make_parts_no_auth() -> Parts {
        let req = Request::builder()
            .uri("http://localhost/mcp/v1/call")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        parts
    }

    // ── Test: Bearer header is extracted correctly ────────────────────────────
    #[test]
    fn bearer_header_extracts_key() {
        let parts = make_parts_with_header("Bearer ddc_dev_abc123def456abc123def456abc123de");
        let result = extract_key(&parts, AuthMode::HeaderOnly);
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
        assert_eq!(result.unwrap(), "ddc_dev_abc123def456abc123def456abc123de");
    }

    // ── Test: query param works for HeaderOrQuery mode ───────────────────────
    #[test]
    fn query_param_fallback_for_sse() {
        let parts = make_parts_with_query("api_key=ddc_dev_abc123def456abc123def456abc123de");
        let result = extract_key(&parts, AuthMode::HeaderOrQuery);
        assert!(result.is_ok(), "Expected Ok for SSE auth, got: {:?}", result.err());
        assert_eq!(result.unwrap(), "ddc_dev_abc123def456abc123def456abc123de");
    }

    // ── Test: query param rejected for HeaderOnly mode ───────────────────────
    #[test]
    fn query_param_rejected_on_header_only_mode() {
        let parts = make_parts_with_query("api_key=ddc_dev_abc123def456abc123def456abc123de");
        let result = extract_key(&parts, AuthMode::HeaderOnly);
        assert!(result.is_err(), "Expected Err for query param on HeaderOnly endpoint");
        assert_eq!(result.unwrap_err().status, StatusCode::UNAUTHORIZED);
    }

    // ── Test: missing key returns 401 ─────────────────────────────────────────
    #[test]
    fn missing_key_returns_401() {
        let parts = make_parts_no_auth();
        let result = extract_key(&parts, AuthMode::HeaderOnly);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, StatusCode::UNAUTHORIZED);
    }

    // ── Test: non-Bearer scheme is rejected ───────────────────────────────────
    #[test]
    fn non_bearer_scheme_rejected() {
        let parts = make_parts_with_header("Basic dXNlcjpwYXNz");
        let result = extract_key(&parts, AuthMode::HeaderOnly);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    // ── Test: empty Bearer token is rejected ─────────────────────────────────
    #[test]
    fn empty_bearer_token_rejected() {
        let parts = make_parts_with_header("Bearer ");
        let result = extract_key(&parts, AuthMode::HeaderOnly);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, StatusCode::UNAUTHORIZED);
    }
}
