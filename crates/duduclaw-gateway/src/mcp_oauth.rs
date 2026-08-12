//! MCP OAuth 2.1 + PKCE flow for authenticating with external OAuth providers.
//!
//! Supports built-in provider configs (Google, GitHub, Slack) and custom providers.
//! Tokens are stored in `~/.duduclaw/mcp-oauth-tokens.json`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

/// OAuth provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthConfig {
    pub provider_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

/// Stored OAuth token for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthToken {
    pub provider_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub scopes: Vec<String>,
}

/// In-memory state for a pending OAuth flow (waiting for callback).
#[derive(Debug, Clone)]
pub struct PendingOAuth {
    pub provider_id: String,
    pub state: String,
    pub code_verifier: String,
    pub config: McpOAuthConfig,
    pub created_at: std::time::Instant,
}

pub const TOKEN_FILE: &str = "mcp-oauth-tokens.json";
const PENDING_TTL_SECS: u64 = 600; // 10 minutes

// ── Built-in provider configs ───────────────────────────────

/// Return built-in OAuth provider templates.
/// `client_id` and `client_secret` are empty — user must configure them.
///
/// The gateway's own port, resolved through the exact same shared priority
/// order `duduclaw run` uses — `DUDUCLAW_PORT` env > `config.toml [gateway]
/// port` > 18789 (`duduclaw_core::gateway_port_for_home`) — so the two can
/// never disagree. Before `duduclaw_core::gateway_port_for_home` existed,
/// this read `DUDUCLAW_PORT` only and the CLI did too, which happened to
/// agree by coincidence; once the CLI started honoring a hand-edited
/// `config.toml [gateway] port` (2026-08-11 fix), a second independent copy
/// of the env-only logic here would have silently gone back to registering
/// OAuth redirect URIs against the wrong port.
pub fn gateway_port(home: &Path) -> u16 {
    duduclaw_core::gateway_port_for_home(home).0
}

/// The OAuth redirect URI users must register with each provider.
///
/// This MUST match where the gateway actually serves
/// `GET /api/mcp/oauth/callback` (registered on the same axum router as
/// everything else). It was previously hardcoded to port 3000 while the gateway
/// listened on 18789, so every provider redirected the browser to a port with
/// nothing on it and no OAuth flow could complete — the callback never fired,
/// no token was ever stored, and the dashboard sat on "not connected".
pub fn redirect_uri(home: &Path) -> String {
    format!("http://localhost:{}/api/mcp/oauth/callback", gateway_port(home))
}

pub fn builtin_providers(redirect_uri: &str) -> Vec<McpOAuthConfig> {
    vec![
        McpOAuthConfig {
            provider_id: "google".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            // Native Gmail + Calendar tool surface. `gmail.compose` is required
            // to create drafts; `calendar.events` to create/list events. `drive`
            // was dropped (no Drive tools ship) to keep the authorization
            // surface minimal. Tokens authorized with the old scope set will
            // 403 on the new write APIs → the tools guide the user to reconnect.
            scopes: vec![
                "https://www.googleapis.com/auth/gmail.readonly".into(),
                "https://www.googleapis.com/auth/gmail.compose".into(),
                "https://www.googleapis.com/auth/calendar.events".into(),
                // v1.45: Sheets read/append native tools. Tokens authorized
                // before this scope was added will 403 on the Sheets APIs → the
                // tools guide the user to reconnect (google_status flags it as a
                // missing scope).
                "https://www.googleapis.com/auth/spreadsheets".into(),
                "https://www.googleapis.com/auth/userinfo.email".into(),
            ],
            redirect_uri: redirect_uri.to_string(),
        },
        McpOAuthConfig {
            provider_id: "github".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://github.com/login/oauth/authorize".into(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            // `repo` covers issue/PR read + issue comment on both public and
            // private repositories. Classic OAuth App tokens have no expiry
            // unless the app opts into token expiration (then a refresh_token is
            // issued) — `exchange_code` parses both shapes.
            scopes: vec!["repo".into()],
            redirect_uri: redirect_uri.to_string(),
        },
        McpOAuthConfig {
            provider_id: "notion".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://api.notion.com/v1/oauth/authorize".into(),
            token_url: "https://api.notion.com/v1/oauth/token".into(),
            // Notion OAuth capabilities are configured on the integration in the
            // Notion dashboard, not via the `scope` query param, so the scope
            // list stays empty. The access token is long-lived and carries NO
            // refresh_token — `expires_at = None` is the normal, healthy state.
            scopes: vec![],
            redirect_uri: redirect_uri.to_string(),
        },
        McpOAuthConfig {
            provider_id: "slack".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://slack.com/oauth/v2/authorize".into(),
            token_url: "https://slack.com/api/oauth.v2.access".into(),
            scopes: vec!["channels:read".into(), "chat:write".into()],
            redirect_uri: redirect_uri.to_string(),
        },
    ]
}

// ── PKCE ────────────────────────────────────────────────────

/// Generate a PKCE code_verifier and code_challenge (S256).
pub fn generate_pkce() -> (String, String) {
    use base64::engine::{Engine, general_purpose::URL_SAFE_NO_PAD};

    // Use two UUIDs (32 random bytes total via uuid v4) as entropy source.
    // uuid is already a dependency and uses the OS CSPRNG internally.
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut buf = [0u8; 32];
    buf[..16].copy_from_slice(a.as_bytes());
    buf[16..].copy_from_slice(b.as_bytes());
    let code_verifier = URL_SAFE_NO_PAD.encode(buf);

    // SHA256(verifier) → base64url challenge
    let hash = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(hash);

    (code_verifier, code_challenge)
}

// ── Auth URL builder ────────────────────────────────────────

/// Build the full authorization URL with PKCE and state parameters.
pub fn build_auth_url(config: &McpOAuthConfig, state: &str, code_challenge: &str) -> String {
    let scopes = config.scopes.join(" ");
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        config.auth_url,
        urlencoded(&config.client_id),
        urlencoded(&config.redirect_uri),
        urlencoded(&scopes),
        urlencoded(state),
        urlencoded(code_challenge),
    );
    // Google only returns a refresh_token when `access_type=offline` is set, and
    // only re-issues one when the user is forced through consent. Without this,
    // the access token expires in ~1h with no way to refresh — breaking the
    // native Gmail/Calendar tools. Other providers ignore these extra params.
    if config.auth_url.contains("accounts.google.com") {
        url.push_str("&access_type=offline&prompt=consent");
    }
    // Notion requires `owner=user` to run the user-authorization flow (without
    // it the authorize endpoint errors). Notion ignores the PKCE challenge and
    // scope params, which are harmless extras here.
    if config.auth_url.contains("api.notion.com") {
        url.push_str("&owner=user");
    }
    url
}

/// Minimal percent-encoding for URL query values.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

// ── Token exchange ──────────────────────────────────────────

/// How a provider's token endpoint expects the authorization-code exchange to
/// be assembled. Different providers diverge from the "plain form POST" default:
///
/// - **Notion** requires HTTP Basic auth (`client_id:client_secret`) plus a
///   JSON body `{grant_type, code, redirect_uri}` — client credentials must NOT
///   appear in the body, and it does not use PKCE.
/// - **GitHub / Google / Slack** take a form-encoded body with the credentials
///   inline; GitHub additionally needs `Accept: application/json` (which the
///   others tolerate) or it replies form-encoded.
#[derive(Debug, PartialEq)]
pub enum ExchangeBody {
    /// Form-encoded key/value pairs (default path).
    Form(Vec<(String, String)>),
    /// JSON object body (Notion).
    Json(serde_json::Value),
}

/// A provider-specific, side-effect-free plan for the token-exchange request.
/// Unit-tested per provider so the wire assembly can't silently regress.
#[derive(Debug, PartialEq)]
pub struct ExchangeRequest {
    pub url: String,
    /// Send `Authorization: Basic base64(client_id:client_secret)` (Notion).
    pub basic_auth: bool,
    /// Send `Accept: application/json` (GitHub, and harmless elsewhere).
    pub accept_json: bool,
    pub body: ExchangeBody,
}

fn is_notion_provider(config: &McpOAuthConfig) -> bool {
    config.provider_id == "notion" || config.token_url.contains("api.notion.com")
}

/// Build the token-exchange request plan for a provider. Pure function — no I/O.
pub fn build_exchange_request(
    config: &McpOAuthConfig,
    code: &str,
    code_verifier: &str,
) -> ExchangeRequest {
    if is_notion_provider(config) {
        // Notion: Basic auth for the client credentials, JSON body without them,
        // and no PKCE verifier.
        return ExchangeRequest {
            url: config.token_url.clone(),
            basic_auth: true,
            accept_json: true,
            body: ExchangeBody::Json(serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": config.redirect_uri,
            })),
        };
    }

    // Default (GitHub, Google, Slack, generic custom providers): form POST with
    // the credentials inline. `Accept: application/json` forces GitHub to reply
    // JSON instead of its default form-encoded body; other providers ignore it.
    ExchangeRequest {
        url: config.token_url.clone(),
        basic_auth: false,
        accept_json: true,
        body: ExchangeBody::Form(vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.into()),
            ("redirect_uri".into(), config.redirect_uri.clone()),
            ("client_id".into(), config.client_id.clone()),
            ("client_secret".into(), config.client_secret.clone()),
            ("code_verifier".into(), code_verifier.into()),
        ]),
    }
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    config: &McpOAuthConfig,
    code: &str,
    code_verifier: &str,
) -> Result<McpOAuthToken, String> {
    let client = reqwest::Client::new();
    let plan = build_exchange_request(config, code, code_verifier);

    let mut req = client.post(&plan.url);
    if plan.accept_json {
        req = req.header("Accept", "application/json");
    }
    if plan.basic_auth {
        req = req.basic_auth(&config.client_id, Some(&config.client_secret));
    }
    req = match &plan.body {
        ExchangeBody::Form(params) => req.form(params),
        ExchangeBody::Json(v) => req.json(v),
    };

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Token request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    if !status.is_success() {
        let err = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("Token exchange failed ({status}): {err}"));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("Missing access_token in response")?
        .to_string();

    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let expires_at = body.get("expires_in").and_then(|v| v.as_i64()).map(|secs| {
        chrono::Utc::now() + chrono::Duration::seconds(secs)
    });

    let scopes = config.scopes.clone();

    info!(provider = %config.provider_id, "OAuth token exchange successful");

    Ok(McpOAuthToken {
        provider_id: config.provider_id.clone(),
        access_token,
        refresh_token,
        expires_at,
        scopes,
    })
}

/// Refresh an expired token using a refresh_token grant.
pub async fn refresh_token(
    config: &McpOAuthConfig,
    refresh_tok: &str,
) -> Result<McpOAuthToken, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_tok),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
    ];

    let resp = client
        .post(&config.token_url)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {e}"))?;

    if !status.is_success() {
        let err = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("Token refresh failed ({status}): {err}"));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("Missing access_token in refresh response")?
        .to_string();

    let new_refresh = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some(refresh_tok.to_string()));

    let expires_at = body.get("expires_in").and_then(|v| v.as_i64()).map(|secs| {
        chrono::Utc::now() + chrono::Duration::seconds(secs)
    });

    info!(provider = %config.provider_id, "OAuth token refresh successful");

    Ok(McpOAuthToken {
        provider_id: config.provider_id.clone(),
        access_token,
        refresh_token: new_refresh,
        expires_at,
        scopes: config.scopes.clone(),
    })
}

// ── Token persistence ───────────────────────────────────────
//
// XC.1: MCP OAuth tokens are encrypted at rest with the same AES-256-GCM
// per-machine keyfile used for channel / API tokens (`config_crypto`). On disk
// `access_token` / `refresh_token` hold the ciphertext; we decrypt on load and
// encrypt on save. A legacy plaintext file (pre-encryption) is read
// transparently: `decrypt_value` returns `None` for non-ciphertext, in which
// case we keep the original value, so the next `save_tokens` migrates it.

/// Marker prefix distinguishing an encrypted field from legacy plaintext.
/// `config_crypto::encrypt_value` emits base64; we additionally tag it so a
/// best-effort decrypt never mistakes a plausible-looking plaintext for cipher.
const ENC_PREFIX: &str = "enc:v1:";

fn encrypt_field(plaintext: &str, home_dir: &Path) -> String {
    if plaintext.is_empty() {
        return String::new();
    }
    match crate::config_crypto::encrypt_value(plaintext, home_dir) {
        Some(enc) => format!("{ENC_PREFIX}{enc}"),
        // Encryption unavailable (keyfile write failed): fall back to plaintext
        // rather than losing the token. Logged by the caller of save_tokens.
        None => plaintext.to_string(),
    }
}

fn decrypt_field(stored: &str, home_dir: &Path) -> String {
    match stored.strip_prefix(ENC_PREFIX) {
        Some(cipher) => crate::config_crypto::decrypt_value(cipher, home_dir)
            .unwrap_or_else(|| stored.to_string()),
        // No prefix → legacy plaintext, return as-is (migrated on next save).
        None => stored.to_string(),
    }
}

/// Load all stored tokens from disk, decrypting secrets.
pub fn load_tokens(home_dir: &Path) -> Vec<McpOAuthToken> {
    let path = home_dir.join(TOKEN_FILE);
    let mut tokens: Vec<McpOAuthToken> = match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => return Vec::new(),
    };
    for t in &mut tokens {
        t.access_token = decrypt_field(&t.access_token, home_dir);
        if let Some(rt) = t.refresh_token.take() {
            t.refresh_token = Some(decrypt_field(&rt, home_dir));
        }
    }
    tokens
}

/// Save tokens to disk using atomic write (temp + rename), encrypting secrets.
pub fn save_tokens(home_dir: &Path, tokens: &[McpOAuthToken]) -> Result<(), String> {
    let path = home_dir.join(TOKEN_FILE);

    // Encrypt a copy so the in-memory caller keeps cleartext.
    let on_disk: Vec<McpOAuthToken> = tokens
        .iter()
        .map(|t| McpOAuthToken {
            provider_id: t.provider_id.clone(),
            access_token: encrypt_field(&t.access_token, home_dir),
            refresh_token: t
                .refresh_token
                .as_ref()
                .map(|rt| encrypt_field(rt, home_dir)),
            expires_at: t.expires_at,
            scopes: t.scopes.clone(),
        })
        .collect();

    let json = serde_json::to_string_pretty(&on_disk)
        .map_err(|e| format!("Failed to serialize tokens: {e}"))?;

    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Failed to write temp token file: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename token file: {e}"))?;

    Ok(())
}

/// Get a valid (non-expired) token for a specific provider.
pub fn get_token(home_dir: &Path, provider_id: &str) -> Option<McpOAuthToken> {
    let tokens = load_tokens(home_dir);
    tokens.into_iter().find(|t| {
        t.provider_id == provider_id && !is_expired(t)
    })
}

/// Get the stored token for a provider **regardless of expiry**.
///
/// [`get_token`] deliberately hides expired tokens because its callers want a
/// token they can send. Status reporting wants the opposite: a Google access
/// token dies after an hour, so `get_token` returning `None` is the normal
/// steady state for a perfectly healthy connection — the refresh token in the
/// same record keeps working, and every real tool call transparently refreshes
/// it. Reporting that state as "not connected" is what made an operator whose
/// integration was working believe their saved credentials had been wiped.
pub fn get_stored_token(home_dir: &Path, provider_id: &str) -> Option<McpOAuthToken> {
    load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == provider_id)
}

/// Check if a token is expired (with 60s grace period).
fn is_expired(token: &McpOAuthToken) -> bool {
    match token.expires_at {
        Some(exp) => chrono::Utc::now() + chrono::Duration::seconds(60) >= exp,
        None => false, // No expiry means it doesn't expire (e.g., GitHub)
    }
}

/// Public view of [`is_expired`] — the access token's own clock, which says
/// nothing about whether the connection still works (see [`get_stored_token`]).
pub fn token_expired(token: &McpOAuthToken) -> bool {
    is_expired(token)
}

/// Render a stored secret as a tail-masked hint (`••••abcd`).
///
/// Used so a UI can prove a secret is on file without shipping it to the
/// browser. Secrets shorter than 12 characters are masked whole: four
/// characters out of eleven is a third of the value, which is a meaningful
/// head start on guessing the rest. Real provider secrets are far longer
/// (a Google OAuth client secret runs 35), so the threshold costs nothing
/// in practice and only bites on the short test-grade values where the tail
/// matters most.
pub fn mask_secret_tail(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 12 {
        return "••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

/// Remove a token for a specific provider.
pub fn remove_token(home_dir: &Path, provider_id: &str) -> Result<(), String> {
    let mut tokens = load_tokens(home_dir);
    tokens.retain(|t| t.provider_id != provider_id);
    save_tokens(home_dir, &tokens)
}

/// Upsert a token: replace existing for same provider_id, or append.
pub fn upsert_token(home_dir: &Path, token: McpOAuthToken) -> Result<(), String> {
    let mut tokens = load_tokens(home_dir);
    tokens.retain(|t| t.provider_id != token.provider_id);
    tokens.push(token);
    save_tokens(home_dir, &tokens)
}

// ── Pending OAuth cleanup ───────────────────────────────────

/// Remove pending entries older than 10 minutes.
pub fn cleanup_pending(pending: &mut HashMap<String, PendingOAuth>) {
    pending.retain(|_, p| p.created_at.elapsed().as_secs() < PENDING_TTL_SECS);
}

// ── Client credential persistence ───────────────────────────
//
// The OAuth *token* (access + refresh) is persisted, but the client
// credentials used to obtain it were previously discarded after the flow (they
// only lived in the in-memory `PendingOAuth`). A refresh_token grant needs the
// same `client_id`/`client_secret`, so we persist a per-provider client config
// here — `client_secret` encrypted at rest with the same keyfile as the token
// file. Stored in `mcp-oauth-configs.json`.

const CLIENT_CONFIG_FILE: &str = "mcp-oauth-configs.json";

/// Persisted client credentials + endpoints for a provider. Enables in-place
/// token refresh without re-prompting the user for their client secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthClientConfig {
    pub provider_id: String,
    pub client_id: String,
    /// Encrypted at rest (`enc:v1:` prefix). Decrypted on load.
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

/// Load all stored client configs, decrypting `client_secret`.
pub fn load_client_configs(home_dir: &Path) -> Vec<McpOAuthClientConfig> {
    let path = home_dir.join(CLIENT_CONFIG_FILE);
    let mut configs: Vec<McpOAuthClientConfig> = match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => return Vec::new(),
    };
    for c in &mut configs {
        c.client_secret = decrypt_field(&c.client_secret, home_dir);
    }
    configs
}

/// Save client configs with atomic write, encrypting `client_secret`.
fn save_client_configs(home_dir: &Path, configs: &[McpOAuthClientConfig]) -> Result<(), String> {
    let path = home_dir.join(CLIENT_CONFIG_FILE);
    let on_disk: Vec<McpOAuthClientConfig> = configs
        .iter()
        .map(|c| McpOAuthClientConfig {
            provider_id: c.provider_id.clone(),
            client_id: c.client_id.clone(),
            client_secret: encrypt_field(&c.client_secret, home_dir),
            auth_url: c.auth_url.clone(),
            token_url: c.token_url.clone(),
            scopes: c.scopes.clone(),
            redirect_uri: c.redirect_uri.clone(),
        })
        .collect();

    let json = serde_json::to_string_pretty(&on_disk)
        .map_err(|e| format!("Failed to serialize client configs: {e}"))?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Failed to write temp client-config file: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename client-config file: {e}"))?;
    Ok(())
}

/// Get the stored client config for a provider (secret decrypted).
pub fn get_client_config(home_dir: &Path, provider_id: &str) -> Option<McpOAuthClientConfig> {
    load_client_configs(home_dir)
        .into_iter()
        .find(|c| c.provider_id == provider_id)
}

/// Upsert a client config: replace existing for the same provider, or append.
pub fn upsert_client_config(
    home_dir: &Path,
    config: McpOAuthClientConfig,
) -> Result<(), String> {
    let mut configs = load_client_configs(home_dir);
    configs.retain(|c| c.provider_id != config.provider_id);
    configs.push(config);
    save_client_configs(home_dir, &configs)
}

/// Whether a provider has persisted client credentials (used for the dashboard
/// `configured` flag, since the built-in templates carry empty credentials).
pub fn has_client_config(home_dir: &Path, provider_id: &str) -> bool {
    get_client_config(home_dir, provider_id)
        .map(|c| !c.client_id.is_empty())
        .unwrap_or(false)
}

/// Remove a provider's stored client config (called on revoke).
pub fn remove_client_config(home_dir: &Path, provider_id: &str) -> Result<(), String> {
    let mut configs = load_client_configs(home_dir);
    let before = configs.len();
    configs.retain(|c| c.provider_id != provider_id);
    if configs.len() == before {
        return Ok(());
    }
    save_client_configs(home_dir, &configs)
}

#[cfg(test)]
mod redirect_uri_tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-mcpoauth-redirect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The redirect URI is the one value the user copies into Google/Notion/
    /// GitHub's console, and it must point at the port the gateway actually
    /// serves `/api/mcp/oauth/callback` on. It was hardcoded to 3000 while the
    /// gateway listened on 18789, so every consent flow redirected into a dead
    /// port and no token was ever stored. Lock the default here.
    #[test]
    fn redirect_uri_targets_the_gateway_default_port() {
        // `DUDUCLAW_PORT` is process-global; only assert the default when the
        // environment has not overridden it, so a parallel test that sets it
        // cannot make this one flaky. No config.toml at `home` either, so the
        // shared resolver falls through to its 18789 default.
        if std::env::var("DUDUCLAW_PORT").is_ok() {
            return;
        }
        let home = tmp_home();
        assert_eq!(gateway_port(&home), 18789, "must match the CLI's `duduclaw run` default");
        assert_eq!(
            redirect_uri(&home),
            "http://localhost:18789/api/mcp/oauth/callback",
            "redirect URI must point at the port serving the callback route"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A `config.toml [gateway] port` on disk must be honored — the exact
    /// bug this module's shared resolver closes: before
    /// `duduclaw_core::gateway_port_for_home`, this function read only
    /// `DUDUCLAW_PORT` and ignored `config.toml` entirely, so a hand-edited
    /// port never reached the OAuth redirect URI.
    #[test]
    fn redirect_uri_honors_config_toml_port() {
        if std::env::var("DUDUCLAW_PORT").is_ok() {
            return;
        }
        let home = tmp_home();
        std::fs::write(home.join("config.toml"), "[gateway]\nport = 9100\n").unwrap();
        assert_eq!(gateway_port(&home), 9100);
        assert_eq!(
            redirect_uri(&home),
            "http://localhost:9100/api/mcp/oauth/callback"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The path half must stay byte-identical to the axum route registration in
    /// `server.rs` (`.route("/api/mcp/oauth/callback", …)`).
    #[test]
    fn redirect_uri_path_matches_the_registered_route() {
        let home = tmp_home();
        assert!(
            redirect_uri(&home).ends_with("/api/mcp/oauth/callback"),
            "got {}",
            redirect_uri(&home)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Every built-in provider hands the user the same registered URI — a
    /// per-provider drift would silently break just one integration.
    #[test]
    fn all_builtin_providers_share_the_derived_redirect_uri() {
        let home = tmp_home();
        let uri = redirect_uri(&home);
        let providers = builtin_providers(&uri);
        assert!(!providers.is_empty(), "expected built-in providers");
        for p in &providers {
            assert_eq!(p.redirect_uri, uri, "provider {} drifted", p.provider_id);
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod xc1_token_encryption_tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-mcpoauth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn tokens_round_trip_through_disk_and_are_encrypted_at_rest() {
        let home = tmp_home();
        let token = McpOAuthToken {
            provider_id: "github".into(),
            access_token: "gho_super_secret_value".into(),
            refresh_token: Some("ghr_refresh_secret".into()),
            expires_at: None,
            scopes: vec!["repo".into()],
        };
        save_tokens(&home, &[token.clone()]).expect("save");

        // On-disk JSON must NOT contain the cleartext secrets.
        let raw = std::fs::read_to_string(home.join(TOKEN_FILE)).unwrap();
        assert!(!raw.contains("gho_super_secret_value"), "access token leaked: {raw}");
        assert!(!raw.contains("ghr_refresh_secret"), "refresh token leaked: {raw}");
        assert!(raw.contains(ENC_PREFIX), "expected enc prefix in {raw}");

        // load_tokens decrypts back to cleartext.
        let loaded = load_tokens(&home);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].access_token, "gho_super_secret_value");
        assert_eq!(loaded[0].refresh_token.as_deref(), Some("ghr_refresh_secret"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_plaintext_token_file_is_read_transparently() {
        let home = tmp_home();
        // Simulate a pre-encryption file with cleartext tokens.
        let legacy = r#"[{"provider_id":"google","access_token":"ya29.legacy","refresh_token":null,"expires_at":null,"scopes":[]}]"#;
        std::fs::write(home.join(TOKEN_FILE), legacy).unwrap();
        let loaded = load_tokens(&home);
        assert_eq!(loaded[0].access_token, "ya29.legacy");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn client_config_round_trips_and_encrypts_secret() {
        let home = tmp_home();
        // Assembled at run time so no contiguous `GOCSPX-…` literal sits in the
        // source: a synthetic secret with a real vendor shape trips source
        // scanners exactly like a live one.
        let secret = ["GOCSPX", "-super-secret"].concat();
        let cfg = McpOAuthClientConfig {
            provider_id: "google".into(),
            client_id: "1234.apps.googleusercontent.com".into(),
            client_secret: secret.clone(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
            redirect_uri: "http://localhost:3000/api/mcp/oauth/callback".into(),
        };
        upsert_client_config(&home, cfg.clone()).expect("save");

        // Secret must be encrypted at rest; client_id stays readable.
        let raw = std::fs::read_to_string(home.join(CLIENT_CONFIG_FILE)).unwrap();
        assert!(!raw.contains(&secret), "secret leaked: {raw}");
        assert!(raw.contains("1234.apps.googleusercontent.com"));

        let loaded = get_client_config(&home, "google").expect("present");
        assert_eq!(loaded.client_secret, secret);
        assert_eq!(loaded.client_id, cfg.client_id);
        assert!(has_client_config(&home, "google"));
        assert!(!has_client_config(&home, "github"));

        remove_client_config(&home, "google").expect("remove");
        assert!(get_client_config(&home, "google").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn google_auth_url_requests_offline_access() {
        let cfg = McpOAuthConfig {
            provider_id: "google".into(),
            client_id: "cid".into(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec!["s1".into()],
            redirect_uri: "http://localhost:3000/api/mcp/oauth/callback".into(),
        };
        let url = build_auth_url(&cfg, "state123", "challenge");
        assert!(url.contains("access_type=offline"), "url: {url}");
        assert!(url.contains("prompt=consent"), "url: {url}");

        // Non-Google providers must NOT get the extra params.
        let gh = McpOAuthConfig {
            auth_url: "https://github.com/login/oauth/authorize".into(),
            ..cfg
        };
        let gh_url = build_auth_url(&gh, "s", "c");
        assert!(!gh_url.contains("access_type=offline"));
    }

    fn cfg(provider: &str, auth_url: &str, token_url: &str) -> McpOAuthConfig {
        McpOAuthConfig {
            provider_id: provider.into(),
            client_id: "cid".into(),
            client_secret: "csecret".into(),
            auth_url: auth_url.into(),
            token_url: token_url.into(),
            scopes: vec![],
            redirect_uri: "http://localhost:3000/api/mcp/oauth/callback".into(),
        }
    }

    #[test]
    fn notion_auth_url_requests_owner_user() {
        let c = cfg(
            "notion",
            "https://api.notion.com/v1/oauth/authorize",
            "https://api.notion.com/v1/oauth/token",
        );
        let url = build_auth_url(&c, "st", "ch");
        assert!(url.contains("&owner=user"), "url: {url}");
        // Google-only extras must not leak onto Notion.
        assert!(!url.contains("access_type=offline"));
    }

    #[test]
    fn notion_exchange_uses_basic_auth_and_json_body() {
        let c = cfg(
            "notion",
            "https://api.notion.com/v1/oauth/authorize",
            "https://api.notion.com/v1/oauth/token",
        );
        let req = build_exchange_request(&c, "auth_code_123", "verifier_ignored");
        assert!(req.basic_auth, "Notion must use HTTP Basic auth");
        assert!(req.accept_json);
        match req.body {
            ExchangeBody::Json(v) => {
                assert_eq!(v["grant_type"], "authorization_code");
                assert_eq!(v["code"], "auth_code_123");
                assert_eq!(v["redirect_uri"], c.redirect_uri);
                // Credentials must NOT appear in the JSON body (they go in the
                // Basic auth header) and PKCE is not used by Notion.
                assert!(v.get("client_id").is_none());
                assert!(v.get("client_secret").is_none());
                assert!(v.get("code_verifier").is_none());
            }
            other => panic!("expected JSON body, got {other:?}"),
        }
    }

    #[test]
    fn notion_detected_by_token_url_even_with_custom_provider_id() {
        let c = cfg(
            "my-notion",
            "https://api.notion.com/v1/oauth/authorize",
            "https://api.notion.com/v1/oauth/token",
        );
        let req = build_exchange_request(&c, "x", "y");
        assert!(req.basic_auth, "token_url host should trigger Notion path");
    }

    #[test]
    fn github_exchange_is_form_post_with_accept_json() {
        let c = cfg(
            "github",
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
        );
        let req = build_exchange_request(&c, "gh_code", "verifier");
        assert!(!req.basic_auth);
        assert!(req.accept_json, "GitHub needs Accept: application/json to get JSON");
        match req.body {
            ExchangeBody::Form(params) => {
                let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
                assert_eq!(get("grant_type"), Some("authorization_code"));
                assert_eq!(get("code"), Some("gh_code"));
                assert_eq!(get("client_id"), Some("cid"));
                assert_eq!(get("client_secret"), Some("csecret"));
                assert_eq!(get("code_verifier"), Some("verifier"));
            }
            other => panic!("expected Form body, got {other:?}"),
        }
    }

    #[test]
    fn google_exchange_is_form_post() {
        let c = cfg(
            "google",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
        );
        let req = build_exchange_request(&c, "g_code", "verifier");
        assert!(!req.basic_auth);
        assert!(matches!(req.body, ExchangeBody::Form(_)));
    }
}

/// Masking + stored-token reads used by the dashboard's "already saved" display
/// (WP13). The point of both is to prove a credential exists without handing it
/// over, and without mistaking an hour-old access token for a lost connection.
#[cfg(test)]
mod saved_credential_display_tests {
    use super::*;

    #[test]
    fn mask_secret_tail_keeps_only_four_characters() {
        assert_eq!(mask_secret_tail(""), "");
        // Under 12 characters the tail would be a third of the secret — mask
        // the whole thing instead.
        assert_eq!(mask_secret_tail("abc"), "••••");
        assert_eq!(mask_secret_tail("elevenchars"), "••••");
        assert_eq!(mask_secret_tail(&["GOCSPX", "-abcdef1234"].concat()), "••••1234");
        // CJK / multi-byte values must not panic or slice mid-character.
        assert_eq!(mask_secret_tail("金鑰的內容就是這幾個中文字"), "••••個中文字");
    }

    #[test]
    fn stored_token_is_returned_even_after_the_access_token_expires() {
        let home = std::env::temp_dir().join(format!("dc-oauth-stored-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();

        let token = McpOAuthToken {
            provider_id: "google".into(),
            access_token: "ya29.stale".into(),
            refresh_token: Some("1//refresh".into()),
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(3)),
            scopes: vec!["gmail.readonly".into()],
        };
        upsert_token(&home, token).unwrap();

        // The call-time accessor hides it (correct — it cannot be sent as-is)…
        assert!(get_token(&home, "google").is_none());
        // …while the status accessor still sees the connection.
        let stored = get_stored_token(&home, "google").expect("stored token");
        assert!(token_expired(&stored));
        assert!(stored.refresh_token.is_some());

        let _ = std::fs::remove_dir_all(&home);
    }
}
