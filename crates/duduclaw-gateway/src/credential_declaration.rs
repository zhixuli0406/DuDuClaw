//! Signed, replay-protected employee declarations. Requests carry metadata only.
use std::{net::SocketAddr, path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};
use axum::{body::Bytes, extract::{ConnectInfo, DefaultBodyLimit, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::post, Router};
use serde::{Deserialize, Serialize};
use crate::computer_use_sessions::auth::{authenticate, check_peer, RateLimiter, ReplayGuard, SIGNATURE_HEADER, TIMESTAMP_HEADER};

const ROUTE: &str = "/api/internal/credentials/declare";
const DOMAIN: &str = "credential-declaration-response-v1";
const RESPONSE_HEADER: &str = "x-duduclaw-credential-signature";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    op: String,
    credential_id: String,
    service: String,
}

struct RouteState {
    home: PathBuf,
    rate: RateLimiter,
    replay: ReplayGuard,
    capacity: Arc<tokio::sync::Semaphore>,
}

pub fn router(home: PathBuf) -> Router {
    Router::new().route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(Arc::new(RouteState { home, rate: RateLimiter::default(), replay: ReplayGuard::default(), capacity: Arc::new(tokio::sync::Semaphore::new(8)) }))
}

async fn handle(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    if check_peer(peer).is_err() { return StatusCode::UNAUTHORIZED.into_response(); }
    let Ok(permit) = state.capacity.clone().try_acquire_owned() else { return StatusCode::SERVICE_UNAVAILABLE.into_response(); };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        handle_sync(&state, peer, &headers, &body)
    }).await.unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())
}

fn handle_sync(state: &RouteState, peer: SocketAddr, headers: &HeaderMap, body: &[u8]) -> Response {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|v| v.as_secs()).unwrap_or(0);
    let Ok(caller) = authenticate(&state.home, peer, headers, body, now) else { return StatusCode::UNAUTHORIZED.into_response(); };
    if !state.rate.allow(&caller.agent_id, Instant::now()) || !state.replay.record(&caller.nonce, Instant::now()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(request) = serde_json::from_slice::<Declaration>(body) else { return StatusCode::BAD_REQUEST.into_response(); };
    if request.op != "credential_declare" { return StatusCode::BAD_REQUEST.into_response(); }
    let Some(identity) = duduclaw_core::load_identity_key(&state.home) else { return StatusCode::UNAUTHORIZED.into_response(); };
    let token = duduclaw_core::mint_identity_token(&identity, &caller.agent_id);
    let timestamp = headers.get(TIMESTAMP_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let signature = headers.get(SIGNATURE_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(key) = crate::mcp_internal_key::valid_internal_keys(&state.home).into_iter().find(|key|
        duduclaw_core::verify_internal_request_signature(key.as_bytes(), &caller.agent_id, &token, timestamp, &caller.nonce, body, signature)
    ) else { return StatusCode::UNAUTHORIZED.into_response(); };
    let result = crate::credential_ledger::declare(&state.home, &caller.agent_id, &request.credential_id, &request.service);
    let value = match result {
        Ok(()) => serde_json::json!({"ok":true}),
        Err(_) => serde_json::json!({"error":"憑證宣告遭拒；請確認持有權限及管理員設定。"}),
    };
    let Ok(bytes) = serde_json::to_vec(&value) else { return StatusCode::INTERNAL_SERVER_ERROR.into_response(); };
    let signature = duduclaw_core::internal_request_signature(key.as_bytes(), &caller.agent_id, &token, DOMAIN, &caller.nonce, &bytes);
    ([("content-type", "application/json"), (RESPONSE_HEADER, signature.as_str())], bytes).into_response()
}

pub async fn declare(home: &Path, agent: &str, credential_id: &str, service: &str) -> Result<(), String> {
    let key = std::env::var(duduclaw_core::ENV_MCP_API_KEY).map_err(|_| "missing gateway identity")?;
    let token = std::env::var(duduclaw_core::ENV_AGENT_TOKEN).map_err(|_| "missing employee identity")?;
    if key.is_empty() || token.is_empty() || !duduclaw_core::is_valid_agent_id(agent) { return Err("invalid employee identity".into()); }
    let body = serde_json::to_vec(&Declaration { op: "credential_declare".into(), credential_id: credential_id.into(), service: service.into() }).map_err(|_| "invalid declaration")?;
    if body.len() > 8192 { return Err("declaration too large".into()); }
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|v| v.as_secs()).unwrap_or(0).to_string();
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    let signature = duduclaw_core::internal_request_signature(key.as_bytes(), agent, &token, &timestamp, &nonce, &body);
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(5)).build().map_err(|_| "client unavailable")?;
    let mut response = client.post(format!("http://127.0.0.1:{port}{ROUTE}"))
        .header("Content-Type", "application/json").header("X-Duduclaw-Agent-Id", agent)
        .header("X-Duduclaw-Timestamp", timestamp).header("X-Duduclaw-Nonce", &nonce)
        .header("X-Duduclaw-Signature", signature).body(body).send().await.map_err(|_| "gateway unreachable")?;
    if !response.status().is_success() { return Err("gateway refused declaration".into()); }
    let signature = response.headers().get(RESPONSE_HEADER).and_then(|v| v.to_str().ok()).ok_or("unsigned gateway response")?.to_owned();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "response unreadable")? {
        if bytes.len() + chunk.len() > 8192 { return Err("response too large".into()); }
        bytes.extend_from_slice(&chunk);
    }
    if !duduclaw_core::verify_internal_request_signature(key.as_bytes(), agent, &token, DOMAIN, &nonce, &bytes, &signature) {
        return Err("unverifiable gateway response".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "invalid response")?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) { return Err("憑證宣告遭拒；請確認持有權限及管理員設定。".into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // F5 regression coverage includes removed employee notes; not run.
    #[test]
    fn declaration_rejects_value_and_identity_fields() {
        for extra in ["value", "agent_id", "password", "token", "note"] {
            let mut value = serde_json::json!({"op":"credential_declare","credential_id":"cred_test","service":"crm"});
            value[extra] = serde_json::json!("must-not-be-accepted");
            assert!(serde_json::from_value::<Declaration>(value).is_err());
        }
    }
    #[test]
    fn declaration_authenticates_body_and_rejects_replay() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        let home = tempfile::tempdir().unwrap();
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let key = crate::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
        let token = duduclaw_core::mint_identity_token(&identity, "bot");
        let state = RouteState { home: home.path().into(), rate: RateLimiter::default(), replay: ReplayGuard::default(), capacity: Arc::new(tokio::sync::Semaphore::new(1)) };
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
        let body = br#"{"op":"wrong_operation","credential_id":"cred_test","service":"crm"}"#;
        let signature = duduclaw_core::internal_request_signature(key.as_bytes(), "bot", &token, &timestamp, &nonce, body);
        let mut headers = HeaderMap::new();
        for (name, value) in [(AGENT_ID_HEADER, "bot"), (TIMESTAMP_HEADER, timestamp.as_str()), (NONCE_HEADER, nonce.as_str()), (SIGNATURE_HEADER, signature.as_str())] {
            headers.insert(name, value.parse().unwrap());
        }
        let peer = "127.0.0.1:80".parse().unwrap();
        assert_eq!(handle_sync(&state, peer, &headers, b"{}").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(handle_sync(&state, peer, &headers, body).status(), StatusCode::BAD_REQUEST);
        assert_eq!(handle_sync(&state, peer, &headers, body).status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[test]
    fn declaration_refuses_unsigned_and_remote_requests() {
        let home = tempfile::tempdir().unwrap();
        let state = RouteState { home: home.path().into(), rate: RateLimiter::default(), replay: ReplayGuard::default(), capacity: Arc::new(tokio::sync::Semaphore::new(1)) };
        for peer in ["127.0.0.1:80", "192.0.2.1:80"] {
            assert_eq!(handle_sync(&state, peer.parse().unwrap(), &HeaderMap::new(), b"{}").status(), StatusCode::UNAUTHORIZED);
        }
    }
}

/// The gateway's tool loop can observe arguments without sending them to an audit endpoint.
pub(crate) struct ToolObservation {
    home: PathBuf,
    agent: String,
}
impl ToolObservation {
    pub(crate) fn new(home: &Path, agent: &str) -> Self { Self { home: home.into(), agent: agent.into() } }
}
#[async_trait::async_trait]
impl duduclaw_llm::ClientCallBoundary for ToolObservation {
    async fn before_call(&self, _tool: &str, args: &serde_json::Value) -> Result<(), String> {
        observe_arguments(&self.home, &self.agent, args).await;
        Ok(())
    }
    async fn after_success(&self, _tool: &str) -> Result<(), String> { Ok(()) }
}

pub(crate) async fn observe_arguments(home: &Path, agent: &str, args: &serde_json::Value) {
    let text = observation_text(args);
    if !text.is_empty() {
        crate::credential_ledger::observe_tool_bounded(home, agent, text).await;
    }
}

/// Preserve string bytes (especially PEM newlines) rather than fingerprinting
/// their JSON escapes. Labels retain contextual token detection without reading
/// arbitrary depth or materializing an unbounded argument document.
fn observation_text(args: &serde_json::Value) -> String {
    const MAX_BYTES: usize = 64 * 1024;
    const MAX_NODES: usize = 4096;
    const MAX_DEPTH: usize = 128;
    let mut text = String::new();
    let mut pending = vec![(None, args, 0usize)];
    let mut visited = 0usize;
    while let Some((label, value, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > MAX_DEPTH { break; }
        match value {
            serde_json::Value::String(value) => {
                let label: &str = label.unwrap_or("");
                let bytes = label.len().saturating_add(value.len()).saturating_add(3);
                if bytes > MAX_BYTES.saturating_sub(text.len()) { break; }
                text.push_str(label);
                text.push_str(": ");
                text.push_str(value);
                text.push('\n');
            }
            serde_json::Value::Array(values) => {
                let remaining = MAX_NODES.saturating_sub(visited + pending.len());
                pending.extend(values.iter().take(remaining).rev().map(|value| (label, value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                let remaining = MAX_NODES.saturating_sub(visited + pending.len());
                pending.extend(values.iter().take(remaining).map(|(key, value)| (Some(key.as_str()), value, depth + 1)));
            }
            _ => {}
        }
    }
    text
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn tool_observation_preserves_pem_and_token_string_bytes() {
        let pem = ["-----BEGIN PRIVATE KEY-----", "synthetic-body", "-----END PRIVATE KEY-----"].join("\n");
        let token = "synthetic-opaque-token-body";
        let text = observation_text(&serde_json::json!({"nested": [{"pem":pem,"token":token}]}));
        assert!(text.contains(&pem));
        assert!(text.contains(&format!("token: {token}\n")));
        assert!(!text.contains("\\n"));
    }

    #[test]
    fn tool_observation_bounds_total_string_bytes() {
        let args = serde_json::json!({"token":"密".repeat(70_000), "other":["x", "x", "x", "x"]});
        assert!(observation_text(&args).len() <= 64 * 1024);
        let many = serde_json::Value::Array((0..10_000).map(|_| serde_json::json!("small")).collect());
        assert!(observation_text(&many).lines().count() <= 4096);
    }
}
