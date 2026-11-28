//! Gateway-owned person-key MCP bridge. Only the Authorization bearer selects
//! the holder; no server employee, JWT or body-supplied identity is used.
//!
//! POST /api/internal/person-key (loopback, at most 64 KiB):
//! - {"op":"authenticate"} -> {ok:true,key_id,user_id,scopes}; one call charged.
//! - {"op":"call","tool":...,"args":{...}} -> {ok:true,result}.
//! my_tasks_list accepts only optional string `status` and positive integer
//! `limit` (1..=200, default 50). It calls tasks.list for at most 50 live ACL
//! employees in id order, collects at most 5 * limit rows, and sorts the
//! collected rows newest-first. Omitted employees/rows set `truncated: true`.
//! my_agents_status accepts {} and projects agents.list with agents.callable
//! membership into {agents:[{id,name,status,can_call}]} (name is display_name).
//! Only the ordinary handler charges RPCs; no extra charge for a tool wrapper.
//! Refusals are {ok:false,code,message}: 400 bad request, 401 invalid key,
//! 403 scope/loopback/handler refusal, 429 rate limit, 503 unavailable authority.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use duduclaw_auth::{AccessLevel, UserContext, UserDb};
use duduclaw_core::mcp_person_tools::{PersonTool, PersonToolHost, person_tool};
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;

pub const ROUTE: &str = "/api/internal/person-key";
pub const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_TASK_LIMIT: usize = 200;
const MAX_TASK_EMPLOYEES: usize = 50;
const TASK_ROW_MULTIPLIER: usize = 5;

pub struct PersonKeyRouteState {
    pub home: PathBuf,
    pub user_db: Arc<UserDb>,
    pub handler: Arc<MethodHandler>,
}

impl PersonKeyRouteState {
    pub fn new(home: PathBuf, user_db: Arc<UserDb>, handler: Arc<MethodHandler>) -> Self {
        Self { home, user_db, handler }
    }
}

pub fn router(state: Arc<PersonKeyRouteState>) -> Router {
    Router::new().route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)).with_state(state)
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum PersonRequest {
    Authenticate {},
    Call { tool: String, args: Value },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskArgs {
    #[serde(default, deserialize_with = "present")]
    status: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}
fn default_limit() -> usize { 50 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

enum ToolCall {
    Tasks(TaskArgs),
    Agents,
}

impl ToolCall {
    fn parse(name: &str, args: Value) -> Result<(Self, &'static PersonTool), Refusal> {
        let tool = person_tool(name).filter(|t| t.executes_in == PersonToolHost::Gateway)
            .ok_or_else(Refusal::bad_request)?;
        if !args.is_object() { return Err(Refusal::bad_request()); }
        let call = match tool.name {
            "my_tasks_list" => {
                let args: TaskArgs = serde_json::from_value(args).map_err(|_| Refusal::bad_request())?;
                if !(1..=MAX_TASK_LIMIT).contains(&args.limit) { return Err(Refusal::bad_request()); }
                Self::Tasks(args)
            }
            "my_agents_status" => {
                let _: EmptyArgs = serde_json::from_value(args).map_err(|_| Refusal::bad_request())?;
                Self::Agents
            }
            _ => return Err(Refusal::bad_request()),
        };
        Ok((call, tool))
    }
}

struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl Refusal {
    fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self { status, code, message: message.to_owned() }
    }
    fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "person_key_bad_request", "Invalid person-key request or tool arguments.")
    }
    fn invalid() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "person_key_invalid", "The personal key is invalid or disabled.")
    }
    fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "person_key_unavailable", "The personal-key service is unavailable.")
    }
    fn scope() -> Self {
        Self::new(StatusCode::FORBIDDEN, "person_key_scope", "The personal key does not grant this tool's scope.")
    }
    fn authority(error: String) -> Self {
        match error.as_str() {
            "member key invalid" | "member keys disabled" => Self::invalid(),
            "member_key_rate_limited" => Self::new(StatusCode::TOO_MANY_REQUESTS,
                "member_key_rate_limited", "Too many calls for this personal key."),
            _ => Self::unavailable(),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"ok":false, "code":self.code, "message":self.message}))).into_response()
    }
}

fn bearer(headers: &HeaderMap) -> Result<String, Refusal> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or_else(Refusal::invalid)?;
    if values.next().is_some() { return Err(Refusal::invalid()); }
    let value = value.to_str().map_err(|_| Refusal::invalid())?;
    let raw = value.strip_prefix("Bearer ").ok_or_else(Refusal::invalid)?;
    if !duduclaw_auth::member_keys::is_member_key(raw) { return Err(Refusal::invalid()); }
    Ok(raw.to_owned())
}

struct Authority {
    context: UserContext,
    scopes: Vec<String>,
}

fn authority(home: &std::path::Path, db: &UserDb, ctx: &UserContext) -> Result<Authority, Refusal> {
    let live = crate::member_keys::refresh_authority(home, db, ctx).map_err(Refusal::authority)?;
    let settings = crate::member_keys::read_config(home).map_err(Refusal::authority)?;
    if !settings.enabled { return Err(Refusal::invalid()); }
    let scopes: BTreeSet<String> = live.mcp_scopes.into_iter()
        .filter(|s| settings.allow_mcp_scopes.contains(s)).collect();
    Ok(Authority { context: live.context, scopes: scopes.into_iter().collect() })
}

async fn authenticate(state: &PersonKeyRouteState, raw: String) -> Result<Authority, Refusal> {
    let home = state.home.clone();
    let db = state.user_db.clone();
    tokio::task::spawn_blocking(move || {
        let ctx = crate::member_keys::authenticate(&home, &db, &raw).map_err(Refusal::authority)?;
        authority(&home, &db, &ctx)
    }).await.map_err(|_| Refusal::unavailable())?
}

async fn refresh(state: &PersonKeyRouteState, ctx: &UserContext) -> Result<Authority, Refusal> {
    let home = state.home.clone();
    let db = state.user_db.clone();
    let ctx = ctx.clone();
    tokio::task::spawn_blocking(move || authority(&home, &db, &ctx))
        .await.map_err(|_| Refusal::unavailable())?
}

fn require_scope(live: &Authority, tool: &PersonTool) -> Result<(), Refusal> {
    if live.scopes.iter().any(|s| s == tool.scope) { Ok(()) } else { Err(Refusal::scope()) }
}

async fn handle(
    State(state): State<Arc<PersonKeyRouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if !peer.ip().is_loopback() {
        return Refusal::new(StatusCode::FORBIDDEN, "person_key_loopback", "This endpoint requires a loopback peer.").into_response();
    }
    let Ok(body) = body else { return Refusal::bad_request().into_response(); };
    let Ok(request) = serde_json::from_slice::<PersonRequest>(&body) else {
        return Refusal::bad_request().into_response();
    };
    let result = match request {
        PersonRequest::Authenticate {} => authenticate_op(&state, &headers).await,
        PersonRequest::Call { tool, args } => {
            let mut context = None;
            let result = async {
                let (call, definition) = ToolCall::parse(&tool, args)?;
                let live = authenticate(&state, bearer(&headers)?).await?;
                context = Some(live.context.clone());
                require_scope(&live, definition)?;
                let result = execute(&state, &live.context, definition, call).await?;
                // Do not release a previously collected result after authority narrowed.
                require_scope(&refresh(&state, &live.context).await?, definition)?;
                Ok(json!({"ok":true, "result":result}))
            }.await;
            // Only canonical names enter the audit, never arbitrary request text.
            let audit_tool = person_tool(&tool).map(|t| t.name).unwrap_or("unknown");
            let outcome = result.as_ref().map(|_| "ok").unwrap_or_else(|e: &Refusal| e.code);
            let event = AuditEvent::new("person_key_mcp_call", "", if result.is_ok() { Severity::Info } else { Severity::Warning },
                json!({"key_id":context.as_ref().and_then(|ctx| ctx.member_key_id.as_deref()),
                    "user_id":context.as_ref().map(|ctx| ctx.user_id.as_str()), "tool":audit_tool, "outcome":outcome}));
            let home = state.home.clone();
            if !matches!(tokio::task::spawn_blocking(move || try_append_audit_event(&home, &event)).await, Ok(Ok(()))) {
                return Refusal::unavailable().into_response();
            }
            result
        }
    };
    match result {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn authenticate_op(state: &PersonKeyRouteState, headers: &HeaderMap) -> Result<Value, Refusal> {
    let live = authenticate(state, bearer(headers)?).await?;
    let home = state.home.clone();
    let db = state.user_db.clone();
    let ctx = live.context;
    tokio::task::spawn_blocking(move || {
        crate::member_keys::record_call(&home, &db, &ctx, "mcp.authenticate", false).map_err(Refusal::authority)?;
        let live = authority(&home, &db, &ctx)?;
        let id = live.context.member_key_id.as_deref().ok_or_else(Refusal::invalid)?;
        Ok(json!({"ok":true, "key_id":id, "user_id":live.context.user_id, "scopes":live.scopes}))
    }).await.map_err(|_| Refusal::unavailable())?
}

/// Preserve every normal handler gate, and recheck scope between composite RPCs.
async fn rpc(state: &PersonKeyRouteState, ctx: &UserContext, tool: &PersonTool, method: &str, args: Value) -> Result<Value, Refusal> {
    let live = refresh(state, ctx).await?;
    require_scope(&live, tool)?;
    let frame = state.handler.handle(method, args, &live.context).await;
    match frame {
        WsFrame::Response { ok: true, payload: Some(payload), .. } => Ok(payload),
        WsFrame::Response { ok: false, error: Some(error), .. } => {
            // The ordinary handler intentionally masks some credential failures;
            // recover their HTTP category by checking authority, never by falling back.
            require_scope(&refresh(state, ctx).await?, tool)?;
            let code = error.get("code").and_then(Value::as_str).unwrap_or("");
            let message = error.as_str().or_else(|| error.get("message").and_then(Value::as_str))
                .unwrap_or("The requested operation was refused.");
            if code == "member_key_rate_limited" {
                return Err(Refusal::new(StatusCode::TOO_MANY_REQUESTS, "member_key_rate_limited", message));
            }
            if matches!(code, "member_key_unavailable" | "member_key_refused")
                || matches!(message, "authentication service unavailable" | "member key invalid or unavailable" | "Task store not initialized")
                || message.split_once(": ").is_some_and(|(category, _)| category == "list tasks") {
                return Err(Refusal::unavailable());
            }
            Err(Refusal::new(StatusCode::FORBIDDEN, "person_key_forbidden", message))
        }
        _ => Err(Refusal::unavailable()),
    }
}

async fn execute(state: &PersonKeyRouteState, ctx: &UserContext, tool: &PersonTool, call: ToolCall) -> Result<Value, Refusal> {
    match call {
        ToolCall::Tasks(args) => {
            // Candidate names are exclusively gateway-authenticated live ACLs.
            // Each scoped RPC independently authorizes them again.
            let agents: BTreeSet<String> = ctx.agent_access.keys().chain(ctx.team_access.keys())
                .filter(|id| !ctx.hides_agent(id)
                    && (ctx.has_agent_access(id, AccessLevel::Viewer) || ctx.can_assign_to(id)))
                .cloned().collect();
            let mut tasks = Vec::new();
            let row_ceiling = args.limit * TASK_ROW_MULTIPLIER;
            let mut truncated = agents.len() > MAX_TASK_EMPLOYEES;
            if agents.is_empty() {
                // Still traverse a normal member-key gate for rate/audit accounting.
                rpc(state, ctx, tool, "agents.list", json!({})).await?;
            }
            for agent in agents.into_iter().take(MAX_TASK_EMPLOYEES) {
                if tasks.len() == row_ceiling {
                    truncated = true;
                    break;
                }
                let remaining = row_ceiling - tasks.len();
                let mut params = json!({"agent_id":agent, "limit":remaining});
                if let Some(status) = &args.status { params["status"] = json!(status); }
                // Bound SQL materialization and enrichment within the member contract.
                let payload = rpc(state, ctx, tool, "tasks.list", params).await?;
                truncated |= payload.get("more_may_exist").and_then(Value::as_bool).unwrap_or(false);
                let rows = payload.get("tasks").and_then(Value::as_array).ok_or_else(Refusal::unavailable)?;
                truncated |= rows.len() >= remaining;
                tasks.extend(rows.iter().take(remaining).cloned());
            }
            tasks.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str())
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
            truncated |= tasks.len() > args.limit;
            tasks.truncate(args.limit);
            Ok(json!({"tasks":tasks, "truncated":truncated}))
        }
        ToolCall::Agents => {
            let listed = rpc(state, ctx, tool, "agents.list", json!({})).await?;
            let callable = rpc(state, ctx, tool, "agents.callable", json!({})).await?;
            let listed = listed.get("agents").and_then(Value::as_array).ok_or_else(Refusal::unavailable)?;
            let callable = callable.get("agents").and_then(Value::as_array).ok_or_else(Refusal::unavailable)?;
            let callable_ids: BTreeSet<&str> = callable.iter().map(|a| a["name"].as_str().ok_or_else(Refusal::unavailable))
                .collect::<Result<_, _>>()?;
            let mut agents = Vec::with_capacity(listed.len());
            for agent in listed {
                let id = agent["name"].as_str().ok_or_else(Refusal::unavailable)?;
                let name = agent["display_name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id);
                let status = agent["status"].as_str().ok_or_else(Refusal::unavailable)?;
                agents.push(json!({"id":id, "name":name, "status":status, "can_call":callable_ids.contains(id)}));
            }
            agents.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            Ok(json!({"agents":agents}))
        }
    }
}

#[cfg(test)]
mod tests;
