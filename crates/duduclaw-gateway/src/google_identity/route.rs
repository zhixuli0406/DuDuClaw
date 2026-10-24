//! `POST /api/internal/google-token` — how an employee's MCP server gets a
//! short-lived access token for a person identity. Same authentication as
//! the knowledge and computer-use routes (`computer_use_sessions::auth`:
//! loopback peer, signed employee identity, timestamp window, single-use
//! nonce, 120 requests per employee per minute).
//!
//! Body `{tool, turn_id?, run_id?}`. The employee is the one the signature
//! proves; nothing in the body can name another. The gateway reads the
//! employee's `[google]` itself and looks the turn's requester up in its own
//! registry (`knowledge::turn_requesters`).
//!
//! | answer | meaning |
//! |---|---|
//! | `{ok, mode: "delegate", access_token, expires_in, account_email}` | use this token |
//! | `{ok, mode: "home", fallback, notice?}` | use the home account in-process |
//! | `{ok: false, code, message}` (403) | refused; `code` ∈ [`super::REFUSAL_CODES`] |
//!
//! Every decision is one `google_token` row in `tool_calls.jsonl` (no token).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use duduclaw_auth::db::UserDb;
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::Deserialize;
use serde_json::{Value, json};

use super::requester_gate::people_for;
use super::resolve::{Resolved, resolve_person_for_tool};
use super::{Refusal, audit_issue};
use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};

pub const ROUTE: &str = "/api/internal/google-token";
pub const MAX_BODY_BYTES: usize = 4 * 1024;

pub struct GoogleTokenRouteState {
    pub home: PathBuf,
    pub users: Option<Arc<UserDb>>,
    replay: ReplayGuard,
    rate: RateLimiter,
}

impl GoogleTokenRouteState {
    pub fn new(home: PathBuf, users: Option<Arc<UserDb>>) -> Self {
        Self {
            home,
            users,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenRequest {
    pub tool: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
}

pub fn router(state: Arc<GoogleTokenRouteState>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "code": code, "message": message})),
    )
        .into_response()
}

fn unauthorized() -> Response {
    refuse(
        StatusCode::UNAUTHORIZED,
        "no_verified_caller",
        "拒絕存取：這個端點只接受 DuDuClaw gateway 啟動的 AI 員工在本機呼叫，並需通過員工身分驗證。",
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A short identifier: only used to label the audit row.
fn valid_tool(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

async fn handle(
    State(state): State<Arc<GoogleTokenRouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() {
        return unauthorized();
    }
    let Ok(body) = body else {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "請求內容太大或無法讀取。",
        );
    };
    let caller = match authenticate(&state.home, peer, &headers, &body, unix_now()) {
        Ok(c) => c,
        Err(f) => {
            tracing::debug!(reason = f.as_str(), "google-token route refused");
            return unauthorized();
        }
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Google 權杖請求太頻繁（每位員工每分鐘最多 120 次），請稍後再試。",
        );
    }
    if !state.replay.record(&caller.nonce, now) {
        return unauthorized();
    }
    let Ok(request) = serde_json::from_slice::<TokenRequest>(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Google 權杖請求格式不正確。",
        );
    };
    if !valid_tool(&request.tool) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Google 權杖請求格式不正確。",
        );
    }
    let requester = crate::knowledge::turn_requesters::resolve_with_run(
        &caller.agent_id,
        request.turn_id.as_deref(),
        request.run_id.as_deref(),
    );
    match serve(&state, &caller.agent_id, &request.tool, &requester).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(r) => refuse(StatusCode::FORBIDDEN, r.code, &r.message_zh),
    }
}

/// The decision behind the route, for an already-authenticated employee.
pub(crate) async fn serve(
    state: &GoogleTokenRouteState,
    agent: &str,
    tool: &str,
    requester: &RequesterIdentity,
) -> Result<Value, Refusal> {
    if !crate::google_workspace::integration_enabled(&state.home) {
        let outcome = Err(Refusal::hard(
            "disabled",
            "管理員尚未開啟 Google 整合，請先到「整合 → Google」開啟。",
        ));
        audit_issue(&state.home, agent, tool, requester.kind(), &outcome);
        return outcome.map(|_| Value::Null);
    }
    let people = people_for(&state.home, state.users.as_ref());
    let outcome = resolve_person_for_tool(&state.home, agent, requester, people.as_ref(), tool).await;
    audit_issue(&state.home, agent, tool, requester.kind(), &outcome);
    match outcome? {
        Resolved::Delegated { email, token } => Ok(json!({
            "ok": true,
            "mode": "delegate",
            "access_token": token.access_token,
            "expires_in": token.expires_in,
            "account_email": email,
        })),
        Resolved::Personal { kind, user_id, email, token } => Ok(json!({
            "ok": true, "mode": "delegate", "identity_kind": kind, "google_identity_user": user_id,
            "access_token": token.access_token, "expires_in": token.expires_in, "account_email": email,
        })),
        Resolved::UseHome { fallback, notice } => Ok(json!({
            "ok": true,
            "mode": "home",
            "fallback": fallback,
            "notice": notice,
        })),
    }
}
