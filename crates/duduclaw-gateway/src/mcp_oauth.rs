//! MCP OAuth 2.1 + PKCE flow for authenticating with external OAuth providers.
//!
//! Supports built-in provider configs (Google, GitHub, Slack) and custom providers.
//! Tokens are stored in `~/.duduclaw/mcp-oauth-tokens.json`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

#[cfg(test)]
pub(crate) mod google_home_test_fixture;
#[path = "mcp_oauth/home_lifecycle.rs"]
pub(crate) mod home_lifecycle;
#[path = "mcp_oauth/home_store.rs"]
pub(crate) mod home_store;

/// Opaque naming facade; secret ownership stays in the lexical SourceOwner.
pub(crate) mod home_oauth_material {
    pub(crate) use crate::google_identity::home_source::{
        HomeOAuthBearer, HomeOAuthMaterialProof, HomeOAuthResolved, resolve_home_oauth_pinned,
        validate_home_oauth_material,
    };
}

#[path = "mcp_oauth/home_clients.rs"]
mod home_clients;

/// Nonsecret classification only, never a token/authority or a public view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeRevokePresence {
    NoCredential,
    GoogleCredential,
    OtherProvider,
}
/// The caller must already hold this home's global Google grant lease.
/// Missing/hidden public tokens alone never prove that revoke is safe.
pub(crate) fn home_presence_for_personal_revoke(
    home: &Path,
    lease: &crate::google_personal::grants::Lease,
) -> home_store::Result<HomeRevokePresence> {
    home_store::read_revoke_presence(home, lease)
}

/// Read the Google credential only to compare grant ownership before a
/// Personal disconnect revokes at the provider. This does not make a legacy
/// credential usable by tools and requires the caller's global grant lease.
pub(crate) fn home_access_token_for_personal_revoke(
    home: &Path,
    lease: &crate::google_personal::grants::Lease,
) -> home_store::Result<zeroize::Zeroizing<String>> {
    home_store::read_revoke_access_token(home, lease)
}

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

fn require_generic_non_google(config: &McpOAuthConfig) -> Result<(), String> {
    if crate::google_oauth_policy::is_google_oauth_route(
        &config.provider_id,
        "",
        &config.auth_url,
        &config.token_url,
        &config.scopes,
    ) {
        Err("Google requires a bound Home flow.".into())
    } else {
        Ok(())
    }
}

/// Closed RAM flow. Google never carries an unbound legacy config. No
/// Debug/Clone/serialization or field constructor can bypass this distinction.
pub struct PendingOAuth {
    flow: PendingOAuthFlow,
}
enum PendingOAuthFlow {
    Google(home_lifecycle::GoogleHomePending),
    Other {
        config: McpOAuthConfig,
        verifier: zeroize::Zeroizing<String>,
        created_at: std::time::Instant,
    },
}
impl PendingOAuth {
    pub(crate) fn other(config: McpOAuthConfig, verifier: String) -> Result<Self, String> {
        require_generic_non_google(&config)?;
        Ok(Self {
            flow: PendingOAuthFlow::Other {
                config,
                verifier: zeroize::Zeroizing::new(verifier),
                created_at: std::time::Instant::now(),
            },
        })
    }
    fn is_google(&self) -> bool {
        matches!(&self.flow, PendingOAuthFlow::Google(_))
    }
    fn expired(&self) -> bool {
        match &self.flow {
            PendingOAuthFlow::Google(p) => p.is_expired(),
            PendingOAuthFlow::Other { created_at, .. } => {
                created_at.elapsed().as_secs() >= PENDING_TTL_SECS
            }
        }
    }
}
/// B1 grant is already dropped. Revalidate SAME ticket after the final RAM
/// write-lock await before exposing URL/state or replacing the latest flow.
pub(crate) async fn insert_google_start(
    home: &Path,
    started: home_lifecycle::StartedGoogleHomeAuthorization,
    pending: &tokio::sync::RwLock<HashMap<String, PendingOAuth>>,
) -> home_store::Result<(String, String)> {
    let (url, state, google) = started.into_response_parts();
    let mut map = pending.write().await;
    home_lifecycle::validate_google_home_pending(home, &google)?;
    cleanup_pending(&mut map);
    map.retain(|_, entry| !entry.is_google());
    map.insert(
        state.clone(),
        PendingOAuth {
            flow: PendingOAuthFlow::Google(google),
        },
    );
    Ok((url, state))
}
/// One identifiable RAM state is consumed even for provider rejection/missing
/// code. Query scope/error_description are never token evidence or output.
pub(crate) async fn complete_callback_query(
    home: &Path,
    pending: &tokio::sync::RwLock<HashMap<String, PendingOAuth>>,
    query: &str,
    actor_is_live_admin: impl Fn(&str) -> bool + Send + Sync,
) -> Result<String, ()> {
    // Unknown descriptions/scopes are never decoded or echoed. Even an
    // oversized rejection can consume its one unambiguous bounded state;
    // oversize/malformed code cannot become a provider request.
    let mut states = Vec::new();
    let mut code = None;
    let mut failed = query.len() > 16 * 1024;
    fn decoded(value: &str) -> String {
        let field = zeroize::Zeroizing::new(format!("v={value}"));
        url::form_urlencoded::parse(field.as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    }
    for field in query.split('&') {
        let (raw_key, raw_value) = field.split_once('=').unwrap_or((field, ""));
        if raw_key.len() > 32 {
            continue;
        }
        let key = decoded(raw_key);
        match key.as_str() {
            "state" => {
                if raw_value.len() > 384 || states.len() >= 128 {
                    return Err(());
                }
                states.push(decoded(raw_value));
            }
            "code" => {
                if code.is_some() || raw_value.len() > 48 * 1024 {
                    failed = true;
                } else if !failed {
                    code = Some(zeroize::Zeroizing::new(decoded(raw_value)));
                }
            }
            "error" => failed = true,
            _ => {}
        }
    }
    if states.is_empty() || states.iter().any(|s| s != &states[0]) {
        return Err(());
    }
    if states.len() != 1 {
        failed = true;
    }
    let state = &states[0];
    if state.is_empty() || state.len() > 128 || state.chars().any(char::is_control) {
        return Err(());
    }
    let entry = {
        let mut map = pending.write().await;
        cleanup_pending(&mut map);
        map.remove(state)
    }
    .ok_or(())?;
    if failed {
        return Err(());
    }
    let code = code.filter(|c| !c.is_empty()).ok_or(())?;
    match entry.flow {
        PendingOAuthFlow::Google(google) => {
            if !actor_is_live_admin(google.consent_actor()) {
                return Err(());
            }
            home_lifecycle::complete_google_home_authorization_with_admin_check(
                home,
                google,
                state,
                code,
                &actor_is_live_admin,
            )
            .await
            .map_err(|_| ())?;
            Ok("google".into())
        }
        PendingOAuthFlow::Other {
            config, verifier, ..
        } => {
            exchange_and_store(home, &config, &code, &verifier)
                .await
                .map_err(|_| ())?;
            Ok(config.provider_id)
        }
    }
}

pub const TOKEN_FILE: &str = "mcp-oauth-tokens.json";
const PENDING_TTL_SECS: u64 = 600; // 10 minutes

// ── Built-in provider configs ───────────────────────────────

/// Return built-in OAuth provider templates.
/// `client_id` and `client_secret` are empty — user must configure them.
///
/// The gateway's own port, resolved through the exact same shared priority
/// order `duduclaw run` uses — `DUDUCLAW_PORT` env > `config.toml [gateway]
/// port` > 18889 (`duduclaw_core::gateway_port_for_home`) — so the two can
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
/// listened on 18889, so every provider redirected the browser to a port with
/// nothing on it and no OAuth flow could complete — the callback never fired,
/// no token was ever stored, and the dashboard sat on "not connected".
pub fn redirect_uri(home: &Path) -> String {
    format!(
        "http://localhost:{}/api/mcp/oauth/callback",
        gateway_port(home)
    )
}

pub fn builtin_providers(redirect_uri: &str) -> Vec<McpOAuthConfig> {
    vec![
        McpOAuthConfig {
            provider_id: "google".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            // This exact full Home request is captured by B1 and callback.
            // Legacy token rows are not upgraded by this template change.
            scopes: crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect(),
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

/// Complete a non-Google OAuth exchange and publication under the same grant
/// lease used by personal connections. CLI/HTTP entry points must use this
/// wrapper rather than separating exchange and upsert across the fence.
pub async fn exchange_and_store(
    home: &Path,
    config: &McpOAuthConfig,
    code: &str,
    verifier: &str,
) -> Result<(), String> {
    require_generic_non_google(config)?;
    let _lease = crate::google_personal::grants::acquire(home).await?;
    let token = exchange_code(config, code, verifier).await?;
    upsert_token(home, token)
}

/// Generic provider transport; Google must consume its bound Home flow.
pub async fn exchange_code(
    config: &McpOAuthConfig,
    code: &str,
    code_verifier: &str,
) -> Result<McpOAuthToken, String> {
    require_generic_non_google(config)?;
    #[cfg(test)]
    home_store::test_probe::provider(&config.client_id);
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

    let expires_at = body
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));

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
    require_generic_non_google(config).map_err(|_| home_store::Error::PendingWiring.to_string())?;
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

    let expires_at = body
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));

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
    #[cfg(test)]
    home_store::test_probe::crypto(home_dir);
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
    #[cfg(test)]
    home_store::test_probe::crypto(home_dir);
    match stored.strip_prefix(ENC_PREFIX) {
        Some(cipher) => crate::config_crypto::decrypt_value(cipher, home_dir)
            .unwrap_or_else(|| stored.to_string()),
        // No prefix → legacy plaintext, return as-is (migrated on next save).
        None => stored.to_string(),
    }
}

/// Exact encrypted storage size for AES-GCM nonce(12)+tag(16), base64 and
/// enc:v1 prefix; preflight also checks plaintext fallback separately.
fn encrypted_size_placeholder(plain: &str) -> home_store::Result<String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    let size = plain
        .len()
        .checked_add(28)
        .and_then(|n| n.checked_add(2))
        .and_then(|n| (n / 3).checked_mul(4))
        .and_then(|n| n.checked_add(ENC_PREFIX.len()))
        .ok_or(home_store::Error::Invalid)?;
    if size > home_store::raw_array::MAX_BYTES {
        return Err(home_store::Error::Invalid);
    }
    Ok("A".repeat(size))
}
fn decode_secret(home: &Path, provider: &str, stored: &str) -> Option<String> {
    if provider != "google" {
        return Some(decrypt_field(stored, home));
    }
    if let Some(cipher) = stored.strip_prefix(ENC_PREFIX) {
        #[cfg(test)]
        {
            home_store::test_probe::crypto(home);
            home_store::test_probe::google_crypto(home);
        }
        crate::config_crypto::decrypt_value(cipher, home)
    } else if stored.starts_with("enc:") {
        // Reserved encryption versions are never legacy plaintext secrets.
        None
    } else {
        Some(stored.into())
    }
}
fn decode_public_token(home: &Path, stored: &McpOAuthToken) -> Option<McpOAuthToken> {
    let mut token = stored.clone();
    token.access_token = decode_secret(home, &stored.provider_id, &stored.access_token)?;
    token.refresh_token = match &stored.refresh_token {
        Some(r) => Some(decode_secret(home, &stored.provider_id, r)?),
        None => None,
    };
    Some(token)
}

/// Load all stored tokens from disk, decrypting secrets.
pub fn load_tokens(home_dir: &Path) -> Vec<McpOAuthToken> {
    home_store::load_decoded_public(home_dir, None).unwrap_or_default()
}

/// Save tokens to disk using atomic write (temp + rename), encrypting secrets.
pub fn save_tokens(home_dir: &Path, tokens: &[McpOAuthToken]) -> Result<(), String> {
    home_store::save_public(home_dir, tokens).map_err(|e| e.to_string())
}

/// Get a valid (non-expired) token for a specific provider.
pub fn get_token(home_dir: &Path, provider_id: &str) -> Option<McpOAuthToken> {
    home_store::load_decoded_public(home_dir, Some(provider_id))
        .ok()?
        .into_iter()
        .find(|t| !is_expired(t))
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
    home_store::load_decoded_public(home_dir, Some(provider_id))
        .ok()?
        .into_iter()
        .next()
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
    home_store::remove_public(home_dir, provider_id).map_err(|e| e.to_string())
}

/// Upsert a token: replace existing for same provider_id, or append.
pub fn upsert_token(home_dir: &Path, token: McpOAuthToken) -> Result<(), String> {
    home_store::upsert_public(home_dir, token).map_err(|e| e.to_string())
}

// ── Pending OAuth cleanup ───────────────────────────────────

/// Remove pending entries older than 10 minutes.
pub fn cleanup_pending(pending: &mut HashMap<String, PendingOAuth>) {
    pending.retain(|_, p| !p.expired());
}

// ── Client credential persistence ───────────────────────────
//
// The OAuth *token* (access + refresh) is persisted, but the client
// credentials used to obtain it were previously discarded after the flow (they
// only lived in the in-memory `PendingOAuth`). A refresh_token grant needs the
// same `client_id`/`client_secret`, so we persist a per-provider client config
// here — `client_secret` encrypted at rest with the same keyfile as the token
// file. Stored in `mcp-oauth-configs.json`.

pub(crate) const CLIENT_CONFIG_FILE: &str = "mcp-oauth-configs.json";

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

/// Explicit legacy all-provider view; it is not an effect authority.
pub fn load_client_configs(home_dir: &Path) -> Vec<McpOAuthClientConfig> {
    home_clients::load(home_dir, None).unwrap_or_default()
}
/// Select and capture one provider before decrypting only its secret.
pub fn get_client_config(home_dir: &Path, provider_id: &str) -> Option<McpOAuthClientConfig> {
    home_clients::load(home_dir, Some(provider_id))
        .ok()?
        .into_iter()
        .next()
}
/// Target-only raw-preserving update; Google uses the common source fence.
pub fn upsert_client_config(home_dir: &Path, config: McpOAuthClientConfig) -> Result<(), String> {
    home_clients::upsert(home_dir, config).map_err(|e| e.to_string())
}
/// Saved client metadata, not Home token/master state or provider acceptance.
pub fn has_client_config(home_dir: &Path, provider_id: &str) -> bool {
    home_clients::has(home_dir, provider_id)
}
pub fn remove_client_config(home_dir: &Path, provider_id: &str) -> Result<(), String> {
    home_clients::remove(home_dir, provider_id).map_err(|e| e.to_string())
}

#[cfg(test)]
mod redirect_uri_tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("ddc-mcpoauth-redirect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The redirect URI is the one value the user copies into Google/Notion/
    /// GitHub's console, and it must point at the port the gateway actually
    /// serves `/api/mcp/oauth/callback` on. It was hardcoded to 3000 while the
    /// gateway listened on 18889, so every consent flow redirected into a dead
    /// port and no token was ever stored. Lock the default here.
    #[test]
    fn redirect_uri_targets_the_gateway_default_port() {
        // `DUDUCLAW_PORT` is process-global; only assert the default when the
        // environment has not overridden it, so a parallel test that sets it
        // cannot make this one flaky. No config.toml at `home` either, so the
        // shared resolver falls through to its 18889 default.
        if std::env::var("DUDUCLAW_PORT").is_ok() {
            return;
        }
        let home = tmp_home();
        assert_eq!(
            gateway_port(&home),
            18889,
            "must match the CLI's `duduclaw run` default"
        );
        assert_eq!(
            redirect_uri(&home),
            "http://localhost:18889/api/mcp/oauth/callback",
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
        assert!(
            !raw.contains("gho_super_secret_value"),
            "access token leaked: {raw}"
        );
        assert!(
            !raw.contains("ghr_refresh_secret"),
            "refresh token leaked: {raw}"
        );
        assert!(raw.contains(ENC_PREFIX), "expected enc prefix in {raw}");

        // load_tokens decrypts back to cleartext.
        let loaded = load_tokens(&home);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].access_token, "gho_super_secret_value");
        assert_eq!(
            loaded[0].refresh_token.as_deref(),
            Some("ghr_refresh_secret")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_plaintext_non_google_token_file_is_read_transparently() {
        let home = tmp_home();
        // Simulate a pre-encryption file with cleartext tokens.
        let legacy = r#"[{"provider_id":"github","access_token":"gho.legacy","refresh_token":null,"expires_at":null,"scopes":[]}]"#;
        std::fs::write(home.join(TOKEN_FILE), legacy).unwrap();
        let loaded = load_tokens(&home);
        assert_eq!(loaded[0].access_token, "gho.legacy");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_plaintext_google_token_is_hidden_until_reconnect() {
        let home = tmp_home();
        let legacy = r#"[{"provider_id":"google","access_token":"ya29.legacy","refresh_token":null,"expires_at":null,"scopes":[]}]"#;
        std::fs::write(home.join(TOKEN_FILE), legacy).unwrap();
        assert!(load_tokens(&home).is_empty());
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
        assert!(
            req.accept_json,
            "GitHub needs Accept: application/json to get JSON"
        );
        match req.body {
            ExchangeBody::Form(params) => {
                let get = |k: &str| {
                    params
                        .iter()
                        .find(|(pk, _)| pk == k)
                        .map(|(_, v)| v.as_str())
                };
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
        assert_eq!(
            mask_secret_tail(&["GOCSPX", "-abcdef1234"].concat()),
            "••••1234"
        );
        // CJK / multi-byte values must not panic or slice mid-character.
        assert_eq!(
            mask_secret_tail("金鑰的內容就是這幾個中文字"),
            "••••個中文字"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_token_is_returned_even_after_the_access_token_expires() {
        let home = std::env::temp_dir().join(format!("dc-oauth-stored-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();

        // Publish a valid callback at an owned past wall time, then observe
        // expiry after returning to the real clock. No forbidden Google upsert.
        {
            let past = u64::try_from(chrono::Utc::now().timestamp()).unwrap() - 4 * 3600;
            let _clock = home_store::oauth_testing::ClockGuard::fixed(past);
            google_home_test_fixture::connect(&home, "ya29.stale", Some("1//refresh")).await;
        }

        // The call-time accessor hides it (correct — it cannot be sent as-is)…
        assert!(get_token(&home, "google").is_none());
        // …while the status accessor still sees the connection.
        let stored = get_stored_token(&home, "google").expect("stored token");
        assert!(token_expired(&stored));
        assert!(stored.refresh_token.is_some());

        let _ = std::fs::remove_dir_all(&home);
    }
}
