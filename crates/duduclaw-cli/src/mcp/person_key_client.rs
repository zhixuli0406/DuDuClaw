//! Live gateway authority for member MCP credentials. No validity cache, no
//! bearer persistence, no redirects/proxies, and no caller-selected identity.
use std::path::Path;
use std::time::Duration;
use serde::Deserialize;
use serde_json::{Value, json};
use crate::mcp_auth::{AuthError, AuthenticatedCall, Principal, PrincipalKind};

const UNAVAILABLE: &str = "本人金鑰驗證服務無法使用，請稍後再試。";
const MALFORMED: &str = "本人金鑰驗證服務的回應格式不正確。";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Identity {
    key_id: String,
    user_id: String,
    scopes: Vec<String>,
}

fn identity(value: Value) -> Result<AuthenticatedCall, AuthError> {
    let identity: Identity = serde_json::from_value(value).map_err(|_| AuthError::UnknownKey)?;
    if identity.key_id.is_empty() || identity.user_id.is_empty() {
        return Err(AuthError::UnknownKey);
    }
    let scopes = identity.scopes.iter().map(|scope| {
        if !duduclaw_core::mcp_scopes::PERSON_KEY_MCP_SCOPE_STRINGS.contains(&scope.as_str()) {
            return Err(AuthError::UnknownKey);
        }
        crate::mcp_auth::parse_scopes(scope)?.into_iter().next().ok_or(AuthError::UnknownKey)
    }).collect::<Result<_, _>>()?;
    Ok(AuthenticatedCall::without_reference(Principal {
        client_id: format!("person:{}", identity.user_id), scopes, is_external: true,
        created_at: chrono::Utc::now(),
    }).with_kind(PrincipalKind::PersonKey { key_id: identity.key_id, user_id: identity.user_id }))
}

pub(crate) async fn authenticate(home: &Path, raw: &str) -> Result<AuthenticatedCall, AuthError> {
    let body = json!({"op":"authenticate"});
    let result = tokio::time::timeout(AUTH_TIMEOUT, post_with_timeout(home, raw, &body, AUTH_TIMEOUT))
        .await.map_err(|_| AuthError::UnknownKey)?
        .map_err(|_| AuthError::UnknownKey)?;
    Ok(identity(result)?.with_person_key(raw.to_owned()))
}

/// Compatibility for synchronous callers. Async transports call `authenticate` directly.
/// Keep reqwest async: the workspace does not enable its blocking client.
pub(crate) fn authenticate_blocking(home: &Path, raw: &str) -> Result<AuthenticatedCall, AuthError> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|_| AuthError::StoreUnavailable)?.block_on(authenticate(home, raw))
}

pub(crate) async fn post(home: &Path, bearer: &str, body: &Value) -> Result<Value, String> {
    post_with_timeout(home, bearer, body, Duration::from_secs(30)).await
}

async fn post_with_timeout(home: &Path, bearer: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never())
        // The total deadline includes connecting and reading the response body.
        .connect_timeout(Duration::from_secs(2)).timeout(timeout)
        .build().map_err(|_| UNAVAILABLE)?;
    let bytes = serde_json::to_vec(body).map_err(|_| MALFORMED)?;
    if bytes.len() > 64 * 1024 { return Err("人工具請求過大。".into()); }
    let mut response = client.post(format!("http://127.0.0.1:{port}/api/internal/person-key"))
        .bearer_auth(bearer).header("content-type", "application/json").body(bytes)
        .send().await.map_err(|_| UNAVAILABLE)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE { return Err(MALFORMED.into()); }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| MALFORMED)?;
    if status.as_u16() == 200 && value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(value);
    }
    // Never leak credentials even if an upstream accidentally echoes one.
    let message = value.get("message").and_then(Value::as_str).unwrap_or(UNAVAILABLE);
    let message = message.replace(bearer, "[REDACTED]");
    Err(duduclaw_core::truncate_chars(&crate::mcp_redact::redact(&message), 500))
}

#[cfg(test)]
#[path = "person_key_client_tests.rs"]
pub(crate) mod tests;
