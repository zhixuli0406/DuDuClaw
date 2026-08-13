//! OAuth 2.1 authorization surface for the remote MCP endpoint — WP3.1-T2.
//!
//! Makes `duduclaw http-server` connectable from OAuth-only MCP clients
//! (claude.ai custom connectors, Claude mobile). DuDuClaw is single-tenant
//! self-hosted, so this is deliberately the *smallest* spec-conforming
//! authorization server: the operator is the only "user", and consent is
//! proven by pasting an existing **internal** MCP API key.
//!
//! Spec surface (verified against the MCP 2025-06-18 authorization spec +
//! the RFCs it cites):
//!
//! - RFC 9728 Protected Resource Metadata: `GET /.well-known/oauth-protected-resource`
//!   (the MCP endpoint also stamps `WWW-Authenticate: Bearer resource_metadata=…`
//!   on its 401s — that's how clients discover this document)
//! - RFC 8414 AS Metadata: `GET /.well-known/oauth-authorization-server`
//! - RFC 7591 Dynamic Client Registration: `POST /oauth/register`
//!   (public clients only, `token_endpoint_auth_method: "none"`)
//! - Authorization endpoint: `GET /oauth/authorize` (code flow, PKCE S256
//!   REQUIRED) → consent page → `POST /oauth/decision` → 302 back with code
//! - Token endpoint: `POST /oauth/token` (`authorization_code` +
//!   `refresh_token` grants; refresh rotation)
//!
//! Security posture (fail closed everywhere):
//! - Issued principals are ALWAYS `is_external = true` — the C4 external
//!   tool predicate applies. Scopes are the intersection of what the client
//!   requested and [`EXTERNALLY_GRANTABLE_SCOPES`]; connector/execute/admin
//!   surfaces can never be minted over OAuth, no matter what was requested.
//! - The consent credential must be an *internal* key (an external key
//!   cannot escalate itself into more grants).
//! - Tokens/refresh tokens/codes are random 192-bit values; only SHA-256
//!   digests are stored at rest. Codes are single-use with a 10-minute TTL;
//!   access tokens live 1 hour; refresh tokens 30 days and rotate on use.
//! - `redirect_uri` must exactly match a registered value (string equality),
//!   and registration only accepts `https://…` or loopback `http://…` URIs.
//! - Client-supplied text (client_name) is HTML-escaped before rendering.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::mcp_auth::{AuthError, EXTERNALLY_GRANTABLE_SCOPES, Principal, parse_scopes};
use crate::mcp_http_server::HttpState;

const CODE_TTL_SECS: i64 = 600; // RFC 6749 recommends ≤10 min
const ACCESS_TOKEN_TTL_SECS: i64 = 3600;
const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;
const MAX_CLIENTS: usize = 200;
const MAX_REDIRECT_URIS: usize = 10;
const MAX_CLIENT_NAME_CHARS: usize = 100;

pub(crate) const STORE_FILE: &str = "mcp_oauth_issued.json";

// ── Store ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegisteredClient {
    client_name: String,
    redirect_uris: Vec<String>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    /// Space-joined scope tokens already filtered to the grantable set.
    scope: String,
    issued_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuedToken {
    client_id: String,
    client_name: String,
    scope: String,
    expires_unix: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OAuthStore {
    #[serde(default)]
    clients: HashMap<String, RegisteredClient>,
    /// Keyed by SHA-256 hex of the authorization code.
    #[serde(default)]
    codes: HashMap<String, PendingCode>,
    /// Keyed by SHA-256 hex of the access token.
    #[serde(default)]
    tokens: HashMap<String, IssuedToken>,
    /// Keyed by SHA-256 hex of the refresh token.
    #[serde(default)]
    refresh: HashMap<String, IssuedToken>,
}

fn store_path(home: &Path) -> PathBuf {
    home.join(STORE_FILE)
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn load_store(home: &Path) -> OAuthStore {
    let Ok(raw) = std::fs::read_to_string(store_path(home)) else {
        return OAuthStore::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Prune expired material, then persist atomically under the cross-process
/// advisory lock (project convention 3).
fn save_store(home: &Path, mut store: OAuthStore) -> std::io::Result<()> {
    let now = now_unix();
    store.codes.retain(|_, c| now - c.issued_unix <= CODE_TTL_SECS);
    store.tokens.retain(|_, t| t.expires_unix > now);
    store.refresh.retain(|_, t| t.expires_unix > now);
    let path = store_path(home);
    let json = serde_json::to_string_pretty(&store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    duduclaw_core::with_file_lock(&path, || {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path)
    })
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}

/// 192-bit random hex string (two v4 UUIDs' random payloads).
fn random_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

// ── Bearer validation hook (called from the auth path) ────────────────────────

/// Prefix distinguishing OAuth-issued access tokens from static `ddc_*` keys.
pub(crate) const OAUTH_TOKEN_PREFIX: &str = "ddc_oauth_";

/// Validate an OAuth-issued access token. Returns the external Principal it
/// was minted for, or an auth error (expired/unknown ⇒ the caller's 401).
pub(crate) fn validate_oauth_token(raw: &str, home: &Path) -> Result<Principal, AuthError> {
    let store = load_store(home);
    let entry = store
        .tokens
        .get(&sha256_hex(raw))
        .ok_or(AuthError::UnknownKey)?;
    if entry.expires_unix <= now_unix() {
        return Err(AuthError::UnknownKey);
    }
    let scopes = parse_scopes(&entry.scope.replace(' ', ",")).unwrap_or_default();
    Ok(Principal {
        // `oauth_` prefix (not `oauth:`) — namespace client-id charset
        // is [a-zA-Z0-9_-] and the colon would fail resolve() with a 500.
        client_id: format!("oauth_{}", entry.client_id),
        scopes,
        // OAuth tokens are for remote clients by definition — the C4
        // external predicate must always apply.
        is_external: true,
        created_at: chrono::Utc::now(),
    })
}

// ── Base-URL derivation ───────────────────────────────────────────────────────

/// Derive the externally-visible base URL from the request (tunnel-friendly:
/// honours `X-Forwarded-Proto`; falls back to plain http on loopback).
fn base_url(headers: &HeaderMap) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        .filter(|v| v == "http" || v == "https")
        .unwrap_or_else(|| "http".to_string());
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1")
        .trim()
        .to_string();
    format!("{proto}://{host}")
}

/// The `WWW-Authenticate` value the MCP endpoint's 401s must carry (RFC 9728).
pub(crate) fn www_authenticate_value(headers: &HeaderMap) -> String {
    format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
        base_url(headers)
    )
}

// ── Metadata endpoints ────────────────────────────────────────────────────────

fn grantable_scope_strings() -> Vec<String> {
    EXTERNALLY_GRANTABLE_SCOPES.iter().map(|s| s.to_string()).collect()
}

pub(crate) async fn protected_resource_metadata(headers: HeaderMap) -> Response {
    let base = base_url(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": grantable_scope_strings(),
    }))
    .into_response()
}

pub(crate) async fn authorization_server_metadata(headers: HeaderMap) -> Response {
    let base = base_url(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": grantable_scope_strings(),
    }))
    .into_response()
}

// ── Dynamic client registration (RFC 7591) ────────────────────────────────────

/// Anchored redirect-URI policy: `https://…` anywhere, plain `http://…` only
/// on loopback hosts (native-app callbacks). No custom schemes.
fn redirect_uri_acceptable(uri: &str) -> bool {
    if uri.len() > 2000 || uri.chars().any(|c| c.is_control()) {
        return false;
    }
    if uri.starts_with("https://") {
        return uri.len() > "https://".len();
    }
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let prefix = format!("http://{host}");
        if let Some(rest) = uri.strip_prefix(&prefix) {
            // Anchor the host: what follows must be a port, a path, or the
            // end — `http://localhost.evil.com/` must not pass.
            return rest.is_empty() || rest.starts_with(':') || rest.starts_with('/');
        }
    }
    false
}

fn sanitize_client_name(raw: Option<&str>) -> String {
    let name: String = raw
        .unwrap_or("Unnamed MCP client")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_NAME_CHARS)
        .collect();
    if name.trim().is_empty() {
        "Unnamed MCP client".to_string()
    } else {
        name
    }
}

pub(crate) async fn register_handler(
    State(state): State<HttpState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(uris) = body.get("redirect_uris").and_then(|v| v.as_array()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata",
            "redirect_uris is required");
    };
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata",
            "redirect_uris must contain 1..=10 entries");
    }
    let mut redirect_uris = Vec::new();
    for u in uris {
        match u.as_str() {
            Some(s) if redirect_uri_acceptable(s) => redirect_uris.push(s.to_string()),
            _ => {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri",
                    "redirect_uris must be https:// or loopback http:// URLs");
            }
        }
    }
    let client_name = sanitize_client_name(body.get("client_name").and_then(|v| v.as_str()));

    let mut store = load_store(&state.home_dir);
    if store.clients.len() >= MAX_CLIENTS {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata",
            "registration limit reached — prune mcp_oauth_issued.json");
    }
    let client_id = format!("mcp_{}", uuid::Uuid::new_v4().simple());
    store.clients.insert(
        client_id.clone(),
        RegisteredClient {
            client_name: client_name.clone(),
            redirect_uris: redirect_uris.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    );
    if save_store(&state.home_dir, store).is_err() {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error",
            "failed to persist registration");
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": client_name,
            "redirect_uris": redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    )
        .into_response()
}

// ── Authorization endpoint + consent ──────────────────────────────────────────

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Filter a space-delimited OAuth scope string down to the externally
/// grantable set (unknown scopes are dropped — OAuth allows narrowing).
fn filter_scope(requested: &str) -> String {
    let grantable = grantable_scope_strings();
    requested
        .split_whitespace()
        .filter(|s| grantable.iter().any(|g| g == s))
        .collect::<Vec<_>>()
        .join(" ")
}

fn html_error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><meta charset=\"utf-8\"><title>DuDuClaw OAuth</title>\
             <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\">\
             <h2>🐾 DuDuClaw</h2><p>{}</p></body>",
            escape_html(msg)
        )),
    )
        .into_response()
}

pub(crate) async fn authorize_handler(
    State(state): State<HttpState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let store = load_store(&state.home_dir);
    // Client + redirect_uri must validate BEFORE any redirect is possible
    // (never bounce a browser to an unverified URI).
    let Some(client_id) = q.get("client_id") else {
        return html_error(StatusCode::BAD_REQUEST, "missing client_id");
    };
    let Some(client) = store.clients.get(client_id) else {
        return html_error(StatusCode::BAD_REQUEST, "unknown client_id — register first via /oauth/register");
    };
    let Some(redirect_uri) = q.get("redirect_uri") else {
        return html_error(StatusCode::BAD_REQUEST, "missing redirect_uri");
    };
    if !client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return html_error(StatusCode::BAD_REQUEST, "redirect_uri is not registered for this client");
    }

    let state_param = q.get("state").cloned().unwrap_or_default();
    let bounce = |err: &str| {
        let sep = if redirect_uri.contains('?') { '&' } else { '?' };
        let mut url = format!("{redirect_uri}{sep}error={err}");
        if !state_param.is_empty() {
            url.push_str(&format!("&state={}", urlencoding::encode(&state_param)));
        }
        Redirect::to(&url).into_response()
    };

    if q.get("response_type").map(String::as_str) != Some("code") {
        return bounce("unsupported_response_type");
    }
    let challenge = q.get("code_challenge").cloned().unwrap_or_default();
    let challenge_ok = (43..=128).contains(&challenge.len())
        && challenge.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c));
    if !challenge_ok || q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        // PKCE S256 is REQUIRED (OAuth 2.1 / MCP spec).
        return bounce("invalid_request");
    }

    let scope = filter_scope(q.get("scope").map(String::as_str).unwrap_or(""));
    let scope_note = if scope.is_empty() {
        "基本工具面（7 個基礎工具）".to_string()
    } else {
        format!("基本工具面＋<code>{}</code>", escape_html(&scope))
    };

    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>DuDuClaw 授權</title>\
<body style=\"font-family:system-ui;max-width:34rem;margin:4rem auto;line-height:1.6\">\
<h2>🐾 DuDuClaw 連線授權</h2>\
<p>應用程式 <b>{name}</b> 想連接你的 DuDuClaw（remote MCP）。</p>\
<p>授權範圍：{scope_note}</p>\
<p>貼上一把<b>內部 MCP API key</b>（<code>config.toml [mcp_keys]</code> 中 \
<code>is_external = false</code> 的 key）以確認你是操作者本人：</p>\
<form method=\"post\" action=\"/oauth/decision\">\
<input type=\"hidden\" name=\"client_id\" value=\"{client_id}\">\
<input type=\"hidden\" name=\"redirect_uri\" value=\"{redirect_uri}\">\
<input type=\"hidden\" name=\"state\" value=\"{state_v}\">\
<input type=\"hidden\" name=\"code_challenge\" value=\"{challenge}\">\
<input type=\"hidden\" name=\"scope\" value=\"{scope}\">\
<p><input type=\"password\" name=\"operator_key\" placeholder=\"ddc_…\" \
style=\"width:100%;padding:.5rem\" autocomplete=\"off\" required></p>\
<p><button name=\"action\" value=\"approve\" style=\"padding:.5rem 1.5rem\">同意連線</button> \
<button name=\"action\" value=\"deny\" style=\"padding:.5rem 1.5rem\">拒絕</button></p>\
</form>\
<p style=\"color:#888;font-size:.85rem\">OAuth 簽發的存取權杖永遠是「外部客戶端」等級：\
只能使用基礎工具面與你在 key 簽發時明示授與的範圍，連接器／執行類／管理工具永不開放。</p>\
</body>",
        name = escape_html(&client.client_name),
        scope_note = scope_note,
        client_id = escape_html(client_id),
        redirect_uri = escape_html(redirect_uri),
        state_v = escape_html(&state_param),
        challenge = escape_html(&challenge),
        scope = escape_html(&scope),
    );
    Html(page).into_response()
}

#[derive(Deserialize)]
pub(crate) struct DecisionForm {
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: String,
    code_challenge: String,
    #[serde(default)]
    scope: String,
    operator_key: String,
    action: String,
}

pub(crate) async fn decision_handler(
    State(http_state): State<HttpState>,
    Form(f): Form<DecisionForm>,
) -> Response {
    let mut store = load_store(&http_state.home_dir);
    // Re-validate against the store — hidden form fields are client-supplied.
    let Some(client) = store.clients.get(&f.client_id) else {
        return html_error(StatusCode::BAD_REQUEST, "unknown client_id");
    };
    if !client.redirect_uris.iter().any(|u| u == &f.redirect_uri) {
        return html_error(StatusCode::BAD_REQUEST, "redirect_uri is not registered for this client");
    }
    let sep = if f.redirect_uri.contains('?') { '&' } else { '?' };
    let state_suffix = if f.state.is_empty() {
        String::new()
    } else {
        format!("&state={}", urlencoding::encode(&f.state))
    };

    if f.action != "approve" {
        return Redirect::to(&format!("{}{sep}error=access_denied{state_suffix}", f.redirect_uri))
            .into_response();
    }

    // Operator proof: a valid INTERNAL key. External keys cannot mint grants
    // (no self-escalation), and failures never redirect a code anywhere.
    match crate::mcp_auth::authenticate_with_key(&f.operator_key, &http_state.home_dir) {
        Ok(p) if !p.is_external => {}
        _ => {
            return html_error(
                StatusCode::UNAUTHORIZED,
                "operator key rejected — paste an internal (is_external = false) MCP API key",
            );
        }
    }

    let code = random_secret();
    store.codes.insert(
        sha256_hex(&code),
        PendingCode {
            client_id: f.client_id.clone(),
            redirect_uri: f.redirect_uri.clone(),
            code_challenge: f.code_challenge.clone(),
            scope: filter_scope(&f.scope),
            issued_unix: now_unix(),
        },
    );
    if save_store(&http_state.home_dir, store).is_err() {
        return html_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist grant");
    }
    Redirect::to(&format!(
        "{}{sep}code={}{state_suffix}",
        f.redirect_uri,
        urlencoding::encode(&code)
    ))
    .into_response()
}

// ── Token endpoint ────────────────────────────────────────────────────────────

fn oauth_error(status: StatusCode, code: &str, desc: &str) -> Response {
    let mut resp = (
        status,
        Json(json!({ "error": code, "error_description": desc })),
    )
        .into_response();
    resp.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
    resp
}

/// PKCE S256: base64url-nopad(SHA-256(verifier)) must equal the stored
/// challenge. Constant-time comparison (subtle) as belt-and-suspenders.
fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    use subtle::ConstantTimeEq;
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

fn mint_tokens(store: &mut OAuthStore, client_id: &str, client_name: &str, scope: &str) -> Value {
    let access = format!("{OAUTH_TOKEN_PREFIX}{}", random_secret());
    let refresh = format!("{OAUTH_TOKEN_PREFIX}r_{}", random_secret());
    let now = now_unix();
    store.tokens.insert(
        sha256_hex(&access),
        IssuedToken {
            client_id: client_id.to_string(),
            client_name: client_name.to_string(),
            scope: scope.to_string(),
            expires_unix: now + ACCESS_TOKEN_TTL_SECS,
        },
    );
    store.refresh.insert(
        sha256_hex(&refresh),
        IssuedToken {
            client_id: client_id.to_string(),
            client_name: client_name.to_string(),
            scope: scope.to_string(),
            expires_unix: now + REFRESH_TOKEN_TTL_SECS,
        },
    );
    json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TOKEN_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope,
    })
}

pub(crate) async fn token_handler(
    State(state): State<HttpState>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let grant_type = f.get("grant_type").map(String::as_str).unwrap_or("");
    let mut store = load_store(&state.home_dir);
    match grant_type {
        "authorization_code" => {
            let (Some(code), Some(client_id), Some(redirect_uri), Some(verifier)) = (
                f.get("code"),
                f.get("client_id"),
                f.get("redirect_uri"),
                f.get("code_verifier"),
            ) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request",
                    "code, client_id, redirect_uri and code_verifier are required");
            };
            // Single-use: remove up front so a failed exchange still burns it.
            let Some(pending) = store.codes.remove(&sha256_hex(code)) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "unknown or reused code");
            };
            let _ = save_store(&state.home_dir, store.clone());
            if now_unix() - pending.issued_unix > CODE_TTL_SECS
                || &pending.client_id != client_id
                || &pending.redirect_uri != redirect_uri
                || !pkce_matches(verifier, &pending.code_challenge)
            {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant",
                    "code expired or does not match this client/redirect_uri/PKCE verifier");
            }
            let client_name = store
                .clients
                .get(client_id)
                .map(|c| c.client_name.clone())
                .unwrap_or_else(|| "Unnamed MCP client".to_string());
            let body = mint_tokens(&mut store, client_id, &client_name, &pending.scope);
            if save_store(&state.home_dir, store).is_err() {
                return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error",
                    "failed to persist tokens");
            }
            let mut resp = Json(body).into_response();
            resp.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
            resp
        }
        "refresh_token" => {
            let (Some(raw), Some(client_id)) = (f.get("refresh_token"), f.get("client_id")) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request",
                    "refresh_token and client_id are required");
            };
            let key = sha256_hex(raw);
            let Some(entry) = store.refresh.remove(&key) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "unknown refresh token");
            };
            if entry.expires_unix <= now_unix() || &entry.client_id != client_id {
                let _ = save_store(&state.home_dir, store);
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant",
                    "refresh token expired or client mismatch");
            }
            // Rotation: old refresh token is already removed; mint a new pair.
            let body = mint_tokens(&mut store, &entry.client_id, &entry.client_name, &entry.scope);
            if save_store(&state.home_dir, store).is_err() {
                return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error",
                    "failed to persist tokens");
            }
            let mut resp = Json(body).into_response();
            resp.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
            resp
        }
        other => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            &format!("unsupported grant_type '{other}'"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_policy_is_anchored_and_fail_closed() {
        assert!(redirect_uri_acceptable("https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_uri_acceptable("http://localhost:33418/callback"));
        assert!(redirect_uri_acceptable("http://127.0.0.1/cb"));
        assert!(redirect_uri_acceptable("http://[::1]:8080/cb"));
        // Anchoring: loopback lookalikes and other schemes are refused.
        assert!(!redirect_uri_acceptable("http://localhost.evil.com/cb"));
        assert!(!redirect_uri_acceptable("http://evil.com/cb"));
        assert!(!redirect_uri_acceptable("myapp://callback"));
        assert!(!redirect_uri_acceptable("https://"));
        assert!(!redirect_uri_acceptable("javascript:alert(1)"));
    }

    #[test]
    fn scope_filter_narrows_to_grantable_only() {
        assert_eq!(filter_scope("memory:read admin odoo:execute wiki:read"), "memory:read wiki:read");
        assert_eq!(filter_scope(""), "");
        assert_eq!(filter_scope("bogus"), "");
    }

    #[test]
    fn pkce_s256_roundtrip() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert!(pkce_matches(verifier, &challenge));
        assert!(!pkce_matches("wrong-verifier-wrong-verifier-wrong-verifier", &challenge));
    }

    #[test]
    fn issued_tokens_validate_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OAuthStore::default();
        let body = mint_tokens(&mut store, "mcp_abc", "Test Client", "memory:read");
        save_store(dir.path(), store).unwrap();
        let access = body["access_token"].as_str().unwrap();

        let p = validate_oauth_token(access, dir.path()).unwrap();
        assert!(p.is_external, "OAuth principals must be external");
        assert_eq!(p.client_id, "oauth_mcp_abc");
        assert!(p.scopes.contains(&crate::mcp_auth::Scope::MemoryRead));

        // Unknown token → rejected.
        assert!(validate_oauth_token("ddc_oauth_deadbeef", dir.path()).is_err());

        // Expired token → rejected.
        let mut store = load_store(dir.path());
        for t in store.tokens.values_mut() {
            t.expires_unix = now_unix() - 10;
        }
        // Bypass save_store's pruning to keep the expired row on disk.
        std::fs::write(
            store_path(dir.path()),
            serde_json::to_string(&store).unwrap(),
        )
        .unwrap();
        assert!(validate_oauth_token(access, dir.path()).is_err());
    }

    #[test]
    fn client_name_is_sanitized_and_escaped() {
        assert_eq!(sanitize_client_name(Some("Evil\u{7}<script>")), "Evil<script>");
        assert_eq!(escape_html("Evil<script>"), "Evil&lt;script&gt;");
        assert_eq!(sanitize_client_name(None), "Unnamed MCP client");
        assert_eq!(sanitize_client_name(Some("   ")), "Unnamed MCP client");
    }
}
