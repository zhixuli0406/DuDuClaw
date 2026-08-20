//! `GET /healthz` — liveness probe.
//!
//! Deliberately cheap: no auth, no DB round-trip. Cloud Run (and any
//! orchestrator) should be able to tell the process is alive without that
//! check depending on storage health too.

use axum::http::StatusCode;
use axum::response::IntoResponse;

pub async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}
