//! Structured, fail-closed error responses.
//!
//! Every handler error ends up here so the wire format is uniform and
//! internal detail never leaks to the caller: DB errors, IO paths and the
//! like are logged server-side (`tracing::error!`) and replaced with a
//! generic message before going out over HTTP.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("payload too large")]
    TooLarge,
    #[error("rate limited")]
    RateLimited,
    #[error("internal error")]
    Internal(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            RelayError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            RelayError::NotFound => (StatusCode::NOT_FOUND, "not_found", "not found".to_string()),
            RelayError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            RelayError::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "payload too large".to_string(),
            ),
            RelayError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "rate limited".to_string(),
            ),
            RelayError::Internal(detail) => {
                // Detail is logged server-side only — never forwarded to the caller.
                tracing::error!(detail = %detail, "internal relay error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal error".to_string(),
                )
            }
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

impl From<rusqlite::Error> for RelayError {
    fn from(e: rusqlite::Error) -> Self {
        RelayError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn bad_request_maps_to_400() {
        let resp = RelayError::BadRequest("nope".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn internal_never_leaks_detail_in_status_mapping() {
        // We can't easily inspect the JSON body without a body-collecting
        // harness here; this at minimum locks the status code mapping so a
        // future edit can't silently turn Internal into a leaking 200/4xx.
        let resp = RelayError::Internal("super secret path /etc/x".into()).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
