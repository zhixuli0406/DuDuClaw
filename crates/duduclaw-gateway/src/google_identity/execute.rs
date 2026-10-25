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

use super::{
    Refusal, native_tools,
    requester_gate::people_for,
    resolve::{Resolved, resolve_person},
};
use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};
use crate::google_workspace::GoogleBackend;

#[path = "execute_authority.rs"]
mod authority;

pub const ROUTE: &str = "/api/internal/google-execute";
pub const MAX_BODY: usize = 256 * 1024;
pub const MAX_RESPONSE: usize = 512 * 1024;

pub struct ExecuteState {
    home: PathBuf,
    users: Option<Arc<UserDb>>,
    replay: ReplayGuard,
    rate: RateLimiter,
}
impl ExecuteState {
    pub fn new(home: PathBuf, users: Option<Arc<UserDb>>) -> Self {
        Self {
            home,
            users,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteRequest {
    tool: String,
    arguments: Value,
    credential_ref: CredentialRef,
}

pub fn router(state: Arc<ExecuteState>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
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
            refuse(StatusCode::FORBIDDEN, error.code, &error.message_zh)
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

async fn credential_check(
    state: &ExecuteState,
    agent: &str,
    request: &ExecuteRequest,
) -> Result<(), Refusal> {
    let (home, reference, agent, tool) = (
        state.home.clone(),
        request.credential_ref.clone(),
        agent.to_string(),
        request.tool.clone(),
    );
    tokio::task::spawn_blocking(move || {
        super::credential_registry::authorize(&home, &reference, &agent, &tool)
    })
    .await
    .map_err(|_| Refusal::hard("credential_refused", "Google 專用連線暫時無法查驗。"))?
    .map_err(|message| Refusal::hard("credential_refused", message))
}

async fn serve(
    state: &ExecuteState,
    agent: &str,
    request: &ExecuteRequest,
) -> Result<Value, Refusal> {
    credential_check(state, agent, request).await?;
    if !crate::google_workspace::integration_enabled(&state.home) {
        return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
    }
    // A dedicated API key proves the employee, never a person or system turn.
    let requester = RequesterIdentity::Unknown;
    let people = people_for(&state.home, state.users.as_ref());
    let authority = authority::Authority::capture(state, agent).await?;
    let outcome = resolve_person(&state.home, agent, &requester, people.as_ref()).await;
    if let Ok(resolved) = &outcome {
        authority.check_selected(resolved)?;
    }
    let (backend, account, fallback, notice) = match outcome {
        Ok(Resolved::Delegated { email, token }) => (
            GoogleBackend::Direct(token.access_token),
            email,
            false,
            None,
        ),
        Ok(Resolved::UseHome { fallback, notice }) => {
            let backend = crate::google_workspace::resolve_backend(&state.home)
                .await
                .map_err(|_| {
                    Refusal::hard("account_unavailable", "公司共用 Google 帳號暫時無法使用。")
                })?;
            (backend, String::new(), fallback, notice)
        }
        Ok(Resolved::Personal { .. }) => {
            return Err(Refusal::hard(
                "personal_external_unsupported",
                "這條專用外部連線尚未支援個人 Google 帳號。",
            ));
        }
        Err(error) => return Err(error),
    };
    // Minting/refresh may await a provider; revocation during that wait must
    // stop the effect instead of relying on the pre-mint metadata check.
    let backend = authority
        .validate(state, agent, request, fallback, backend)
        .await?;
    let access = HostAccess(backend);
    let mut result = if request.tool == "google_status" {
        let message = if account.is_empty() {
            "目前使用公司共用 Google 帳號。".to_string()
        } else {
            format!("目前代表 {account} 使用 Google；連線可用。")
        };
        json!({"content":[{"type":"text","text":message}]})
    } else {
        crate::google_workspace::with_dedicated_write_policy(dispatch_native(
            &state.home,
            &request.tool,
            &request.arguments,
            &access,
        ))
        .await
    };
    if let Some(notice) = notice
        && let Some(content) = result.get_mut("content").and_then(Value::as_array_mut)
    {
        content.insert(0, json!({"type":"text","text":notice}));
    }
    let success = result.get("isError").and_then(Value::as_bool) != Some(true);
    if serde_json::to_vec(&json!({"ok":true,"result":&result}))
        .map_or(true, |bytes| bytes.len() > MAX_RESPONSE)
    {
        return Err(Refusal::hard("result_unconfirmed", UNCERTAIN));
    }
    audit(
        &state.home,
        agent,
        &request.tool,
        &account,
        success,
        fallback,
        "",
    );
    Ok(result)
}

struct HostAccess(GoogleBackend);
#[async_trait::async_trait]
impl native_tools::NativeGoogleAccess for HostAccess {
    async fn backend(&self, _: &Path, _: &str) -> Result<GoogleBackend, String> {
        Ok(match &self.0 {
            GoogleBackend::Direct(token) => GoogleBackend::Direct(token.clone()),
            GoogleBackend::AppsScript(config) => GoogleBackend::AppsScript(config.clone()),
        })
    }
    async fn token(&self, _: &Path, _: &str) -> Result<String, String> {
        match &self.0 {
            GoogleBackend::Direct(token) => Ok(token.clone()),
            // No credential acquisition may run after the final authority
            // check. Direct acquisition already ran before selecting a bridge.
            GoogleBackend::AppsScript(_) => Err("這個工具需要有效的 Google 直接連線。".into()),
        }
    }
}

async fn dispatch_native(home: &Path, tool: &str, args: &Value, access: &HostAccess) -> Value {
    use native_tools::*;
    match tool {
        "gmail_search" => handle_gmail_search(args, home, access).await,
        "gmail_read" => handle_gmail_read(args, home, access).await,
        "gmail_create_draft" => handle_gmail_create_draft(args, home, access).await,
        "calendar_list_events" => handle_calendar_list_events(args, home, access).await,
        "calendar_create_event" => handle_calendar_create_event(args, home, access).await,
        "sheets_read" => handle_sheets_read(args, home, access).await,
        "sheets_append" => handle_sheets_append(args, home, access).await,
        "forms_get" => handle_forms_get(args, home, access).await,
        "forms_list_responses" => handle_forms_list_responses(args, home, access).await,
        "gtasks_lists" => handle_gtasks_lists(home, access).await,
        "gtasks_list" => handle_gtasks_list(args, home, access).await,
        "gtasks_create" => handle_gtasks_create(args, home, access).await,
        "gtasks_complete" => handle_gtasks_complete(args, home, access).await,
        "drive_search" => handle_drive_search(args, home, access).await,
        "drive_read" => handle_drive_read(args, home, access).await,
        "docs_read" => handle_docs_read(args, home, access).await,
        "docs_append" => handle_docs_append(args, home, access).await,
        "slides_read" => handle_slides_read(args, home, access).await,
        _ => json!({"content":[{"type":"text","text":"Google 工具不存在。"}],"isError":true}),
    }
}

fn audit(
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
