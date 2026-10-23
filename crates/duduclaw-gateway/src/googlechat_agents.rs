//! An AI employee's own Google Chat app.
//!
//! Each employee may be bound to its own Chat app (its own Cloud project and
//! service account). Google posts that app's events to
//! `POST /webhook/googlechat/agents/<employee id>`; every request is verified
//! against THAT employee's project number (JWT audience) before anything is
//! read, and the message is answered by that employee with that app's
//! credentials. An employee without an app keeps the shared app
//! (`/webhook/googlechat`, unchanged).
//!
//! The route is always mounted. Every request without a verified token —
//! missing or malformed bearer, an id with no app configured right now
//! (config is re-read on every request), a token that does not verify —
//! answers the same 401, checked in that order so nothing is read or
//! decrypted for a request without a plausible token. A single source address
//! is limited to 120 requests a minute. Errors never fall through to the
//! shared app.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tracing::{info, warn};

use crate::channel_reply::{ReplyContext, set_channel_connected};
use crate::googlechat::{GoogleChatCreds, GoogleChatState};

/// Route template (axum 0.8 syntax).
pub const ROUTE: &str = "/webhook/googlechat/agents/{agent}";

/// Cached creds per employee, keyed by the service account they sign as, so
/// the OAuth token is reused across messages. A changed key (different
/// `client_email`) replaces the entry; [`forget`] drops it on save/remove.
fn creds_cache() -> &'static Mutex<HashMap<String, Arc<GoogleChatCreds>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<GoogleChatCreds>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Drop the cached creds of `agent_id` (its app changed or was removed).
pub fn forget(agent_id: &str) {
    creds_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(agent_id);
}

/// The employee's own Chat app creds, or `None` when it has none.
pub async fn agent_creds(home: &Path, agent_id: &str) -> Option<Arc<GoogleChatCreds>> {
    let app = crate::agent_channels::store::agent_chat_app(home, agent_id).await?;
    creds_for(home, agent_id, app.service_account.expose())
}

fn creds_for(home: &Path, agent_id: &str, sa_json: &str) -> Option<Arc<GoogleChatCreds>> {
    let email = crate::agent_channels::googlechat_config::client_email(sa_json)?;
    let mut cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.get(agent_id).filter(|c| c.client_email() == email) {
        return Some(c.clone());
    }
    let creds = Arc::new(GoogleChatCreds::for_agent(sa_json, home, agent_id)?);
    cache.insert(agent_id.to_string(), creds.clone());
    Some(creds)
}

/// Router for every employee's own app endpoint.
pub fn router(ctx: Arc<ReplyContext>) -> Router {
    Router::new().route(ROUTE, post(webhook)).with_state(ctx)
}

/// Requests per minute one source address may make to these endpoints
/// before being refused (Google spreads a Chat app's calls over many
/// addresses, so this only bounds a single noisy source).
const MAX_REQUESTS_PER_IP_PER_MIN: u32 = 120;
/// Largest event body read (Chat events are a few KB).
const MAX_BODY_BYTES: usize = 1024 * 1024;

fn ip_allowed(ip: std::net::IpAddr) -> bool {
    use std::time::Instant;
    static SEEN: OnceLock<Mutex<HashMap<std::net::IpAddr, (Instant, u32)>>> = OnceLock::new();
    let mut map = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 4096 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 120);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() >= 60 {
        *entry = (now, 0);
    }
    entry.1 += 1;
    entry.1 <= MAX_REQUESTS_PER_IP_PER_MIN
}

async fn webhook(
    State(ctx): State<Arc<ReplyContext>>,
    UrlPath(agent): UrlPath<String>,
    request: axum::extract::Request,
) -> Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    if let Some(ip) = peer
        && !ip_allowed(ip)
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    handle(ctx, &agent, &parts.headers, body).await
}

/// The bearer token when it is present and its header is a plausible Chat
/// signer token (RS256 with a short key id). Checked before any employee
/// config is read or decrypted (B11 M4).
fn plausible_bearer(headers: &HeaderMap) -> Option<&str> {
    let auth = headers.get("authorization").and_then(|h| h.to_str().ok())?;
    let token = crate::webhook_jwt::bearer_token(auth)?;
    let header = jsonwebtoken::decode_header(token).ok()?;
    let kid_ok = header
        .kid
        .as_deref()
        .is_some_and(|k| !k.is_empty() && k.len() <= 128 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    (header.alg == jsonwebtoken::Algorithm::RS256 && kid_ok).then_some(token)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
}

/// Verify and dispatch one request for `agent` (fail closed). Every refusal
/// before a verified token is the same 401, so a caller cannot tell which
/// employees have an app (B11 L1).
pub(crate) async fn handle(
    ctx: Arc<ReplyContext>,
    agent: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = plausible_bearer(headers) else {
        warn!(agent = duduclaw_core::truncate_chars(agent, 64), "Google Chat (own app): missing or malformed bearer token");
        return unauthorized();
    };
    if !duduclaw_core::is_valid_agent_id(agent) || ctx.registry.read().await.get(agent).is_none() {
        return unauthorized();
    }
    let Some(app) = crate::agent_channels::store::agent_chat_app(&ctx.home_dir, agent).await else {
        return unauthorized();
    };
    let Some(creds) = creds_for(&ctx.home_dir, agent, app.service_account.expose()) else {
        warn!(agent, "Google Chat (own app): stored service-account key is unusable — rejecting");
        return unauthorized();
    };
    if let Err(e) = crate::googlechat::verify_request(&creds, token, &app.project_number).await {
        warn!(agent, "Google Chat (own app): JWT verification failed: {e}");
        return unauthorized();
    }
    let label = crate::agent_channels::label("googlechat", agent);
    set_channel_connected(&ctx.channel_status, &label, true, None, Some(&ctx.event_tx)).await;
    let state = Arc::new(GoogleChatState { ctx, creds, agent: Some(agent.to_string()) });
    crate::googlechat::dispatch_event(state, body).await
}

/// Check one employee's app credentials (OAuth token grant) and record the
/// result under `googlechat:<id>`. Returns the error text on failure.
pub async fn probe(ctx: &ReplyContext, agent: &str) -> Result<(), String> {
    let label = crate::agent_channels::label("googlechat", agent);
    let Some(creds) = agent_creds(&ctx.home_dir, agent).await else {
        ctx.channel_status.write().await.remove(&label);
        crate::agent_channels::identity::clear(&label);
        return Err("not configured".into());
    };
    crate::agent_channels::identity::set(&label, creds.client_email());
    let result = creds.token().await.map(|_| ());
    let error = result.as_ref().err().cloned();
    set_channel_connected(
        &ctx.channel_status,
        &label,
        result.is_ok(),
        error,
        Some(&ctx.event_tx),
    )
    .await;
    result
}

/// Probe every employee that has its own app (boot). Runs in the background.
pub fn spawn_boot_probes(ctx: Arc<ReplyContext>) {
    tokio::spawn(async move {
        let agents: Vec<String> = {
            let reg = ctx.registry.read().await;
            reg.list()
                .into_iter()
                .filter(|a| {
                    crate::agent_channels::store::googlechat_configured(
                        a.config
                            .channels
                            .as_ref()
                            .and_then(|c| c.googlechat.as_ref()),
                    )
                })
                .map(|a| a.config.agent.name.clone())
                .collect()
        };
        for agent in agents {
            match probe(&ctx, &agent).await {
                Ok(()) => info!(agent, "Google Chat (own app) credentials verified"),
                Err(e) => warn!(agent, "Google Chat (own app) credentials check failed: {e}"),
            }
        }
    });
}
