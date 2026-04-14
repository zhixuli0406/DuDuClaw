//! MCP OAuth 2.1 + PKCE flow for authenticating with external OAuth providers.
//!
//! Supports built-in provider configs (Google, GitHub, Slack) and custom providers.
//! Tokens are stored in `~/.duduclaw/mcp-oauth-tokens.json`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

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

const TOKEN_FILE: &str = "mcp-oauth-tokens.json";
const PENDING_TTL_SECS: u64 = 600; // 10 minutes

// ── Built-in provider configs ───────────────────────────────

/// Return built-in OAuth provider templates.
/// `client_id` and `client_secret` are empty — user must configure them.
pub fn builtin_providers(redirect_uri: &str) -> Vec<McpOAuthConfig> {
    vec![
        McpOAuthConfig {
            provider_id: "google".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec![
                "https://www.googleapis.com/auth/drive".into(),
                "https://www.googleapis.com/auth/gmail.readonly".into(),
                "https://www.googleapis.com/auth/calendar".into(),
            ],
            redirect_uri: redirect_uri.to_string(),
        },
        McpOAuthConfig {
            provider_id: "github".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://github.com/login/oauth/authorize".into(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            scopes: vec!["repo".into(), "read:org".into()],
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
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        config.auth_url,
        urlencoded(&config.client_id),
        urlencoded(&config.redirect_uri),
        urlencoded(&scopes),
        urlencoded(state),
        urlencoded(code_challenge),
    )
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

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    config: &McpOAuthConfig,
    code: &str,
    code_verifier: &str,
) -> Result<McpOAuthToken, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &config.redirect_uri),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
        ("code_verifier", code_verifier),
    ];

    let resp = client
        .post(&config.token_url)
        .header("Accept", "application/json")
        .form(&params)
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

/// Load all stored tokens from disk.
pub fn load_tokens(home_dir: &Path) -> Vec<McpOAuthToken> {
    let path = home_dir.join(TOKEN_FILE);
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save tokens to disk using atomic write (temp + rename).
pub fn save_tokens(home_dir: &Path, tokens: &[McpOAuthToken]) -> Result<(), String> {
    let path = home_dir.join(TOKEN_FILE);
    let json = serde_json::to_string_pretty(tokens)
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

/// Check if a token is expired (with 60s grace period).
fn is_expired(token: &McpOAuthToken) -> bool {
    match token.expires_at {
        Some(exp) => chrono::Utc::now() + chrono::Duration::seconds(60) >= exp,
        None => false, // No expiry means it doesn't expire (e.g., GitHub)
    }
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
