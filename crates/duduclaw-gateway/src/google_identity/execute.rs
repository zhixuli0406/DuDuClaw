//! Dedicated MCP keys get tool results, never Google bearers. A credential
//! digest is metadata only; the signed local host attests actual possession
//! after the MCP dispatcher has applied its normal employee policy gates.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use duduclaw_auth::db::UserDb;
use duduclaw_core::google_identity::{CredentialRef, native_google_scope};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Refusal;
use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};

#[path = "account_selection.rs"]
mod selection;

pub const ROUTE: &str = "/api/internal/google-execute";
pub const MAX_BODY: usize = 256 * 1024;
pub const MAX_RESPONSE: usize = 512 * 1024;

pub struct ExecuteState {
    home: PathBuf,
    users: Option<Arc<UserDb>>,
    replay: ReplayGuard,
    rate: RateLimiter,
    selections: selection::SelectionStore,
}
impl ExecuteState {
    pub fn new(home: PathBuf, users: Option<Arc<UserDb>>) -> Self {
        Self {
            home,
            users,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
            selections: selection::SelectionStore::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteRequest {
    tool: String,
    arguments: Value,
    credential_ref: CredentialRef,
    #[serde(default)]
    selection_stamp: Option<String>,
}

pub fn router(state: Arc<ExecuteState>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .route(
            duduclaw_core::google_identity::GOOGLE_SELECTION_ROUTE,
            post(selection::handle),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}
fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok":false,"code":code,"message":message})),
    )
        .into_response()
}
fn unauthorized() -> Response {
    refuse(
        StatusCode::UNAUTHORIZED,
        "no_verified_host",
        "無法確認這個 Google 請求來自 DuDuClaw 主程式。",
    )
}
const UNCERTAIN: &str =
    "Google 操作結果無法確認。若這是寫入，請先到 Google 檢查是否已完成，勿直接重試。";

async fn handle(
    State(state): State<Arc<ExecuteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() {
        return unauthorized();
    }
    let Ok(body) = body else {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "Google 請求過大或無法讀取。",
        );
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let caller = match authenticate(&state.home, peer, &headers, &body, now) {
        Ok(caller) => caller,
        Err(_) => return unauthorized(),
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Google 請求過於頻繁，請稍後再試。",
        );
    }
    if !state.replay.record(&caller.nonce, now) {
        return unauthorized();
    }
    let request = match serde_json::from_slice::<ExecuteRequest>(&body) {
        Ok(request)
            if native_google_scope(&request.tool).is_some()
                && request.arguments.is_object()
                && request.credential_ref.is_well_formed() =>
        {
            request
        }
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Google 專用連線的請求格式不正確。",
            );
        }
    };
    match tokio::time::timeout(
        Duration::from_secs(120),
        Box::pin(serve(&state, &caller.agent_id, &request)),
    )
    .await
    {
        Ok(Ok(result)) => {
            let response = json!({"ok":true,"result":result});
            if serde_json::to_vec(&response).map_or(true, |bytes| bytes.len() > MAX_RESPONSE) {
                return refuse(StatusCode::BAD_GATEWAY, "result_unconfirmed", UNCERTAIN);
            }
            Json(response).into_response()
        }
        Ok(Err(error)) => {
            audit(
                &state.home,
                &caller.agent_id,
                &request.tool,
                "",
                false,
                false,
                error.code,
            );
            let status = if error.code == "result_unconfirmed" {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::FORBIDDEN
            };
            refuse(status, error.code, &error.message_zh)
        }
        Err(_) => {
            audit(
                &state.home,
                &caller.agent_id,
                &request.tool,
                "",
                false,
                false,
                "timeout_unconfirmed",
            );
            refuse(StatusCode::GATEWAY_TIMEOUT, "result_unconfirmed", UNCERTAIN)
        }
    }
}

async fn serve(
    state: &ExecuteState,
    agent: &str,
    request: &ExecuteRequest,
) -> Result<Value, Refusal> {
    if let Some(id) = request.selection_stamp.as_deref() {
        // Synchronous one-shot consumption precedes every credential/resolve
        // await. Even a later failure never puts the private entry back.
        let pending = selection::claim_selection(
            state,
            agent,
            &request.credential_ref,
            &request.tool,
            id,
            &request.arguments,
            crate::google_identity::effect_authority::Clock::now(),
        )?;
        if pending.is_metadata_status() {
            return pending.metadata_status();
        }
        return pending.execute_native(request.arguments.clone()).await;
    }
    if matches!(
        duduclaw_core::google_identity::effective_for(&state.home, Some(agent)).effective,
        duduclaw_core::google_identity::Effective::Personal { .. }
    ) {
        return Err(Refusal::hard(
            "personal_external_unsupported",
            "個人 Google 專用連線需要本次已核准的帳號選擇，這次沒有執行操作。",
        ));
    }
    crate::google_identity::effect_authority::execute_immediate_native(
        &state.home,
        state.users.as_ref(),
        agent,
        &request.credential_ref,
        &request.tool,
        request.arguments.clone(),
    )
    .await
}

pub(super) fn audit(
    home: &Path,
    agent: &str,
    tool: &str,
    account: &str,
    success: bool,
    fallback: bool,
    reason: &str,
) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home,
        agent,
        "google_execute",
        tool,
        success,
        &[
            ("google_tool", json!(tool)),
            ("google_account", json!(account)),
            ("requester_kind", json!("unknown")),
            ("fallback", json!(fallback)),
            ("reason", json!(reason)),
        ],
    );
}
