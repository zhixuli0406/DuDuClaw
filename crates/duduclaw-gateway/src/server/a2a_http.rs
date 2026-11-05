//! HTTP envelope only; all operations enter the gateway's live RPC gates.
use super::*;
use axum::http::{HeaderMap, StatusCode};
use crate::a2a::{self, Error};
use serde_json::Value;

struct HttpLimits {
    discovery: duduclaw_security::rate_limiter::RateLimiter,
    rpc: duduclaw_security::rate_limiter::RateLimiter,
    refused: duduclaw_security::rate_limiter::RateLimiter,
    discovery_global: duduclaw_security::rate_limiter::RateLimiter,
    rpc_global: duduclaw_security::rate_limiter::RateLimiter,
    refused_global: duduclaw_security::rate_limiter::RateLimiter,
}

impl HttpLimits {
    fn new() -> Self {
        use duduclaw_security::rate_limiter::RateLimiter;
        let window = std::time::Duration::from_secs(60);
        Self {
            discovery: RateLimiter::new(120, window),
            rpc: RateLimiter::new(120, window),
            refused: RateLimiter::new(120, window),
            discovery_global: RateLimiter::with_max_keys(600, window, 1),
            rpc_global: RateLimiter::with_max_keys(1200, window, 1),
            refused_global: RateLimiter::with_max_keys(600, window, 1),
        }
    }

    async fn allow(&self, kind: Traffic, ip: std::net::IpAddr) -> bool {
        let (global, peers) = match kind {
            Traffic::Discovery => (&self.discovery_global, &self.discovery),
            Traffic::Rpc => (&self.rpc_global, &self.rpc),
            Traffic::Refused => (&self.refused_global, &self.refused),
        };
        peers.check_and_record(&ip.to_string()).await && global.check_and_record("global").await
    }
}

#[derive(Clone, Copy)]
enum Traffic { Discovery, Rpc, Refused }
static LIMITS: std::sync::LazyLock<HttpLimits> = std::sync::LazyLock::new(HttpLimits::new);
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

fn response(status: StatusCode, id: Value, result: Result<Value, Error>) -> axum::response::Response {
    let value = match result {
        Ok(value) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error.value()}),
    };
    (status, [("a2a-version", "1.0"), ("cache-control", "no-store")], Json(value)).into_response()
}

fn refused(status: StatusCode, error: Error) -> axum::response::Response {
    response(status, Value::Null, Err(error))
}

pub(super) async fn rpc(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let config = match a2a::Config::from_home(&state.home_dir) {
        Ok(config) => config,
        Err(error) => return refused(StatusCode::SERVICE_UNAVAILABLE, error),
    };
    let ip = match a2a::proxy::client_ip(&config, peer.ip(), &headers) {
        Ok(ip) => ip,
        Err(error) => return refused(StatusCode::BAD_REQUEST, error),
    };
    let ctx = match knowledge_caller(&state, &headers) {
        Ok(ctx) => ctx,
        Err(_) => {
            if !LIMITS.allow(Traffic::Refused, ip).await {
                return refused(StatusCode::TOO_MANY_REQUESTS,
                    Error::new(-32000, "Rate limit exceeded", "RESOURCE_EXHAUSTED"));
            }
            let _ = a2a::audit(&state.home_dir, "unauthenticated", "http", "refused");
            let mut response = refused(StatusCode::UNAUTHORIZED,
                Error::new(-32000, "Dashboard bearer token required", "UNAUTHENTICATED"));
            response.headers_mut().insert("www-authenticate", axum::http::HeaderValue::from_static("Bearer"));
            return response;
        }
    };
    // Anonymous discovery and rejected authentication cannot spend this budget.
    if !LIMITS.allow(Traffic::Rpc, ip).await {
        return refused(StatusCode::TOO_MANY_REQUESTS,
            Error::new(-32000, "Rate limit exceeded", "RESOURCE_EXHAUSTED"));
    }
    if headers.get_all("a2a-version").iter().count() > 1 {
        return refused(StatusCode::BAD_REQUEST, Error::invalid());
    }
    if let Err(error) = a2a::version(headers.get("a2a-version").and_then(|v| v.to_str().ok())) {
        return refused(StatusCode::BAD_REQUEST, error);
    }
    if headers.get("content-type").and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next()).map(str::trim) != Some("application/json") {
        return refused(StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Error::new(-32005, "Content type not supported", "CONTENT_TYPE_NOT_SUPPORTED"));
    }
    let Ok(_slot) = SLOTS.try_acquire() else {
        return refused(StatusCode::TOO_MANY_REQUESTS, Error::new(-32000, "Server busy", "RESOURCE_EXHAUSTED"));
    };
    let bytes = match tokio::time::timeout(std::time::Duration::from_secs(10),
        axum::body::to_bytes(body, 64 * 1024)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return refused(StatusCode::PAYLOAD_TOO_LARGE, Error::invalid()),
        Err(_) => return refused(StatusCode::REQUEST_TIMEOUT, Error::invalid()),
    };
    let request: Value = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return refused(StatusCode::BAD_REQUEST, Error::new(-32700, "Invalid JSON payload", "PARSE_ERROR")),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return refused(StatusCode::BAD_REQUEST, Error::new(-32600, "Invalid request", "INVALID_REQUEST"));
    };
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !(id.is_string() || id.is_number()) || method.len() > 80 {
        return refused(StatusCode::BAD_REQUEST, Error::new(-32600, "Invalid request", "INVALID_REQUEST"));
    }
    let params = request.get("params").cloned().unwrap_or_else(|| serde_json::json!({}));
    if !params.is_object() { return response(StatusCode::OK, id, Err(Error::invalid())); }
    let operation = a2a::canonical_method(method);
    let frame = state.handler.handle(&format!("a2a.{operation}"), params, &ctx).await;
    match frame {
        WsFrame::Response { ok: true, payload: Some(mut value), .. } => {
            if operation == "GetExtendedAgentCard" {
                let Some(signer) = a2a_signer() else {
                    return response(StatusCode::SERVICE_UNAVAILABLE, id, Err(Error::unavailable()));
                };
                signer.sign_card(&mut value);
            }
            response(StatusCode::OK, id, Ok(value))
        }
        WsFrame::Response { error: Some(error), .. } if error.is_object() => {
            let status = match error["data"][0]["reason"].as_str() {
                Some("PERMISSION_DENIED") => StatusCode::FORBIDDEN,
                Some("RESOURCE_EXHAUSTED") => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::OK,
            };
            (status, [("a2a-version", "1.0"), ("cache-control", "no-store")],
                Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}))).into_response()
        }
        _ => response(StatusCode::FORBIDDEN, id, Err(Error::denied())),
    }
}

pub(super) async fn card(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> axum::response::Response {
    let config = match a2a::Config::from_home(&state.home_dir) {
        Ok(config) => config,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let ip = match a2a::proxy::client_ip(&config, peer.ip(), &headers) {
        Ok(ip) => ip,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !LIMITS.allow(Traffic::Discovery, ip).await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let card = match state.handler.a2a_card(None).await {
        Ok(card) => card,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    signed_card_response(card, a2a_signer())
}

fn signed_card_response(mut card: Value, signer: Option<&crate::a2a_signing::A2aSigner>) -> axum::response::Response {
    let Some(signer) = signer else { return StatusCode::SERVICE_UNAVAILABLE.into_response(); };
    signer.sign_card(&mut card);
    ([("cache-control", "no-store"), ("a2a-version", "1.0")], Json(card)).into_response()
}

#[cfg(test)]
mod tests;
