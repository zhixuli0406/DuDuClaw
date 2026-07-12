// auth_device.rs — OAuth 2.0 Device Authorization Grant (RFC 8628) login for
// subscription seats, plus the proxy-side seat forwarding for those seats.
//
// `duduclaw auth device --provider copilot|qwen` runs the device flow, prints
// the user code + verification URL, polls until the user authorizes, then
// stores the resulting seat credential (AES-256-GCM encrypted, same pattern as
// every other stored token) into `config.toml [[accounts]]` as an OAuth
// account tagged with the provider. The AccountRotator then carries the seat as
// a first-class pool member; `duduclaw proxy` exchanges it for a short-lived
// upstream token and forwards requests to the provider's chat/completions API.
//
// G2 — subscription OAuth breadth. Competitor Hermes consumes Claude Max /
// ChatGPT Codex / GitHub Copilot / Qwen subscriptions and re-exports them via a
// local proxy; this closes the Copilot + Qwen seats.
//
// Verified endpoints (first-hand sources cited inline):
//   - GitHub device flow: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow
//   - Copilot token mint + editor headers + public client id `Iv1.b507a08c87ecfe98`:
//     ericc-ch/copilot-api `src/lib/api-config.ts` + `src/services/github/*`
//   - Qwen device flow endpoints + client id: QwenLM/qwen-code
//     `packages/core/src/qwen/qwenOAuth2.ts`
//
// Qwen NOTE (honest status): Qwen's free OAuth tier was discontinued 2026-04-15
// and the flow was removed from the qwen-code auth dialog. The endpoints below
// are transcribed from the qwen-code source (first-hand) but CANNOT be
// live-verified against a working subscription. The seam is complete and the
// Copilot path is fully live-testable; Qwen forwarding is PENDING-LIVE.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::warn;

use duduclaw_core::error::{DuDuClawError, Result};

// ── Provider configuration ───────────────────────────────────────────────────

/// Static configuration for one provider's device-authorization flow.
#[derive(Debug, Clone)]
pub struct DeviceFlowConfig {
    /// Provider id stored on the account (`"github"` for Copilot, `"qwen"`).
    pub provider_id: &'static str,
    /// Human-facing display name.
    pub display: &'static str,
    /// RFC 8628 device authorization endpoint.
    pub device_code_url: &'static str,
    /// Token endpoint (polled + used for refresh).
    pub token_url: &'static str,
    /// Public OAuth client id (documented; override via config).
    pub default_client_id: &'static str,
    /// Requested scope (space-delimited).
    pub scope: &'static str,
    /// Whether the flow uses PKCE (RFC 7636).
    pub uses_pkce: bool,
    /// `false` marks a PENDING-LIVE / unverified flow (warned at runtime).
    pub verified: bool,
}

/// GitHub Copilot device flow.
///
/// Public client id `Iv1.b507a08c87ecfe98` is the legacy VS Code / Copilot CLI
/// / OpenCode OAuth App id — the one GitHub authorizes for the
/// `copilot_internal/v2/token` exchange. Source: ericc-ch/copilot-api
/// `src/lib/api-config.ts` (`GITHUB_CLIENT_ID`). Not a secret: it is a public
/// OAuth *client* identifier shipped in every VS Code install.
pub const COPILOT: DeviceFlowConfig = DeviceFlowConfig {
    provider_id: "github",
    display: "GitHub Copilot",
    device_code_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    default_client_id: "Iv1.b507a08c87ecfe98",
    scope: "read:user",
    uses_pkce: false,
    verified: true,
};

/// Qwen Portal device flow. Endpoints + client id from QwenLM/qwen-code
/// `packages/core/src/qwen/qwenOAuth2.ts`. PENDING-LIVE (see module note).
pub const QWEN: DeviceFlowConfig = DeviceFlowConfig {
    provider_id: "qwen",
    display: "Qwen Portal",
    device_code_url: "https://chat.qwen.ai/api/v1/oauth2/device/code",
    token_url: "https://chat.qwen.ai/api/v1/oauth2/token",
    default_client_id: "f0304373b74a44d2b584a3fb70ca9e56",
    scope: "openid profile email model.completion",
    uses_pkce: true,
    verified: false,
};

/// Copilot chat/completions upstream (OpenAI-compatible). Source:
/// ericc-ch/copilot-api (individual-account base is `api.githubcopilot.com`).
pub const COPILOT_API_BASE: &str = "https://api.githubcopilot.com";
/// GitHub REST API base — used for the Copilot token exchange.
pub const GITHUB_API_BASE: &str = "https://api.github.com";

/// Resolve a CLI `--provider` value (`copilot`/`github`, `qwen`) to its config.
pub fn config_for(provider: &str) -> Option<DeviceFlowConfig> {
    match provider.to_ascii_lowercase().as_str() {
        "copilot" | "github" => Some(COPILOT),
        "qwen" | "qwen-portal" => Some(QWEN),
        _ => None,
    }
}

// ── Device-code response parsing (pure, unit-tested) ─────────────────────────

/// The device-authorization response the user must act on.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Optional pre-filled URL (`verification_uri_complete`).
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

/// Parse a device-authorization response body. Missing `interval` defaults to
/// the RFC 8628 minimum of 5s; missing `expires_in` to 900s (15 min).
pub fn parse_device_code(v: &Value) -> std::result::Result<DeviceCode, String> {
    let device_code = v
        .get("device_code")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("device 授權回應缺少 device_code")?
        .to_string();
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("device 授權回應缺少 user_code")?
        .to_string();
    let verification_uri = v
        .get("verification_uri")
        .or_else(|| v.get("verification_url"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("device 授權回應缺少 verification_uri")?
        .to_string();
    let verification_uri_complete = v
        .get("verification_uri_complete")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(900);
    let interval = v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5).max(1);
    Ok(DeviceCode {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval,
    })
}

/// Outcome of one token-poll.
#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    /// Keep polling at the current interval.
    Pending,
    /// Keep polling, but add 5s to the interval (RFC 8628 `slow_down`).
    SlowDown,
    /// The user authorized — carries the raw token response body.
    Authorized(Value),
    /// The user denied the request.
    Denied,
    /// The device code expired before authorization.
    Expired,
    /// A terminal error we cannot recover from.
    Error(String),
}

/// Interpret a token-endpoint poll response.
///
/// A body carrying a non-empty `access_token` is success. Otherwise the OAuth
/// `error` field drives the state machine. Unknown non-empty errors are
/// terminal (fail-closed) rather than an infinite pending loop.
pub fn interpret_token_response(v: &Value) -> PollOutcome {
    if v.get("access_token")
        .and_then(|x| x.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return PollOutcome::Authorized(v.clone());
    }
    match v.get("error").and_then(|x| x.as_str()).unwrap_or("") {
        "authorization_pending" => PollOutcome::Pending,
        "slow_down" => PollOutcome::SlowDown,
        "access_denied" => PollOutcome::Denied,
        "expired_token" => PollOutcome::Expired,
        "" => PollOutcome::Error("token 回應無 access_token 也無 error 欄位".to_string()),
        other => PollOutcome::Error(format!("裝置授權失敗：{other}")),
    }
}

/// Next poll interval after a `slow_down` (RFC 8628: add 5 seconds).
pub fn bump_interval(current: u64) -> u64 {
    current.saturating_add(5)
}

// ── PKCE (RFC 7636) ──────────────────────────────────────────────────────────

/// Generate a `(code_verifier, code_challenge)` pair using the S256 method.
///
/// The verifier is 32 random bytes base64url-encoded (no padding); the
/// challenge is the base64url SHA-256 of the verifier's ASCII bytes.
pub fn generate_pkce() -> (String, String) {
    use rand::Rng;
    use sha2::{Digest, Sha256};
    let raw: [u8; 32] = rand::thread_rng().r#gen();
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

// ── Token masking (never log a raw token) ────────────────────────────────────

/// Mask a token for logs: keep the first 4 chars, replace the rest with `*`
/// (CJK-safe — operates on chars, not bytes). Short tokens show only `*`.
pub fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    match chars.len() {
        0 => String::new(),
        1..=4 => "*".repeat(chars.len()),
        n => {
            let mut s: String = chars[..4].iter().collect();
            s.extend(std::iter::repeat('*').take(n - 4));
            s
        }
    }
}

// ── Stored seat credential ───────────────────────────────────────────────────

/// The credential we persist per provider (encrypted in `oauth_token_enc`).
///
/// - **Copilot** persists the long-lived GitHub OAuth `access_token` as a plain
///   string. The proxy exchanges it on-demand for a short-lived Copilot token.
/// - **Qwen** persists a JSON bundle (`access_token` + `refresh_token` +
///   `resource_url` + `expires_at`) because the Qwen access token is itself
///   short-lived and refreshed via the refresh token.
pub fn seat_credential_from_token_response(cfg: &DeviceFlowConfig, tok: &Value) -> String {
    if cfg.provider_id == "qwen" {
        // Persist the whole bundle so the proxy can refresh.
        let expires_at = tok
            .get("expires_in")
            .and_then(|x| x.as_u64())
            .map(|secs| now_unix().saturating_add(secs));
        json!({
            "access_token": tok.get("access_token").and_then(|x| x.as_str()).unwrap_or(""),
            "refresh_token": tok.get("refresh_token").and_then(|x| x.as_str()).unwrap_or(""),
            "resource_url": tok.get("resource_url").and_then(|x| x.as_str()).unwrap_or(""),
            "expires_at": expires_at,
        })
        .to_string()
    } else {
        // Copilot: the raw GitHub OAuth token.
        tok.get("access_token")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Config storage ───────────────────────────────────────────────────────────

/// Upsert a subscription-seat OAuth account into `config.toml [[accounts]]`.
///
/// The credential is stored AES-256-GCM encrypted in `oauth_token_enc` (same
/// keyfile pattern as every other stored token). Any existing account with the
/// same id is replaced (re-login refreshes the seat). Atomic temp+rename write.
async fn store_seat_account(
    home: &Path,
    id: &str,
    provider_id: &str,
    label: &str,
    credential: &str,
) -> Result<()> {
    let config_path = home.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path)
        .await
        .unwrap_or_default();
    let mut table: toml::Table = content
        .parse()
        .map_err(|e| DuDuClawError::Config(format!("config.toml 解析失敗：{e}")))?;

    let encrypted = crate::encrypt_api_key(credential, &home.to_path_buf()).ok_or_else(|| {
        DuDuClawError::Config("無法加密 seat 憑證（keyfile 產生失敗？）".to_string())
    })?;

    let mut account = toml::map::Map::new();
    account.insert("id".into(), toml::Value::String(id.into()));
    account.insert("type".into(), toml::Value::String("oauth".into()));
    account.insert("provider".into(), toml::Value::String(provider_id.into()));
    account.insert("label".into(), toml::Value::String(label.into()));
    account.insert("priority".into(), toml::Value::Integer(3));
    // Encrypted-only: never persist the plaintext seat credential.
    account.insert("oauth_token_enc".into(), toml::Value::String(encrypted));

    let accounts = table
        .entry("accounts".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let arr = accounts
        .as_array_mut()
        .ok_or_else(|| DuDuClawError::Config("config.toml [[accounts]] 格式錯誤".to_string()))?;
    // Upsert by id.
    arr.retain(|a| {
        a.as_table()
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            != Some(id)
    });
    arr.push(toml::Value::Table(account));

    let serialized = toml::to_string_pretty(&table)
        .map_err(|e| DuDuClawError::Config(format!("config.toml 序列化失敗：{e}")))?;
    let tmp = config_path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, serialized)
        .await
        .map_err(|e| DuDuClawError::Config(format!("寫入暫存 config 失敗：{e}")))?;
    tokio::fs::rename(&tmp, &config_path)
        .await
        .map_err(|e| DuDuClawError::Config(format!("提交 config 失敗：{e}")))?;
    Ok(())
}

// ── CLI entry point ──────────────────────────────────────────────────────────

/// Resolve the OAuth client id: CLI flag → `config.toml [auth.<provider>]
/// client_id` → the documented public default.
fn resolve_client_id(cfg: &DeviceFlowConfig, cli: Option<&str>, config: &toml::Table) -> String {
    if let Some(c) = cli.filter(|s| !s.is_empty()) {
        return c.to_string();
    }
    if let Some(c) = config
        .get("auth")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(cfg.provider_id))
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("client_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return c.to_string();
    }
    cfg.default_client_id.to_string()
}

/// Run `duduclaw auth device --provider <provider>`.
pub async fn run(provider: &str, cli_client_id: Option<String>, home: &Path) -> Result<()> {
    let cfg = config_for(provider).ok_or_else(|| {
        DuDuClawError::Config(format!(
            "未知的 provider `{provider}`（支援：copilot、qwen）"
        ))
    })?;

    let config: toml::Table = tokio::fs::read_to_string(home.join("config.toml"))
        .await
        .unwrap_or_default()
        .parse()
        .unwrap_or_default();
    let client_id = resolve_client_id(&cfg, cli_client_id.as_deref(), &config);

    if !cfg.verified {
        warn!(
            provider = cfg.provider_id,
            "此 provider 的 device flow 為 PENDING-LIVE（端點取自開源實作，未經實機驗證）"
        );
        println!(
            "⚠ {} 的裝置授權流程為 PENDING-LIVE：端點取自 qwen-code 原始碼，Qwen 免費 OAuth 已於 2026-04-15 停用，可能無法完成登入。",
            cfg.display
        );
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| DuDuClawError::Gateway(format!("HTTP client 建立失敗：{e}")))?;

    // PKCE (Qwen only).
    let pkce = if cfg.uses_pkce {
        Some(generate_pkce())
    } else {
        None
    };

    // 1. Request a device code.
    let mut form: Vec<(&str, String)> = vec![
        ("client_id", client_id.clone()),
        ("scope", cfg.scope.to_string()),
    ];
    if let Some((_, challenge)) = &pkce {
        form.push(("code_challenge", challenge.clone()));
        form.push(("code_challenge_method", "S256".to_string()));
    }
    let resp = client
        .post(cfg.device_code_url)
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("device code 請求失敗：{e}")))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("device code 回應非 JSON：{e}")))?;
    if !status.is_success() {
        return Err(DuDuClawError::Gateway(format!(
            "device code 端點回應 {status}：{body}"
        )));
    }
    let dc = parse_device_code(&body).map_err(DuDuClawError::Config)?;

    // 2. Prompt the user.
    println!("\n=== {} 裝置授權 ===", cfg.display);
    println!("1. 開啟瀏覽器： {}", dc.verification_uri);
    if let Some(complete) = &dc.verification_uri_complete {
        println!("   （或直接開啟已帶碼連結： {complete}）");
    }
    println!("2. 輸入使用者代碼： {}", dc.user_code);
    println!("等待授權中…（每 {}s 輪詢一次）\n", dc.interval);

    // 3. Poll for the token.
    let deadline = now_unix().saturating_add(dc.expires_in);
    let mut interval = dc.interval;
    let tok = loop {
        if now_unix() >= deadline {
            return Err(DuDuClawError::Gateway(
                "裝置代碼已逾期（未在時限內完成授權）".to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;

        let mut poll_form: Vec<(&str, String)> = vec![
            ("client_id", client_id.clone()),
            ("device_code", dc.device_code.clone()),
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
        ];
        if let Some((verifier, _)) = &pkce {
            poll_form.push(("code_verifier", verifier.clone()));
        }
        let presp = client
            .post(cfg.token_url)
            .header("Accept", "application/json")
            .form(&poll_form)
            .send()
            .await
            .map_err(|e| DuDuClawError::Gateway(format!("token 輪詢請求失敗：{e}")))?;
        let pbody: Value = presp.json().await.unwrap_or(Value::Null);
        match interpret_token_response(&pbody) {
            PollOutcome::Pending => continue,
            PollOutcome::SlowDown => {
                interval = bump_interval(interval);
                continue;
            }
            PollOutcome::Authorized(t) => break t,
            PollOutcome::Denied => {
                return Err(DuDuClawError::Gateway("使用者拒絕了授權請求".to_string()))
            }
            PollOutcome::Expired => {
                return Err(DuDuClawError::Gateway("裝置代碼已逾期".to_string()))
            }
            PollOutcome::Error(e) => return Err(DuDuClawError::Gateway(e)),
        }
    };

    // 4. Persist the seat credential (encrypted).
    let credential = seat_credential_from_token_response(&cfg, &tok);
    if credential.is_empty() {
        return Err(DuDuClawError::Gateway(
            "授權成功但回應未包含可用的 access_token".to_string(),
        ));
    }
    let id = format!("{}-seat", cfg.provider_id);
    store_seat_account(home, &id, cfg.provider_id, cfg.display, &credential).await?;

    println!(
        "\n✓ {} 座位已儲存（帳號 id `{}`，憑證加密於 config.toml）",
        cfg.display, id
    );
    println!(
        "  現在啟動 proxy 即可轉發此座位： duduclaw proxy --bind 127.0.0.1:8788"
    );
    if !cfg.verified {
        println!("  （提醒：Qwen 轉發為 PENDING-LIVE，尚未實機驗證）");
    }
    Ok(())
}

// ── Proxy-side seat forwarding ───────────────────────────────────────────────

/// In-memory cache of minted, short-lived Copilot tokens keyed by a hash of the
/// GitHub OAuth token. Refreshed when <5 minutes of validity remain.
#[derive(Default)]
pub struct CopilotTokenCache {
    inner: Mutex<HashMap<String, CachedCopilotToken>>,
}

#[derive(Clone)]
struct CachedCopilotToken {
    token: String,
    /// Unix expiry seconds.
    expires_at: u64,
}

/// Refresh when fewer than this many seconds of validity remain.
const COPILOT_REFRESH_MARGIN_SECS: u64 = 300;

impl CopilotTokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a valid Copilot token for the given GitHub OAuth token, minting a
    /// fresh one via `api.github.com/copilot_internal/v2/token` when the cache
    /// is cold or the cached token is within the refresh margin of expiry.
    ///
    /// Fail-closed: a mint failure returns `Err` (the proxy then 502s) — it
    /// never falls through to another provider.
    pub async fn get_or_mint(
        &self,
        client: &reqwest::Client,
        github_token: &str,
    ) -> std::result::Result<String, String> {
        let key = cache_key(github_token);
        {
            let guard = self.inner.lock().await;
            if let Some(c) = guard.get(&key) {
                if c.expires_at > now_unix().saturating_add(COPILOT_REFRESH_MARGIN_SECS) {
                    return Ok(c.token.clone());
                }
            }
        }
        let (token, expires_at) = mint_copilot_token(client, github_token).await?;
        let mut guard = self.inner.lock().await;
        guard.insert(
            key,
            CachedCopilotToken {
                token: token.clone(),
                expires_at,
            },
        );
        Ok(token)
    }
}

/// A stable, non-reversible cache key for a token (never store the raw token as
/// a map key so a heap dump doesn't trivially leak it).
fn cache_key(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Exchange a GitHub OAuth token for a short-lived Copilot token.
///
/// `GET https://api.github.com/copilot_internal/v2/token` with
/// `Authorization: token <github_oauth_token>`. Response carries `token` and
/// `expires_at` (unix seconds). Source: ericc-ch/copilot-api
/// `src/services/github/get-copilot-token.ts`.
pub async fn mint_copilot_token(
    client: &reqwest::Client,
    github_token: &str,
) -> std::result::Result<(String, u64), String> {
    let url = format!("{GITHUB_API_BASE}/copilot_internal/v2/token");
    let resp = client
        .get(&url)
        .header("Authorization", format!("token {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.99.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .send()
        .await
        .map_err(|e| format!("Copilot token 交換請求失敗：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Copilot token 回應非 JSON：{e}"))?;
    if !status.is_success() {
        return Err(format!("Copilot token 交換失敗（{status}）"));
    }
    let token = body
        .get("token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("Copilot token 回應缺少 token 欄位")?
        .to_string();
    let expires_at = body
        .get("expires_at")
        .and_then(|x| x.as_u64())
        // A missing expiry is treated as a short 25-min TTL so we re-mint soon.
        .unwrap_or_else(|| now_unix().saturating_add(1500));
    Ok((token, expires_at))
}

/// Headers required by the Copilot chat/completions upstream. Source:
/// ericc-ch/copilot-api `copilotHeaders()`.
pub fn copilot_chat_headers(copilot_token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {copilot_token}")),
        ("Content-Type", "application/json".to_string()),
        ("Editor-Version", "vscode/1.99.0".to_string()),
        ("Editor-Plugin-Version", "copilot-chat/0.26.7".to_string()),
        ("Copilot-Integration-Id", "vscode-chat".to_string()),
        ("User-Agent", "GitHubCopilotChat/0.26.7".to_string()),
        ("X-GitHub-Api-Version", "2025-04-01".to_string()),
    ]
}

/// The Copilot chat/completions endpoint URL.
pub fn copilot_completions_url() -> String {
    format!("{COPILOT_API_BASE}/chat/completions")
}

/// A curated set of model ids advertised for a seat provider, tagged with the
/// provider prefix so the proxy's `resolve_provider_and_model` routes them
/// back. Only surfaced when a live seat exists (fail-closed catalogue).
pub fn seat_model_ids(provider_id: &str) -> &'static [&'static str] {
    match provider_id {
        "github" => &[
            "gpt-4o",
            "gpt-4.1",
            "o3-mini",
            "claude-3.5-sonnet",
            "claude-sonnet-4",
            "gemini-2.0-flash-001",
        ],
        "qwen" => &["qwen3-coder-plus", "qwen3-coder-flash", "qwen-max-latest"],
        _ => &[],
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_for_aliases() {
        assert_eq!(config_for("copilot").unwrap().provider_id, "github");
        assert_eq!(config_for("github").unwrap().provider_id, "github");
        assert_eq!(config_for("qwen").unwrap().provider_id, "qwen");
        assert!(config_for("bogus").is_none());
    }

    #[test]
    fn copilot_is_verified_qwen_is_pending() {
        assert!(COPILOT.verified);
        assert!(!QWEN.verified, "Qwen must be flagged PENDING-LIVE");
        assert!(QWEN.uses_pkce);
        assert!(!COPILOT.uses_pkce);
    }

    #[test]
    fn parse_device_code_defaults_and_fields() {
        let v = json!({
            "device_code": "dc123",
            "user_code": "WXYZ-1234",
            "verification_uri": "https://github.com/login/device"
        });
        let dc = parse_device_code(&v).unwrap();
        assert_eq!(dc.device_code, "dc123");
        assert_eq!(dc.user_code, "WXYZ-1234");
        assert_eq!(dc.interval, 5, "missing interval defaults to 5");
        assert_eq!(dc.expires_in, 900, "missing expires_in defaults to 900");
        assert!(dc.verification_uri_complete.is_none());
    }

    #[test]
    fn parse_device_code_accepts_verification_url_alias_and_complete() {
        let v = json!({
            "device_code": "dc",
            "user_code": "uc",
            "verification_url": "https://chat.qwen.ai/authorize",
            "verification_uri_complete": "https://chat.qwen.ai/authorize?code=uc",
            "interval": 8,
            "expires_in": 600
        });
        let dc = parse_device_code(&v).unwrap();
        assert_eq!(dc.verification_uri, "https://chat.qwen.ai/authorize");
        assert_eq!(
            dc.verification_uri_complete.as_deref(),
            Some("https://chat.qwen.ai/authorize?code=uc")
        );
        assert_eq!(dc.interval, 8);
    }

    #[test]
    fn parse_device_code_missing_required_fields_errors() {
        assert!(parse_device_code(&json!({ "user_code": "x", "verification_uri": "y" })).is_err());
        assert!(parse_device_code(&json!({ "device_code": "x", "verification_uri": "y" })).is_err());
        assert!(parse_device_code(&json!({ "device_code": "x", "user_code": "y" })).is_err());
    }

    #[test]
    fn interpret_token_response_states() {
        assert_eq!(
            interpret_token_response(&json!({ "access_token": "gho_xxx" })),
            PollOutcome::Authorized(json!({ "access_token": "gho_xxx" }))
        );
        assert_eq!(
            interpret_token_response(&json!({ "error": "authorization_pending" })),
            PollOutcome::Pending
        );
        assert_eq!(
            interpret_token_response(&json!({ "error": "slow_down" })),
            PollOutcome::SlowDown
        );
        assert_eq!(
            interpret_token_response(&json!({ "error": "access_denied" })),
            PollOutcome::Denied
        );
        assert_eq!(
            interpret_token_response(&json!({ "error": "expired_token" })),
            PollOutcome::Expired
        );
        // Unknown error is terminal, not an infinite pending loop.
        assert!(matches!(
            interpret_token_response(&json!({ "error": "incorrect_client_credentials" })),
            PollOutcome::Error(_)
        ));
        // Empty body is terminal.
        assert!(matches!(
            interpret_token_response(&json!({})),
            PollOutcome::Error(_)
        ));
        // Empty access_token is NOT success.
        assert!(matches!(
            interpret_token_response(&json!({ "access_token": "" })),
            PollOutcome::Error(_)
        ));
    }

    #[test]
    fn bump_interval_adds_five() {
        assert_eq!(bump_interval(5), 10);
        assert_eq!(bump_interval(u64::MAX), u64::MAX, "saturates");
    }

    #[test]
    fn pkce_challenge_is_deterministic_sha256_of_verifier() {
        use sha2::{Digest, Sha256};
        let (verifier, challenge) = generate_pkce();
        // Recompute the challenge from the verifier and compare.
        let expect =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expect);
        // base64url no-pad: no '+', '/', or '=' characters.
        assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
    }

    #[test]
    fn mask_token_keeps_prefix_only() {
        assert_eq!(mask_token(""), "");
        assert_eq!(mask_token("abcd"), "****");
        assert_eq!(mask_token("gho_secret"), "gho_******");
        // CJK-safe: operates on chars (6 chars → keep 4, mask 2).
        assert_eq!(mask_token("金鑰值一二三"), "金鑰值一**");
    }

    #[test]
    fn copilot_seat_credential_is_raw_github_token() {
        let tok = json!({ "access_token": "gho_abc", "refresh_token": "ignored" });
        assert_eq!(
            seat_credential_from_token_response(&COPILOT, &tok),
            "gho_abc"
        );
    }

    #[test]
    fn qwen_seat_credential_is_json_bundle() {
        let tok = json!({
            "access_token": "qw_access",
            "refresh_token": "qw_refresh",
            "resource_url": "https://portal.qwen.ai/v1",
            "expires_in": 3600
        });
        let bundle = seat_credential_from_token_response(&QWEN, &tok);
        let parsed: Value = serde_json::from_str(&bundle).unwrap();
        assert_eq!(parsed["access_token"], "qw_access");
        assert_eq!(parsed["refresh_token"], "qw_refresh");
        assert_eq!(parsed["resource_url"], "https://portal.qwen.ai/v1");
        assert!(parsed["expires_at"].as_u64().is_some());
    }

    #[test]
    fn resolve_client_id_precedence() {
        let empty = toml::Table::new();
        // CLI flag wins.
        assert_eq!(
            resolve_client_id(&COPILOT, Some("cli-id"), &empty),
            "cli-id"
        );
        // Default when nothing set.
        assert_eq!(
            resolve_client_id(&COPILOT, None, &empty),
            COPILOT.default_client_id
        );
        // Config override.
        let cfg: toml::Table = "[auth.github]\nclient_id = \"cfg-id\"\n".parse().unwrap();
        assert_eq!(resolve_client_id(&COPILOT, None, &cfg), "cfg-id");
    }

    #[test]
    fn seat_model_ids_only_for_known_providers() {
        assert!(!seat_model_ids("github").is_empty());
        assert!(!seat_model_ids("qwen").is_empty());
        assert!(seat_model_ids("anthropic").is_empty());
    }

    #[test]
    fn cache_key_is_stable_and_not_reversible() {
        let k1 = cache_key("gho_secret");
        let k2 = cache_key("gho_secret");
        assert_eq!(k1, k2);
        assert_ne!(k1, "gho_secret");
        assert!(!k1.contains("secret"));
    }

    #[test]
    fn copilot_headers_carry_editor_identity_and_bearer() {
        let h = copilot_chat_headers("cop_tok");
        let map: std::collections::HashMap<_, _> = h.into_iter().collect();
        assert_eq!(map.get("Authorization").unwrap(), "Bearer cop_tok");
        assert_eq!(map.get("Copilot-Integration-Id").unwrap(), "vscode-chat");
        assert!(map.contains_key("Editor-Version"));
    }
}
