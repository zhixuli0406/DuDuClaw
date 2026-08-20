//! DuDuClaw Cloud Relay — library surface shared by `main.rs` and tests.
//!
//! Two jobs, one small standalone service (see the crate README for the
//! full picture):
//! 1. **Webhook relay**: forward inbound channel webhooks (LINE today) to
//!    the owning box's live WebSocket connection as opaque bytes — this
//!    process never verifies signatures or holds a channel secret.
//! 2. **Find**: a same-public-IP LAN device discovery page.
//!
//! The box-side WebSocket client that connects to `/v1/device/ws` and
//! consumes the wire protocol documented in `device_ws.rs`/`hook.rs` is a
//! separate component (not part of this crate).

pub mod channel;
pub mod config;
pub mod crypto;
pub mod db;
pub mod device_register;
pub mod device_ws;
pub mod error;
pub mod find;
pub mod healthz;
pub mod hook;
pub mod queue;
pub mod ratelimit;
pub mod registry;
pub mod state;
pub mod validate;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;

use state::AppState;

/// Build the full route table. Shared by `main.rs` (the real server) and
/// the integration tests (bound to a real ephemeral port so WebSocket
/// upgrades exercise the actual network stack).
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz::healthz_handler))
        .route("/v1/device/register", post(device_register::register_handler))
        .route("/v1/device/ws", get(device_ws::device_ws_handler))
        .route(
            "/v1/hook/{channel}/{device_id}",
            post(hook::hook_handler).layer(DefaultBodyLimit::max(hook::MAX_BODY_BYTES)),
        )
        .route("/v1/find", get(find::find_handler))
        .with_state(state)
}
