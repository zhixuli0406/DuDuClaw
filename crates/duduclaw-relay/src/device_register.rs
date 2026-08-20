//! `POST /v1/device/register` — first-time (trust-on-first-use) device key
//! registration.
//!
//! There is no shared secret gating registration — the device generates its
//! own Ed25519 keypair locally and announces the public half here. Safety
//! comes from two properties instead: (1) a strict `device_id` format, and
//! (2) re-registering an *existing* `device_id` with a *different* key is
//! refused (409), so a stranger cannot hijack a `device_id` someone else is
//! already using. Re-registering with the *same* key is idempotent (e.g. to
//! update the display name).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::crypto::decode_pubkey;
use crate::db::RegisterOutcome;
use crate::error::RelayError;
use crate::state::AppState;
use crate::validate::{sanitize_device_name, validate_device_id};

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub device_id: String,
    pub pubkey_b64: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub status: &'static str,
    pub device_id: String,
    pub already_registered: bool,
}

pub async fn register_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, RelayError> {
    if !state.register_rate_limiter.allow(&peer.ip().to_string()).await {
        return Err(RelayError::RateLimited);
    }

    validate_device_id(&req.device_id).map_err(RelayError::BadRequest)?;
    decode_pubkey(&req.pubkey_b64).map_err(RelayError::BadRequest)?;
    let name = req.name.as_deref().map(sanitize_device_name);

    let outcome = state
        .db
        .register_device(&req.device_id, &req.pubkey_b64, name.as_deref())?;

    match outcome {
        RegisterOutcome::Created => Ok(Json(RegisterResponse {
            status: "ok",
            device_id: req.device_id,
            already_registered: false,
        })),
        RegisterOutcome::AlreadyRegistered => Ok(Json(RegisterResponse {
            status: "ok",
            device_id: req.device_id,
            already_registered: true,
        })),
        RegisterOutcome::Conflict => Err(RelayError::Conflict(
            "device_id already registered with a different key".to_string(),
        )),
    }
}
