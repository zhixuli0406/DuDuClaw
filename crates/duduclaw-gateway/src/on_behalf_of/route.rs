//! Internal human-principal permission check for separate MCP processes.
//! Authentication proves the employee; a host-owned opaque reference proves
//! who its runtime is for. No principal supplied in the body is accepted.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

pub const ROUTE: &str = "/api/internal/human-principal";

pub struct RouteState {
    home: PathBuf,
    runtime: Option<crate::license_runtime::LicenseRuntime>,
    replay: ReplayGuard,
    rate: RateLimiter,
}
impl RouteState {
    pub fn new(home: PathBuf, runtime: Option<crate::license_runtime::LicenseRuntime>) -> Self {
        Self {
            home,
            runtime,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
        }
    }
    async fn team_enabled(&self) -> bool {
        // Do not use DUDUCLAW_EDITION, an edition override, or the MCP
        // process's own unverified license JSON. A foreign runtime is invalid.
        let Some(runtime) = &self.runtime else {
            return false;
        };
        if runtime.home_dir() != self.home || !crate::team_edition::team_grants_enabled(&self.home)
        {
            return false;
        }
        let tier = runtime.current_tier().await;
        duduclaw_core::EditionProfile::resolve(None, None, Some(tier.as_toml_key()))
            == duduclaw_core::EditionProfile::Enterprise
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Call,
    Assign,
    SystemOperation,
}
impl Operation {
    fn right(&self) -> Option<duduclaw_core::HumanRight> {
        match self {
            Self::Call => Some(duduclaw_core::HumanRight::Call),
            Self::Assign => Some(duduclaw_core::HumanRight::Assign),
            Self::SystemOperation => None,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    target: String,
    operation: Operation,
    context: String,
}

pub fn router(state: Arc<RouteState>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(state)
}
fn refusal(status: StatusCode) -> Response {
    (
        status,
        Json(json!({"ok":false,"message":"無法驗證交辦人權限，已拒絕交辦。"})),
    )
        .into_response()
}
async fn handle(
    State(state): State<Arc<RouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() {
        return refusal(StatusCode::UNAUTHORIZED);
    }
    let Ok(body) = body else {
        return refusal(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Ok(caller) = authenticate(&state.home, peer, &headers, &body, now_unix) else {
        return refusal(StatusCode::UNAUTHORIZED);
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) {
        return refusal(StatusCode::TOO_MANY_REQUESTS);
    }
    if !state.replay.record(&caller.nonce, now) {
        return refusal(StatusCode::UNAUTHORIZED);
    }
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        return refusal(StatusCode::BAD_REQUEST);
    };
    if !duduclaw_core::is_valid_agent_id(&request.target) {
        return refusal(StatusCode::BAD_REQUEST);
    }
    let Some(principal) = super::context::lookup(&state.home, &caller.agent_id, &request.context)
    else {
        return refusal(StatusCode::FORBIDDEN);
    };
    // The existing employee-tree check runs before this request in MCP;
    // this route only narrows human rights and does not dispatch anything.
    let allowed = match request.operation.right() {
        None => request.target == caller.agent_id
            && super::access::system_operation_permitted(&state.home, principal.as_deref()),
        Some(right) => match principal.as_deref() {
            None => true,
            Some(raw) => match duduclaw_core::HumanPrincipal::parse(raw) {
                Ok(p) => super::access::principal_permits_with_team(
                    &state.home,
                    &p,
                    &request.target,
                    right,
                    state.team_enabled().await,
                ).unwrap_or(false),
                Err(_) => false,
            },
        },
    };
    if !allowed
        || super::context::lookup(&state.home, &caller.agent_id, &request.context)
            != Some(principal.clone())
    {
        return refusal(StatusCode::FORBIDDEN);
    }
    (
        StatusCode::OK,
        Json(json!({"ok":true,"principal":principal})),
    )
        .into_response()
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
