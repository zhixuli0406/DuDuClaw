//! `POST /api/internal/computer-use` — the only way into the tool-driven
//! sessions. JSON body `{op, ...}`:
//!
//! | op | body | answer |
//! |---|---|---|
//! | `start` | `width?`, `height?`, `task?`, `turn_id?` | session id, size, limits, `network`, `reachable_hosts`, `unreachable_hosts`, `network_message` |
//! | `screenshot` | `session_id?` | `png_base64`, `fully_masked`, `mask_reason` (`helper_failed` / `several_pages` / `title_sensitive` / `title_unreadable`, or `null`) + counters |
//! | `action` | `session_id?`, `turn_id?`, `action: {type: click/type/key/scroll/navigate, …}` | `message` (+ `host` for navigate) + counters |
//! | `stop` | `session_id?` | statistics |
//! | `status` | — | `active` + counters |
//!
//! `turn_id` is the caller's `DUDUCLAW_TURN_ID`; the gateway looks it up in
//! its own record of live turns of the verified employee to find where a
//! high-risk confirmation may be asked ([`super::turns`]). Nothing in the
//! body names a chat.
//!
//! Errors are `{ok:false, code, message}` with an HTTP status per code
//! ([`super::ErrorCode::http_status`]); every authentication failure is the
//! same `unauthorized` answer. Each op runs under its own total time bound
//! ([`op_budget`]) a little below the MCP client's timeout.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use serde_json::Value;

use super::actions::ActionRequest;
use super::{ComputerUseSessions, ErrorCode, OpError, StartRequest};

/// The route path.
pub const ROUTE: &str = "/api/internal/computer-use";
/// Request body cap.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Server-side bound on one `start` (approval wait up to 300 s, then the
/// container start and display wait). The MCP client waits 420 s.
pub const START_BUDGET: Duration = Duration::from_secs(415);
/// Server-side bound on one `screenshot` (client: 360 s).
pub const SCREENSHOT_BUDGET: Duration = Duration::from_secs(355);
/// Server-side bound on one `action` (approval up to 300 s, confirmation up
/// to 60 s, then the action; client: 400 s).
pub const ACTION_BUDGET: Duration = Duration::from_secs(395);
/// Server-side bound on one `stop` (client: 360 s).
pub const STOP_BUDGET: Duration = Duration::from_secs(355);
/// Server-side bound on one `status` (client: 15 s).
pub const STATUS_BUDGET: Duration = Duration::from_secs(10);

/// One request body.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Start {
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        turn_id: Option<String>,
    },
    Screenshot {
        #[serde(default)]
        session_id: Option<String>,
    },
    Action {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        turn_id: Option<String>,
        action: ActionRequest,
    },
    Stop {
        #[serde(default)]
        session_id: Option<String>,
    },
    Status {},
}

/// The total time one op may take on the server.
pub fn op_budget(request: &Request) -> Duration {
    match request {
        Request::Start { .. } => START_BUDGET,
        Request::Screenshot { .. } => SCREENSHOT_BUDGET,
        Request::Action { .. } => ACTION_BUDGET,
        Request::Stop { .. } => STOP_BUDGET,
        Request::Status {} => STATUS_BUDGET,
    }
}

/// The router, with its own state. The caller merges it into the gateway
/// app, which is served with `into_make_service_with_connect_info`.
pub fn router(sessions: Arc<ComputerUseSessions>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(sessions)
}

fn error_response(err: &OpError) -> Response {
    let status = StatusCode::from_u16(err.code.http_status()).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(err.to_json())).into_response()
}

fn unauthorized() -> Response {
    error_response(&OpError::new(
        ErrorCode::Unauthorized,
        "拒絕存取：這個端點只接受 DuDuClaw gateway 啟動的 MCP 程序在本機呼叫，並需通過員工身分驗證。",
    ))
}

fn valid_session_id(id: &Option<String>) -> bool {
    id.as_deref().is_none_or(|id| {
        id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    })
}

fn valid_turn_id(id: &Option<String>) -> bool {
    id.as_deref().is_none_or(super::turns::valid_turn_id)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn handle(
    State(sessions): State<Arc<ComputerUseSessions>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // The peer check needs no body; the signature covers the body, so it
    // must be read before the rest of authentication.
    if let Err(failure) = super::auth::check_peer(peer) {
        tracing::debug!(reason = failure.as_str(), "computer-use route refused");
        return unauthorized();
    }
    let body = match body {
        Ok(body) => body,
        Err(_) => {
            return error_response(&OpError::new(
                ErrorCode::PayloadTooLarge,
                "請求內容太大或無法讀取（上限 64 KiB）。",
            ));
        }
    };
    let caller = match super::auth::authenticate(sessions.home(), peer, &headers, &body, unix_now()) {
        Ok(caller) => caller,
        Err(failure) => {
            tracing::debug!(reason = failure.as_str(), "computer-use route refused");
            return unauthorized();
        }
    };
    let now = Instant::now();
    if !sessions.rate.allow(&caller.agent_id, now) {
        return error_response(&OpError::new(
            ErrorCode::RateLimited,
            "電腦操作請求太頻繁（每位員工每分鐘最多 120 次），請稍後再試。",
        ));
    }
    if !sessions.replay.record(&caller.nonce, now) {
        tracing::debug!(reason = super::auth::AuthFailure::Replayed.as_str(), "computer-use route refused");
        return unauthorized();
    }
    let agent_id = caller.agent_id;
    let request: Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            return error_response(&OpError::new(
                ErrorCode::BadRequest,
                "請求格式不正確：需要 JSON 物件，op 為 start、screenshot、action、stop 或 status 之一。",
            ));
        }
    };
    let budget = op_budget(&request);
    let op = async {
        match request {
            Request::Start { width, height, task, turn_id } if valid_turn_id(&turn_id) => {
                sessions
                    .start(&agent_id, StartRequest { width, height, task, turn_id })
                    .await
            }
            Request::Start { .. } => Err(OpError::new(ErrorCode::BadRequest, "turn_id 格式不正確。")),
            Request::Screenshot { session_id } if valid_session_id(&session_id) => {
                sessions.screenshot(&agent_id, session_id.as_deref()).await
            }
            Request::Action { session_id, turn_id, action }
                if valid_session_id(&session_id) && valid_turn_id(&turn_id) =>
            {
                sessions
                    .action(&agent_id, session_id.as_deref(), turn_id.as_deref(), &action)
                    .await
            }
            Request::Action { session_id, .. } if valid_session_id(&session_id) => {
                Err(OpError::new(ErrorCode::BadRequest, "turn_id 格式不正確。"))
            }
            Request::Stop { session_id } if valid_session_id(&session_id) => {
                sessions.stop(&agent_id, session_id.as_deref()).await
            }
            Request::Status {} => Ok(sessions.status(&agent_id).await),
            // A malformed session id can never name a session of this employee.
            _ => Err(super::not_found()),
        }
    };
    let outcome: Result<Value, OpError> = match tokio::time::timeout(budget, op).await {
        Ok(outcome) => outcome,
        Err(_) => {
            tracing::warn!(agent = %agent_id, "computer-use request exceeded its time bound");
            Err(OpError::new(
                ErrorCode::Timeout,
                format!(
                    "電腦操作請求處理超過 {} 秒，已中止；請先截圖確認目前畫面再繼續。",
                    budget.as_secs()
                ),
            ))
        }
    };
    match outcome {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(err) => error_response(&err),
    }
}
