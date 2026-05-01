// mcp_http_errors.rs — JSON-RPC ↔ HTTP status mapping for MCP HTTP transport (W20-P1)
//
// Centralises error conversion so the HTTP handler only needs to call
// `into_axum_response` with a JSON-RPC value and the transport layer handles
// Content-Type, status codes, and Retry-After.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

// ── JSON-RPC code → HTTP status mapping ──────────────────────────────────────

/// Map a JSON-RPC error code to the appropriate HTTP status code.
///
/// | JSON-RPC code | Meaning           | HTTP status |
/// |---------------|-------------------|-------------|
/// | -32700        | Parse error       | 400         |
/// | -32600        | Invalid request   | 400         |
/// | -32601        | Method not found  | 404         |
/// | -32602        | Invalid params    | 400         |
/// | -32603        | Internal error    | 500         |
/// | -32003        | Scope / permission| 403         |
/// | -32029        | Rate limited      | 429         |
/// | other         | Unknown           | 500         |
pub fn map_to_http_status(jsonrpc_code: i64) -> StatusCode {
    match jsonrpc_code {
        -32700 | -32600 | -32602 => StatusCode::BAD_REQUEST,
        -32601 => StatusCode::NOT_FOUND,
        -32603 => StatusCode::INTERNAL_SERVER_ERROR,
        -32003 => StatusCode::FORBIDDEN,
        -32029 => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// ── Axum response builder ─────────────────────────────────────────────────────

/// Convert a JSON-RPC response Value into an Axum HTTP response.
///
/// * If `jsonrpc` contains an `"error"` key, the HTTP status is derived from
///   the error code using `map_to_http_status`.
/// * A `Retry-After` header is added for 429 responses when the error message
///   contains a parseable retry-after hint.
/// * Successful responses always return 200.
pub fn into_axum_response(jsonrpc: Value) -> Response {
    if let Some(err) = jsonrpc.get("error") {
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
        let status = map_to_http_status(code);

        if status == StatusCode::TOO_MANY_REQUESTS {
            // Extract retry_after from the error message if present.
            let retry_after = extract_retry_after(
                err.get("message").and_then(|m| m.as_str()).unwrap_or(""),
            );
            let mut response = (status, Json(jsonrpc)).into_response();
            if let Some(secs) = retry_after {
                response.headers_mut().insert(
                    "Retry-After",
                    secs.to_string().parse().unwrap_or_else(|_| "1".parse().unwrap()),
                );
            }
            return response;
        }

        return (status, Json(jsonrpc)).into_response();
    }

    // Successful result → 200 OK
    (StatusCode::OK, Json(jsonrpc)).into_response()
}

/// Parse "retry after N seconds" from a rate-limit error message.
/// Returns `Some(secs)` if a number can be found after "retry after".
fn extract_retry_after(message: &str) -> Option<u64> {
    // Example message: "Rate limited: Rate limited, retry after 3 seconds"
    let lower = message.to_lowercase();
    let idx = lower.find("retry after")?;
    let rest = &message[idx + "retry after".len()..];
    rest.split_whitespace()
        .find_map(|tok| tok.trim_end_matches(',').parse::<u64>().ok())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_to_http_status_400_codes() {
        assert_eq!(map_to_http_status(-32700), StatusCode::BAD_REQUEST);
        assert_eq!(map_to_http_status(-32600), StatusCode::BAD_REQUEST);
        assert_eq!(map_to_http_status(-32602), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_map_to_http_status_404() {
        assert_eq!(map_to_http_status(-32601), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_map_to_http_status_403() {
        assert_eq!(map_to_http_status(-32003), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_map_to_http_status_429() {
        assert_eq!(map_to_http_status(-32029), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_map_to_http_status_500_default() {
        assert_eq!(map_to_http_status(-32603), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(map_to_http_status(0), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(map_to_http_status(9999), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_extract_retry_after_parses_seconds() {
        let msg = "Rate limited: Rate limited, retry after 3 seconds";
        assert_eq!(extract_retry_after(msg), Some(3));
    }

    #[test]
    fn test_extract_retry_after_no_match() {
        assert_eq!(extract_retry_after("some other error"), None);
        assert_eq!(extract_retry_after(""), None);
    }

    #[test]
    fn test_into_axum_response_success_is_200() {
        let json = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        let resp = into_axum_response(json);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn test_into_axum_response_scope_error_is_403() {
        let json = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32003, "message": "Insufficient scope" }
        });
        let resp = into_axum_response(json);
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_into_axum_response_rate_limit_is_429_with_retry_after() {
        let json = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32029, "message": "Rate limited, retry after 5 seconds" }
        });
        let resp = into_axum_response(json);
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().contains_key("Retry-After"));
        let retry = resp.headers()["Retry-After"].to_str().unwrap();
        assert_eq!(retry, "5");
    }

    #[test]
    fn test_into_axum_response_parse_error_is_400() {
        let json = serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": { "code": -32700, "message": "Parse error" }
        });
        let resp = into_axum_response(json);
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
