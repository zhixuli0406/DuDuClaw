use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use duduclaw_auth::{UserContext, UserDb};
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{Refusal, resolve};
use crate::computer_use_sessions::auth::{self, RateLimiter, ReplayGuard};

pub(crate) const ROUTE: &str = "/api/internal/odoo-identity";
const MAX_BODY_BYTES: usize = 4 * 1024;

pub(crate) struct OdooIdentityRouteState {
    pub home: PathBuf,
    pub users: Arc<UserDb>,
    replay: ReplayGuard,
    rate: RateLimiter,
}

impl OdooIdentityRouteState {
    pub fn new(home: PathBuf, users: Arc<UserDb>) -> Self {
        Self { home, users, replay: ReplayGuard::default(), rate: RateLimiter::default() }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmployeeRequest {
    tool: String,
    turn_id: Option<String>,
    run_id: Option<String>,
    have_rev: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonRequest {
    tool: String,
    have_rev: Option<i64>,
}

pub(crate) fn router(state: Arc<OdooIdentityRouteState>) -> Router {
    Router::new().route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)).with_state(state)
}

/// Closed tool-name classification: no prefix-based authorization. New tools
/// must be reviewed and added explicitly, particularly to the person subset.
fn read_tool(tool: &str) -> bool {
    matches!(tool, "odoo_search" | "odoo_read" | "odoo_status" | "odoo_crm_leads"
        | "odoo_sale_orders" | "odoo_inventory_products" | "odoo_inventory_check"
        | "odoo_invoice_list" | "odoo_payment_status" | "odoo_report"
        | "odoo_partner_search" | "odoo_schema_overview" | "odoo_schema_fields")
}

fn person_write(tool: &str) -> Result<bool, Refusal> {
    use duduclaw_core::mcp_person_tools::{person_tool, PersonToolHost};
    match person_tool(tool).filter(|t| t.executes_in == PersonToolHost::McpWithPersonalOdoo) {
        Some(t) if t.scope == "odoo:read" => Ok(false),
        Some(t) if t.scope == "odoo:write" => Ok(true),
        _ => Err(Refusal::bad_request()),
    }
}

fn employee_tool(tool: &str) -> bool {
    read_tool(tool) || person_write(tool).is_ok() || matches!(tool, "odoo_connect" | "odoo_execute"
        | "odoo_crm_create_lead" | "odoo_crm_update_stage" | "odoo_sale_create_quotation"
        | "odoo_sale_confirm" | "odoo_message_send" | "odoo_attachment_add")
}

fn audit_tool(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body).ok()
        .and_then(|v| v.get("tool").and_then(Value::as_str)
            .filter(|tool| employee_tool(tool)).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

async fn handle(
    State(state): State<Arc<OdooIdentityRouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if auth::check_peer(peer).is_err() { return Refusal::unauthorized().into_response(); }
    let Ok(body) = body else { return Refusal::bad_request().into_response(); };
    // Authentication reads keys/SQLite; policy, unseal and audit are blocking
    // too. Keep all of them off the async executor, with no guards over await.
    let result = tokio::task::spawn_blocking(move || {
        if headers.contains_key(header::AUTHORIZATION) {
            person_request(&state, &headers, &body)
        } else {
            employee_request(&state, peer, &headers, &body)
        }
    }).await.unwrap_or_else(|_| Err(Refusal::unavailable()));
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

fn employee_request(state: &OdooIdentityRouteState, peer: SocketAddr,
    headers: &HeaderMap, body: &[u8]) -> Result<Value, Refusal> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Refusal::unavailable())?.as_secs();
    let caller = auth::authenticate(&state.home, peer, headers, body, now)
        .map_err(|_| Refusal::unauthorized())?;
    let mut have_rev = None;
    let result = (|| {
        let now = Instant::now();
        if !state.rate.allow(&caller.agent_id, now) { return Err(Refusal::rate_limited()); }
        if !state.replay.record(&caller.nonce, now) { return Err(Refusal::unauthorized()); }
        let request: EmployeeRequest = serde_json::from_slice(body).map_err(|_| Refusal::bad_request())?;
        have_rev = request.have_rev;
        // Bad access policy refuses every tool before identity/credential work.
        let settings = resolve::Settings::load(&state.home)?;
        if !employee_tool(&request.tool) { return Err(Refusal::bad_request()); }
        let requester = crate::knowledge::turn_requesters::resolve_with_run(
            &caller.agent_id, request.turn_id.as_deref(), request.run_id.as_deref(),
        );
        resolve::employee(&state.home, &state.users, &caller.agent_id, &requester, &settings)
    })();
    let decision = result.as_ref().ok();
    // Persist the employee-attributed decision before serializing any secret.
    duduclaw_security::audit::try_append_tool_call_with_extras(
        &state.home, &caller.agent_id, "odoo_identity", "identity decision", result.is_ok(),
        &[("tool", json!(audit_tool(body))),
          ("identity", json!(decision.map(|d| d.identity_name()))),
          ("user_id", json!(decision.and_then(|d| d.user_id()))),
          ("outcome", json!(result.as_ref().map(|_| "ok").unwrap_or_else(|e| e.code)))],
    ).map_err(|_| Refusal::unavailable())?;
    result.map(|decision| decision.response(have_rev))
}

fn bearer(headers: &HeaderMap) -> Result<Zeroizing<String>, Refusal> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or_else(Refusal::unauthorized)?;
    if values.next().is_some() { return Err(Refusal::unauthorized()); }
    let raw = value.to_str().ok().and_then(|value| value.strip_prefix("Bearer "))
        .filter(|raw| duduclaw_auth::member_keys::is_member_key(raw))
        .ok_or_else(Refusal::unauthorized)?;
    Ok(Zeroizing::new(raw.to_owned()))
}

/// Both the key row and the administrator allow-list are re-read, including
/// just before releasing a credential. No cached scopes grant authority.
fn require_scope(state: &OdooIdentityRouteState, context: &UserContext, write: bool) -> Result<(), Refusal> {
    let live = crate::member_keys::refresh_authority(&state.home, &state.users, context)
        .map_err(Refusal::authority)?;
    let settings = crate::member_keys::read_config(&state.home).map_err(Refusal::authority)?;
    if !settings.enabled { return Err(Refusal::unauthorized()); }
    let has = |scope: &str| live.mcp_scopes.iter().any(|s| s == scope)
        && settings.allow_mcp_scopes.iter().any(|s| s == scope);
    if !has("odoo:read") || (write && !has("odoo:write")) { return Err(Refusal::scope()); }
    Ok(())
}

fn person_request(state: &OdooIdentityRouteState, headers: &HeaderMap, body: &[u8]) -> Result<Value, Refusal> {
    let mut context = None;
    let mut have_rev = None;
    let result = (|| {
        let raw = bearer(headers)?;
        let ctx = crate::member_keys::authenticate(&state.home, &state.users, &raw)
            .map_err(Refusal::authority)?;
        context = Some(ctx.clone());
        crate::member_keys::record_call(&state.home, &state.users, &ctx, "mcp.odoo_identity", false)
            .map_err(Refusal::authority)?;
        let request: PersonRequest = serde_json::from_slice(body).map_err(|_| Refusal::bad_request())?;
        have_rev = request.have_rev;
        let settings = resolve::Settings::load(&state.home)?;
        let write = person_write(&request.tool)?;
        require_scope(state, &ctx, write)?;
        let decision = resolve::person_key(&state.home, &state.users, &ctx.user_id, &settings, write)?;
        require_scope(state, &ctx, write)?;
        Ok(decision)
    })();
    let decision = result.as_ref().ok();
    let event = AuditEvent::new("odoo_identity", "", if result.is_ok() { Severity::Info } else { Severity::Warning },
        json!({"mode":"person_key", "key_id":context.as_ref().and_then(|ctx| ctx.member_key_id.as_deref()),
            "user_id":context.as_ref().map(|ctx| ctx.user_id.as_str()), "tool":audit_tool(body),
            "identity":decision.map(|d| d.identity_name()),
            "outcome":result.as_ref().map(|_| "ok").unwrap_or_else(|e: &Refusal| e.code)}));
    try_append_audit_event(&state.home, &event).map_err(|_| Refusal::unavailable())?;
    result.map(|decision| decision.response(have_rev))
}
