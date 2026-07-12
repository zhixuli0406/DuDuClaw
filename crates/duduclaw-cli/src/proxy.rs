// proxy.rs — local OpenAI-compatible reverse-proxy (G2 Part B).
//
// `duduclaw proxy --bind 127.0.0.1:PORT` turns the DuDuClaw account pool into a
// local OpenAI-compatible endpoint so external tools (Aider / Cline / Codex …)
// can borrow the subscription / API-key quota the rotator manages.
//
//   POST /v1/chat/completions   — OpenAI chat-completions (streaming + buffered)
//   GET  /v1/models             — vendored model catalogue
//   GET  /healthz               — health check (no auth)
//
// Flow: request → Bearer auth → rate limit → resolve (provider, model) from the
// `model` field → `AccountRotator::select_for_provider` → forward through the
// unified `duduclaw-llm` provider layer → OpenAI-compat response.
//
// Fail-closed: no usable account ⇒ 503 with an explicit zh-TW reason (never a
// silent empty completion). Defaults to loopback; a Bearer proxy key is always
// required (generated + printed at startup if none is configured).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use duduclaw_agent::account_rotator::{create_from_config, AccountRotator, AuthMethod};
use duduclaw_llm::providers::build_provider;
use duduclaw_llm::{
    split_model_id, ApiAuth, ChatMessage, ChatRequest, ChatResponse, ContentPart, LlmError,
    ModelRegistry, Role, StopReason, StreamEvent, SystemBlock,
};

use crate::auth_device::{self, CopilotTokenCache};
use crate::mcp_rate_limit::{OpType, RateLimiter};

// ── Pure conversions (fully unit-tested) ─────────────────────────────────────

/// Resolve a request `model` field into `(provider, bare_model)`.
///
/// A `provider/model` prefix wins (`"anthropic/claude-sonnet-5"` →
/// `("anthropic", "claude-sonnet-5")`); a bare id falls back to
/// `default_provider` (`"gpt-4o"` with default `"openai"` → `("openai", "gpt-4o")`).
pub fn resolve_provider_and_model(model: &str, default_provider: &str) -> (String, String) {
    match split_model_id(model) {
        (Some(p), bare) => (p.to_string(), bare.to_string()),
        (None, bare) => (default_provider.to_string(), bare.to_string()),
    }
}

/// Extract flat text from an OpenAI `content` value (string, or an array of
/// `{type,text}` parts). Non-text parts (images) are ignored in v1.
fn extract_text_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Whether the client asked for a streamed response.
pub fn wants_stream(body: &Value) -> bool {
    body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// The `model` field, if present and non-empty.
pub fn request_model(body: &Value) -> Option<&str> {
    body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// Convert an OpenAI chat-completions body into a normalized [`ChatRequest`],
/// using `bare_model` as the (provider-prefix-stripped) model id.
///
/// `system` / `developer` messages become cached-hint-free system blocks;
/// `user` / `assistant` become typed messages; `tool` messages map to a
/// `ToolResult` part on a synthetic user turn. Returns `Err` when there are no
/// non-system messages to answer.
pub fn openai_to_chat_request(body: &Value, bare_model: &str) -> Result<ChatRequest, String> {
    let mut system: Vec<SystemBlock> = Vec::new();
    let mut messages: Vec<ChatMessage> = Vec::new();

    for m in body
        .get("messages")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let text = extract_text_content(m.get("content").unwrap_or(&Value::Null));
        match role {
            "system" | "developer" => system.push(SystemBlock::uncached(text)),
            "assistant" => messages.push(ChatMessage::assistant(text)),
            "tool" => {
                let call_id = m
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                messages.push(ChatMessage {
                    role: Role::User,
                    parts: vec![ContentPart::ToolResult {
                        call_id,
                        content: text,
                        is_error: false,
                    }],
                });
            }
            _ => messages.push(ChatMessage::user(text)),
        }
    }

    if messages.is_empty() {
        return Err("請求缺少可回應的 messages（至少需要一則非 system 訊息）".to_string());
    }

    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .filter(|&n| n > 0)
        .unwrap_or(4096);
    let temperature = body
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map(|t| t as f32);

    let mut req = ChatRequest::new(bare_model);
    req.system = system;
    req.messages = messages;
    req.max_tokens = max_tokens;
    req.temperature = temperature;
    Ok(req)
}

/// OpenAI `finish_reason` string for a normalized stop reason.
pub fn finish_reason_str(stop: &StopReason) -> &'static str {
    match stop {
        StopReason::EndTurn => "stop",
        StopReason::ToolUse => "tool_calls",
        StopReason::MaxTokens => "length",
        StopReason::ContentFilter | StopReason::Refusal => "content_filter",
        StopReason::Other(_) => "stop",
    }
}

/// OpenAI `tool_calls` array from the tool-call parts of a response.
fn tool_calls_json(resp: &ChatResponse) -> Vec<Value> {
    resp.parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolCall { id, name, args } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": args.to_string(),
                },
            })),
            _ => None,
        })
        .collect()
}

/// Build a buffered `chat.completion` response object.
pub fn chat_response_to_openai(
    resp: &ChatResponse,
    model_echo: &str,
    id: &str,
    created: i64,
) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": resp.text(),
    });
    let tool_calls = tool_calls_json(resp);
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let prompt = resp.usage.input_tokens + resp.usage.cache_read_tokens + resp.usage.cache_write_tokens;
    let completion = resp.usage.output_tokens + resp.usage.reasoning_tokens;
    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model_echo,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason_str(&resp.stop),
        }],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
        },
    })
}

/// A streaming text delta chunk (`chat.completion.chunk`).
pub fn stream_text_chunk(delta: &str, model_echo: &str, id: &str, created: i64) -> Value {
    json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model_echo,
        "choices": [{
            "index": 0,
            "delta": { "content": delta },
            "finish_reason": Value::Null,
        }],
    })
}

/// The terminal streaming chunk carrying any tool calls + the finish reason.
pub fn stream_finish_chunk(
    resp: &ChatResponse,
    model_echo: &str,
    id: &str,
    created: i64,
) -> Value {
    let mut delta = json!({});
    let tool_calls = tool_calls_json(resp);
    if !tool_calls.is_empty() {
        delta["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model_echo,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason_str(&resp.stop),
        }],
    })
}

/// The `GET /v1/models` catalogue from a registry.
pub fn models_list_json(registry: &ModelRegistry, created: i64) -> Value {
    let data: Vec<Value> = registry
        .models()
        .map(|m| {
            json!({
                "id": m.qualified_id(),
                "object": "model",
                "created": created,
                "owned_by": m.provider,
            })
        })
        .collect();
    json!({ "object": "list", "data": data })
}

/// Constant-time Bearer-token check against the configured proxy key.
///
/// Returns `false` for a missing/malformed header or a mismatched token.
/// The compare is length-safe and constant-time (`subtle::ConstantTimeEq`),
/// never short-circuiting on the first differing byte.
pub fn check_proxy_auth(auth_header: Option<&str>, expected: &str) -> bool {
    let Some(header) = auth_header else { return false };
    let token = match header.strip_prefix("Bearer ").or_else(|| header.strip_prefix("bearer ")) {
        Some(t) => t.trim(),
        None => return false,
    };
    let a = token.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        // ConstantTimeEq requires equal lengths; unequal length ⇒ reject, but
        // still run a dummy compare to keep timing uniform.
        let _ = b.ct_eq(b);
        return false;
    }
    a.ct_eq(b).into()
}

// ── Server state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ProxyState {
    rotator: Arc<AccountRotator>,
    registry: Arc<ModelRegistry>,
    proxy_key: Arc<String>,
    default_provider: Arc<String>,
    limiter: Arc<RateLimiter>,
    /// Shared HTTP client for subscription-seat forwarding (Copilot / Qwen).
    http: Arc<reqwest::Client>,
    /// Minted short-lived Copilot tokens, refreshed on demand.
    copilot_cache: Arc<CopilotTokenCache>,
    /// DuDuClaw home — needed to persist rotated Qwen seat credentials.
    home: Arc<std::path::PathBuf>,
}

/// Provider ids that forward through a subscription seat (not the `duduclaw-llm`
/// API layer). A model tagged with one of these is served from a stored OAuth
/// seat credential; absent a seat the model is never advertised (fail-closed).
fn is_seat_provider(provider: &str) -> bool {
    matches!(provider, "github" | "qwen")
}

/// Whether the *selected account* should forward through the seat path.
///
/// Only an **OAuth** account of a seat provider is a seat. An API-key account
/// for the same provider (e.g. a qwen account holding a DashScope API key)
/// must take the normal `duduclaw-llm` OpenAI-compat path — previously the
/// provider-only check hijacked it into the seat branch and 503'd on the
/// missing seat credential.
pub fn takes_seat_path(provider: &str, auth_method: &AuthMethod) -> bool {
    is_seat_provider(provider) && *auth_method == AuthMethod::OAuth
}

// ── Small response helpers ───────────────────────────────────────────────────

fn err_json(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "message": message, "type": "duduclaw_proxy" } })),
    )
        .into_response()
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn new_completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

/// Map an upstream `LlmError` to (HTTP status, zh-TW message) and record the
/// outcome on the rotator so cooldowns/health track real failures.
async fn on_upstream_error(
    rotator: &AccountRotator,
    account_id: &str,
    err: &LlmError,
) -> (StatusCode, String) {
    match err {
        LlmError::RateLimited { .. } => {
            rotator.on_rate_limited(account_id).await;
            (StatusCode::TOO_MANY_REQUESTS, "上游供應商限流（rate limited），已冷卻此帳號".to_string())
        }
        LlmError::Billing => {
            rotator.on_billing_exhausted(account_id).await;
            (StatusCode::TOO_MANY_REQUESTS, "上游帳號額度用盡（billing），已標記 24h 冷卻".to_string())
        }
        LlmError::Auth => {
            rotator.on_error(account_id).await;
            (StatusCode::BAD_GATEWAY, "上游驗證失敗（金鑰無效或過期）".to_string())
        }
        LlmError::ContextWindowExceeded => {
            (StatusCode::BAD_REQUEST, "請求超出模型上下文視窗".to_string())
        }
        LlmError::InvalidRequest(m) => {
            (StatusCode::BAD_REQUEST, format!("請求格式錯誤：{m}"))
        }
        other => {
            rotator.on_error(account_id).await;
            (StatusCode::BAD_GATEWAY, format!("上游轉發失敗：{other}"))
        }
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

fn authorized(headers: &HeaderMap, key: &str) -> bool {
    let header = headers.get("Authorization").and_then(|v| v.to_str().ok());
    check_proxy_auth(header, key)
}

/// Per-client rate-limit bucket key, derived from the client socket IP.
///
/// The default bind is loopback (every client keys to `proxy:127.0.0.1`,
/// behaving like the old single bucket), but a LAN-exposed proxy gets one
/// bucket per client IP so one abusive client can't starve the rest.
pub fn proxy_client_bucket_key(ip: &std::net::IpAddr) -> String {
    format!("proxy:{ip}")
}

/// Rate-limit gate shared by both endpoints, applied BEFORE auth so an
/// unauthenticated flood can't probe the pool: per-client-IP bucket first
/// (60 req/min each), then the legacy global `proxy` bucket as a total-cap
/// backstop across all clients.
fn rate_limited(limiter: &RateLimiter, addr: &SocketAddr) -> Option<Response> {
    if limiter
        .check(&proxy_client_bucket_key(&addr.ip()), OpType::HttpRequest)
        .is_err()
    {
        return Some(err_json(
            StatusCode::TOO_MANY_REQUESTS,
            "proxy 請求頻率過高（此用戶端每分鐘上限 60）",
        ));
    }
    if limiter.check("proxy", OpType::HttpRequest).is_err() {
        return Some(err_json(
            StatusCode::TOO_MANY_REQUESTS,
            "proxy 請求頻率過高（全域每分鐘上限 60）",
        ));
    }
    None
}

/// Provider-tagged seat model entries, appended to the catalogue only when a
/// live seat for that provider exists. `seat_providers` is the set of provider
/// ids with an available stored seat; models for any other provider are never
/// emitted (fail-closed — no seat ⇒ no models ⇒ the client 404s that model).
pub fn seat_models_json(seat_providers: &[&str], created: i64) -> Vec<Value> {
    let mut out = Vec::new();
    for provider in seat_providers {
        for id in auth_device::seat_model_ids(provider) {
            out.push(json!({
                "id": format!("{provider}/{id}"),
                "object": "model",
                "created": created,
                "owned_by": *provider,
            }));
        }
    }
    out
}

async fn models_handler(
    State(st): State<ProxyState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    // Rate-limit before auth so an unauthenticated flood can't probe the pool.
    if let Some(resp) = rate_limited(&st.limiter, &addr) {
        return resp;
    }
    if !authorized(&headers, &st.proxy_key) {
        return err_json(StatusCode::UNAUTHORIZED, "缺少或無效的 Bearer proxy key");
    }
    let created = now_ts();
    let mut catalogue = models_list_json(&st.registry, created);
    // Append seat models only for providers that currently have a live seat.
    let mut seat_providers: Vec<&str> = Vec::new();
    for provider in ["github", "qwen"] {
        if st.rotator.has_seat_for_provider(provider).await {
            seat_providers.push(provider);
        }
    }
    if let Some(data) = catalogue.get_mut("data").and_then(|d| d.as_array_mut()) {
        data.extend(seat_models_json(&seat_providers, created));
    }
    Json(catalogue).into_response()
}

async fn completions_handler(
    State(st): State<ProxyState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // 1. Rate limit BEFORE auth (per-client-IP bucket + global backstop), so
    //    an unauthenticated flood can't burn CPU on constant-time key checks.
    if let Some(resp) = rate_limited(&st.limiter, &addr) {
        return resp;
    }
    // 2. Auth (fail-closed).
    if !authorized(&headers, &st.proxy_key) {
        return err_json(StatusCode::UNAUTHORIZED, "缺少或無效的 Bearer proxy key");
    }
    // 3. Resolve provider + model.
    let Some(model) = request_model(&body) else {
        return err_json(StatusCode::BAD_REQUEST, "請求缺少 model 欄位");
    };
    let (provider, bare) = resolve_provider_and_model(model, &st.default_provider);

    // 4. Select an account from the rotator's pool for this provider.
    let Some(sel) = st.rotator.select_for_provider(&provider).await else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("provider `{provider}` 無可用帳號（帳號池為空或全部冷卻中）"),
        );
    };

    // 4b. Subscription-seat forwarding (Copilot / Qwen). A seat carries a stored
    //     OAuth credential on `seat_token` (never a raw API key). We exchange it
    //     for a short-lived upstream token and forward directly to the seat's
    //     native OpenAI-compatible endpoint (bypassing the duduclaw-llm layer).
    //     Only OAuth accounts are seats — an API-key qwen account (DashScope
    //     key) falls through to the normal OpenAI-compat path below.
    if takes_seat_path(&provider, &sel.auth_method) {
        let Some(seat_token) = sel.seat_token.clone() else {
            return err_json(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!(
                    "provider `{provider}` 座位缺少憑證（請先執行 `duduclaw auth device --provider {}`）",
                    if provider == "github" { "copilot" } else { "qwen" }
                ),
            );
        };
        return forward_seat(&st, &provider, &bare, &sel.id, &seat_token, &body).await;
    }

    // 5. Forwarding needs a usable API key. Subscription OAuth seats (Codex /
    //    Copilot / Qwen) have no raw key — forwarding them through the API layer
    //    is PENDING-LIVE (needs a CLI-runtime bridge), so fail-closed here.
    let Some(key) = sel.raw_key.clone() else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!(
                "選定帳號 `{}`（{}）為訂閱制 OAuth seat，proxy 轉發需 API key 帳號（OAuth 轉發為 PENDING-LIVE）",
                sel.id, sel.provider
            ),
        );
    };
    // 6. Build the upstream provider client.
    let Some(client) = build_provider(&provider, ApiAuth::new(key)) else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("未知的 provider `{provider}`（無對應轉發後端）"),
        );
    };
    // 7. Normalize the request.
    let req = match openai_to_chat_request(&body, &bare) {
        Ok(r) => r,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, &e),
    };

    let account_id = sel.id.clone();
    let model_echo = model.to_string();
    let id = new_completion_id();
    let created = now_ts();

    if wants_stream(&body) {
        let upstream = match client.stream(&req).await {
            Ok(s) => s,
            Err(e) => {
                let (status, msg) = on_upstream_error(&st.rotator, &account_id, &e).await;
                return err_json(status, &msg);
            }
        };
        // Optimistic success accounting: the stream was accepted upstream.
        st.rotator.on_success(&account_id, 0).await;

        // Buffered providers (OpenAI Responses / Gemini) stream no text deltas —
        // their whole answer arrives inside the terminal Done event. For those we
        // materialize a text chunk from `Done`; real-SSE providers (Anthropic /
        // openai-compat) already delivered the text via TextDelta, so we must NOT
        // re-emit it there or the client sees the answer twice.
        let buffered = matches!(provider.as_str(), "openai" | "gemini" | "google");
        let m1 = model_echo.clone();
        let id1 = id.clone();
        let sse = upstream.flat_map(move |item| {
            let m = m1.clone();
            let id = id1.clone();
            let events: Vec<Result<Event, std::convert::Infallible>> = match item {
                Ok(StreamEvent::TextDelta(t)) => vec![Ok(Event::default()
                    .data(stream_text_chunk(&t, &m, &id, created).to_string()))],
                Ok(StreamEvent::Done(resp)) => {
                    let mut evs = Vec::new();
                    if buffered {
                        let text = resp.text();
                        if !text.is_empty() {
                            evs.push(Ok(Event::default()
                                .data(stream_text_chunk(&text, &m, &id, created).to_string())));
                        }
                    }
                    evs.push(Ok(Event::default()
                        .data(stream_finish_chunk(&resp, &m, &id, created).to_string())));
                    evs
                }
                // Reasoning + partial tool-call deltas are dropped in v1;
                // completed tool calls surface in the Done finish chunk.
                Ok(_) => vec![],
                // An upstream mid-stream error: emit a terminal error chunk.
                Err(e) => vec![Ok(Event::default()
                    .data(json!({ "error": { "message": format!("{e}") } }).to_string()))],
            };
            futures_util::stream::iter(events)
        });
        // Terminate with the OpenAI `[DONE]` sentinel.
        let done = futures_util::stream::once(async {
            Ok::<Event, std::convert::Infallible>(Event::default().data("[DONE]"))
        });
        let body_stream = sse.chain(done);
        return Sse::new(body_stream)
            .keep_alive(KeepAlive::default())
            .into_response();
    }

    // Buffered path.
    match client.complete(&req).await {
        Ok(resp) => {
            st.rotator.on_success(&account_id, 0).await;
            Json(chat_response_to_openai(&resp, &model_echo, &id, created)).into_response()
        }
        Err(e) => {
            let (status, msg) = on_upstream_error(&st.rotator, &account_id, &e).await;
            err_json(status, &msg)
        }
    }
}

// ── Subscription-seat forwarding ─────────────────────────────────────────────

/// Rewrite the request body's `model` field to the provider-prefix-stripped
/// bare id, so the upstream sees its own native model name (`github/gpt-4o` →
/// `gpt-4o`). Returns a new JSON value; never mutates the caller's copy.
pub fn body_with_bare_model(body: &Value, bare: &str) -> Value {
    let mut out = body.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("model".to_string(), Value::String(bare.to_string()));
    }
    out
}

// ── Qwen seat refresh (expiry-aware; flow itself is PENDING-LIVE) ────────────

/// Refresh when within this many seconds of `expires_at` (clock-skew margin).
const QWEN_REFRESH_SKEW_SECS: u64 = 60;

fn now_unix() -> u64 {
    now_ts().max(0) as u64
}

/// Whether a stored Qwen seat bundle's access token is expired (or expiring
/// within the skew window). A bundle without `expires_at` (legacy shape) is
/// treated as still-valid — same behavior as before refresh support existed.
pub fn qwen_bundle_expired(bundle: &Value, now_unix: u64) -> bool {
    match bundle.get("expires_at").and_then(|v| v.as_u64()) {
        Some(exp) => now_unix.saturating_add(QWEN_REFRESH_SKEW_SECS) >= exp,
        None => false,
    }
}

/// Form body for the RFC 6749 §6 refresh-token grant (public PKCE client —
/// no client secret, matching qwen-code's refresh call).
pub fn qwen_refresh_form(refresh_token: &str, client_id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", client_id.to_string()),
    ]
}

/// Merge a refresh-token response into a new persisted seat bundle.
///
/// The refresh response may omit `refresh_token` (no rotation) and
/// `resource_url`; those fall back to the old bundle's values. A missing
/// `access_token` is an error (fail-closed — never persist a broken bundle).
pub fn merge_refreshed_qwen_bundle(
    old: &Value,
    tok: &Value,
    now_unix: u64,
) -> std::result::Result<Value, String> {
    let access = tok
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("refresh 回應缺少 access_token")?;
    let pick = |field: &str| -> String {
        tok.get(field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| old.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
            .unwrap_or("")
            .to_string()
    };
    let expires_at = tok
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .map(|secs| now_unix.saturating_add(secs));
    Ok(json!({
        "access_token": access,
        "refresh_token": pick("refresh_token"),
        "resource_url": pick("resource_url"),
        "expires_at": expires_at,
    }))
}

/// POST the refresh-token grant to `token_url` and merge the response into a
/// new seat bundle. One retry on any failure, then fail-closed (Err). The
/// URL is a parameter so tests can point it at a local mock server; the
/// production caller passes [`auth_device::QWEN`]'s `token_url` + client id.
pub async fn refresh_qwen_bundle_at(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    bundle: &Value,
) -> std::result::Result<Value, String> {
    let refresh_token = bundle
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("憑證缺少 refresh_token，無法刷新")?;
    let form = qwen_refresh_form(refresh_token, client_id);

    let mut last_err = String::new();
    for attempt in 0..2u8 {
        match http.post(token_url).form(&form).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                if status.is_success() {
                    match merge_refreshed_qwen_bundle(bundle, &body, now_unix()) {
                        Ok(nb) => return Ok(nb),
                        Err(e) => last_err = e,
                    }
                } else {
                    let err_desc = body
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown_error");
                    last_err = format!("token endpoint 回應 {status}（{err_desc}）");
                }
            }
            Err(e) => last_err = format!("token endpoint 連線失敗：{e}"),
        }
        if attempt == 0 {
            warn!(error = %last_err, "Qwen seat refresh failed — retrying once");
        }
    }
    Err(last_err)
}

/// Persist a rotated Qwen seat credential (encrypted-only) back onto the
/// matching `[[accounts]]` row in `config.toml`. Atomic temp+rename write.
async fn persist_qwen_seat_credential(
    home: &Path,
    account_id: &str,
    credential: &str,
) -> std::result::Result<(), String> {
    let config_path = home.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|e| format!("讀取 config.toml 失敗：{e}"))?;
    let mut table: toml::Table = content
        .parse()
        .map_err(|e| format!("config.toml 解析失敗：{e}"))?;

    let encrypted = crate::encrypt_api_key(credential, &home.to_path_buf())
        .ok_or("無法加密 seat 憑證（keyfile 產生失敗？）")?;

    let mut found = false;
    if let Some(arr) = table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
        for acc in arr.iter_mut() {
            if let Some(t) = acc.as_table_mut() {
                if t.get("id").and_then(|v| v.as_str()) == Some(account_id) {
                    // Encrypted-only: never persist the plaintext credential.
                    t.remove("oauth_token");
                    t.insert("oauth_token_enc".into(), toml::Value::String(encrypted.clone()));
                    found = true;
                }
            }
        }
    }
    if !found {
        return Err(format!("config.toml 中找不到帳號 `{account_id}`"));
    }

    let serialized = toml::to_string_pretty(&table)
        .map_err(|e| format!("config.toml 序列化失敗：{e}"))?;
    let tmp = config_path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, serialized)
        .await
        .map_err(|e| format!("寫入暫存檔失敗：{e}"))?;
    tokio::fs::rename(&tmp, &config_path)
        .await
        .map_err(|e| format!("原子替換 config.toml 失敗：{e}"))?;
    Ok(())
}

/// Forward a request through a stored subscription seat to its native
/// OpenAI-compatible endpoint. Fail-closed: any credential/mint failure returns
/// an explicit error status — it never falls back to another provider.
async fn forward_seat(
    st: &ProxyState,
    provider: &str,
    bare: &str,
    account_id: &str,
    seat_token: &str,
    body: &Value,
) -> Response {
    let stream = wants_stream(body);
    let fwd_body = body_with_bare_model(body, bare);

    // Resolve (url, headers) per provider.
    let (url, headers): (String, Vec<(&'static str, String)>) = match provider {
        "github" => {
            // Mint (or reuse a cached) short-lived Copilot token from the stored
            // long-lived GitHub OAuth token.
            match st.copilot_cache.get_or_mint(&st.http, seat_token).await {
                Ok(copilot_token) => (
                    auth_device::copilot_completions_url(),
                    auth_device::copilot_chat_headers(&copilot_token),
                ),
                Err(e) => {
                    st.rotator.on_error(account_id).await;
                    return err_json(
                        StatusCode::BAD_GATEWAY,
                        &format!("Copilot token 取得失敗：{e}"),
                    );
                }
            }
        }
        "qwen" => {
            // PENDING-LIVE: the Qwen seat credential is a JSON bundle. We parse
            // the access token + resource_url from it. Live verification is
            // blocked (Qwen free OAuth discontinued 2026-04-15).
            let mut bundle: Value = serde_json::from_str(seat_token).unwrap_or(Value::Null);

            // Expiry-aware refresh: an expired access token with a stored
            // refresh_token is refreshed via the token endpoint (one retry,
            // then fail-closed). The rotated bundle is persisted encrypted
            // AND swapped into the rotator's in-memory seat so subsequent
            // requests don't re-refresh with an already-rotated (revoked)
            // refresh token.
            if qwen_bundle_expired(&bundle, now_unix()) {
                match refresh_qwen_bundle_at(
                    &st.http,
                    auth_device::QWEN.token_url,
                    auth_device::QWEN.default_client_id,
                    &bundle,
                )
                .await
                {
                    Ok(new_bundle) => {
                        let credential = new_bundle.to_string();
                        if let Err(e) =
                            persist_qwen_seat_credential(&st.home, account_id, &credential).await
                        {
                            warn!(account = %account_id, error = %e,
                                "Qwen seat refreshed but persisting the rotated credential failed");
                        }
                        st.rotator.update_seat_token(account_id, &credential).await;
                        bundle = new_bundle;
                    }
                    Err(e) => {
                        st.rotator.on_error(account_id).await;
                        return err_json(
                            StatusCode::SERVICE_UNAVAILABLE,
                            &format!(
                                "Qwen seat token 已過期且刷新失敗：{e}。請重新登入：`duduclaw auth device --provider qwen`"
                            ),
                        );
                    }
                }
            }

            let access = bundle
                .get("access_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let resource = bundle
                .get("resource_url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            match (access, resource) {
                (Some(access), Some(resource)) => {
                    let base = resource.trim_end_matches('/');
                    // qwen-code builds the OpenAI-compatible base as
                    // `https://{resource_url}/v1`; accept an already-qualified
                    // URL too.
                    let url = if base.starts_with("http") {
                        format!("{base}/chat/completions")
                    } else {
                        format!("https://{base}/v1/chat/completions")
                    };
                    (
                        url,
                        vec![
                            ("Authorization", format!("Bearer {access}")),
                            ("Content-Type", "application/json".to_string()),
                        ],
                    )
                }
                _ => {
                    return err_json(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Qwen 座位憑證不完整（缺 access_token 或 resource_url）— 此路徑為 PENDING-LIVE",
                    );
                }
            }
        }
        other => {
            return err_json(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("provider `{other}` 非座位轉發後端"),
            );
        }
    };

    // Build + send the upstream request.
    let mut req = st.http.post(&url).json(&fwd_body);
    for (name, value) in &headers {
        req = req.header(*name, value);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            st.rotator.on_error(account_id).await;
            return err_json(StatusCode::BAD_GATEWAY, &format!("座位上游轉發失敗：{e}"));
        }
    };

    let status = resp.status();
    let axum_status =
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    if !status.is_success() {
        // Record the failure on the rotator; classify auth/limit for cooldown.
        if status.as_u16() == 401 || status.as_u16() == 403 {
            st.rotator.on_error(account_id).await;
        } else if status.as_u16() == 429 {
            st.rotator.on_rate_limited(account_id).await;
        }
        let text = resp.text().await.unwrap_or_default();
        let snippet = duduclaw_core::truncate_chars(&text, 240);
        return err_json(axum_status, &format!("座位上游回應 {status}：{snippet}"));
    }

    st.rotator.on_success(account_id, 0).await;

    if stream {
        // True SSE passthrough: stream upstream bytes verbatim.
        let byte_stream = resp.bytes_stream();
        Response::builder()
            .status(axum_status)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(Body::from_stream(byte_stream))
            .unwrap_or_else(|_| err_json(StatusCode::BAD_GATEWAY, "無法建構串流回應"))
    } else {
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return err_json(StatusCode::BAD_GATEWAY, &format!("讀取座位回應失敗：{e}"))
            }
        };
        Response::builder()
            .status(axum_status)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| err_json(StatusCode::BAD_GATEWAY, "無法建構回應"))
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Resolve the Bearer proxy key: CLI flag → `DUDUCLAW_PROXY_KEY` env →
/// `config.toml [proxy] key`. If none is set, a random key is generated and
/// printed (the proxy always requires a Bearer token — fail-closed).
fn resolve_proxy_key(cli_key: Option<String>, config: &toml::Table) -> String {
    if let Some(k) = cli_key.filter(|s| !s.is_empty()) {
        return k;
    }
    if let Ok(k) = std::env::var("DUDUCLAW_PROXY_KEY") {
        if !k.is_empty() {
            return k;
        }
    }
    if let Some(k) = config
        .get("proxy")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("key"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return k.to_string();
    }
    // Generate one and surface it so the operator can wire external tools.
    use rand::Rng;
    let raw: [u8; 24] = rand::thread_rng().r#gen();
    let key = format!("ddk-proxy-{}", hex::encode(raw));
    println!("⚠ 未設定 proxy key，已產生臨時金鑰（重啟即失效）：\n    {key}");
    println!("  設定固定金鑰：config.toml [proxy] key，或環境變數 DUDUCLAW_PROXY_KEY");
    key
}

/// Run the local reverse-proxy server.
pub async fn run(
    bind: &str,
    cli_key: Option<String>,
    default_provider: Option<String>,
) -> duduclaw_core::error::Result<()> {
    use duduclaw_core::error::DuDuClawError;

    let home = crate::duduclaw_home();
    let bind_addr: SocketAddr = bind
        .parse()
        .map_err(|e| DuDuClawError::Gateway(format!("無效的 bind 位址 '{bind}'：{e}")))?;
    if !bind_addr.ip().is_loopback() {
        warn!(bind = %bind_addr, "proxy 綁定非 loopback 位址 — 帳號池將對外網暴露，請確保有防火牆保護");
    }

    // Load config + build the rotator from its rotation settings.
    let config: toml::Table = tokio::fs::read_to_string(home.join("config.toml"))
        .await
        .unwrap_or_default()
        .parse()
        .unwrap_or_default();
    let rotator = create_from_config(&config);
    let loaded = rotator
        .load_from_config(&home)
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("帳號池載入失敗：{e}")))?;

    let default_provider = default_provider
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anthropic".to_string());
    let proxy_key = resolve_proxy_key(cli_key, &config);

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| DuDuClawError::Gateway(format!("HTTP client 建立失敗：{e}")))?;

    let state = ProxyState {
        rotator: Arc::new(rotator),
        registry: Arc::new(ModelRegistry::vendored()),
        proxy_key: Arc::new(proxy_key),
        default_provider: Arc::new(default_provider.clone()),
        limiter: Arc::new(RateLimiter::new()),
        http: Arc::new(http),
        copilot_cache: Arc::new(CopilotTokenCache::new()),
        home: Arc::new(home.clone()),
    };

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(models_handler))
        .route("/v1/chat/completions", post(completions_handler))
        .with_state(state);

    info!(
        bind = %bind_addr,
        accounts = loaded,
        default_provider = %default_provider,
        "DuDuClaw 本地 proxy 啟動（OpenAI-compat endpoint）"
    );
    println!("DuDuClaw proxy listening on http://{bind_addr}  (accounts={loaded}, default_provider={default_provider})");
    println!("  POST http://{bind_addr}/v1/chat/completions");
    println!("  GET  http://{bind_addr}/v1/models");

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("無法綁定 {bind_addr}：{e}")))?;
    // ConnectInfo is required for per-client-IP rate-limit buckets.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| DuDuClawError::Gateway(format!("proxy 伺服器錯誤：{e}")))?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_llm::NormalizedUsage;

    #[test]
    fn resolve_provider_prefix_wins_else_default() {
        assert_eq!(
            resolve_provider_and_model("anthropic/claude-sonnet-5", "openai"),
            ("anthropic".to_string(), "claude-sonnet-5".to_string())
        );
        assert_eq!(
            resolve_provider_and_model("gpt-4o", "openai"),
            ("openai".to_string(), "gpt-4o".to_string())
        );
        // Compat preset id as prefix.
        assert_eq!(
            resolve_provider_and_model("deepseek/deepseek-chat", "anthropic"),
            ("deepseek".to_string(), "deepseek-chat".to_string())
        );
    }

    #[test]
    fn openai_request_maps_roles_and_params() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": "you are helpful" },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" },
                { "role": "user", "content": [ {"type":"text","text":"世界"} ] }
            ],
            "max_tokens": 256,
            "temperature": 0.5
        });
        let req = openai_to_chat_request(&body, "gpt-4o").unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.max_tokens, 256);
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.system[0].text, "you are helpful");
        // user, assistant, user(array) → 3 messages.
        assert_eq!(req.messages.len(), 3);
        // The array-content user message extracts "世界".
        match &req.messages[2].parts[0] {
            ContentPart::Text(t) => assert_eq!(t, "世界"),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn openai_request_defaults_max_tokens_and_requires_messages() {
        let body = json!({ "model": "m", "messages": [ {"role":"user","content":"x"} ] });
        let req = openai_to_chat_request(&body, "m").unwrap();
        assert_eq!(req.max_tokens, 4096);
        assert_eq!(req.temperature, None);

        // System-only (no answerable message) → error, not a silent empty request.
        let sys_only = json!({ "model": "m", "messages": [ {"role":"system","content":"s"} ] });
        assert!(openai_to_chat_request(&sys_only, "m").is_err());
    }

    #[test]
    fn tool_role_becomes_tool_result() {
        let body = json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": "call it" },
                { "role": "tool", "tool_call_id": "call_1", "content": "42" }
            ]
        });
        let req = openai_to_chat_request(&body, "m").unwrap();
        assert_eq!(req.messages.len(), 2);
        match &req.messages[1].parts[0] {
            ContentPart::ToolResult { call_id, content, is_error } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(content, "42");
                assert!(!is_error);
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    fn sample_response() -> ChatResponse {
        ChatResponse {
            parts: vec![
                ContentPart::Text("hello ".into()),
                ContentPart::ToolCall {
                    id: "t1".into(),
                    name: "search".into(),
                    args: json!({ "q": "rust" }),
                },
                ContentPart::Text("world".into()),
            ],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 2,
                ..Default::default()
            },
            model_used: "m".into(),
            provider: "openai".into(),
        }
    }

    #[test]
    fn buffered_response_shape_and_usage() {
        let resp = sample_response();
        let v = chat_response_to_openai(&resp, "gpt-4o", "chatcmpl-x", 123);
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["choices"][0]["message"]["content"], "hello world");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        // tool_calls carried through, arguments serialized as a string.
        let tc = &v["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "search");
        assert_eq!(tc["function"]["arguments"], "{\"q\":\"rust\"}");
        // usage: prompt = input(10)+cache_read(2) = 12; completion = 5; total = 17.
        assert_eq!(v["usage"]["prompt_tokens"], 12);
        assert_eq!(v["usage"]["completion_tokens"], 5);
        assert_eq!(v["usage"]["total_tokens"], 17);
    }

    #[test]
    fn stream_chunks_shape() {
        let text = stream_text_chunk("hi", "gpt-4o", "id1", 1);
        assert_eq!(text["object"], "chat.completion.chunk");
        assert_eq!(text["choices"][0]["delta"]["content"], "hi");
        assert!(text["choices"][0]["finish_reason"].is_null());

        let resp = sample_response();
        let fin = stream_finish_chunk(&resp, "gpt-4o", "id1", 1);
        assert_eq!(fin["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(fin["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "search");
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(finish_reason_str(&StopReason::EndTurn), "stop");
        assert_eq!(finish_reason_str(&StopReason::ToolUse), "tool_calls");
        assert_eq!(finish_reason_str(&StopReason::MaxTokens), "length");
        assert_eq!(finish_reason_str(&StopReason::Refusal), "content_filter");
        assert_eq!(finish_reason_str(&StopReason::Other("x".into())), "stop");
    }

    #[test]
    fn models_list_is_openai_shaped() {
        let reg = ModelRegistry::vendored();
        let v = models_list_json(&reg, 7);
        assert_eq!(v["object"], "list");
        let data = v["data"].as_array().unwrap();
        assert!(!data.is_empty(), "vendored registry should list models");
        assert_eq!(data[0]["object"], "model");
        assert!(data[0]["id"].as_str().unwrap().contains('/'));
    }

    #[test]
    fn proxy_auth_accepts_exact_bearer_only() {
        assert!(check_proxy_auth(Some("Bearer secret-key"), "secret-key"));
        assert!(check_proxy_auth(Some("bearer secret-key"), "secret-key"));
        // Wrong token.
        assert!(!check_proxy_auth(Some("Bearer wrong"), "secret-key"));
        // Missing "Bearer " prefix.
        assert!(!check_proxy_auth(Some("secret-key"), "secret-key"));
        // No header at all.
        assert!(!check_proxy_auth(None, "secret-key"));
        // Length mismatch is rejected (no panic in ct_eq).
        assert!(!check_proxy_auth(Some("Bearer short"), "a-much-longer-key"));
    }

    #[test]
    fn seat_provider_classification() {
        assert!(is_seat_provider("github"));
        assert!(is_seat_provider("qwen"));
        assert!(!is_seat_provider("anthropic"));
        assert!(!is_seat_provider("openai"));
    }

    /// HIGH-D regression: only an OAuth account of a seat provider routes
    /// through the seat path. A qwen account holding a DashScope API key must
    /// take the normal OpenAI-compat path — previously the provider-only check
    /// hijacked it into the seat branch and it 503'd on the missing seat token.
    #[test]
    fn seat_path_requires_oauth_account() {
        // OAuth seats route through the seat path.
        assert!(takes_seat_path("qwen", &AuthMethod::OAuth));
        assert!(takes_seat_path("github", &AuthMethod::OAuth));
        // API-key accounts of the same providers take the API layer.
        assert!(!takes_seat_path("qwen", &AuthMethod::ApiKey));
        assert!(!takes_seat_path("github", &AuthMethod::ApiKey));
        // Non-seat providers never take the seat path, whatever the auth.
        assert!(!takes_seat_path("anthropic", &AuthMethod::OAuth));
        assert!(!takes_seat_path("openai", &AuthMethod::ApiKey));
        // And the API layer knows the qwen preset, so the API-key path is
        // actually servable (not a silent dead end).
        assert!(build_provider("qwen", ApiAuth::new("sk-dashscope")).is_some());
    }

    #[test]
    fn body_with_bare_model_rewrites_model_field_only() {
        let body = json!({
            "model": "github/gpt-4o",
            "messages": [ {"role":"user","content":"hi"} ],
            "stream": true
        });
        let out = body_with_bare_model(&body, "gpt-4o");
        assert_eq!(out["model"], "gpt-4o");
        // Other fields are preserved; the caller's copy is untouched.
        assert_eq!(out["stream"], true);
        assert_eq!(out["messages"][0]["content"], "hi");
        assert_eq!(body["model"], "github/gpt-4o", "input must not be mutated");
    }

    #[test]
    fn seat_models_are_fail_closed_on_absent_seat() {
        // No seat providers → no seat models at all.
        assert!(seat_models_json(&[], 1).is_empty());
        // A live github seat → github-tagged models appear, qwen does not.
        let with_github = seat_models_json(&["github"], 1);
        assert!(!with_github.is_empty());
        assert!(with_github
            .iter()
            .all(|m| m["id"].as_str().unwrap().starts_with("github/")));
        assert!(with_github
            .iter()
            .all(|m| m["owned_by"] == "github"));
    }

    #[test]
    fn proxy_rate_limit_bursts_to_429() {
        // Mirrors the handler path: HttpRequest bucket is 60/min. The 61st
        // check for the same client id must be rejected (→ 429 in the handler).
        let limiter = RateLimiter::new();
        for _ in 0..60 {
            assert!(limiter.check("proxy", OpType::HttpRequest).is_ok());
        }
        assert!(
            limiter.check("proxy", OpType::HttpRequest).is_err(),
            "burst beyond 60 req/min must trip the token bucket (proxy returns 429)"
        );
    }

    #[test]
    fn per_client_bucket_keys_are_ip_scoped() {
        use std::net::IpAddr;
        let a: IpAddr = "192.168.1.10".parse().unwrap();
        let b: IpAddr = "192.168.1.11".parse().unwrap();
        let v6: IpAddr = "::1".parse().unwrap();
        assert_eq!(proxy_client_bucket_key(&a), "proxy:192.168.1.10");
        assert_eq!(proxy_client_bucket_key(&v6), "proxy:::1");
        // Distinct clients → distinct buckets; never colliding with the
        // global backstop key "proxy".
        assert_ne!(proxy_client_bucket_key(&a), proxy_client_bucket_key(&b));
        assert_ne!(proxy_client_bucket_key(&a), "proxy");
    }

    #[test]
    fn per_client_buckets_isolate_clients_and_global_backstop_caps_total() {
        use std::net::SocketAddr;
        let limiter = RateLimiter::new();
        let attacker: SocketAddr = "192.168.1.66:50000".parse().unwrap();
        let friend: SocketAddr = "192.168.1.7:50001".parse().unwrap();

        // The attacker exhausts THEIR per-client bucket (60/min)…
        for _ in 0..60 {
            assert!(rate_limited(&limiter, &attacker).is_none());
        }
        assert!(
            rate_limited(&limiter, &attacker).is_some(),
            "61st request from the same IP must be limited"
        );
        // …while a different client is blocked only by the global backstop
        // (already drained by the attacker's 60 accepted requests) — its
        // per-client bucket is untouched.
        assert!(limiter
            .check(&proxy_client_bucket_key(&friend.ip()), OpType::HttpRequest)
            .is_ok());
        assert!(
            rate_limited(&limiter, &friend).is_some(),
            "global total-cap backstop still applies across clients"
        );
    }

    // ── Qwen seat refresh plumbing (flow itself is PENDING-LIVE) ─────────────

    fn qwen_bundle(access: &str, refresh: &str, expires_at: Option<u64>) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "resource_url": "portal.qwen.ai",
            "expires_at": expires_at,
        })
    }

    #[test]
    fn qwen_expiry_detection_with_skew_and_legacy_bundles() {
        let now = 1_000_000u64;
        // Fresh token (expires well past the skew window) → not expired.
        assert!(!qwen_bundle_expired(&qwen_bundle("a", "r", Some(now + 3600)), now));
        // Hard-expired → expired.
        assert!(qwen_bundle_expired(&qwen_bundle("a", "r", Some(now - 1)), now));
        // Inside the 60s skew window → refresh proactively.
        assert!(qwen_bundle_expired(&qwen_bundle("a", "r", Some(now + 30)), now));
        // Legacy bundle without expires_at → treated as valid (old behavior).
        assert!(!qwen_bundle_expired(&json!({"access_token":"a"}), now));
        assert!(!qwen_bundle_expired(&Value::Null, now));
    }

    #[test]
    fn qwen_refresh_form_is_rfc6749_public_client() {
        let form = qwen_refresh_form("rt-1", "client-1");
        assert!(form.contains(&("grant_type", "refresh_token".to_string())));
        assert!(form.contains(&("refresh_token", "rt-1".to_string())));
        assert!(form.contains(&("client_id", "client-1".to_string())));
        assert_eq!(form.len(), 3, "public PKCE client: no client_secret");
    }

    #[test]
    fn merged_bundle_rotates_and_falls_back() {
        let old = qwen_bundle("old-at", "old-rt", Some(1));
        // Full rotation: everything replaced, expires_at recomputed.
        let tok = json!({ "access_token": "new-at", "refresh_token": "new-rt", "expires_in": 600 });
        let nb = merge_refreshed_qwen_bundle(&old, &tok, 1000).unwrap();
        assert_eq!(nb["access_token"], "new-at");
        assert_eq!(nb["refresh_token"], "new-rt");
        assert_eq!(nb["resource_url"], "portal.qwen.ai", "resource_url falls back to old");
        assert_eq!(nb["expires_at"], 1600);
        // No refresh_token in response → keep the old one (no rotation).
        let tok2 = json!({ "access_token": "at2", "expires_in": 60 });
        let nb2 = merge_refreshed_qwen_bundle(&old, &tok2, 0).unwrap();
        assert_eq!(nb2["refresh_token"], "old-rt");
        // Missing access_token → fail-closed.
        assert!(merge_refreshed_qwen_bundle(&old, &json!({"expires_in": 60}), 0).is_err());
    }

    /// Spin up a local mock token endpoint and drive the real refresh path.
    async fn mock_token_endpoint(
        responses: Vec<(u16, Value)>,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::post;
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_h = hits.clone();
        let responses = Arc::new(responses);
        let app = axum::Router::new().route(
            "/token",
            post(move || {
                let hits = hits_h.clone();
                let responses = responses.clone();
                async move {
                    let i = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (code, body) = responses
                        .get(i.min(responses.len().saturating_sub(1)))
                        .cloned()
                        .unwrap_or((500, json!({"error":"exhausted"})));
                    (
                        StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                        Json(body),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/token"), hits)
    }

    #[tokio::test]
    async fn qwen_refresh_succeeds_against_mock_endpoint() {
        let (url, hits) = mock_token_endpoint(vec![(
            200,
            json!({ "access_token": "fresh-at", "refresh_token": "fresh-rt", "expires_in": 3600 }),
        )])
        .await;
        let http = reqwest::Client::new();
        let old = qwen_bundle("stale-at", "old-rt", Some(1));
        let nb = refresh_qwen_bundle_at(&http, &url, "cid", &old).await.unwrap();
        assert_eq!(nb["access_token"], "fresh-at");
        assert_eq!(nb["refresh_token"], "fresh-rt");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "no retry on success");
    }

    #[tokio::test]
    async fn qwen_refresh_retries_once_then_fails_closed() {
        let (url, hits) = mock_token_endpoint(vec![
            (500, json!({"error":"server_error"})),
            (500, json!({"error":"server_error"})),
        ])
        .await;
        let http = reqwest::Client::new();
        let old = qwen_bundle("stale-at", "old-rt", Some(1));
        let err = refresh_qwen_bundle_at(&http, &url, "cid", &old).await.unwrap_err();
        assert!(err.contains("server_error"), "error carries upstream reason: {err}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one retry, then fail-closed"
        );
    }

    #[tokio::test]
    async fn qwen_refresh_without_refresh_token_fails_closed() {
        // No endpoint should even be contacted.
        let http = reqwest::Client::new();
        let old = json!({ "access_token": "stale", "expires_at": 1 });
        let err = refresh_qwen_bundle_at(&http, "http://127.0.0.1:1/token", "cid", &old)
            .await
            .unwrap_err();
        assert!(err.contains("refresh_token"), "{err}");
    }

    #[tokio::test]
    async fn persist_rotated_credential_is_encrypted_only() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            r#"
[[accounts]]
id = "qwen-seat"
type = "oauth"
provider = "qwen"
oauth_token = "PLAINTEXT-LEGACY"
"#,
        )
        .unwrap();
        persist_qwen_seat_credential(home.path(), "qwen-seat", "{\"access_token\":\"new\"}")
            .await
            .unwrap();
        let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
        let table: toml::Table = raw.parse().unwrap();
        let acc = table["accounts"].as_array().unwrap()[0].as_table().unwrap();
        assert!(acc.contains_key("oauth_token_enc"), "rotated token stored encrypted");
        assert!(!acc.contains_key("oauth_token"), "plaintext legacy copy removed");
        assert!(!raw.contains("new"), "credential must not appear in plaintext");

        // Unknown account id → explicit error (fail-closed, no silent no-op).
        assert!(
            persist_qwen_seat_credential(home.path(), "nope", "{}").await.is_err()
        );
    }

    #[test]
    fn resolve_proxy_key_precedence() {
        let empty = toml::Table::new();
        // CLI flag wins.
        assert_eq!(
            resolve_proxy_key(Some("cli-key".to_string()), &empty),
            "cli-key"
        );
        // Config key used when no CLI/env.
        let cfg: toml::Table = "[proxy]\nkey = \"cfg-key\"\n".parse().unwrap();
        assert_eq!(resolve_proxy_key(None, &cfg), "cfg-key");
    }
}
