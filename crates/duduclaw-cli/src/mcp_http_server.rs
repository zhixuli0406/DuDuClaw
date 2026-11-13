// mcp_http_server.rs — Axum-based HTTP/SSE MCP server (W20-P1 Phase 2B/C)
//
// Routes:
//   POST /mcp/v1/call               — single JSON-RPC 2.0 tool call (Bearer auth only)
//   GET  /mcp/v1/stream             — SSE long-lived event stream (Bearer or ?api_key=)
//   POST /mcp/v1/stream/call        — inject tool call into a named SSE stream
//   GET  /healthz                   — health check (no auth)
//
// W22-P0 ADR-002: All responses carry x-duduclaw-version + x-duduclaw-capabilities headers
// via the inject_capability_headers + negotiate_capabilities middleware stack.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tracing::info;

use crate::mcp_auth::Principal;
use crate::mcp_capability::{inject_capability_headers, negotiate_capabilities};
use crate::mcp_dispatch::McpDispatcher;
use crate::mcp_http_errors::into_axum_response;
use crate::mcp_namespace::{CallerIdentity, NamespaceContext, client_is_agent, resolve_for_caller};
use crate::mcp_rate_limit::OpType;
use crate::mcp_sse_store::SseEventStore;

// ── Server config ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpServerConfig {
    pub bind: SocketAddr,
    pub public_origin: Option<String>,
    pub trusted_proxies: Vec<IpAddr>,
    pub allow_insecure_lan: bool,
    pub home_dir: std::path::PathBuf,
    pub enable_sse: bool,
    /// Request timeout for tool calls (default 30s).
    pub call_timeout: Duration,
}

impl HttpServerConfig {
    pub fn new(bind: SocketAddr, home_dir: std::path::PathBuf) -> Self {
        Self {
            bind,
            public_origin: None,
            trusted_proxies: Vec::new(),
            allow_insecure_lan: false,
            home_dir,
            enable_sse: true,
            call_timeout: Duration::from_secs(30),
        }
    }

    /// Pin discovery and audiences to operator configuration, never request headers.
    pub fn external_origin(&self) -> Result<String, String> {
        let origin = match &self.public_origin {
            Some(raw) => {
                let url = url::Url::parse(raw).map_err(|_| "Invalid MCP public origin")?;
                if url.scheme() != "https" || url.host_str().is_none()
                    || !url.username().is_empty() || url.password().is_some()
                    || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
                    return Err("MCP public origin must be an HTTPS origin without credentials, path, query or fragment".into());
                }
                url.origin().ascii_serialization()
            }
            None => format!("http://{}", self.bind),
        };
        if !self.bind.ip().is_loopback() && !self.allow_insecure_lan
            && (self.public_origin.is_none() || self.trusted_proxies.is_empty()) {
            return Err("Non-loopback MCP HTTP requires --public-origin https://HOST and --trusted-proxy IP; insecure LAN requires explicit --allow-insecure-lan".into());
        }
        Ok(origin)
    }
}

// ── Shared Axum state ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpState {
    pub external_origin: String,
    pub resource: String,
    pub dispatcher: Arc<McpDispatcher>,
    pub sse_store: Arc<SseEventStore>,
    pub call_timeout: Duration,
    pub home_dir: std::path::PathBuf,
    /// v1.82: the bounded `mcp_auth_failed` audit of this server
    /// (`crate::mcp_auth_audit`).
    pub(crate) auth_audit: Arc<crate::mcp_auth_audit::AuthFailureAudit>,
}

// Bounded process-local tables. Anonymous, registration and authenticated
// identities use separate storage so anonymous churn cannot fill principal slots.
#[derive(Default)]
pub(crate) struct Admission {
    buckets: std::collections::HashMap<String, (std::time::Instant, u32)>,
}

impl Admission {
    pub(crate) fn allow(&mut self, key: String, limit: u32) -> bool {
        let now = std::time::Instant::now();
        self.buckets.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        if !self.buckets.contains_key(&key) && self.buckets.len() >= 4096 { return false; }
        let (_, count) = self.buckets.entry(key).or_insert((now, 0));
        if *count >= limit { return false; }
        *count += 1;
        true
    }
}

fn admission() -> &'static std::sync::Mutex<Admission> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<Admission>> = std::sync::OnceLock::new();
    TABLE.get_or_init(Default::default)
}

pub(crate) fn oauth_client_admitted(home: &std::path::Path, client: &str, source: &str) -> bool {
    let key = format!("oauth:{}:{}:{source}", home.display(), duduclaw_core::mcp_oauth_store::digest(client));
    admission().lock().map(|mut t| {
        t.allow(format!("oauth-ip:{}:{source}", home.display()), 120) && t.allow(key, 12)
    }).unwrap_or(false)
}

fn busy_response(status: StatusCode) -> Response {
    let mut response = (status, Json(serde_json::json!({"error":"temporarily_unavailable"}))).into_response();
    response.headers_mut().insert("Retry-After", "60".parse().unwrap());
    response.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
    response
}

fn http_rate_response() -> Response {
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(crate::mcp_dispatch::jsonrpc_error(
        &Value::Null, -32029, "HTTP rate limit exceeded, retry after 60 seconds",
    ))).into_response();
    response.headers_mut().insert("Retry-After", "60".parse().unwrap());
    response.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
    response
}

#[derive(Clone)]
struct AdmissionState {
    anonymous: Arc<std::sync::Mutex<Admission>>,
    principals: Arc<std::sync::Mutex<Admission>>,
    registration: Arc<std::sync::Mutex<Admission>>,
    hints: Arc<std::sync::Mutex<CredentialHints>>,
    trusted_proxies: Vec<IpAddr>,
    http: HttpState,
}

/// Walk XFF from the immediate peer back to the first untrusted hop. A proxy
/// must append the actual connecting address, or replace XFF, at its boundary.
fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpAddr]) -> String {
    let Some(peer) = peer else { return "unknown-peer".into(); };
    if !trusted.contains(&peer) { return peer.to_string(); }
    let mut chain = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        let Ok(value) = value.to_str() else { return peer.to_string(); };
        for part in value.split(',') {
            let Ok(ip) = part.trim().parse::<IpAddr>() else { return peer.to_string(); };
            chain.push(ip);
            if chain.len() > 32 { return peer.to_string(); }
        }
    }
    chain.into_iter().rev().find(|ip| !trusted.contains(ip)).unwrap_or(peer).to_string()
}

#[derive(Clone)]
pub(crate) struct SourceIp(pub String);

#[derive(Clone, Default)]
pub(crate) struct RegistrationBudget(Arc<std::sync::Mutex<Admission>>);

impl RegistrationBudget {
    pub(crate) fn admit_creation(&self) -> bool {
        self.0.lock().map(|mut t| t.allow("global".into(), 10)).unwrap_or(false)
    }
}

#[derive(Clone)]
struct ProxyBoundary {
    required: bool,
    trusted: Vec<IpAddr>,
}

// Outermost layer: no headers, body, quota, credential or store processing
// until the actual TCP peer passes. Missing ConnectInfo fails closed.
async fn require_proxy_peer(
    State(boundary): State<ProxyBoundary>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    if boundary.required && !request.extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .is_some_and(|peer| boundary.trusted.contains(&peer.0.ip())) {
        return (StatusCode::FORBIDDEN, "Trusted proxy connection required").into_response();
    }
    next.run(request).await
}

// This cache ONLY selects a quota bucket. It never authenticates a request.
// Every protected handler still performs live authentication after admission.
// One router reloads at most once per second, with at most one reload in flight:
// random bearer strings cannot trigger per-request store/config deserialization.
#[derive(Default)]
struct CredentialHints {
    refreshed: Option<std::time::Instant>,
    loading: bool,
    principals: std::collections::HashMap<String, String>,
    #[cfg(test)]
    reloads: usize,
}

impl CredentialHints {
    fn start_reload(&mut self, now: std::time::Instant) -> bool {
        if self.loading || self.refreshed.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(1)) {
            return false;
        }
        self.loading = true;
        #[cfg(test)]
        { self.reloads += 1; }
        true
    }
}

// Deliberately only a bounded lookup candidate, not the key format validator.
fn static_hint_candidate(raw: &str) -> bool {
    raw.len() <= 64 && ["ddc_dev_", "ddc_prod_", "ddc_staging_"].iter().any(|prefix| raw.starts_with(prefix))
}

fn load_credential_hints(home: &std::path::Path, resource: &str) -> std::collections::HashMap<String, String> {
    use duduclaw_core::mcp_oauth_store::{digest, load_store, scope_subset, MAX_TOKEN_ROWS};
    use std::io::Read;
    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    const MAX_STATIC_HINTS: usize = 4096;
    let mut hints = std::collections::HashMap::new();
    let now = chrono::Utc::now();
    if let Ok(store) = load_store(home) {
        for (hash, token) in store.tokens.iter().take(MAX_TOKEN_ROWS) {
            if let Some(grant) = store.grants.get(&token.family_id)
                && !token.used && token.expires_unix > now.timestamp()
                && token.resource == resource && grant.resource == resource && !resource.is_empty()
                && token.client_id == grant.client_id && scope_subset(&token.scope, &grant.scope) {
                hints.insert(hash.clone(), format!("oauth_{}", token.client_id));
            }
        }
    }
    // Oversized/invalid configuration gets no hint; its keys can still pass the
    // anonymous gate and reach the normal live authenticator.
    if let Ok(file) = std::fs::File::open(home.join("config.toml")) {
        let mut bytes = Vec::new();
        if file.take(MAX_CONFIG_BYTES as u64 + 1).read_to_end(&mut bytes).is_ok()
            && bytes.len() <= MAX_CONFIG_BYTES
            && let Ok(raw) = std::str::from_utf8(&bytes)
            && let Ok(config) = toml::from_str::<toml::Value>(raw)
            && let Some(keys) = config.get("mcp_keys").and_then(|v| v.as_table()) {
            for (key, entry) in keys.iter().take(MAX_STATIC_HINTS) {
                if !static_hint_candidate(key) { continue; }
                let Some(client) = entry.get("client_id").and_then(|v| v.as_str()) else { continue; };
                let Some(created) = entry.get("created_at").and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { continue; };
                // RM-134: a config entry the authority store would not accept
                // (unknown once the store exists, not the recorded internal
                // key) earns no admission bucket of its own.
                use duduclaw_core::mcp_key_store::Authority;
                let accepted = match duduclaw_core::mcp_key_store::resolve(home, key) {
                    Ok(Authority::Legacy(_)) => true,
                    Ok(Authority::Internal(_)) => client == duduclaw_core::mcp_key_store::RESERVED_INTERNAL_CLIENT_ID,
                    _ => false,
                };
                if accepted && duduclaw_core::mcp_key_store::legacy_age_accepts(created.with_timezone(&chrono::Utc), now) {
                    hints.insert(digest(key), client.to_string());
                }
            }
        }
    }
    // RM-134: dashboard keys live in the hashed store; its digest is the same
    // `digest(raw)` this map is keyed by. Inserted last so a store row wins
    // over a not-yet-removed config copy. An unreadable store gives no hints
    // (the live authenticator then denies those keys anyway).
    if let Ok(keys) = duduclaw_core::mcp_key_store::usable_keys(home, now) {
        for key in keys.into_iter().take(MAX_STATIC_HINTS) {
            hints.insert(key.key_hash, key.client_id);
        }
    }
    hints
}

async fn admission_principal(headers: &HeaderMap, state: &AdmissionState) -> Result<Option<String>, Response> {
    let Some(raw) = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|raw| crate::mcp_oauth_server::is_oauth_access_token(raw) || static_hint_candidate(raw))
        else { return Ok(None); };
    let hash = duduclaw_core::mcp_oauth_store::digest(raw);
    let reload = state.hints.lock().map(|mut hints| hints.start_reload(std::time::Instant::now()))
        .unwrap_or(false);
    if reload {
        let cache = state.hints.clone();
        let home = state.http.home_dir.clone();
        let resource = state.http.resource.clone();
        // Await the first caller's reload so a cold cache does not require the
        // anonymous quota. Concurrent callers may use the preceding snapshot.
        tokio::task::spawn_blocking(move || {
            let principals = load_credential_hints(&home, &resource);
            if let Ok(mut hints) = cache.lock() {
                hints.principals = principals;
                hints.refreshed = Some(std::time::Instant::now());
                hints.loading = false;
            }
        }).await.map_err(|_| busy_response(StatusCode::SERVICE_UNAVAILABLE))?;
    }
    Ok(state.hints.lock().ok().and_then(|hints| hints.principals.get(&hash).cloned()))
}

async fn pre_auth_admission(
    State(state): State<AdmissionState>,
    mut request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    // v1.82: an authentication refusal below this layer is counted in the
    // bounded `mcp_auth_failed` audit against the TCP peer as the socket saw
    // it (never X-Forwarded-For) and a fixed route label. Requests refused
    // here (proxy / rate gates) never reached authentication.
    let audit_context = crate::mcp_auth_audit::RequestContext {
        audit: state.http.auth_audit.clone(),
        source: crate::mcp_auth_audit::Source::from_peer(
            request.extensions().get::<axum::extract::ConnectInfo<SocketAddr>>().map(|p| p.0.ip()),
        ),
        route: crate::mcp_auth_audit::route_class(&path),
    };
    if path != "/healthz" {
        let peer = request.extensions().get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|p| p.0.ip());
        let peer = client_ip(peer, request.headers(), &state.trusted_proxies);
        // Registration always consumes its dedicated budget, even with a bearer.
        if path == "/oauth/register" && !state.registration.lock().map(|mut t| t.allow(format!("ip:{peer}"), 3)).unwrap_or(false) {
            return busy_response(StatusCode::TOO_MANY_REQUESTS);
        }
        let principal = if request.headers().contains_key("authorization") {
            match admission_principal(request.headers(), &state).await {
                Ok(principal) => principal,
                Err(response) => return response,
            }
        } else { None };
        let allowed = if let Some(principal) = principal {
            state.principals.lock().map(|mut t| t.allow(principal, 60)).unwrap_or(false)
        } else {
            state.anonymous.lock().map(|mut t| t.allow(peer.clone(), 120)).unwrap_or(false)
        };
        if !allowed {
            return if path.starts_with("/oauth/") {
                busy_response(StatusCode::TOO_MANY_REQUESTS)
            } else { http_rate_response() };
        }
        request.extensions_mut().insert(SourceIp(peer));
        request.extensions_mut().insert(RegistrationBudget(state.registration.clone()));
    }
    crate::mcp_auth_audit::observe_request(audit_context, next.run(request)).await
}

// Keep the permit inside the blocking job, including when its HTTP future is
// cancelled. An attacker cannot accumulate unbounded queued flock waiters.
pub(crate) async fn blocking_io<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, Response> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let permit = SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone().try_acquire_owned().map_err(|_| busy_response(StatusCode::SERVICE_UNAVAILABLE))?;
    tokio::task::spawn_blocking(move || { let _permit = permit; work() }).await
        .map_err(|_| busy_response(StatusCode::SERVICE_UNAVAILABLE))
}

pub(crate) async fn blocking_http(work: impl FnOnce() -> Response + Send + 'static) -> Response {
    blocking_io(work).await.unwrap_or_else(|r| r)
}

pub(crate) async fn authenticate_http_call(
    headers: &HeaderMap, state: &HttpState,
) -> Result<(crate::mcp_auth::AuthenticatedCall, NamespaceContext), Response> {
    let headers = headers.clone();
    let home = state.home_dir.clone();
    let resource = state.resource.clone();
    let limiter = state.dispatcher.rate_limiter.clone();
    blocking_io(move || {
        let result = authenticate_bearer_call(&headers, &home, &resource)?;
        if limiter.check(&result.0.principal().client_id, OpType::HttpRequest).is_err() {
            return Err(http_rate_response());
        }
        if let Some(raw) = headers.get("authorization").and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            && crate::mcp_oauth_server::is_oauth_access_token(raw) {
            crate::mcp_oauth_server::note_oauth_use(raw, &home);
        }
        Ok(result)
    }).await?
}

async fn authenticate_http_stream(
    headers: &HeaderMap, query: Option<&str>, state: &HttpState,
) -> Result<(Principal, NamespaceContext), Response> {
    if headers.contains_key("authorization") {
        return authenticate_http_call(headers, state).await.map(|(a, n)| (a.principal().clone(), n));
    }
    let headers = headers.clone();
    let query = query.map(str::to_owned);
    let home = state.home_dir.clone();
    let resource = state.resource.clone();
    let limiter = state.dispatcher.rate_limiter.clone();
    blocking_io(move || {
        let result = authenticate_bearer_or_query(&headers, query.as_deref(), &home, &resource)?;
        if limiter.check(&result.0.client_id, OpType::HttpRequest).is_err() {
            return Err(http_rate_response());
        }
        Ok(result)
    }).await?
}

// ── Auth helper ───────────────────────────────────────────────────────────────

/// Generic 401 for an RM-064 status credential presented to the MCP server.
fn status_credential_refused() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(crate::mcp_dispatch::jsonrpc_error(
            &Value::Null,
            -32003,
            "Authentication failed: this credential cannot be used here",
        )),
    )
        .into_response()
}

/// Extract and validate a Bearer API key from headers.
/// Returns (Principal, NamespaceContext) or an error Response.
pub(crate) fn authenticate_bearer(
    headers: &HeaderMap,
    home_dir: &std::path::Path,
    resource: &str,
) -> Result<(Principal, NamespaceContext), Response> {
    authenticate_bearer_call(headers, home_dir, resource)
        .map(|(call, namespace)| (call.principal().clone(), namespace))
}

pub(crate) fn authenticate_bearer_call(
    headers: &HeaderMap,
    home_dir: &std::path::Path,
    resource: &str,
) -> Result<(crate::mcp_auth::AuthenticatedCall, NamespaceContext), Response> {
    // v1.82: every 401 below carries an internal refusal marker
    // (`mcp_auth_audit::tag*`, a response extension — status, headers and
    // body are unchanged) that the admission middleware turns into the
    // bounded `mcp_auth_failed` audit. Nothing from the header is kept.
    use crate::mcp_auth_audit::{FailureReason, tag, tag_refusal};
    let auth_header = headers
        .get("Authorization")
        .ok_or_else(|| {
            tag(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(crate::mcp_dispatch::jsonrpc_error(
                        &Value::Null,
                        -32003,
                        "Missing Authorization header. Use: Authorization: Bearer <key>",
                    )),
                )
                    .into_response(),
                FailureReason::MissingHeader,
                None,
            )
        })?
        .to_str()
        .map_err(|_| {
            tag(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(crate::mcp_dispatch::jsonrpc_error(
                        &Value::Null,
                        -32003,
                        "Invalid Authorization header encoding",
                    )),
                )
                    .into_response(),
                FailureReason::BadScheme,
                None,
            )
        })?;

    let raw_key = auth_header
        .strip_prefix("Bearer ")
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            // `Bearer ` with nothing after it is an empty key; anything
            // else is not the Bearer scheme.
            let reason = if auth_header.starts_with("Bearer ") {
                FailureReason::MalformedKey
            } else {
                FailureReason::BadScheme
            };
            tag(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(crate::mcp_dispatch::jsonrpc_error(
                        &Value::Null,
                        -32003,
                        "Authorization header must use: Bearer <key>",
                    )),
                )
                    .into_response(),
                reason,
                None,
            )
        })?;

    if crate::mcp_refresh::is_refresh_token_format(raw_key) {
        return Err(tag(
            (StatusCode::UNAUTHORIZED, Json(crate::mcp_dispatch::jsonrpc_error(
                &Value::Null, -32003, crate::mcp_refresh::STDIO_ONLY_NOTICE,
            ))).into_response(),
            FailureReason::WrongCredentialType,
            None,
        ));
    }
    // RM-064: a `ddc_status_` credential belongs to the gateway's status
    // endpoint only. Refuse the type outright — it must never reach the
    // `[mcp_keys]` / OAuth lookups or any scope table.
    if duduclaw_auth::status_tokens::is_status_token(raw_key) {
        return Err(tag(status_credential_refused(), FailureReason::WrongCredentialType, None));
    }

    // WP3.1-T2: OAuth-issued access tokens share the Bearer surface but live
    // in their own store (hashed at rest); everything else stays the static
    // mcp_keys path. Both produce a Principal that flows through the SAME
    // scope/namespace enforcement below.
    let auth_result = if crate::mcp_oauth_server::is_oauth_access_token(raw_key) {
        crate::mcp_oauth_server::validate_oauth_token(raw_key, home_dir, resource)
            .map(crate::mcp_auth::AuthenticatedCall::without_reference)
            .map_err(crate::mcp_auth::Refusal::from)
    } else {
        crate::mcp_auth::authenticate_with_key_call_refusal(raw_key, home_dir)
    };
    let authenticated = auth_result.map_err(|refusal| {
        tag_refusal(
            (
                StatusCode::UNAUTHORIZED,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32003,
                    &format!("Authentication failed: {}", refusal.error),
                )),
            )
                .into_response(),
            &refusal,
        )
    })?;

    let principal = authenticated.principal();
    let ns_ctx = resolve_for_caller(
        principal,
        // HTTP callers are remote: the internal key never inherits this
        // process's employee identity here (it stays in the shared pool); a
        // per-agent key still maps to its employee's own namespace.
        CallerIdentity {
            verified_agent: None,
            client_is_agent: client_is_agent(home_dir, &principal.client_id),
        },
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::mcp_dispatch::jsonrpc_error(
                &Value::Null,
                -32603,
                &format!("Namespace resolution failed: {e}"),
            )),
        )
            .into_response()
    })?;

    Ok((authenticated, ns_ctx))
}

/// Extract and validate a Bearer key OR `?api_key=` query param.
fn authenticate_bearer_or_query(
    headers: &HeaderMap,
    query_api_key: Option<&str>,
    home_dir: &std::path::Path,
    resource: &str,
) -> Result<(Principal, NamespaceContext), Response> {
    // Prefer Authorization header; fall back to query param (SSE only)
    if headers.contains_key("Authorization") {
        return authenticate_bearer(headers, home_dir, resource);
    }

    // v1.82: refusals carry the `mcp_auth_audit` marker, as in
    // `authenticate_bearer_call`.
    use crate::mcp_auth_audit::{FailureReason, tag, tag_refusal};
    if let Some(raw_key) = query_api_key.filter(|k| !k.is_empty()) {
        if crate::mcp_refresh::is_refresh_token_format(raw_key) {
            return Err(tag(
                (StatusCode::UNAUTHORIZED, Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null, -32003, crate::mcp_refresh::STDIO_ONLY_NOTICE,
                ))).into_response(),
                FailureReason::WrongCredentialType,
                None,
            ));
        }
        if duduclaw_auth::status_tokens::is_status_token(raw_key) {
            return Err(tag(status_credential_refused(), FailureReason::WrongCredentialType, None));
        }
        let principal = crate::mcp_auth::authenticate_with_key_refusal(raw_key, home_dir).map_err(|refusal| {
            tag_refusal(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(crate::mcp_dispatch::jsonrpc_error(
                        &Value::Null,
                        -32003,
                        &format!("Authentication failed: {}", refusal.error),
                    )),
                )
                    .into_response(),
                &refusal,
            )
        })?;
        let ns_ctx = resolve_for_caller(
            &principal,
            // HTTP callers are remote: the internal key never inherits this
            // process's employee identity here (it stays in the shared pool); a
            // per-agent key still maps to its employee's own namespace.
            CallerIdentity {
                verified_agent: None,
                client_is_agent: client_is_agent(home_dir, &principal.client_id),
            },
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32603,
                    &format!("Namespace resolution failed: {e}"),
                )),
            )
                .into_response()
        })?;
        return Ok((principal, ns_ctx));
    }

    Err(tag(
        (
            StatusCode::UNAUTHORIZED,
            Json(crate::mcp_dispatch::jsonrpc_error(
                &Value::Null,
                -32003,
                "Missing API key. Provide Authorization: Bearer <key> or ?api_key=<key>",
            )),
        )
            .into_response(),
        FailureReason::MissingHeader,
        None,
    ))
}

// ── Router builder (extracted for testability) ────────────────────────────────

/// The dispatcher every HTTP route uses. v1.82 (RM-134): every caller here is
/// a remote bearer, so every non-external key other than the verified
/// gateway-internal one is refused (and not shown) the tools that act for
/// this server's fixed `default_agent`, acts as its own bound employee for
/// the rest, and is refused everything without a valid binding
/// (`McpDispatcher::for_remote_bearer`, `mcp::remote_access`).
fn http_dispatcher(dispatcher: McpDispatcher) -> Arc<McpDispatcher> {
    Arc::new(dispatcher.for_remote_bearer())
}

/// The MCP HTTP application: every route with every layer applied.
///
/// v1.82 (L1): deliberately **not** an `axum::Router`. `Router::layer` wraps
/// only the routes that exist when it is called, so a `Router` handed out
/// here would let a caller `.route(…)` past the proxy boundary, the
/// admission gate and the authentication-failure audit (which counts and
/// strips the internal refusal marker). This type has no way to add routes
/// or layers: it can only be served ([`Self::into_make_service_with_connect_info`])
/// or called as a `tower::Service`. Inside this module the only way to make
/// one is `finish_app`, which applies the layers after the last route.
///
/// ```compile_fail
/// fn add_route(app: duduclaw_cli::mcp_http_server::McpHttpApp) {
///     let _ = app.route("/extra", axum::routing::get(|| async { "unaudited" }));
/// }
/// ```
#[derive(Clone)]
pub struct McpHttpApp(Router);

impl McpHttpApp {
    /// For `axum::serve` without connection info (the peer is then
    /// unknown to the layers; loopback binds only).
    pub fn into_make_service(self) -> axum::routing::IntoMakeService<Router> {
        self.0.into_make_service()
    }

    /// For `axum::serve`, with the socket peer available to the layers.
    pub fn into_make_service_with_connect_info<C>(
        self,
    ) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, C> {
        self.0.into_make_service_with_connect_info::<C>()
    }
}

impl<B> tower::Service<axum::http::Request<B>> for McpHttpApp
where
    Router: tower::Service<axum::http::Request<B>>,
{
    type Response = <Router as tower::Service<axum::http::Request<B>>>::Response;
    type Error = <Router as tower::Service<axum::http::Request<B>>>::Error;
    type Future = <Router as tower::Service<axum::http::Request<B>>>::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::<axum::http::Request<B>>::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, request: axum::http::Request<B>) -> Self::Future {
        tower::Service::call(&mut self.0, request)
    }
}

/// Build the MCP HTTP application with all routes attached to `state`.
/// Extracted from `run` so integration tests can call routes without binding
/// a port. Returns the fully layered [`McpHttpApp`] (no routes can be added).
pub fn build_router(cfg: &HttpServerConfig, dispatcher: McpDispatcher) -> McpHttpApp {
    let origin = cfg.external_origin().expect("invalid MCP HTTP configuration");
    let state = HttpState {
        resource: format!("{origin}/mcp"),
        external_origin: origin,
        dispatcher: http_dispatcher(dispatcher),
        sse_store: Arc::new(SseEventStore::new()),
        call_timeout: cfg.call_timeout,
        home_dir: cfg.home_dir.clone(),
        auth_audit: crate::mcp_auth_audit::AuthFailureAudit::new(cfg.home_dir.clone()),
    };
    build_app_with_state(cfg, state)
}

/// Build the application from a pre-constructed `HttpState` (so the SSE
/// store and the audit can be shared with the background tasks in `run`).
fn build_app_with_state(cfg: &HttpServerConfig, state: HttpState) -> McpHttpApp {
    finish_app(cfg, state, mcp_routes(cfg))
}

/// Apply every layer to the complete route set — the one place an
/// [`McpHttpApp`] is made, so no route can be added after the layers.
fn finish_app(cfg: &HttpServerConfig, state: HttpState, routes: Router<HttpState>) -> McpHttpApp {
    let admission_state = AdmissionState {
        anonymous: Arc::new(std::sync::Mutex::new(Admission::default())),
        principals: Arc::new(std::sync::Mutex::new(Admission::default())),
        registration: Arc::new(std::sync::Mutex::new(Admission::default())),
        hints: Arc::new(std::sync::Mutex::new(CredentialHints::default())),
        trusted_proxies: cfg.trusted_proxies.clone(),
        http: state.clone(),
    };
    // W22-P0 ADR-002: capability headers cover requests admitted by the proxy boundary.
    // Layer order (axum: last .layer() = outermost = runs first on request, last on response):
    //   outermost: require_proxy_peer — refuses untrusted TCP peers before processing
    //   next: inject_capability_headers — adds standard headers (incl. 422)
    //   inner: negotiate_capabilities   — validates client's x-duduclaw-capabilities header
    //   innermost: pre_auth_admission   — rate gates + v1.82 auth-failure audit
    McpHttpApp(
        routes
            .with_state(state)
            .layer(middleware::from_fn_with_state(admission_state, pre_auth_admission))
            .layer(middleware::from_fn(negotiate_capabilities)) // inner
            .layer(middleware::from_fn(inject_capability_headers))
            .layer(middleware::from_fn_with_state(ProxyBoundary {
                required: !cfg.bind.ip().is_loopback() && !cfg.allow_insecure_lan,
                trusted: cfg.trusted_proxies.clone(),
            }, require_proxy_peer)), // outermost, including health checks and fallback routes
    )
}

/// Every MCP HTTP route, without state or layers.
fn mcp_routes(cfg: &HttpServerConfig) -> Router<HttpState> {
    let mut router = Router::new()
        .route("/healthz", get(healthz_handler))
        // WP3.1-T1: standard MCP Streamable HTTP endpoint (spec transport for
        // claude.ai custom connectors / MCP Inspector / any remote MCP client).
        .route(
            "/mcp",
            post(crate::mcp_streamable::mcp_post_handler)
                .get(crate::mcp_streamable::mcp_get_handler)
                .delete(crate::mcp_streamable::mcp_delete_handler),
        )
        // WP3.1-T2: OAuth 2.1 surface for remote MCP clients (claude.ai
        // custom connectors and friends). Metadata is unauthenticated by
        // design (RFC 9728 / RFC 8414 discovery documents).
        .route(
            "/.well-known/oauth-protected-resource",
            get(crate::mcp_oauth_server::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(crate::mcp_oauth_server::authorization_server_metadata),
        )
        .route(
            "/oauth/register",
            post(crate::mcp_oauth_server::register_handler),
        )
        .route(
            "/oauth/authorize",
            get(crate::mcp_oauth_server::authorize_handler),
        )
        .route(
            "/oauth/decision",
            post(crate::mcp_oauth_server::decision_handler),
        )
        .route("/oauth/token", post(crate::mcp_oauth_server::token_handler))
        .route("/oauth/revoke", post(crate::mcp_oauth_server::revoke_handler))
        .route("/mcp/v1/call", post(call_handler))
        // WP2.12: wearable-transcript ingest (vendor-webhook-friendly shape).
        .route("/ingest/transcript", post(ingest_transcript_handler));

    if cfg.enable_sse {
        router = router
            .route("/mcp/v1/stream", get(stream_handler))
            .route("/mcp/v1/stream/call", post(stream_call_handler));
    }
    router
}

// ── Server entry point ────────────────────────────────────────────────────────

/// Keep the license refresh/feed loops scoped to the standalone HTTP server,
/// including bind errors or cancellation of its `run` future.
struct LicenseTasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for LicenseTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

pub async fn run(mut cfg: HttpServerConfig, dispatcher: McpDispatcher) -> Result<(), String> {
    cfg.external_origin()?;
    if cfg.allow_insecure_lan {
        tracing::warn!("INSECURE MCP LAN OPT-IN: HTTP carries operator keys and OAuth tokens in plaintext; use only on an isolated trusted network");
        eprintln!("WARNING: --allow-insecure-lan permits plaintext MCP/OAuth credentials on the network.");
    }
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await.map_err(|e| format!("Failed to bind {}: {e}", cfg.bind))?;
    cfg.bind = listener.local_addr().map_err(|e| format!("Failed to read MCP listener address: {e}"))?;
    let origin = cfg.external_origin()?;
    // OAuth pages also run without start_gateway. Use the same verified local
    // license and cached distributor document; network refresh runs separately
    // and never delays serving the HTTP routes or changes their authentication.
    let license_runtime = duduclaw_gateway::license_runtime::LicenseRuntime::bootstrap(
        cfg.home_dir.clone(),
        duduclaw_gateway::license_runtime::production_registry(),
    )
    .await;
    duduclaw_gateway::license_runtime::set_global(license_runtime.clone());
    let _license_tasks = LicenseTasks(license_runtime.spawn_background_tasks());

    let state = HttpState {
        resource: format!("{origin}/mcp"),
        external_origin: origin,
        dispatcher: http_dispatcher(dispatcher),
        sse_store: Arc::new(SseEventStore::new()),
        call_timeout: cfg.call_timeout,
        home_dir: cfg.home_dir.clone(),
        auth_audit: crate::mcp_auth_audit::AuthFailureAudit::new(cfg.home_dir.clone()),
    };

    // D4: periodically evict idle SSE connections (no client ever called this before).
    let sse_store = state.sse_store.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            sse_store.evict_idle();
        }
    });

    // v1.82: write the ended minute's detail summaries and the ended hour's
    // chain summaries without waiting for the next failure (the writes
    // themselves run off this task).
    let auth_audit = state.auth_audit.clone();
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(Duration::from_secs(crate::mcp_auth_audit::FLUSH_TICK_SECS));
        loop {
            ticker.tick().await;
            auth_audit.flush();
        }
    });

    let app = build_app_with_state(&cfg, state);

    info!(bind = %cfg.bind, "MCP HTTP server listening");

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| format!("HTTP server error: {e}"))
}

// ── GET /healthz ──────────────────────────────────────────────────────────────

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

// ── POST /mcp/v1/call ─────────────────────────────────────────────────────────

/// Single JSON-RPC 2.0 tool call.
/// Authentication: `Authorization: Bearer <key>` (query params NOT accepted).
async fn call_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Authenticate (header only — no query param to prevent access log leakage)
    let (authenticated, ns_ctx) = match authenticate_http_call(&headers, &state).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let principal = authenticated.principal().clone();
    crate::mcp_auth::with_authenticated_call(authenticated, async {
        let id = body.get("id").cloned().unwrap_or(Value::Null);

        // Validate JSON-RPC
        if body.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
            return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
                &id,
                -32600,
                "jsonrpc field must be '2.0'",
            ));
        }
        let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method != "tools/call" {
            return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
                &id,
                -32601,
                &format!("Method not found: '{method}'. Use 'tools/call'"),
            ));
        }

        let params = body.get("params").cloned().unwrap_or(Value::Null);

        // Dispatch with timeout
        let jsonrpc = match tokio::time::timeout(
            duduclaw_core::tool_catalog::builtin_mcp_call_timeout(
                params.get("name").and_then(Value::as_str).unwrap_or(""), state.call_timeout,
            ),
            state
                .dispatcher
                .dispatch_tool_call(&principal, &ns_ctx, &params, &id),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => crate::mcp_dispatch::jsonrpc_error(
                &id,
                -32603,
                "Request timed out",
            ),
        };

        into_axum_response(jsonrpc)
    })
    .await
}

// ── POST /ingest/transcript ───────────────────────────────────────────────────

/// WP2.12 (ecosystem): wearable-transcript ingest — a thin adapter so vendor
/// webhooks (Omi / Plaud / anything that POSTs JSON with a transcript) can
/// land text in agent memory WITHOUT speaking JSON-RPC. Same Bearer auth as
/// `/mcp/v1/call`, same rate gate, and the write goes through the SAME
/// dispatcher as a `memory_store` tool call — scope checks, the external
/// whitelist, and write-time origin binding all apply. No second write path.
async fn ingest_transcript_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let (authenticated, ns_ctx) = match authenticate_http_call(&headers, &state).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let principal = authenticated.principal().clone();
    crate::mcp_auth::with_authenticated_call(authenticated, async {


        let Some(text) = extract_transcript_text(&payload) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "stored": false,
                    "error": "no transcript text found (accepted fields: text / transcript / summary / segments[].text / transcript_segments[].text)",
                })),
            )
                .into_response();
        };
        let source = payload
            .get("source")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("app").and_then(|v| v.as_str()))
            .unwrap_or("wearable");
        // CJK-safe cap — a runaway payload becomes a truncated memory, not a DoS.
        let text = duduclaw_core::truncate_bytes(&text, 16 * 1024);
        let params = serde_json::json!({
            "name": "memory_store",
            "arguments": {
                "content": format!("[{source} 逐字稿] {text}"),
                "tags": format!("wearable,{source}"),
            },
        });
        let jsonrpc = match tokio::time::timeout(
            state.call_timeout,
            state
                .dispatcher
                .dispatch_tool_call(&principal, &ns_ctx, &params, &Value::Null),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                return (
                    StatusCode::GATEWAY_TIMEOUT,
                    Json(serde_json::json!({ "stored": false, "error": "timed out" })),
                )
                    .into_response();
            }
        };
        let ok = jsonrpc.get("error").is_none();
        let status = if ok {
            StatusCode::OK
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        (
            status,
            Json(serde_json::json!({ "stored": ok, "detail": jsonrpc })),
        )
            .into_response()
    }).await
}

/// Lenient transcript extraction across wearable-vendor payload shapes.
/// Whole-field reads only — no substring guessing.
fn extract_transcript_text(v: &Value) -> Option<String> {
    for k in ["text", "transcript", "summary"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    for k in ["segments", "transcript_segments"] {
        if let Some(arr) = v.get(k).and_then(|x| x.as_array()) {
            let joined = arr
                .iter()
                .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.trim().is_empty() {
                return Some(joined.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod ingest_tests {
    use super::*;

    #[test]
    fn transcript_extraction_covers_vendor_shapes() {
        let flat = serde_json::json!({ "text": " 今天客戶說要改單 " });
        assert_eq!(
            extract_transcript_text(&flat).as_deref(),
            Some("今天客戶說要改單")
        );
        let omi =
            serde_json::json!({ "segments": [ {"text": "a"}, {"text": " b "}, {"speaker": "x"} ] });
        assert_eq!(extract_transcript_text(&omi).as_deref(), Some("a b"));
        let plaud = serde_json::json!({ "summary": "重點：週五交貨" });
        assert_eq!(
            extract_transcript_text(&plaud).as_deref(),
            Some("重點：週五交貨")
        );
        assert!(extract_transcript_text(&serde_json::json!({})).is_none());
        assert!(extract_transcript_text(&serde_json::json!({ "text": "  " })).is_none());
    }
}

// ── GET /mcp/v1/stream ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamQuery {
    conn_id: Option<String>,
    api_key: Option<String>,
}

/// RAII guard that removes the SSE connection from the store when the response
/// stream is dropped (i.e. the client disconnects), so the broadcast `tx` doesn't leak.
struct ConnGuard {
    store: Arc<SseEventStore>,
    conn_id: String,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.store.remove_connection(&self.conn_id);
    }
}

async fn stream_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> impl IntoResponse {
    // Auth: Bearer header OR ?api_key= (SSE clients may not support custom headers)
    let (principal, _ns_ctx) =
        match authenticate_http_stream(&headers, query.api_key.as_deref(), &state).await {
            Ok(p) => p,
            Err(r) => return r,
        };

    let conn_id = query
        .conn_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (tx, rx) = broadcast::channel::<String>(256);
    // HS7: record the owning principal so stream/call can verify ownership.
    state
        .sse_store
        .register_connection(&conn_id, tx, &principal.client_id);

    // D4: remove the connection when this stream is dropped (client disconnect).
    let guard = ConnGuard {
        store: state.sse_store.clone(),
        conn_id: conn_id.clone(),
    };

    let conn_id_clone = conn_id.clone();
    let client_id = principal.client_id.clone();

    // Convert broadcast receiver to a Stream of SSE Events. The `guard` is moved
    // into the closure so it lives as long as the stream and runs on drop.
    let bcast_stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |msg| {
        let _ = &guard; // keep guard alive for the stream's lifetime
        let conn_id = conn_id_clone.clone();
        match msg {
            Ok(data) => Some(Ok::<Event, std::convert::Infallible>(
                Event::default().id(conn_id).data(data),
            )),
            Err(_) => None, // lagged or closed
        }
    });

    // First event: "connected"
    let connected_data = serde_json::json!({
        "type": "connected",
        "conn_id": conn_id,
        "client_id": client_id,
    })
    .to_string();

    let initial = tokio_stream::once(Ok::<Event, std::convert::Infallible>(
        Event::default().event("connected").data(connected_data),
    ));

    let combined = initial.chain(bcast_stream);

    Sse::new(combined)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(30))
                .text("heartbeat"),
        )
        .into_response()
}

// ── POST /mcp/v1/stream/call ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamCallQuery {
    conn_id: String,
}

async fn stream_call_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<StreamCallQuery>,
    Json(body): Json<Value>,
) -> Response {
    let (authenticated, ns_ctx) = match authenticate_http_call(&headers, &state).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let principal = authenticated.principal().clone();
    crate::mcp_auth::with_authenticated_call(authenticated, async {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let params = body.get("params").cloned().unwrap_or(Value::Null);

        let conn_id = query.conn_id.clone();

        // HS7: only the principal that opened the SSE stream may push into it.
        // Fail-closed: a non-existent connection is not owned by anyone.
        if !state.sse_store.is_owner(&conn_id, &principal.client_id) {
            return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
                &id,
                -32003,
                "Forbidden: conn_id does not belong to the authenticated principal",
            ));
        }

        // Push progress event
        let progress = serde_json::json!({
            "status": "running",
            "tool": params.get("name").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "conn_id": conn_id,
        });
        state
            .sse_store
            .push_event(&conn_id, "tool_progress", &progress.to_string());

        // Dispatch tool call
        let jsonrpc = tokio::time::timeout(
            duduclaw_core::tool_catalog::builtin_mcp_call_timeout(
                params.get("name").and_then(Value::as_str).unwrap_or(""), state.call_timeout,
            ),
            state
                .dispatcher
                .dispatch_tool_call(&principal, &ns_ctx, &params, &id),
        )
        .await
        .unwrap_or_else(|_| {
            crate::mcp_dispatch::jsonrpc_error(
                &id,
                -32603,
                "Request timed out",
            )
        });

        // Push result to SSE stream
        state
            .sse_store
            .push_event(&conn_id, "tool_result", &jsonrpc.to_string());

        (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "accepted": true, "conn_id": conn_id })),
        )
            .into_response()
    })
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // provides Router::oneshot

    use super::*;
    use crate::mcp_dispatch::McpDispatcher;
    use crate::mcp_memory_quota::DailyQuota;
    use crate::mcp_rate_limit::RateLimiter;

    #[tokio::test]
    async fn ddc_refresh_is_stdio_only_on_http_and_sse() {
        let home = tempfile::tempdir().unwrap();
        let scopes = crate::mcp_auth::parse_scopes("memory:read").unwrap();
        let (raw, meta) = crate::mcp_refresh::issue_refresh_token(
            home.path(), "dev", "external-client", &scopes, true,
        ).unwrap();
        let principal = crate::mcp_refresh::authenticate_with_refresh_token(&raw, home.path()).unwrap();
        assert_eq!(principal.scopes, scopes);
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {raw}").parse().unwrap());
        let rejected = [
            authenticate_bearer(&headers, home.path(), "http://127.0.0.1:8765/mcp").unwrap_err(),
            authenticate_bearer_or_query(&HeaderMap::new(), Some(&raw), home.path(), "http://127.0.0.1:8765/mcp").unwrap_err(),
        ];
        for response in rejected {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
            let error: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(error["error"]["message"], crate::mcp_refresh::STDIO_ONLY_NOTICE);
        }
        crate::mcp_refresh::revoke_token(home.path(), &meta.jti).unwrap();
        assert!(crate::mcp_refresh::authenticate_with_refresh_token(&raw, home.path()).is_err());
    }

    /// RM-064: a real, live `ddc_status_` credential is refused by type on
    /// the bearer path, the SSE query path and the routed `/mcp` endpoint.
    #[tokio::test]
    async fn status_credentials_are_refused_on_every_mcp_surface() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user("alice@test.invalid", "Alice", "isolated-password", duduclaw_auth::UserRole::Admin)
            .unwrap();
        let raw = db.status_token_create(&user.id, "odoo").unwrap().secret;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {raw}").parse().unwrap());
        let resource = "http://127.0.0.1:8765/mcp";
        for response in [
            authenticate_bearer(&headers, home.path(), resource).unwrap_err(),
            authenticate_bearer_or_query(&HeaderMap::new(), Some(&raw), home.path(), resource).unwrap_err(),
        ] {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().to_path_buf());
        let router = build_router(&cfg, make_test_dispatcher());
        let request = Request::builder().method("POST").uri("/mcp")
            .header("authorization", format!("Bearer {raw}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)).unwrap();
        let status = router.oneshot(request).await.unwrap().status();
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::TOO_MANY_REQUESTS,
            "a status credential must never be served by /mcp: {status}"
        );
    }

    #[tokio::test]
    async fn oauth_revocation_endpoint_is_routed() {
        let home = tempfile::tempdir().unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().to_path_buf());
        let router = build_router(&cfg, make_test_dispatcher());
        let request = Request::builder().method("POST").uri("/oauth/revoke")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("client_id=public-client&token=unknown&token_type_hint=unknown")).unwrap();
        assert_eq!(router.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[test]
    fn http_admission_bounds_identities_and_oauth_client_refresh_rate() {
        let mut table = Admission::default();
        for _ in 0..12 { assert!(table.allow("refresh-client".into(), 12)); }
        assert!(!table.allow("refresh-client".into(), 12));
        assert!(table.allow("another-client".into(), 12));
        for n in table.buckets.len()..4096 { assert!(table.allow(n.to_string(), 120)); }
        assert!(!table.allow("overflow".into(), 120));
        assert_eq!(table.buckets.len(), 4096);
        for (start, _) in table.buckets.values_mut() { *start -= Duration::from_secs(61); }
        assert!(table.allow("overflow".into(), 120));
        assert_eq!(table.buckets.len(), 1);
        let home = tempfile::tempdir().unwrap();
        for _ in 0..12 { assert!(oauth_client_admitted(home.path(), "client", "192.0.2.1")); }
        assert!(!oauth_client_admitted(home.path(), "client", "192.0.2.1"));
        assert!(oauth_client_admitted(home.path(), "other", "192.0.2.1"));
    }

    #[tokio::test]
    async fn http_pre_auth_rate_gate_covers_all_routes_and_ignores_forwarded_ip() {
        let home = tempfile::tempdir().unwrap();
        // Corruption must never be read after the IP allowance is exhausted.
        std::fs::write(home.path().join("mcp_oauth_issued.json"), "broken").unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        for n in 0..120 {
            let request = Request::builder().uri("/.well-known/oauth-protected-resource")
                .header("x-forwarded-for", format!("192.0.2.{}", n + 1))
                .body(Body::empty()).unwrap();
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
        }
        for (method, uri) in [("POST", "/oauth/token"), ("POST", "/oauth/revoke"),
            ("POST", "/mcp"), ("POST", "/mcp/v1/call"), ("GET", "/mcp/v1/stream"),
            ("POST", "/mcp/v1/stream/call"), ("POST", "/ingest/transcript")] {
            let request = Request::builder().method(method).uri(uri)
                .header("x-forwarded-for", "198.51.100.1").body(Body::empty()).unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{uri}");
            assert_eq!(response.headers()["retry-after"], "60");
            if uri == "/mcp/v1/call" {
                let bytes = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
                let error: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(error["error"]["code"], -32029);
            }
        }
        assert_eq!(router.oneshot(healthz_request()).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn http_peer_allowances_are_independent_and_token_exchange_is_client_limited() {
        let home = tempfile::tempdir().unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        for n in 0..122 {
            let peer = if n == 121 { "192.0.2.2:1234" } else { "192.0.2.1:1234" };
            let mut request = Request::builder().uri("/.well-known/oauth-protected-resource")
                .body(Body::empty()).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let expected = if n == 120 { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK };
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), expected);
        }
        // The unique home isolates process-global exchange buckets from other tests.
        // Exhaust one (client, IP) pair, then check both dimensions independently.
        for n in 0..16 {
            let (client, peer, expected) = match n {
                12 | 15 => ("rate-client", "198.51.100.1:1234", StatusCode::TOO_MANY_REQUESTS),
                13 => ("rate-client", "198.51.100.2:1234", StatusCode::BAD_REQUEST),
                14 => ("other-rate-client", "198.51.100.1:1234", StatusCode::BAD_REQUEST),
                _ => ("rate-client", "198.51.100.1:1234", StatusCode::BAD_REQUEST),
            };
            let mut request = Request::builder().method("POST").uri("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("client_id={client}&grant_type=unsupported"))).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), expected,
                "token exchange attempt {n}: client={client}, peer={peer}");
        }
    }

    #[tokio::test]
    async fn http_principal_gate_covers_initialize_and_shared_client_keys() {
        let home = tempfile::tempdir().unwrap();
        let key_a = "ddc_dev_0123456789abcdef0123456789abcdef";
        let key_b = "ddc_dev_1123456789abcdef0123456789abcdef";
        let mut config = String::new();
        for key in [key_a, key_b] { config.push_str(&format!(
            "[mcp_keys.{key}]\nclient_id='shared-client'\nscopes=['memory:read']\nis_external=true\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )); }
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().into());
        let dispatcher = make_test_dispatcher();
        let limiter = dispatcher.rate_limiter.clone();
        let router = build_router(&cfg, dispatcher);
        for (key, expected) in [(key_a, StatusCode::OK), (key_b, StatusCode::TOO_MANY_REQUESTS)] {
            if key == key_b {
                for _ in 0..59 { limiter.check("shared-client", OpType::HttpRequest).unwrap(); }
            }
            let request = Request::builder().method("POST").uri("/mcp")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), expected);
        }
    }

    /// RM-134: the http-server authenticates dashboard keys from the hashed
    /// store, gives them their own quota bucket, and refuses an expired or a
    /// disabled key with the same external message.
    #[tokio::test]
    async fn http_bearer_reads_the_hashed_key_store() {
        use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, McpKeyStore, NewMcpKey};
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        let store = McpKeyStore::open(home.path()).unwrap();
        let issue = |name: &str| {
            let created = store.create(NewMcpKey {
                name: name.into(), client_id: format!("{name}-client"), binding: KeyBinding::Integration, env: "prod".into(),
                scopes: vec!["memory:read".into()], expiry: Expiry::Days(30), created_by: Some("admin".into()),
            }, chrono::Utc::now()).unwrap();
            (created.secret, created.key.id)
        };
        let (live, _) = issue("live");
        let (off, off_id) = issue("off");
        let (old, old_id) = issue("old");
        store.set_disabled(&off_id, true, "admin", chrono::Utc::now()).unwrap();
        drop(store);
        rusqlite::Connection::open(duduclaw_core::mcp_key_store::store_path(home.path())).unwrap()
            .execute("UPDATE mcp_api_keys SET expires_at = '2020-01-01T00:00:00Z' WHERE id = ?1", [&old_id]).unwrap();
        let hints = load_credential_hints(home.path(), "");
        assert_eq!(hints.get(&duduclaw_core::mcp_key_store::digest(&live)).map(String::as_str), Some("live-client"));
        assert!(!hints.contains_key(&duduclaw_core::mcp_key_store::digest(&off)));

        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        let send = |key: String| {
            let router = router.clone();
            async move {
                let request = Request::builder().method("POST").uri("/mcp")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
                let response = router.oneshot(request).await.unwrap();
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
                (status, String::from_utf8_lossy(&body).to_string())
            }
        };
        assert_eq!(send(live).await.0, StatusCode::OK);
        let (off_status, off_body) = send(off).await;
        let (old_status, old_body) = send(old).await;
        assert_eq!((off_status, old_status), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
        assert_eq!(off_body, old_body, "expired and disabled are indistinguishable from outside");
        assert!(off_body.contains(crate::mcp_auth::KEY_INACTIVE_MESSAGE));
        // An unreadable store denies.
        std::fs::write(duduclaw_core::mcp_key_store::store_path(home.path()), b"corrupt store bytes, not sqlite").unwrap();
        let (status, body) = send("ddc_prod_0123456789abcdef0123456789abcdef".into()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("fail-closed"), "{body}");
    }

    /// A dispatcher whose home is `home` (the unbound-key refusal reads the
    /// employee directories of the dispatcher's home).
    fn dispatcher_at(home: &std::path::Path) -> McpDispatcher {
        McpDispatcher::new(
            home.to_path_buf(),
            reqwest::Client::new(),
            Arc::new(duduclaw_memory::SqliteMemoryEngine::in_memory().expect("in-memory db")),
            "dudu".to_string(),
            Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
            RateLimiter::new(),
            DailyQuota::new(),
        )
    }

    /// The state `build_router` makes, with its audit handed back so a test
    /// can roll the windows over.
    fn state_with_audit(
        cfg: &HttpServerConfig,
        dispatcher: McpDispatcher,
    ) -> (HttpState, Arc<crate::mcp_auth_audit::AuthFailureAudit>) {
        let origin = cfg.external_origin().unwrap();
        let audit = crate::mcp_auth_audit::AuthFailureAudit::new(cfg.home_dir.clone());
        let state = HttpState {
            resource: format!("{origin}/mcp"),
            external_origin: origin,
            dispatcher: http_dispatcher(dispatcher),
            sse_store: Arc::new(SseEventStore::new()),
            call_timeout: cfg.call_timeout,
            home_dir: cfg.home_dir.clone(),
            auth_audit: audit.clone(),
        };
        (state, audit)
    }

    fn from_peer(mut request: Request<Body>, last: u8) -> Request<Body> {
        request.extensions_mut().insert(axum::extract::ConnectInfo(SocketAddr::from(([192, 0, 2, last], 4000))));
        request
    }

    /// v1.82: every refusal on the HTTP transport keeps its exact status and
    /// body. It leaves one detail line in `mcp_auth_failures.jsonl` (socket
    /// peer — never X-Forwarded-For —, fixed route label, stored key id), an
    /// immediate chain record for a stored key and a count in the hour
    /// summary otherwise; neither file holds anything of the presented
    /// credential, the query string or the User-Agent.
    #[tokio::test]
    async fn http_auth_failures_are_audited_and_the_response_is_unchanged() {
        use crate::mcp_auth_audit::tests::{assert_nothing_of, chain_events, detail_lines, raw, wait_until};
        use crate::mcp_auth_audit::{DETAIL_FILE, EVENT_MCP_AUTH_FAILED, EVENT_MCP_AUTH_FAILED_SUMMARY};
        use axum::http::HeaderValue;
        use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, McpKeyStore, NewMcpKey};

        let home = tempfile::tempdir().unwrap();
        let home_path = home.path();
        std::fs::write(home_path.join("config.toml"), "").unwrap();
        let store = McpKeyStore::open(home_path).unwrap();
        let now = chrono::Utc::now();
        let issue = |name: &str, binding: KeyBinding, client_id: &str| {
            let created = store.create(NewMcpKey {
                name: name.into(), client_id: client_id.into(), binding, env: "prod".into(),
                scopes: vec!["memory:read".into()], expiry: Expiry::Days(30), created_by: Some("admin".into()),
            }, now).unwrap();
            (created.secret, created.key.id)
        };
        let (off, off_id) = issue("off", KeyBinding::Integration, "off-client");
        let (old, old_id) = issue("old", KeyBinding::Integration, "old-client");
        let (gone, gone_id) = issue("gone", KeyBinding::Integration, "gone-client");
        // An employee key whose employee does not exist: refused every tool.
        let (orphan, orphan_id) = issue("orphan", KeyBinding::Employee, "alice");
        store.set_disabled(&off_id, true, "admin", now).unwrap();
        store.delete(&gone_id, "admin", now).unwrap();
        drop(store);
        rusqlite::Connection::open(duduclaw_core::mcp_key_store::store_path(home_path)).unwrap()
            .execute("UPDATE mcp_api_keys SET expires_at = '2020-01-01T00:00:00Z' WHERE id = ?1", [&old_id]).unwrap();
        let scopes = crate::mcp_auth::parse_scopes("memory:read").unwrap();
        let (refresh, _) = crate::mcp_refresh::issue_refresh_token(home_path, "dev", "external-client", &scopes, true).unwrap();
        let users = duduclaw_auth::UserDb::new(&home_path.join("users.db")).unwrap();
        let user = users
            .create_user("alice@test.invalid", "Alice", "isolated-password", duduclaw_auth::UserRole::Admin)
            .unwrap();
        let status_token = users.status_token_create(&user.id, "odoo").unwrap().secret;
        drop(users);

        let unknown = "ddc_prod_0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string();
        let malformed = "not-a-key-Zq9Xw8Vu7Ts6Rp5".to_string();
        let oauth = format!("ddc_oauth_{}", "7c3e9a1f".repeat(8));
        let sse_key = "ddc_prod_5eed4a7b5eed4a7b5eed4a7b5eed4a7b".to_string();
        let store_probe = "ddc_prod_0123456789abcdef0123456789abcdef".to_string();
        let user_agent = "AuditProbe/9.9 (ua-marker-4f2c)";
        let forwarded = "198.51.100.200";

        enum Expected { Json(Value), Bytes(Vec<u8>) }
        let json_error = |message: &str| Expected::Json(crate::mcp_dispatch::jsonrpc_error(&Value::Null, -32003, message));
        let not_found = || json_error("Authentication failed: API key not found in registry");
        let inactive = || json_error(&format!("Authentication failed: {}", crate::mcp_auth::KEY_INACTIVE_MESSAGE));
        let call = |authorization: Option<HeaderValue>| {
            let mut request = Request::builder().method("POST").uri("/mcp/v1/call")
                .header("content-type", "application/json")
                // Client-controlled; never the audited source.
                .header("x-forwarded-for", forwarded)
                .header("user-agent", user_agent)
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"q"}}}"#,
                ))
                .unwrap();
            if let Some(value) = authorization {
                request.headers_mut().insert("authorization", value);
            }
            request
        };
        let bearer = |key: &str| Some(HeaderValue::from_str(&format!("Bearer {key}")).unwrap());
        let sse = Request::builder()
            .uri(format!("/mcp/v1/stream?api_key={sse_key}&conn_id=probe-conn"))
            .header("x-forwarded-for", forwarded)
            .header("user-agent", user_agent)
            .body(Body::empty())
            .unwrap();

        // (case, request, status, exact body, reason, stored key id, route)
        let cases: Vec<(&str, Request<Body>, StatusCode, Expected, &str, Option<String>, &str)> = vec![
            ("missing", call(None), StatusCode::UNAUTHORIZED,
                json_error("Missing Authorization header. Use: Authorization: Bearer <key>"), "missing_header", None, "mcp_v1_call"),
            ("basic", call(Some(HeaderValue::from_static("Basic dXNlcjpwYXNz"))), StatusCode::UNAUTHORIZED,
                json_error("Authorization header must use: Bearer <key>"), "bad_scheme", None, "mcp_v1_call"),
            ("encoding", call(Some(HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap())), StatusCode::UNAUTHORIZED,
                json_error("Invalid Authorization header encoding"), "bad_scheme", None, "mcp_v1_call"),
            ("empty", call(Some(HeaderValue::from_static("Bearer "))), StatusCode::UNAUTHORIZED,
                json_error("Authorization header must use: Bearer <key>"), "malformed_key", None, "mcp_v1_call"),
            ("malformed", call(bearer(&malformed)), StatusCode::UNAUTHORIZED,
                json_error("Authentication failed: API key has invalid format"), "malformed_key", None, "mcp_v1_call"),
            ("unknown", call(bearer(&unknown)), StatusCode::UNAUTHORIZED, not_found(), "unknown_key", None, "mcp_v1_call"),
            ("refresh", call(bearer(&refresh)), StatusCode::UNAUTHORIZED,
                json_error(crate::mcp_refresh::STDIO_ONLY_NOTICE), "wrong_credential_type", None, "mcp_v1_call"),
            ("status", call(bearer(&status_token)), StatusCode::UNAUTHORIZED,
                json_error("Authentication failed: this credential cannot be used here"), "wrong_credential_type", None, "mcp_v1_call"),
            ("oauth", call(bearer(&oauth)), StatusCode::UNAUTHORIZED, not_found(), "unknown_key", None, "mcp_v1_call"),
            ("sse", sse, StatusCode::UNAUTHORIZED, not_found(), "unknown_key", None, "mcp_v1_stream"),
            ("disabled", call(bearer(&off)), StatusCode::UNAUTHORIZED, inactive(), "key_disabled", Some(off_id.clone()), "mcp_v1_call"),
            ("expired", call(bearer(&old)), StatusCode::UNAUTHORIZED, inactive(), "key_expired", Some(old_id.clone()), "mcp_v1_call"),
            ("deleted", call(bearer(&gone)), StatusCode::UNAUTHORIZED, not_found(), "key_deleted", Some(gone_id.clone()), "mcp_v1_call"),
            ("unbound", call(bearer(&orphan)), StatusCode::FORBIDDEN,
                Expected::Json(crate::mcp_dispatch::jsonrpc_error(&serde_json::json!(1), -32003, crate::mcp::REMOTE_UNBOUND_MESSAGE)),
                "key_unbound", Some(orphan_id.clone()), "mcp_v1_call"),
        ];
        let mut expected_detail: Vec<(String, &str, Option<String>, &str)> = Vec::new();

        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home_path.into());
        let (state, audit) = state_with_audit(&cfg, dispatcher_at(home_path));
        let app = build_app_with_state(&cfg, state);
        let check = |name: String, status: StatusCode, expected: Expected, response: Response| async move {
            assert_eq!(response.status(), status, "{name}: status changed");
            assert!(response.extensions().get::<crate::mcp_auth_audit::AuthFailureTag>().is_none(), "{name}");
            let body = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
            match expected {
                Expected::Json(value) => {
                    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), value, "{name}: body changed")
                }
                Expected::Bytes(bytes) => assert_eq!(body.to_vec(), bytes, "{name}: body changed"),
            }
        };
        for (index, (name, request, status, expected, reason, key_id, route)) in cases.into_iter().enumerate() {
            let last = 10 + index as u8;
            let response = app.clone().oneshot(from_peer(request, last)).await.unwrap();
            check(name.to_string(), status, expected, response).await;
            expected_detail.push((format!("192.0.2.{last}"), reason, key_id, route));
        }

        // OAuth consent: a refused operator key (the disabled one again).
        let register = Request::builder().method("POST").uri("/oauth/register")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"redirect_uris":["https://client.example/cb"]}"#)).unwrap();
        let response = app.clone().oneshot(from_peer(register, 60)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let client: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        let form = format!(
            "client_id={}&redirect_uri={}&code_challenge=abc&operator_key={}&action=approve",
            urlencoding::encode(client["client_id"].as_str().unwrap()),
            urlencoding::encode("https://client.example/cb"),
            urlencoding::encode(&off),
        );
        let decision = Request::builder().method("POST").uri("/oauth/decision")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", forwarded)
            .header("user-agent", user_agent)
            .body(Body::from(form)).unwrap();
        let refused = crate::mcp_oauth_server::html_error(
            StatusCode::UNAUTHORIZED,
            "operator key rejected — paste an internal (is_external = false) MCP API key",
        );
        let refused = axum::body::to_bytes(refused.into_body(), 65536).await.unwrap().to_vec();
        let response = app.clone().oneshot(from_peer(decision, 61)).await.unwrap();
        check("consent".into(), StatusCode::UNAUTHORIZED, Expected::Bytes(refused), response).await;
        expected_detail.push(("192.0.2.61".into(), "key_disabled", Some(off_id.clone()), "oauth_decision"));

        // An unreadable key store (last: it breaks every stored key).
        std::fs::write(duduclaw_core::mcp_key_store::store_path(home_path), b"corrupt store bytes, not sqlite").unwrap();
        let decided = crate::mcp_auth::authenticate_with_key_call_refusal(&store_probe, home_path).unwrap_err();
        assert_eq!(decided.audit_reason(), crate::mcp_auth_audit::FailureReason::StoreUnavailable);
        let response = app.clone().oneshot(from_peer(call(bearer(&store_probe)), 62)).await.unwrap();
        check("store".into(), StatusCode::UNAUTHORIZED, json_error(&format!("Authentication failed: {}", decided.error)), response).await;
        expected_detail.push(("192.0.2.62".into(), "store_unavailable", None, "mcp_v1_call"));

        // Detail: one line per refusal; chain: the four stored-key pairs at once.
        wait_until(|| {
            detail_lines(home_path).len() >= expected_detail.len()
                && chain_events(home_path, EVENT_MCP_AUTH_FAILED).len() >= 4
        })
        .await;
        let detail = detail_lines(home_path);
        assert_eq!(detail.len(), expected_detail.len(), "{detail:?}");
        for (source, reason, key_id, route) in &expected_detail {
            let line = detail.iter().find(|l| l["source"] == source.as_str())
                .unwrap_or_else(|| panic!("no detail line for {reason} from {source}"));
            assert_eq!(line["reason"], *reason, "{source}");
            assert_eq!(line["route"], *route, "{source}");
            assert_eq!(line["transport"], "http");
            assert_eq!(line["count"], 1);
            assert_eq!(line.get("key_id").and_then(Value::as_str), key_id.as_deref(), "{reason}");
        }
        let chain = chain_events(home_path, EVENT_MCP_AUTH_FAILED);
        assert_eq!(chain.len(), 4, "{chain:?}");
        for (reason, key_id) in [("key_disabled", &off_id), ("key_expired", &old_id), ("key_deleted", &gone_id), ("key_unbound", &orphan_id)] {
            let event = chain.iter().find(|e| e["details"]["key_id"] == key_id.as_str())
                .unwrap_or_else(|| panic!("no chain record for {reason}"));
            assert_eq!(event["details"]["reason"], reason);
            assert_eq!(event["details"]["aggregation"], "first_in_hour");
            assert_eq!(event["details"]["route"], "mcp_v1_call");
        }
        assert!(chain_events(home_path, EVENT_MCP_AUTH_FAILED_SUMMARY).is_empty(), "anonymous failures wait for the hour");

        // The hour ends: the second consent refusal of the disabled key and
        // every anonymous failure are summarised.
        audit.flush_at(std::time::Instant::now() + Duration::from_secs(61), chrono::Utc::now().timestamp() + 3600);
        wait_until(|| {
            !chain_events(home_path, EVENT_MCP_AUTH_FAILED_SUMMARY).is_empty()
                && chain_events(home_path, EVENT_MCP_AUTH_FAILED).len() >= 5
        })
        .await;
        let per_key: Vec<Value> = chain_events(home_path, EVENT_MCP_AUTH_FAILED).into_iter()
            .filter(|e| e["details"]["aggregation"] == "hour_summary").collect();
        assert_eq!(per_key.len(), 1, "{per_key:?}");
        assert_eq!(per_key[0]["details"]["key_id"], off_id.as_str());
        assert_eq!(per_key[0]["details"]["count"], 1);
        assert_eq!(per_key[0]["details"]["route"], crate::mcp_auth_audit::ROUTE_MIXED);
        let summary = chain_events(home_path, EVENT_MCP_AUTH_FAILED_SUMMARY);
        assert_eq!(summary.len(), 1);
        let summary = &summary[0]["details"];
        assert_eq!(summary["count"], 11);
        assert_eq!(summary["distinct_sources"], 11);
        assert_eq!(summary["detail_log"], DETAIL_FILE);
        for (reason, count) in [("missing_header", 1), ("bad_scheme", 2), ("malformed_key", 2), ("unknown_key", 3),
            ("wrong_credential_type", 2), ("store_unavailable", 1)] {
            assert_eq!(summary["reasons"][reason], count, "{reason}");
        }

        let raw_chain = raw(home_path, "security_audit.jsonl");
        let raw_detail = raw(home_path, DETAIL_FILE);
        assert!(!raw_chain.contains("192.0.2."), "no source address in the chain");
        for text in [&raw_chain, &raw_detail] {
            assert!(!text.contains(forwarded), "X-Forwarded-For is never the audited source");
            assert!(!text.contains("dXNlcjpwYXNz"), "no Authorization value is kept");
            assert!(!text.contains("ua-marker-4f2c"), "no User-Agent is kept");
            assert!(!text.contains("api_key") && !text.contains("conn_id") && !text.contains("/mcp/v1/stream"),
                "no raw URI or query is kept");
            for secret in [&off, &old, &gone, &orphan, &unknown, &malformed, &refresh, &status_token, &oauth, &sse_key, &store_probe] {
                assert_nothing_of(secret, text);
            }
        }
    }

    /// v1.82 (L1): the audit layer wraps every route composed into the
    /// application — including one added to the route set the way a future
    /// route would be — and the fallback. `McpHttpApp` itself offers no way
    /// to add a route after the layers (see its `compile_fail` doc test).
    #[tokio::test]
    async fn a_route_composed_into_the_app_cannot_bypass_the_audit_layer() {
        use crate::mcp_auth_audit::tests::{detail_lines, wait_until};
        async fn tagging() -> Response {
            crate::mcp_auth_audit::tag(
                (StatusCode::UNAUTHORIZED, "refused").into_response(),
                crate::mcp_auth_audit::FailureReason::UnknownKey,
                None,
            )
        }
        let home = tempfile::tempdir().unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.path().into());
        let (state, _) = state_with_audit(&cfg, make_test_dispatcher());
        let app = finish_app(&cfg, state, mcp_routes(&cfg).route("/extra", get(tagging)));
        let request = Request::builder().uri("/extra").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(from_peer(request, 99)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.extensions().get::<crate::mcp_auth_audit::AuthFailureTag>().is_none(), "the marker is stripped");
        assert!(response.headers().contains_key("x-duduclaw-version"), "every layer ran");
        wait_until(|| !detail_lines(home.path()).is_empty()).await;
        let lines = detail_lines(home.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["source"], "192.0.2.99");
        assert_eq!(lines[0]["route"], "other");
        // The fallback passes the same layers.
        let request = Request::builder().uri("/no-such-route").body(Body::empty()).unwrap();
        let response = app.oneshot(from_peer(request, 98)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().contains_key("x-duduclaw-version"));
    }

    #[test]
    fn non_loopback_requires_https_proxy_or_explicit_insecure_opt_in() {
        let mut cfg = HttpServerConfig::new("0.0.0.0:8765".parse().unwrap(), "/tmp".into());
        assert!(cfg.external_origin().is_err());
        cfg.public_origin = Some("https://mcp.example.com/".into());
        assert!(cfg.external_origin().is_err());
        cfg.trusted_proxies.push("127.0.0.1".parse().unwrap());
        assert_eq!(cfg.external_origin().unwrap(), "https://mcp.example.com");
        for invalid in ["http://mcp.example.com", "https://user:pass@mcp.example.com", "https://mcp.example.com/mcp", "https://mcp.example.com/?x=1", "https://mcp.example.com/#fragment"] {
            cfg.public_origin = Some(invalid.into());
            assert!(cfg.external_origin().is_err(), "{invalid}");
        }
        cfg.public_origin = None;
        cfg.trusted_proxies.clear();
        cfg.allow_insecure_lan = true;
        assert_eq!(cfg.external_origin().unwrap(), "http://0.0.0.0:8765");
        let cfg = HttpServerConfig::new("[::1]:8765".parse().unwrap(), "/tmp".into());
        assert_eq!(cfg.external_origin().unwrap(), "http://[::1]:8765");
    }

    #[test]
    fn trusted_proxy_uses_last_untrusted_hop_and_rejects_spoofing() {
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        let relay: IpAddr = "192.0.2.10".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.99, 198.51.100.5, 192.0.2.10".parse().unwrap());
        assert_eq!(client_ip(Some(proxy), &headers, &[]), "127.0.0.1");
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), "192.0.2.10");
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy, relay]), "198.51.100.5");
        headers.insert("x-forwarded-for", "bad, 198.51.100.5".parse().unwrap());
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), "127.0.0.1");
        assert_eq!(client_ip(None, &headers, &[proxy]), "unknown-peer");
    }

    #[tokio::test]
    async fn canonical_discovery_ignores_host_and_forwarded_proto() {
        let home = tempfile::tempdir().unwrap();
        let mut cfg = HttpServerConfig::new("127.0.0.1:8765".parse().unwrap(), home.path().into());
        for configured in [false, true] {
            if configured {
                cfg.public_origin = Some("https://mcp.example.com".into());
                cfg.trusted_proxies.push("127.0.0.1".parse().unwrap());
            }
            let base = if configured { "https://mcp.example.com" } else { "http://127.0.0.1:8765" };
            let router = build_router(&cfg, make_test_dispatcher());
            for path in ["/.well-known/oauth-authorization-server", "/.well-known/oauth-protected-resource"] {
                let request = Request::builder().uri(path).header("host", "attacker.example")
                    .header("x-forwarded-proto", "https").body(Body::empty()).unwrap();
                let response = router.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                if path.ends_with("authorization-server") {
                    assert_eq!(value["issuer"], base);
                    assert_eq!(value["token_endpoint"], format!("{base}/oauth/token"));
                    assert_eq!(value["revocation_endpoint"], format!("{base}/oauth/revoke"));
                } else { assert_eq!(value["resource"], format!("{base}/mcp")); }
            }
        }
    }

    #[tokio::test]
    async fn authenticated_principal_survives_exhausted_proxy_ip_budget() {
        let home = tempfile::tempdir().unwrap();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='separate-principal'\nscopes=['memory:read']\nis_external=true\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:8765".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        for _ in 0..120 {
            let request = Request::builder().uri("/.well-known/oauth-protected-resource")
                .body(Body::empty()).unwrap();
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
        }
        let request = Request::builder().uri("/.well-known/oauth-protected-resource")
            .body(Body::empty()).unwrap();
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
        let request = Request::builder().method("POST").uri("/mcp")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
        assert_eq!(router.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    /// v1.82 (RM-134): the HTTP transport always serves through a
    /// remote-bearer dispatcher; stdio dispatchers are not.
    #[test]
    fn http_routes_use_a_remote_bearer_dispatcher() {
        assert!(!make_test_dispatcher().is_remote_bearer());
        assert!(http_dispatcher(make_test_dispatcher()).is_remote_bearer());
    }

    #[test]
    fn registration_has_dedicated_global_creation_limit() {
        let budget = RegistrationBudget::default();
        for _ in 0..10 { assert!(budget.admit_creation()); }
        assert!(!budget.admit_creation());
        for (start, _) in budget.0.lock().unwrap().buckets.values_mut() { *start -= Duration::from_secs(61); }
        assert!(budget.admit_creation());
    }

    #[tokio::test]
    async fn registration_is_limited_before_store_writes() {
        let home = tempfile::tempdir().unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:8765".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        for n in 0..4 {
            let request = Request::builder().method("POST").uri("/oauth/register")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"redirect_uris":["https://client.example/cb"]}"#)).unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), if n < 3 { StatusCode::CREATED } else { StatusCode::TOO_MANY_REQUESTS });
        }
        assert_eq!(duduclaw_core::mcp_oauth_store::load_store(home.path()).unwrap().clients.len(), 3);
    }

    fn hint_test_state(home: &std::path::Path) -> AdmissionState {
        AdmissionState {
            anonymous: Arc::new(std::sync::Mutex::new(Admission::default())),
            principals: Arc::new(std::sync::Mutex::new(Admission::default())),
            registration: Arc::new(std::sync::Mutex::new(Admission::default())),
            hints: Arc::new(std::sync::Mutex::new(CredentialHints::default())),
            trusted_proxies: vec![],
            http: HttpState {
                external_origin: "http://127.0.0.1:8765".into(),
                resource: "http://127.0.0.1:8765/mcp".into(),
                home_dir: home.into(),
                dispatcher: http_dispatcher(make_test_dispatcher()),
                sse_store: Arc::new(SseEventStore::new()),
                call_timeout: Duration::from_secs(30),
                auth_audit: crate::mcp_auth_audit::AuthFailureAudit::new(home.into()),
            },
        }
    }

    #[test]
    fn credential_hint_reload_is_single_flight_and_time_bounded() {
        let mut cache = CredentialHints::default();
        let now = std::time::Instant::now();
        assert!(cache.start_reload(now));
        assert!(!cache.start_reload(now + Duration::from_secs(10)));
        cache.loading = false;
        cache.refreshed = Some(now);
        for _ in 0..1000 { assert!(!cache.start_reload(now + Duration::from_millis(999))); }
        assert_eq!(cache.reloads, 1);
        assert!(cache.start_reload(now + Duration::from_secs(1)));
        assert_eq!(cache.reloads, 2);
    }

    #[tokio::test]
    async fn unknown_bearers_cannot_force_reload_after_anonymous_exhaustion() {
        let home = tempfile::tempdir().unwrap();
        let state = hint_test_state(home.path());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer ddc_oauth_{:064x}", 0).parse().unwrap());
        assert!(admission_principal(&headers, &state).await.unwrap().is_none());
        // Freeze the refresh clock in this test; no request may bypass the
        // one-second reload decision merely by changing its token string.
        state.hints.lock().unwrap().refreshed = Some(std::time::Instant::now() + Duration::from_secs(60));
        std::fs::write(home.path().join("mcp_oauth_issued.json"), "broken").unwrap();
        let router = Router::new().route("/mcp", post(crate::mcp_streamable::mcp_post_handler))
            .with_state(state.http.clone())
            .layer(middleware::from_fn_with_state(state.clone(), pre_auth_admission));
        for n in 0..125 {
            let request = Request::builder().method("POST").uri("/mcp")
                .header("authorization", format!("Bearer ddc_oauth_{n:064x}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), if n < 120 { StatusCode::UNAUTHORIZED } else { StatusCode::TOO_MANY_REQUESTS });
        }
        assert_eq!(state.hints.lock().unwrap().reloads, 1);
    }

    #[tokio::test]
    async fn stale_credential_hint_never_bypasses_live_authentication() {
        let home = tempfile::tempdir().unwrap();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='revoked-principal'\nscopes=['memory:read']\nis_external=true\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let state = hint_test_state(home.path());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {key}").parse().unwrap());
        assert_eq!(admission_principal(&headers, &state).await.unwrap().as_deref(), Some("revoked-principal"));
        state.hints.lock().unwrap().refreshed = Some(std::time::Instant::now() + Duration::from_secs(60));
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        for _ in 0..120 { assert!(state.anonymous.lock().unwrap().allow("unknown-peer".into(), 120)); }
        let router = Router::new().route("/mcp", post(crate::mcp_streamable::mcp_post_handler))
            .with_state(state.http.clone())
            .layer(middleware::from_fn_with_state(state.clone(), pre_auth_admission));
        let request = Request::builder().method("POST").uri("/mcp")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
        assert_eq!(router.oneshot(request).await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(state.hints.lock().unwrap().reloads, 1);
    }

    // ── Header name constants (mirrors mcp_capability.rs) ─────────────────────

    const HDR_VERSION: &str = "x-duduclaw-version";
    const HDR_CAPABILITIES: &str = "x-duduclaw-capabilities";
    const HDR_MISSING_CAPABILITIES: &str = "x-duduclaw-missing-capabilities";

    // ── Helpers ────────────────────────────────────────────────────────────────

    pub fn ephemeral_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    }

    /// Build a McpDispatcher backed by an in-memory SQLite database.
    /// No real tools are called in these tests; we only need the router to be valid.
    fn make_test_dispatcher() -> McpDispatcher {
        let home_dir = std::env::temp_dir().join("duduclaw_http_server_test");
        let _ = std::fs::create_dir_all(&home_dir);
        let http = reqwest::Client::new();
        let memory =
            Arc::new(duduclaw_memory::SqliteMemoryEngine::in_memory().expect("in-memory db"));
        let odoo = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        McpDispatcher::new(
            home_dir,
            http,
            memory,
            "dudu".to_string(),
            odoo,
            RateLimiter::new(),
            DailyQuota::new(),
        )
    }

    /// Build a default HttpServerConfig pointing at a temp dir.
    fn test_config() -> HttpServerConfig {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        HttpServerConfig::new(addr, std::env::temp_dir())
    }

    /// Shorthand: GET /healthz with no extra headers.
    fn healthz_request() -> Request<Body> {
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap()
    }

    /// Shorthand: GET /healthz with x-duduclaw-capabilities header.
    fn healthz_with_caps(caps: &str) -> Request<Body> {
        Request::builder()
            .uri("/healthz")
            .header(HDR_CAPABILITIES, caps)
            .body(Body::empty())
            .unwrap()
    }

    // ── HttpServerConfig tests ─────────────────────────────────────────────────

    #[test]
    fn ephemeral_port_binds_successfully() {
        let addr = ephemeral_addr();
        assert!(addr.port() > 0);
    }

    #[test]
    fn config_new_has_correct_defaults() {
        let cfg = test_config();
        assert!(cfg.enable_sse, "SSE should be enabled by default");
        assert_eq!(
            cfg.call_timeout,
            Duration::from_secs(30),
            "default call timeout should be 30s"
        );
    }

    #[test]
    fn config_stores_bind_addr_and_home_dir() {
        let bind: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let home = std::path::PathBuf::from("/tmp/test-home");
        let cfg = HttpServerConfig::new(bind, home.clone());
        assert_eq!(cfg.bind, bind);
        assert_eq!(cfg.home_dir, home);
    }

    // ── build_router smoke tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_response_body_contains_ok_status() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["status"], "ok", "healthz body must contain status: ok");
    }

    // ── ADR-002 header injection via build_router ─────────────────────────────

    #[tokio::test]
    async fn healthz_always_has_version_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let version = resp
            .headers()
            .get(HDR_VERSION)
            .expect("x-duduclaw-version must be injected by middleware");
        assert_eq!(version.to_str().unwrap(), "1.2");
    }

    #[tokio::test]
    async fn healthz_always_has_capabilities_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let caps = resp
            .headers()
            .get(HDR_CAPABILITIES)
            .expect("x-duduclaw-capabilities must be injected by middleware");
        let caps_str = caps.to_str().unwrap();
        assert!(
            caps_str.starts_with("memory/"),
            "memory must be first cap: {caps_str}"
        );
    }

    #[tokio::test]
    async fn healthz_capabilities_header_lists_enabled_caps_only() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let caps_str = resp.headers()[HDR_CAPABILITIES].to_str().unwrap();
        assert!(
            caps_str.contains("mcp/2"),
            "mcp/2 must be listed: {caps_str}"
        );
        assert!(
            caps_str.contains("audit/2"),
            "audit/2 must be listed: {caps_str}"
        );
        assert!(
            !caps_str.contains("a2a/"),
            "disabled a2a must be absent: {caps_str}"
        );
    }

    // ── Capability negotiation through the full router ────────────────────────

    #[tokio::test]
    async fn healthz_with_satisfied_capability_returns_200() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router
            .oneshot(healthz_with_caps("memory/3,mcp/2"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_with_disabled_capability_returns_422() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        // a2a is disabled in the registry → 422
        let resp = router.oneshot(healthz_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn healthz_422_includes_missing_capabilities_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router
            .oneshot(healthz_with_caps("a2a/1,secret-manager/1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let missing = resp
            .headers()
            .get(HDR_MISSING_CAPABILITIES)
            .expect("422 must include x-duduclaw-missing-capabilities");
        let missing_str = missing.to_str().unwrap();
        assert!(
            missing_str.contains("a2a/1"),
            "a2a must be listed as missing: {missing_str}"
        );
    }

    #[tokio::test]
    async fn healthz_422_still_has_standard_duduclaw_headers() {
        // Even a 422 from negotiate_capabilities must carry the standard headers
        // (inject_capability_headers is the outer layer — runs on ALL responses)
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            resp.headers().contains_key(HDR_VERSION),
            "422 must carry x-duduclaw-version"
        );
        assert!(
            resp.headers().contains_key(HDR_CAPABILITIES),
            "422 must carry x-duduclaw-capabilities"
        );
    }

    // ── Authentication guard on protected endpoints ───────────────────────────

    #[tokio::test]
    async fn call_endpoint_returns_401_without_auth() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sse_stream_endpoint_returns_401_without_auth() {
        let cfg = test_config(); // enable_sse: true by default
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── SSE routing configuration ─────────────────────────────────────────────

    #[tokio::test]
    async fn sse_stream_returns_404_when_sse_disabled() {
        let mut cfg = test_config();
        cfg.enable_sse = false;
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "SSE endpoint must be absent when enable_sse = false"
        );
    }

    // ── Authentication header format error paths ──────────────────────────────

    #[tokio::test]
    async fn call_endpoint_returns_401_with_non_bearer_auth() {
        // Authorization header exists but doesn't start with "Bearer "
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Basic dXNlcjpwYXNz")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Non-Bearer auth scheme must return 401"
        );
    }

    #[tokio::test]
    async fn call_endpoint_returns_401_with_empty_bearer_token() {
        // "Bearer " prefix present but token is empty
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Bearer ")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Empty Bearer token must return 401"
        );
    }

    #[tokio::test]
    async fn call_endpoint_returns_401_with_invalid_bearer_token() {
        // Valid format but unknown key → authenticate_with_key fails
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Bearer invalid-key-that-does-not-exist")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Invalid Bearer token must return 401"
        );
    }

    #[tokio::test]
    async fn sse_stream_returns_401_with_invalid_query_api_key() {
        // SSE: no Bearer header, but ?api_key= is invalid
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream?api_key=invalid-key")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Invalid query api_key must return 401"
        );
    }

    #[tokio::test]
    async fn stream_call_endpoint_returns_401_without_auth() {
        // POST /mcp/v1/stream/call requires Bearer auth
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/stream/call?conn_id=test-conn")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "stream/call without auth must return 401"
        );
    }

    #[tokio::test]
    async fn stream_call_returns_404_when_sse_disabled() {
        let mut cfg = test_config();
        cfg.enable_sse = false;
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/stream/call?conn_id=test-conn")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "stream/call endpoint must be absent when SSE disabled"
        );
    }
    #[tokio::test]
    async fn proxy_boundary_rejects_untrusted_or_missing_peer_before_body_and_headers() {
        let home = tempfile::tempdir().unwrap();
        let mut cfg = HttpServerConfig::new("0.0.0.0:8765".parse().unwrap(), home.path().into());
        cfg.public_origin = Some("https://mcp.example.com".into());
        cfg.trusted_proxies = vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
        let router = build_router(&cfg, make_test_dispatcher());
        for peer in [None, Some("192.0.2.8:1234"), Some("[2001:db8::8]:1234")] {
            for path in ["/healthz", "/oauth/register", "/oauth/token", "/mcp", "/missing"] {
                let stream = tokio_stream::iter(std::iter::from_fn(|| -> Option<Result<&'static str, std::io::Error>> {
                    panic!("untrusted request body was polled");
                }));
                let mut request = Request::builder().method("POST").uri(path)
                    .header("x-forwarded-for", "127.0.0.1")
                    .header("x-duduclaw-capabilities", "invalid")
                    .header("content-type", "application/json").body(Body::from_stream(stream)).unwrap();
                if let Some(peer) = peer {
                    request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
                }
                let response = router.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert!(!response.headers().contains_key("x-duduclaw-version"));
            }
        }
        for peer in ["127.0.0.1:1234", "[::1]:1234"] {
            let mut request = Request::builder().uri("/healthz").body(Body::empty()).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
        }
        // Both explicitly insecure LAN and loopback proxy deployments still work.
        for (bind, insecure) in [("0.0.0.0:8765", true), ("127.0.0.1:8765", false), ("[::1]:8765", false)] {
            cfg.bind = bind.parse().unwrap(); cfg.allow_insecure_lan = insecure;
            let router = build_router(&cfg, make_test_dispatcher());
            let request = Request::builder().uri("/healthz").body(Body::empty()).unwrap();
            assert_eq!(router.oneshot(request).await.unwrap().status(), StatusCode::OK);
        }
        assert!(!home.path().join("mcp_oauth_issued.json").exists());
    }

    #[tokio::test]
    async fn malformed_registration_cannot_consume_global_creation_quota() {
        let home = tempfile::tempdir().unwrap();
        let cfg = HttpServerConfig::new("127.0.0.1:8765".parse().unwrap(), home.path().into());
        let router = build_router(&cfg, make_test_dispatcher());
        for n in 0..20 {
            let invalid = if n % 2 == 0 { "{" } else { r#"{"redirect_uris":[]}"# };
            let mut request = Request::builder().method("POST").uri("/oauth/register")
                .header("content-type", "application/json").body(Body::from(invalid)).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(SocketAddr::from(([192, 0, 2, n], 1234))));
            assert!(router.clone().oneshot(request).await.unwrap().status().is_client_error());
        }
        assert!(!home.path().join("mcp_oauth_issued.json").exists());
        for n in 0..11 {
            let mut request = Request::builder().method("POST").uri("/oauth/register")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"redirect_uris":["https://client.example/cb"]}"#)).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(SocketAddr::from(([198, 51, 100, n], 1234))));
            assert_eq!(router.clone().oneshot(request).await.unwrap().status(),
                if n < 10 { StatusCode::CREATED } else { StatusCode::TOO_MANY_REQUESTS });
        }
    }

    #[tokio::test]
    async fn invalid_token_probes_cannot_starve_same_client_at_another_proxy_source() {
        use duduclaw_core::mcp_oauth_store::{Consent, Grant, IssuedToken, digest, update_store};
        let home = tempfile::tempdir().unwrap();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='operator'\nscopes=['memory:read']\nis_external=false\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let mut cfg = HttpServerConfig::new("0.0.0.0:8765".parse().unwrap(), home.path().into());
        cfg.public_origin = Some("https://mcp.example.com".into());
        cfg.trusted_proxies.push("127.0.0.1".parse().unwrap());
        update_store(home.path(), |store| {
            store.grants.insert("family".into(), Grant {
                resource: "https://mcp.example.com/mcp".into(), client_id: "client".into(),
                client_name: "Example".into(), scope: "memory:read".into(),
                issued_unix: chrono::Utc::now().timestamp(), last_used_unix: None,
                consent: Consent::capture(home.path(), key, "memory:read").unwrap(),
            });
            store.refresh.insert(digest("valid-refresh"), IssuedToken {
                resource: "https://mcp.example.com/mcp".into(), client_id: "client".into(),
                client_name: "Example".into(), scope: "memory:read".into(),
                expires_unix: chrono::Utc::now().timestamp() + 3600, family_id: "family".into(), used: false,
            });
        }).unwrap();
        let router = build_router(&cfg, make_test_dispatcher());
        for n in 0..14 {
            let (source, token) = if n == 13 { ("198.51.100.2", "valid-refresh") } else { ("192.0.2.1", "invalid") };
            let mut request = Request::builder().method("POST").uri("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("x-forwarded-for", source)
                .body(Body::from(format!("grant_type=refresh_token&client_id=client&refresh_token={token}"))).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), match n { 13 => StatusCode::OK, 12 => StatusCode::TOO_MANY_REQUESTS, _ => StatusCode::BAD_REQUEST });
        }
        // Changing claimed client IDs cannot evade the dedicated source bound.
        let other_home = tempfile::tempdir().unwrap();
        for n in 0..120 { assert!(oauth_client_admitted(other_home.path(), &n.to_string(), "192.0.2.1")); }
        assert!(!oauth_client_admitted(other_home.path(), "another", "192.0.2.1"));
        assert!(oauth_client_admitted(other_home.path(), "another", "198.51.100.2"));
    }

}
