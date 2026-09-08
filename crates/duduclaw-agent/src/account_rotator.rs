//! Unified account rotation for Claude Code SDK.
//!
//! Supports two authentication methods:
//! - **OAuth accounts**: Claude Pro/Team/Max subscriptions via `~/.claude/.credentials.json`
//!   Each profile has its own credentials directory at `~/.claude/profiles/<name>/`
//! - **API Key accounts**: Direct Anthropic API keys via `ANTHROPIC_API_KEY` env var
//!
//! The rotator selects the best account and provides the appropriate env vars
//! for the `claude` CLI subprocess.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use duduclaw_security::secret_manager::SecretManagerConfig;
use duduclaw_security::secret_ref::SecretRef;
use zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::credential_probe::{
    ANTHROPIC_API_BASE, CredentialKind, CredentialProbe, probe_anthropic_credential_at,
};

// ── Types ───────────────────────────────────────────────────

/// Why an account's authentication is dead (2026-09 hardening, D2).
///
/// Split because the two need different operator action: an invalid token is
/// re-issued (`claude setup-token`), an org-disabled one cannot be fixed by
/// the account holder at all. Both are terminal until a human intervenes —
/// neither heals by waiting, which is exactly why the old
/// "3 errors → 2-min cooldown → resurrect" cycle burned a spawn per cron tick
/// for 18 hours on 2026-09-08.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailureKind {
    /// The credential was rejected (HTTP 401): expired, revoked, malformed, or
    /// a short-lived `sk-ant-at01-` access token used as an account credential.
    InvalidToken,
    /// The credential authenticates but the organization has disabled this
    /// access path (HTTP 403 `oauth_not_allowed_for_organization`).
    OrgDisabled,
}

impl std::fmt::Display for AuthFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidToken => "invalid_token",
            Self::OrgDisabled => "org_disabled",
        })
    }
}

/// What we currently know about an account's credential (2026-09 hardening,
/// D5). Orthogonal to `is_healthy` / `cooldown_until`, which describe *usage*
/// outcomes; this describes the credential itself.
///
/// Serializes as a flat lowercase snake string (`"ok"`, `"unverified"`,
/// `"broken"`, `"auth_dead"`) for `accounts.list`; the failure kind travels
/// alongside it via [`CredentialState::credential_detail`] and the richer
/// [`Display`](std::fmt::Display) token (`auth_dead:org_disabled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialState {
    /// Proven good — a real request (or a real probe) succeeded with it.
    Ok,
    /// Loaded but never exercised. The honest default: we have a credential,
    /// we have not yet seen it work.
    #[default]
    Unverified,
    /// The stored credential could not be turned into a usable secret
    /// (undecryptable `*_enc`, or decrypted to an empty string). Never
    /// selectable — waiting cannot fix it; only re-saving the credential can,
    /// and that rebuilds the rotator.
    Broken,
    /// A real authentication failure was observed. Comes back only via
    /// cooldown expiry (one retry), never via a health probe's `Valid`-less
    /// signal.
    AuthDead(AuthFailureKind),
}

impl std::fmt::Display for CredentialState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::Unverified => f.write_str("unverified"),
            Self::Broken => f.write_str("broken"),
            Self::AuthDead(kind) => write!(f, "auth_dead:{kind}"),
        }
    }
}

impl Serialize for CredentialState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Self::Ok => "ok",
            Self::Unverified => "unverified",
            Self::Broken => "broken",
            Self::AuthDead(_) => "auth_dead",
        })
    }
}

impl CredentialState {
    /// One-line, operator-facing (zh-TW) explanation of a bad state, or `None`
    /// when there is nothing to explain.
    ///
    /// Written for the dashboard account card — it names the fix, not the
    /// internal mechanism.
    pub fn credential_detail(&self) -> Option<&'static str> {
        match self {
            Self::Ok | Self::Unverified => None,
            Self::Broken => Some("憑證無法解密或為空，請檢查 ~/.duduclaw/.keyfile 後重新儲存憑證"),
            Self::AuthDead(AuthFailureKind::InvalidToken) => {
                Some("token 無效（401），請重新執行 `claude setup-token` 並更新此帳號")
            }
            Self::AuthDead(AuthFailureKind::OrgDisabled) => {
                Some("此組織已停用 Claude Code 訂閱存取（403），請改用 API key 或洽組織管理員")
            }
        }
    }

    /// Whether this state permanently bars the account from selection.
    ///
    /// `AuthDead` is deliberately NOT blocking: it is bounded by a cooldown so
    /// a re-issued token starts working again without an operator restart.
    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Broken)
    }
}

/// Authentication method for a Claude Code SDK account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Anthropic API key (pay-per-token)
    ApiKey,
    /// Claude.ai OAuth session (subscription-based: Pro/Team/Max)
    OAuth,
}

/// Default provider for an account when `[[accounts]] provider` is absent.
///
/// Historically the rotator was Anthropic-only, so every existing config
/// (which never specified `provider`) must continue to behave as an Anthropic
/// account. This default preserves that byte-identical behavior.
fn default_provider() -> String {
    "anthropic".to_string()
}

/// An account that can be used for Claude CLI invocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub auth_method: AuthMethod,
    /// LLM provider this account authenticates against ("anthropic", "openai",
    /// "gemini", "deepseek", ...). Absent in config → "anthropic" for
    /// back-compat. Rotation/budget/cooldown are all applied *within* a
    /// provider's pool via [`AccountRotator::select_for_provider`].
    #[serde(default = "default_provider")]
    pub provider: String,
    pub priority: u32,
    pub monthly_budget_cents: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    /// For OAuth: profile directory name (e.g. "default", "work")
    #[serde(default)]
    pub profile: String,
    /// For OAuth: email associated with the account
    #[serde(default)]
    pub email: String,
    /// For OAuth: subscription type (pro, team, max)
    #[serde(default)]
    pub subscription: String,
    /// For OAuth: user-visible label (e.g., "工作帳號")
    #[serde(default)]
    pub label: String,
    /// OAuth token expiry (ISO 8601). Accounts past expiry are marked unhealthy.
    #[serde(default)]
    pub expires_at: Option<String>,
    // Runtime state (not persisted in config)
    #[serde(skip)]
    pub api_key: String,
    /// OAuth token from `setup-token` (decrypted at runtime from oauth_token_enc).
    /// When set, injected as CLAUDE_CODE_OAUTH_TOKEN env var.
    /// When empty (default account), CLI uses OS keychain auth.
    #[serde(skip)]
    pub oauth_token: Option<String>,
    #[serde(skip)]
    pub credentials_dir: Option<PathBuf>,
    #[serde(skip)]
    pub is_healthy: bool,
    #[serde(skip)]
    pub consecutive_errors: u32,
    #[serde(skip)]
    pub spent_this_month: u64,
    #[serde(skip)]
    pub cooldown_until: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub last_used: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub total_requests: u64,
    /// What we know about this account's credential (2026-09 hardening).
    /// Loaded accounts start [`CredentialState::Unverified`]; a real success
    /// or a `Valid` probe promotes to `Ok`.
    #[serde(skip)]
    pub credential_state: CredentialState,
    /// How many consecutive authentication failures this account has taken.
    /// Drives the exponential auth-dead backoff (15 min → 6 h cap); reset by
    /// [`AccountRotator::on_success`].
    #[serde(skip)]
    pub auth_dead_strikes: u32,
    /// Earliest moment the health cycle may credential-probe this account
    /// again. `None` — the default — means "probe on the next tick", i.e. the
    /// behaviour that existed before the probe schedule.
    ///
    /// Set only after a *conclusive* probe failure (401 / 403). Without it a
    /// token Anthropic keeps rejecting was re-probed every 60 s forever:
    /// free in dollars, but pointless traffic and one alarming log line a
    /// minute. Inconclusive probes (429 / transport) deliberately leave it
    /// alone so an API outage cannot silently slow down recovery.
    #[serde(skip)]
    pub next_probe_at: Option<DateTime<Utc>>,
    /// Consecutive conclusive probe failures — drives [`probe_backoff`].
    ///
    /// Distinct from [`auth_dead_strikes`](Self::auth_dead_strikes), which
    /// counts *observed spawn* auth failures and schedules the rotation
    /// cooldown. This one schedules only the probe. Reset by a `Valid` probe
    /// and by [`AccountRotator::on_success`]; deliberately NOT bumped by
    /// [`AccountRotator::on_auth_failed`], whose whole point is to get the
    /// next tick to classify the freshly-observed failure.
    #[serde(skip)]
    pub probe_failures: u32,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.api_key.zeroize();
        if let Some(ref mut token) = self.oauth_token {
            token.zeroize();
        }
    }
}

impl Account {
    pub fn is_available(&self) -> bool {
        // D4 hard filter: a credential we could not even decrypt is never a
        // rotation candidate, regardless of health/cooldown. Checked FIRST so
        // no later "recovery" branch can talk its way past it — the 2026-09-08
        // incident's second half was a rotator that happily spawned
        // credential-less children from an `oauth_token_enc` that decrypted to
        // an empty string.
        if self.credential_state.is_blocking() {
            return false;
        }
        if !self.is_healthy {
            // Allow recovery after cooldown expires (e.g., billing-exhausted 24h).
            // Without this, is_healthy=false + expired cooldown = permanently dead.
            let cooldown_expired = self
                .cooldown_until
                .is_some_and(|cd| Utc::now() >= cd);
            if !cooldown_expired {
                return false;
            }
        }
        // API key accounts have budget enforcement
        if self.auth_method == AuthMethod::ApiKey
            && self.spent_this_month >= self.monthly_budget_cents
        {
            return false;
        }
        // Check cooldown (active, not yet expired)
        if self.cooldown_until.is_some_and(|cd| Utc::now() < cd) {
            return false;
        }
        // Check token expiry for OAuth accounts
        if let Some(ref exp) = self.expires_at
            && let Ok(expiry) = exp.parse::<DateTime<Utc>>()
                && Utc::now() > expiry {
                    return false;
                }
        match self.auth_method {
            AuthMethod::ApiKey => !self.api_key.is_empty(),
            AuthMethod::OAuth => {
                if self.provider == "anthropic" {
                    // Claude.ai subscription: needs an explicit setup-token
                    // (CLAUDE_CODE_OAUTH_TOKEN) or a credentials dir (OS keychain).
                    self.oauth_token.is_some() || self.credentials_dir.is_some()
                } else {
                    // Subscription OAuth for a non-Anthropic provider (ChatGPT
                    // Codex / GitHub Copilot / Qwen Portal). Token acquisition is
                    // runtime-managed — the Codex runtime inherits the host
                    // ChatGPT login, so there is no local token/dir to check.
                    // Availability is governed by health / cooldown / expiry
                    // (checked above); a live seat is available by default.
                    true
                }
            }
        }
    }

    /// Days until token expires. Returns None if no expiry set.
    pub fn days_until_expiry(&self) -> Option<i64> {
        let exp = self.expires_at.as_ref()?;
        let expiry = exp.parse::<DateTime<Utc>>().ok()?;
        Some((expiry - Utc::now()).num_days())
    }
}

/// Environment variables to set when invoking a CLI/subprocess for a given
/// account, plus enough metadata for a direct-API caller (e.g. `duduclaw-llm`)
/// to authenticate without spawning a subprocess.
#[derive(Debug, Clone)]
pub struct AccountEnv {
    pub id: String,
    pub auth_method: AuthMethod,
    /// Provider this selection belongs to ("anthropic", "openai", ...).
    pub provider: String,
    /// Raw API key for direct-API callers. `Some` for API-key accounts (any
    /// provider); `None` for OAuth accounts (which have no static key).
    pub raw_key: Option<String>,
    /// Stored subscription-seat credential for a **non-Anthropic OAuth** seat
    /// (the long-lived GitHub OAuth token for Copilot; the Qwen token bundle).
    /// `None` for API-key accounts and for Anthropic OAuth (whose token is
    /// injected as an env var / keychain instead). A direct-API caller must NOT
    /// treat this as an API key — it is a seat credential the proxy exchanges
    /// for a short-lived upstream token. See `duduclaw proxy` seat forwarding.
    pub seat_token: Option<String>,
    /// Env vars to set on the subprocess
    pub env_vars: HashMap<String, String>,
}

/// Rotation strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RotationStrategy {
    RoundRobin,
    LeastCost,
    Failover,
    Priority,
}

impl RotationStrategy {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "round_robin" => Self::RoundRobin,
            "least_cost" => Self::LeastCost,
            "failover" => Self::Failover,
            _ => Self::Priority,
        }
    }
}

/// WP10 M4 — coarse reason the rotator has nothing to hand out, used purely to
/// pick the right recovery horizon in the user-facing message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// A billing-class cooldown is active (24 h) — recovery is hours away.
    LongCooldown,
    /// Rate-limit or transient-error cooldown — recovery is minutes away.
    ShortCooldown,
    /// Cannot attribute it to a cooldown; the caller must hedge.
    Unknown,
}

/// Public status for monitoring.
///
/// `Default` is implemented so a caller constructing one field-by-field (the
/// gateway's `account_status_to_json` test) can use `..Default::default()` and
/// survive future field additions.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountStatus {
    pub id: String,
    pub auth_method: String,
    /// LLM provider this account authenticates against ("anthropic", "openai",
    /// "gemini", "deepseek", ...) — see [`Account::provider`]. Surfaced so
    /// `accounts.list` can show/filter by provider (WP-A).
    pub provider: String,
    pub priority: u32,
    pub is_healthy: bool,
    pub spent_this_month: u64,
    pub monthly_budget_cents: u64,
    pub total_requests: u64,
    pub is_available: bool,
    pub email: String,
    pub subscription: String,
    pub label: String,
    pub expires_at: Option<String>,
    pub days_until_expiry: Option<i64>,
    /// Credential state as a flat string (`ok` / `unverified` / `broken` /
    /// `auth_dead`) — see [`CredentialState`].
    pub credential_state: CredentialState,
    /// zh-TW one-liner explaining a bad `credential_state`, or `None`.
    pub credential_detail: Option<&'static str>,
    /// Consecutive authentication failures (drives the auth-dead backoff).
    pub auth_dead_strikes: u32,
    /// RFC 3339 timestamp of the earliest next credential probe, or `None`
    /// when the next health tick may probe. See [`Account::next_probe_at`].
    pub next_probe_at: Option<String>,
    /// Consecutive conclusive probe failures (drives the probe backoff).
    pub probe_failures: u32,
}

// ── AccountRotator ──────────────────────────────────────────

/// Base auth-dead cooldown; doubles per consecutive strike up to
/// [`AUTH_DEAD_CAP_MINUTES`].
const AUTH_DEAD_BASE_MINUTES: i64 = 15;

/// Ceiling for any auth-dead / probe-failure cooldown (6 hours).
const AUTH_DEAD_CAP_MINUTES: i64 = 6 * 60;

/// Cooldown for the `strikes`-th consecutive authentication failure:
/// `min(15 min × 2^(strikes-1), 6 h)`.
///
/// Pure so the backoff ladder is testable without a clock. `strikes == 0`
/// (never expected — callers increment first) is treated as the first strike.
pub(crate) fn auth_dead_backoff(strikes: u32) -> chrono::Duration {
    // 2^16 × 15 min is already three orders of magnitude past the cap; the
    // clamp exists purely so the shift can never overflow.
    let exp = strikes.saturating_sub(1).min(16);
    let minutes = AUTH_DEAD_BASE_MINUTES.saturating_mul(1i64 << exp);
    chrono::Duration::minutes(minutes.min(AUTH_DEAD_CAP_MINUTES))
}

/// Base delay before a conclusively-dead credential is probed again.
const PROBE_BACKOFF_BASE_MINUTES: i64 = 1;

/// Ceiling for the probe schedule (30 minutes). Much shorter than
/// [`AUTH_DEAD_CAP_MINUTES`] on purpose: a probe is free, so the only thing
/// being rationed here is noise, and a re-issued token should still be noticed
/// within half an hour without an operator restarting anything.
const PROBE_BACKOFF_CAP_MINUTES: i64 = 30;

/// Delay before the `failures`-th consecutive **conclusive** probe failure is
/// re-probed: `min(1 min × 2^(failures-1), 30 min)` — 1, 2, 4, 8, 16, 30, 30…
///
/// Pure so the ladder is testable without a clock. `failures == 0` (never
/// expected — callers increment first) is treated as the first failure.
pub(crate) fn probe_backoff(failures: u32) -> chrono::Duration {
    // 2^16 min is already three orders of magnitude past the cap; the clamp
    // exists purely so the shift can never overflow.
    let exp = failures.saturating_sub(1).min(16);
    let minutes = PROBE_BACKOFF_BASE_MINUTES.saturating_mul(1i64 << exp);
    chrono::Duration::minutes(minutes.min(PROBE_BACKOFF_CAP_MINUTES))
}

pub struct AccountRotator {
    accounts: Arc<RwLock<Vec<Account>>>,
    strategy: RotationStrategy,
    round_robin_index: Arc<RwLock<usize>>,
    cooldown_seconds: u64,
    /// API base the health probe authenticates against. Real Anthropic in
    /// production; a local listener under test (see
    /// [`with_probe_base_url`](Self::with_probe_base_url)).
    probe_base_url: String,
}

impl AccountRotator {
    pub fn new(strategy: RotationStrategy, cooldown_seconds: u64) -> Self {
        Self {
            accounts: Arc::new(RwLock::new(Vec::new())),
            strategy,
            round_robin_index: Arc::new(RwLock::new(0)),
            cooldown_seconds,
            probe_base_url: ANTHROPIC_API_BASE.to_string(),
        }
    }

    /// Point the credential health probe at a different API base.
    ///
    /// Builder-style; production never calls this (the default is the real
    /// Anthropic API). Tests use it to drive `probe_and_restore` against a
    /// local listener with no network access.
    pub fn with_probe_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.probe_base_url = base_url.into();
        self
    }

    /// Load accounts from config.toml + detect OAuth sessions from ~/.claude/
    pub async fn load_from_config(&self, home_dir: &Path) -> Result<usize, String> {
        let config_path = home_dir.join("config.toml");
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_default();
        let table: toml::Table = content.parse().unwrap_or_default();

        let mut loaded = Vec::new();

        // 1. Load API key accounts from [[accounts]]
        if let Some(accs) = table.get("accounts").and_then(|v| v.as_array()) {
            for acc in accs {
                if let Some(acc_table) = acc.as_table() {
                    let id = acc_table.get("id").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    let auth_type = acc_table.get("type").and_then(|v| v.as_str()).unwrap_or("api_key");

                    if auth_type == "api_key" {
                        let api_key = resolve_api_key(home_dir, acc_table).await;
                        // D4: an entry that *declares* an encrypted key but
                        // resolves to nothing is BROKEN, not absent. Skipping
                        // it (the old behavior) hid a wrong/rotated `.keyfile`
                        // behind a silently smaller pool.
                        let broken =
                            api_key.is_empty() && has_nonempty_field(acc_table, API_KEY_ENC_FIELDS);
                        if broken {
                            warn!(
                                account = id,
                                reason = "api_key_enc present but decrypted to nothing",
                                "Account credential is BROKEN — it will never be selected. \
                                 Check `~/.duduclaw/.keyfile` (was it regenerated or copied \
                                 from another machine?) and re-save this account's API key."
                            );
                        } else if api_key.is_empty() {
                            continue;
                        }
                        let provider = acc_table
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("anthropic")
                            .to_string();
                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::ApiKey,
                            provider,
                            priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(10) as u32,
                            monthly_budget_cents: acc_table.get("monthly_budget_cents").and_then(|v| v.as_integer()).unwrap_or(5000) as u64,
                            tags: Vec::new(),
                            profile: String::new(),
                            email: String::new(),
                            subscription: String::new(),
                            label: acc_table.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            expires_at: None,
                            api_key,
                            oauth_token: None,
                            credentials_dir: None,
                            is_healthy: !broken,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
                            credential_state: if broken {
                                CredentialState::Broken
                            } else {
                                CredentialState::Unverified
                            },
                            auth_dead_strikes: 0,
                            next_probe_at: None,
                            probe_failures: 0,
                        });
                    } else if auth_type == "oauth" {
                        let profile = acc_table.get("profile").and_then(|v| v.as_str()).unwrap_or("default");
                        // Subscription source. Absent → "anthropic" (Claude.ai),
                        // preserving byte-identical behavior for every existing
                        // config. A non-anthropic value (openai/github/qwen) marks
                        // a consumer subscription seat from another provider.
                        let provider = acc_table
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("anthropic")
                            .to_string();
                        let email = acc_table.get("email").and_then(|v| v.as_str()).unwrap_or("");
                        let sub = acc_table.get("subscription").and_then(|v| v.as_str()).unwrap_or("");
                        let label = acc_table.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        let expires_at = acc_table.get("expires_at").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let creds_dir = resolve_oauth_credentials(profile);
                        let oauth_token = resolve_oauth_token(home_dir, acc_table).await;

                        // D4: `oauth_token_enc` declared but resolving to
                        // None/"" is the exact trap `detect_default_oauth_session`'s
                        // own doc comment describes and nothing enforced — the
                        // account loaded normally and spawned credential-less
                        // children with zero warnings. An OAuth entry with NO
                        // `_enc` field is not broken: it legitimately relies on
                        // the OS keychain (behavior unchanged).
                        let token_usable =
                            oauth_token.as_ref().is_some_and(|t| !t.trim().is_empty());
                        let broken =
                            !token_usable && has_nonempty_field(acc_table, OAUTH_TOKEN_ENC_FIELDS);
                        if broken {
                            warn!(
                                account = id,
                                reason = "oauth_token_enc present but decrypted to nothing",
                                "Account credential is BROKEN — it will never be selected. \
                                 Check `~/.duduclaw/.keyfile` (was it regenerated or copied \
                                 from another machine?) and re-save this account's token \
                                 (`claude setup-token`)."
                            );
                        }
                        // An empty-string token must never reach the spawn env
                        // as `CLAUDE_CODE_OAUTH_TOKEN=`; normalize it away.
                        let oauth_token = if token_usable { oauth_token } else { None };

                        let has_auth = oauth_token.is_some() || creds_dir.is_some();

                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::OAuth,
                            provider,
                            priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(5) as u32,
                            monthly_budget_cents: 0,
                            tags: Vec::new(),
                            profile: profile.to_string(),
                            email: email.to_string(),
                            subscription: sub.to_string(),
                            label: label.to_string(),
                            expires_at,
                            api_key: String::new(),
                            oauth_token,
                            credentials_dir: creds_dir,
                            is_healthy: has_auth && !broken,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
                            credential_state: if broken {
                                CredentialState::Broken
                            } else {
                                CredentialState::Unverified
                            },
                            auth_dead_strikes: 0,
                            next_probe_at: None,
                            probe_failures: 0,
                        });
                    }
                }
            }
        }

        // 2. Auto-detect default OAuth session via `claude auth status`.
        //
        // Gate on an *Anthropic* OAuth account specifically — a foreign-provider
        // OAuth seat (copilot / qwen / codex added via `duduclaw auth device`)
        // must NOT suppress the Anthropic host-login auto-detect, or the
        // anthropic pool ends up empty and every channel reply fails NoAccounts.
        if should_autodetect_anthropic_oauth(&loaded) {
            // Use spawn_blocking to avoid holding a tokio worker thread
            // while waiting for the `claude` CLI subprocess.
            let detected = tokio::task::spawn_blocking(detect_default_oauth_session)
                .await
                .ok()
                .flatten();
            if let Some(creds) = detected {
                loaded.push(creds);
            }
        }

        // 3. Fallback: single API key from [api] or env var
        if loaded.is_empty()
            && let Some(api) = table.get("api").and_then(|v| v.as_table()) {
                let api_key = resolve_api_key(home_dir, api).await;
                if !api_key.is_empty() {
                    loaded.push(Account {
                        id: "main".to_string(),
                        auth_method: AuthMethod::ApiKey,
                        provider: "anthropic".to_string(),
                        priority: 1,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        profile: String::new(),
                        email: String::new(),
                        subscription: String::new(),
                        label: String::new(),
                        expires_at: None,
                        api_key,
                        oauth_token: None,
                        credentials_dir: None,
                        is_healthy: true,
                        consecutive_errors: 0,
                        spent_this_month: 0,
                        cooldown_until: None,
                        last_used: None,
                        total_requests: 0,
                        credential_state: CredentialState::Unverified,
                        auth_dead_strikes: 0,
                        next_probe_at: None,
                        probe_failures: 0,
                    });
                }
            }

        if loaded.is_empty()
            && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
                && !key.is_empty() {
                    loaded.push(Account {
                        id: "env".to_string(),
                        auth_method: AuthMethod::ApiKey,
                        provider: "anthropic".to_string(),
                        priority: 99,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        profile: String::new(),
                        email: String::new(),
                        subscription: String::new(),
                        label: "環境變數".to_string(),
                        expires_at: None,
                        api_key: key,
                        oauth_token: None,
                        credentials_dir: None,
                        is_healthy: true,
                        consecutive_errors: 0,
                        spent_this_month: 0,
                        cooldown_until: None,
                        last_used: None,
                        total_requests: 0,
                        credential_state: CredentialState::Unverified,
                        auth_dead_strikes: 0,
                        next_probe_at: None,
                        probe_failures: 0,
                    });
                }

        let oauth_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::OAuth).count();
        let apikey_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::ApiKey).count();
        let count = loaded.len();

        // Check token expiry warnings
        for acc in &loaded {
            if let Some(days) = acc.days_until_expiry() {
                if days <= 0 {
                    warn!(
                        account = %acc.id,
                        label = %acc.label,
                        "OAuth token EXPIRED — run `claude setup-token` to renew"
                    );
                } else if days <= 7 {
                    warn!(
                        account = %acc.id,
                        label = %acc.label,
                        days_remaining = days,
                        "OAuth token expiring soon — run `claude setup-token` to renew"
                    );
                } else if days <= 30 {
                    info!(
                        account = %acc.id,
                        label = %acc.label,
                        days_remaining = days,
                        "OAuth token will expire in {days} days"
                    );
                }
            }
        }

        info!(total = count, oauth = oauth_count, api_key = apikey_count, strategy = ?self.strategy, "Accounts loaded");
        *self.accounts.write().await = loaded;
        Ok(count)
    }

    /// Select the best available Anthropic account and return env vars for the
    /// `claude` CLI.
    ///
    /// Back-compat shim: identical to `select_for_provider("anthropic")`. Every
    /// pre-existing caller (gateway channel reply, claude_runner, agent runner,
    /// fork `RotatorProvider`) keeps working byte-for-byte.
    pub async fn select(&self) -> Option<AccountEnv> {
        self.select_for_provider("anthropic").await
    }

    /// [`select`](Self::select) restricted to an agent's configured
    /// `agent.toml [model] account_pool`.
    ///
    /// An empty `pool` is byte-identical to [`select`](Self::select).
    /// See [`select_for_provider_with_pool`](Self::select_for_provider_with_pool)
    /// for the full semantics (including the fail-open rule).
    pub async fn select_with_pool(&self, pool: &[String]) -> Option<AccountEnv> {
        self.select_for_provider_with_pool("anthropic", pool).await
    }

    /// Select the best available account *for a specific provider* and return
    /// its env vars + raw key.
    ///
    /// Only accounts whose `provider` matches are considered; health, cooldown,
    /// budget, and the rotation strategy are all applied *within* that provider
    /// pool. If the config declares no accounts for `provider`, a single
    /// ephemeral account is synthesized from the provider's standard env var
    /// (e.g. `OPENAI_API_KEY`) so a user with just that env var still rotates
    /// (trivially) through the same machinery.
    pub async fn select_for_provider(&self, provider: &str) -> Option<AccountEnv> {
        self.select_for_provider_with_pool(provider, &[]).await
    }

    /// [`select_for_provider`](Self::select_for_provider) restricted to an
    /// agent's configured `agent.toml [model] account_pool`.
    ///
    /// The restriction is applied to the **candidate set only** — after the
    /// provider / health / cooldown / budget filters and *before* the rotation
    /// strategy runs — so Priority / LeastCost / Failover / RoundRobin keep
    /// their exact semantics, just over a narrower set.
    ///
    /// Semantics:
    /// - empty `pool` ⇒ byte-identical to [`select_for_provider`](Self::select_for_provider);
    /// - non-empty `pool` ⇒ candidates are those whose account `id` **or**
    ///   `label` equals a pool entry (trimmed, ASCII-case-insensitive — both
    ///   are user-visible in the dashboard account picker);
    /// - **fail-open**: a pool that matches no *available* account (stale ids,
    ///   renamed accounts, everything cooling down) logs a `warn` and falls
    ///   back to the full candidate set. A stale pool must never brick an
    ///   agent — availability outranks the operator's preference here, and the
    ///   warn is the signal to fix the config.
    pub async fn select_for_provider_with_pool(
        &self,
        provider: &str,
        pool: &[String],
    ) -> Option<AccountEnv> {
        let accounts = self.accounts.read().await;
        let has_any_for_provider = accounts.iter().any(|a| a.provider == provider);
        let mut available: Vec<&Account> = accounts
            .iter()
            .filter(|a| a.provider == provider && a.is_available())
            .collect();

        // Candidate-set narrowing by the agent's account pool (fail-open).
        match narrow_by_pool(&available, pool) {
            PoolNarrowing::NotRequested => {}
            PoolNarrowing::Applied(filtered) => available = filtered,
            PoolNarrowing::FailedOpen => {
                // Distinguish "the pool names accounts that do not exist" from
                // "they exist but are all cooling down" — the operator fix is
                // different (edit the pool vs. wait / add capacity). Computed
                // only on this cold path.
                let known = accounts
                    .iter()
                    .any(|a| a.provider == provider && account_in_pool(a, pool));
                warn!(
                    provider,
                    pool = ?pool,
                    pool_accounts_known = known,
                    "account_pool matched no available account — falling back to the full \
                     account set (fail-open). Fix `agent.toml [model] account_pool` if this \
                     is not intended."
                );
            }
        }

        if available.is_empty() {
            if !has_any_for_provider {
                // No configured accounts for this provider → env-var fallback.
                drop(accounts);
                return env_fallback_account_env(provider);
            }
            warn!(provider, "No available accounts for rotation");
            return None;
        }

        let selected = match self.strategy {
            RotationStrategy::Priority | RotationStrategy::Failover => {
                available.iter().min_by_key(|a| a.priority).copied()
            }
            RotationStrategy::LeastCost => {
                // Prefer OAuth (subscription, no per-token cost), then least spent API key.
                let oauth: Vec<&&Account> = available.iter().filter(|a| a.auth_method == AuthMethod::OAuth).collect();
                if !oauth.is_empty() {
                    // Among OAuth accounts, the lowest "cost" tier is the one
                    // with the least spend. Within that equal-cost tier, rotate
                    // fairly using a least-recently-used tiebreaker instead of
                    // always picking index 0 — otherwise the first OAuth account
                    // takes every request and the others never get used.
                    let min_spent = oauth
                        .iter()
                        .map(|a| a.spent_this_month)
                        .min()
                        .unwrap_or(0);
                    oauth
                        .iter()
                        .filter(|a| a.spent_this_month == min_spent)
                        // `None` (never used) sorts before any timestamp, so
                        // unused accounts are preferred first.
                        .min_by_key(|a| a.last_used)
                        .map(|a| **a)
                } else {
                    available.iter().min_by_key(|a| a.spent_this_month).copied()
                }
            }
            RotationStrategy::RoundRobin => {
                let mut idx = self.round_robin_index.write().await;
                let selected = available[*idx % available.len()];
                *idx = (*idx + 1) % available.len();
                Some(selected)
            }
        };

        selected.map(|a| {
            info!(
                account = %a.id,
                provider = %a.provider,
                method = ?a.auth_method,
                email = %a.email,
                "Account selected for rotation"
            );
            build_account_env(a)
        })
    }

    /// Whether the pool currently has an *available* non-Anthropic OAuth
    /// subscription seat for `provider` that carries a stored seat credential.
    ///
    /// Read-only (no rotation side effects), so it is safe to call from the
    /// proxy's model-catalogue handler to fail-closed: no seat ⇒ no advertised
    /// models for that provider ⇒ 404/503, never a silent Anthropic fallback.
    pub async fn has_seat_for_provider(&self, provider: &str) -> bool {
        let accounts = self.accounts.read().await;
        accounts.iter().any(|a| {
            a.provider == provider
                && a.auth_method == AuthMethod::OAuth
                && a.is_available()
                && a.oauth_token.as_ref().is_some_and(|t| !t.is_empty())
        })
    }

    /// Swap the in-memory seat credential after a token refresh (Qwen seat
    /// rotation). Without this, the next request would re-read the stale
    /// in-memory bundle and try to refresh with an already-rotated (revoked)
    /// refresh token. The caller is responsible for the encrypted persist to
    /// `config.toml`; this only updates the live pool.
    pub async fn update_seat_token(&self, account_id: &str, new_token: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts
            .iter_mut()
            .find(|a| a.id == account_id && a.auth_method == AuthMethod::OAuth)
        {
            acc.oauth_token = Some(new_token.to_string());
        } else {
            warn!(account = account_id, "update_seat_token: no OAuth account with this id");
        }
    }

    pub async fn on_success(&self, account_id: &str, cost_cents: u64) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors = 0;
            // Only restore health if not in active cooldown set by another worker.
            // This prevents a stale success from overriding a concurrent rate-limit.
            let in_cooldown = acc.cooldown_until.is_some_and(|cd| Utc::now() < cd);
            if !in_cooldown {
                acc.is_healthy = true;
            }
            // A completed request is the strongest possible evidence that the
            // credential works, so it clears an AuthDead verdict and the
            // backoff ladder even mid-cooldown (mirroring the existing
            // `consecutive_errors = 0`). `Broken` is deliberately sticky: it
            // means we never produced a usable secret in the first place, so a
            // success attributed to this id cannot be evidence about it — and
            // a Broken account is never selectable, so this branch is
            // unreachable in practice. Fail closed rather than resurrect.
            if acc.credential_state != CredentialState::Broken {
                acc.credential_state = CredentialState::Ok;
            }
            acc.auth_dead_strikes = 0;
            // A completed request also settles the probe schedule: there is
            // nothing left to back off from.
            acc.probe_failures = 0;
            acc.next_probe_at = None;
            acc.spent_this_month += cost_cents;
            acc.total_requests += 1;
            acc.last_used = Some(Utc::now());
        }
    }

    /// Record an **authentication** failure (2026-09 hardening, D2).
    ///
    /// Distinct from [`on_error`](Self::on_error) because a dead credential is
    /// not a transient fault: three strikes and a 2-minute cooldown were
    /// exactly wrong for it. The account goes unhealthy immediately, is marked
    /// [`CredentialState::AuthDead`], and books an exponential cooldown
    /// (15 min → 30 → 60 → … capped at 6 h) so a re-issued token still recovers
    /// on its own, but a genuinely dead one stops burning one spawn per cron
    /// tick.
    ///
    /// Callers classify the failure (`oauth_not_allowed_for_organization` and
    /// friends → [`AuthFailureKind::OrgDisabled`]; `invalid bearer token` /
    /// `authentication_failed` → [`AuthFailureKind::InvalidToken`]).
    pub async fn on_auth_failed(&self, account_id: &str, kind: AuthFailureKind) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.is_healthy = false;
            acc.credential_state = CredentialState::AuthDead(kind);
            acc.auth_dead_strikes = acc.auth_dead_strikes.saturating_add(1);
            // A real spawn just failed, so the next health tick must be free
            // to probe and classify *this* failure — clear any probe schedule
            // an earlier round booked. `probe_failures` is deliberately NOT
            // bumped here: it counts probe verdicts, not spawn outcomes, and
            // bumping it would let an observed failure silently lengthen the
            // schedule without a single probe having been answered.
            acc.next_probe_at = None;
            let backoff = auth_dead_backoff(acc.auth_dead_strikes);
            let until = Utc::now() + backoff;
            // Never shorten an existing (e.g. 24 h billing) cooldown.
            acc.cooldown_until = Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
            warn!(
                account = account_id,
                kind = %kind,
                strikes = acc.auth_dead_strikes,
                cooldown_minutes = backoff.num_minutes(),
                "Account authentication FAILED — credential marked auth-dead. \
                 It will be retried once when the cooldown expires; fix the \
                 credential in 設定→帳號 to recover sooner."
            );
        }
    }

    /// Record a generic (non-billing, non-rate-limit) failure for an account.
    ///
    /// **WP10 fix (2026-08-04 field incident)**: marking `is_healthy = false`
    /// used to leave `cooldown_until = None`. `Account::is_available` only
    /// forgives an unhealthy account once its cooldown has *expired*, so an
    /// account with no cooldown at all was permanently unavailable — for a
    /// single-account install that meant every subsequent message failed with
    /// "All accounts exhausted" until the 5-minute rotator cache happened to
    /// rebuild or the 60 s health probe managed a successful
    /// `claude auth status`. Attaching the standard cooldown makes the
    /// degradation self-healing and bounded.
    pub async fn on_error(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors += 1;
            if acc.consecutive_errors >= 3 {
                let until = Utc::now() + chrono::Duration::seconds(self.cooldown_seconds as i64);
                warn!(
                    account = account_id,
                    cooldown = self.cooldown_seconds,
                    "Account marked unhealthy after 3 errors — cooling down (auto-recovers)"
                );
                acc.is_healthy = false;
                // Never shorten an existing (e.g. 24 h billing) cooldown.
                acc.cooldown_until = Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
            }
        }
    }

    pub async fn on_rate_limited(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.cooldown_until = Some(
                Utc::now() + chrono::Duration::seconds(self.cooldown_seconds as i64),
            );
            warn!(account = account_id, cooldown = self.cooldown_seconds, "Account rate-limited");
        }
    }

    /// Billing/credit exhaustion — mark account unhealthy with 24-hour cooldown.
    ///
    /// Unlike rate limiting (minutes), billing exhaustion requires manual top-up
    /// or a new billing cycle, so we use a much longer cooldown.
    pub async fn on_billing_exhausted(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.is_healthy = false;
            acc.cooldown_until = Some(Utc::now() + chrono::Duration::hours(24));
            warn!(
                account = account_id,
                "Account billing exhausted — marked unhealthy with 24h cooldown"
            );
        }
    }

    pub async fn reset_monthly(&self) {
        let mut accounts = self.accounts.write().await;
        for acc in accounts.iter_mut() {
            acc.spent_this_month = 0;
        }
    }

    /// WP10 M4 — why is nothing selectable right now?
    ///
    /// Called on the "no account available" path so the user-facing message can
    /// state a realistic recovery horizon instead of one generic sentence. Only
    /// information already in memory is used; nothing is probed.
    ///
    /// The tiers are separated by cooldown length because that IS the recovery
    /// horizon: billing exhaustion books 24 h, while rate-limit and generic
    /// errors book `cooldown_seconds` (120 s by default). Anything above an
    /// hour is therefore billing-class.
    pub async fn unavailable_reason(&self) -> UnavailableReason {
        let accounts = self.accounts.read().await;
        let now = Utc::now();
        let longest = accounts
            .iter()
            .filter(|a| !a.is_available())
            .filter_map(|a| a.cooldown_until)
            .filter(|cd| *cd > now)
            .max();
        match longest {
            Some(cd) if (cd - now) > chrono::Duration::hours(1) => UnavailableReason::LongCooldown,
            Some(_) => UnavailableReason::ShortCooldown,
            // Unavailable for a non-cooldown reason (expired token, budget
            // exhausted, unhealthy with no cooldown attached) — or no accounts
            // at all. Callers must use conservative wording here.
            None => UnavailableReason::Unknown,
        }
    }

    pub async fn status(&self) -> Vec<AccountStatus> {
        let accounts = self.accounts.read().await;
        accounts.iter().map(|a| AccountStatus {
            id: a.id.clone(),
            auth_method: format!("{:?}", a.auth_method).to_lowercase(),
            provider: a.provider.clone(),
            priority: a.priority,
            is_healthy: a.is_healthy,
            spent_this_month: a.spent_this_month,
            monthly_budget_cents: a.monthly_budget_cents,
            total_requests: a.total_requests,
            is_available: a.is_available(),
            email: {
                if a.email.contains('@') {
                    let parts: Vec<&str> = a.email.splitn(2, '@').collect();
                    let prefix = &parts[0][..parts[0].len().min(2)];
                    format!("{}***@{}", prefix, parts.get(1).unwrap_or(&""))
                } else if a.email.is_empty() {
                    String::new()
                } else {
                    "***".to_string()
                }
            },
            subscription: a.subscription.clone(),
            label: a.label.clone(),
            expires_at: a.expires_at.clone(),
            days_until_expiry: a.days_until_expiry(),
            credential_state: a.credential_state,
            credential_detail: a.credential_state.credential_detail(),
            auth_dead_strikes: a.auth_dead_strikes,
            next_probe_at: a.next_probe_at.map(|t| t.to_rfc3339()),
            probe_failures: a.probe_failures,
        }).collect()
    }

    pub async fn count(&self) -> usize {
        self.accounts.read().await.len()
    }

    /// Test-only: push a pre-built account directly into the rotator.
    ///
    /// Bypasses config file loading and OAuth auto-detection. Cross-crate
    /// integration tests need deterministic account state — in particular,
    /// channel-reply rotation tests inject synthetic OAuth accounts so the
    /// spawn closure can simulate rate-limit / success patterns.
    ///
    /// Not intended for production code. Marked `#[doc(hidden)]` so it does
    /// not appear in public API docs.
    #[doc(hidden)]
    pub async fn push_account_for_test(&self, account: Account) {
        self.accounts.write().await.push(account);
    }

    /// Test-only: the account's current cooldown deadline.
    ///
    /// [`AccountStatus`] deliberately exposes only `is_available` (a boolean),
    /// which cannot tell a 2-minute generic-error cooldown from the 15-minute
    /// auth-dead base — a distinction the gateway's D2 wiring tests must be
    /// able to make. Read-only and `#[doc(hidden)]`, like
    /// [`push_account_for_test`](Self::push_account_for_test).
    #[doc(hidden)]
    pub async fn cooldown_until_for_test(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.accounts
            .read()
            .await
            .iter()
            .find(|a| a.id == account_id)
            .and_then(|a| a.cooldown_until)
    }

    /// Probe every account that carries a probe-able Anthropic secret and
    /// report the verdict, **without touching account state** (D5).
    ///
    /// The diagnostic twin of [`probe_and_restore`](Self::probe_and_restore):
    /// that one is a control loop and only looks at unavailable accounts; this
    /// one looks at *all* of them and changes nothing, so `duduclaw doctor`
    /// can answer "is this token still good?" without perturbing a running
    /// gateway's rotation. Secrets never leave this crate — only the
    /// [`CredentialProbe`] outcome does.
    ///
    /// Probes run sequentially (each capped by the probe's own 10 s timeout);
    /// an account with nothing probe-able reports `probe: None` and costs no
    /// request at all.
    pub async fn probe_credentials_report(&self) -> Vec<CredentialReport> {
        let snapshot: Vec<(CredentialReport, Option<(CredentialKind, String)>)> = {
            let accounts = self.accounts.read().await;
            accounts
                .iter()
                .map(|a| {
                    (
                        CredentialReport {
                            id: a.id.clone(),
                            provider: a.provider.clone(),
                            auth_method: a.auth_method.clone(),
                            state: a.credential_state,
                            probe: None,
                        },
                        probe_secret_for(a),
                    )
                })
                .collect()
        };

        let mut out = Vec::with_capacity(snapshot.len());
        for (mut report, secret) in snapshot {
            if let Some((kind, secret)) = secret {
                report.probe =
                    Some(probe_anthropic_credential_at(&self.probe_base_url, kind, &secret).await);
            }
            out.push(report);
        }
        out
    }

    /// Probe all unhealthy accounts and restore those whose credential really
    /// still authenticates.
    ///
    /// ## 2026-09 rewrite (D3)
    ///
    /// This used to "verify" every OAuth account by running `claude auth
    /// status` and accepting `loggedIn: true`. That signal is true whenever
    /// *any* `CLAUDE_CODE_OAUTH_TOKEN` exists in the environment and says
    /// nothing about the account being probed — so a token that Anthropic had
    /// started answering with `403 oauth_not_allowed_for_organization` was
    /// resurrected every 60 s for 18 hours, each resurrection costing one more
    /// failed scheduled dispatch.
    ///
    /// Now, for accounts that carry a probe-able Anthropic secret (an OAuth
    /// setup-token, or an API key), the probe authenticates **that secret**:
    ///
    /// | outcome | action |
    /// |---|---|
    /// | `Valid` | restore (healthy, no cooldown, state `Ok`, strikes reset, probe schedule cleared) |
    /// | `InvalidCredential` | stay dead, state `AuthDead(InvalidToken)`, cooldown doubled (cap 6 h), next probe backed off |
    /// | `OrgDisabled` | stay dead, state `AuthDead(OrgDisabled)`, cooldown doubled (cap 6 h), next probe backed off |
    /// | `RateLimited` / `Unknown` | untouched — retry next tick |
    ///
    /// A conclusive failure also books [`Account::next_probe_at`], so a
    /// credential the API keeps rejecting is re-checked on a widening
    /// schedule ([`probe_backoff`]: 1 min doubling to a 30 min ceiling)
    /// rather than once a minute forever. Inconclusive outcomes leave the
    /// schedule alone, and a real spawn failure
    /// ([`on_auth_failed`](Self::on_auth_failed)) clears it so the very next
    /// tick classifies the fresh failure.
    ///
    /// Accounts with no probe-able secret (an OS-keychain OAuth session, a
    /// foreign-provider subscription seat) keep the legacy `claude auth
    /// status` / cooldown-expiry path — it remains the only signal available —
    /// **except** when they are already `AuthDead`, which that signal is not
    /// strong enough to clear. Those wait for cooldown expiry and get exactly
    /// one real retry. [`CredentialState::Broken`] accounts are never probed
    /// and never restored.
    ///
    /// Call this periodically (e.g. every 60s) from a background task.
    pub async fn probe_and_restore(&self) -> usize {
        let now = Utc::now();
        let candidates: Vec<ProbeCandidate> = {
            let accounts = self.accounts.read().await;
            accounts.iter()
                .filter(|a| !a.is_healthy || a.cooldown_until.is_some_and(|cd| Utc::now() >= cd))
                .filter(|a| !a.is_available()) // truly unavailable, not just cooled-down-and-ready
                // D4: an undecryptable credential cannot be probed and must
                // never be restored — only re-saving it (which rebuilds the
                // rotator) can fix it.
                .filter(|a| !a.credential_state.is_blocking())
                // Probe schedule: a credential the API has already rejected
                // conclusively waits out its backoff (1 min → 30 min) instead
                // of being re-asked on every 60 s tick. `None` = probe now,
                // which is what every account looks like until its first
                // conclusive failure.
                .filter(|a| a.next_probe_at.is_none_or(|t| now >= t))
                .map(|a| ProbeCandidate {
                    id: a.id.clone(),
                    method: a.auth_method.clone(),
                    state: a.credential_state,
                    secret: probe_secret_for(a),
                })
                .collect()
        };

        if candidates.is_empty() {
            return 0;
        }

        let mut restored = 0u64;

        for candidate in &candidates {
            let id = &candidate.id;
            let method = &candidate.method;

            // ── Path A: a real credential we can actually authenticate ──
            if let Some((kind, secret)) = &candidate.secret {
                let outcome =
                    probe_anthropic_credential_at(&self.probe_base_url, *kind, secret).await;
                match outcome {
                    CredentialProbe::Valid => {
                        let mut accounts = self.accounts.write().await;
                        if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                            acc.is_healthy = true;
                            acc.consecutive_errors = 0;
                            acc.cooldown_until = None;
                            acc.credential_state = CredentialState::Ok;
                            acc.auth_dead_strikes = 0;
                            acc.probe_failures = 0;
                            acc.next_probe_at = None;
                            restored += 1;
                            info!(
                                account = id.as_str(),
                                method = ?method,
                                priority = acc.priority,
                                "Account restored by credential probe (200 from /v1/models)"
                            );
                        }
                    }
                    CredentialProbe::InvalidCredential | CredentialProbe::OrgDisabled => {
                        let failure = if outcome == CredentialProbe::OrgDisabled {
                            AuthFailureKind::OrgDisabled
                        } else {
                            AuthFailureKind::InvalidToken
                        };
                        let mut accounts = self.accounts.write().await;
                        if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                            acc.is_healthy = false;
                            acc.credential_state = CredentialState::AuthDead(failure);
                            let until = doubled_cooldown(acc.cooldown_until);
                            acc.cooldown_until =
                                Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
                            // Space out the *probe* as well as the rotation
                            // cooldown: re-asking the API every 60 s about a
                            // credential it has conclusively rejected buys
                            // nothing and drowns the log.
                            acc.probe_failures = acc.probe_failures.saturating_add(1);
                            let probe_delay = probe_backoff(acc.probe_failures);
                            acc.next_probe_at = Some(Utc::now() + probe_delay);
                            warn!(
                                account = id.as_str(),
                                kind = %failure,
                                until = %until,
                                next_probe_in_minutes = probe_delay.num_minutes(),
                                "Credential probe confirms the account is auth-dead — \
                                 staying out of rotation with a doubled cooldown"
                            );
                        }
                    }
                    // Inconclusive: the probe says nothing about the
                    // credential, so account state must not move in either
                    // direction. Retry on the next tick.
                    CredentialProbe::RateLimited | CredentialProbe::Unknown(_) => {
                        debug!(
                            account = id.as_str(),
                            outcome = ?outcome,
                            "Credential probe inconclusive — leaving account state untouched"
                        );
                    }
                }
                continue;
            }

            // ── Path B: nothing to authenticate with ────────────────────
            // D3: `claude auth status` is the only signal for a keychain
            // session, but it is far too weak to overturn an observed
            // authentication failure.
            if !legacy_status_probe_may_restore(candidate.state) {
                debug!(
                    account = id.as_str(),
                    state = %candidate.state,
                    "Auth-dead account has no probe-able secret — `claude auth status` \
                     must not restore it; waiting for cooldown expiry"
                );
                continue;
            }

            let ok = match method {
                AuthMethod::OAuth => {
                    // Legacy path: keychain / profile sessions and foreign
                    // subscription seats have no secret we can present.
                    tokio::task::spawn_blocking(|| {
                        let claude = duduclaw_core::which_claude();
                        claude.and_then(|bin| {
                            let output = duduclaw_core::platform::command_for(&bin)
                                .args(["auth", "status"])
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::null())
                                .output()
                                .ok()?;
                            if !output.status.success() { return None; }
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;
                            json.get("loggedIn").and_then(|v| v.as_bool()).filter(|&b| b)
                        })
                    }).await.ok().flatten().is_some()
                }
                AuthMethod::ApiKey => {
                    // API key accounts: cooldown expiry already handled by is_available().
                    // If we're here, it means the account is unhealthy for non-cooldown reasons.
                    // Just check if cooldown expired — if so, it's safe to restore.
                    let accounts = self.accounts.read().await;
                    accounts.iter()
                        .find(|a| a.id == *id)
                        .is_some_and(|a| {
                            a.cooldown_until.is_none_or(|cd| Utc::now() >= cd)
                        })
                }
            };

            if ok {
                let mut accounts = self.accounts.write().await;
                if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                    acc.is_healthy = true;
                    acc.consecutive_errors = 0;
                    acc.cooldown_until = None;
                    restored += 1;
                    info!(
                        account = id.as_str(),
                        method = ?method,
                        priority = acc.priority,
                        "Account restored by health probe"
                    );
                }
            }
        }

        restored as usize
    }
}

/// One account's credential verdict, for an operator-facing report
/// (`duduclaw doctor`, D5).
///
/// Carries no secret: the credential is probed inside this crate and only the
/// outcome crosses the boundary.
#[derive(Debug, Clone)]
pub struct CredentialReport {
    pub id: String,
    pub provider: String,
    pub auth_method: AuthMethod,
    /// The account's last known state (before this probe) — surfaced so the
    /// report can say "already marked auth-dead" even when the probe itself
    /// comes back inconclusive.
    pub state: CredentialState,
    /// `None` when the account carries no probe-able Anthropic secret (an
    /// OS-keychain OAuth session, a foreign-provider seat): nothing was sent
    /// and nothing can be concluded.
    pub probe: Option<CredentialProbe>,
}

/// One account's worth of snapshot state for [`AccountRotator::probe_and_restore`].
///
/// Snapshotted under the read lock so no lock is held across the probe's
/// `await` — the pool must stay selectable while a 10-second probe is in
/// flight.
struct ProbeCandidate {
    id: String,
    method: AuthMethod,
    state: CredentialState,
    secret: Option<(CredentialKind, String)>,
}

/// Whether the weak `claude auth status` signal is allowed to restore an
/// account in this credential state (pure, so the rule is testable without a
/// `claude` binary on PATH).
///
/// `AuthDead` is excluded: `loggedIn: true` is reported for *any* ambient
/// session — including one backed by a token Anthropic is currently answering
/// with 403 — so it cannot overturn an observed authentication failure. That
/// false positive is what resurrected a dead account every 60 s for 18 hours
/// on 2026-09-08. `Broken` is excluded because there is nothing to restore.
fn legacy_status_probe_may_restore(state: CredentialState) -> bool {
    !matches!(
        state,
        CredentialState::AuthDead(_) | CredentialState::Broken
    )
}

/// The Anthropic secret this account can be probed with, if any.
///
/// `None` for: a non-Anthropic provider (its seat credential means nothing to
/// `api.anthropic.com` — probing it there would produce a confident, wrong
/// verdict), an OS-keychain OAuth session (the secret lives in the keychain,
/// not here), and an empty credential.
fn probe_secret_for(a: &Account) -> Option<(CredentialKind, String)> {
    if a.provider != "anthropic" {
        return None;
    }
    match a.auth_method {
        AuthMethod::ApiKey => {
            (!a.api_key.trim().is_empty()).then(|| (CredentialKind::ApiKey, a.api_key.clone()))
        }
        AuthMethod::OAuth => a
            .oauth_token
            .as_ref()
            .filter(|t| !t.trim().is_empty())
            .map(|t| (CredentialKind::OAuthToken, t.clone())),
    }
}

/// Next cooldown deadline after a probe confirms the credential is dead:
/// double whatever is left, capped at 6 h.
///
/// An expired / absent cooldown has nothing to double, so it restarts at the
/// [`AUTH_DEAD_BASE_MINUTES`] base — doubling zero would hand the account
/// straight back to the next tick, which is the loop this whole change exists
/// to break.
fn doubled_cooldown(current: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let now = Utc::now();
    let next = match current
        .map(|cd| cd - now)
        .filter(|d| *d > chrono::Duration::zero())
    {
        Some(remaining) => remaining
            .checked_mul(2)
            .unwrap_or_else(|| chrono::Duration::minutes(AUTH_DEAD_CAP_MINUTES)),
        None => chrono::Duration::minutes(AUTH_DEAD_BASE_MINUTES),
    };
    now + next.min(chrono::Duration::minutes(AUTH_DEAD_CAP_MINUTES))
}

// ── OAuth helpers ───────────────────────────────────────────

/// Whether the Anthropic host-login auto-detect should still run for the
/// loaded pool: yes unless an **Anthropic** OAuth account is already
/// configured. Foreign-provider OAuth seats (copilot / qwen / codex from
/// `duduclaw auth device`) do not count — they serve a different provider
/// pool and must never mask the missing Anthropic session (pure, testable).
fn should_autodetect_anthropic_oauth(loaded: &[Account]) -> bool {
    !loaded
        .iter()
        .any(|a| a.auth_method == AuthMethod::OAuth && a.provider == "anthropic")
}

/// Detect the default OAuth session via `claude auth status`.
///
/// Works with all Claude Code versions — does not depend on `.credentials.json`
/// which no longer exists in recent versions. The `claude` CLI manages its own
/// auth state (OS keychain / internal storage).
///
/// ## Two different sessions look identical to `claude auth status`
///
/// `loggedIn: true` is reported both when the CLI found a keychain session
/// **and** when it merely read `CLAUDE_CODE_OAUTH_TOKEN` out of the ambient
/// environment (the `setup-token` flow every container deployment uses).
///
/// Before the P3 env scrub those two were interchangeable here, because a
/// spawned child inherited the gateway's environment and found the token by
/// itself. Since v1.61.0 the spawn environment is an allowlist that
/// deliberately drops `*_TOKEN`, so an account carrying neither `oauth_token`
/// nor a usable keychain leaves the child with no credential at all —
/// every dispatch dies as `authentication_failed`, while a manual
/// `claude -p` in the same container still works (it *does* inherit the env).
///
/// So: when the session came from the env var, capture that token on the
/// account. `build_env_for` then injects it explicitly, which is exactly what
/// the scrub intends — credentials travel as data, not as ambient state.
fn detect_default_oauth_session() -> Option<Account> {
    let claude = duduclaw_core::which_claude()?;
    let claude_dir = dirs::home_dir()?.join(".claude");
    // Captured before the probe so a token-derived session is never mistaken
    // for a keychain one.
    let env_token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());

    let output = duduclaw_core::platform::command_for(&claude)
        .args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;

    let logged_in = json.get("loggedIn").and_then(|v| v.as_bool()).unwrap_or(false);
    if !logged_in {
        return None;
    }

    let subscription = json
        .get("subscriptionType")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let email = json
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    info!(subscription, email, "OAuth session detected via `claude auth status`");

    Some(Account {
        id: "oauth-default".to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority: 1, // OAuth preferred over API key
        monthly_budget_cents: 0,
        tags: Vec::new(),
        profile: "default".to_string(),
        email: email.to_string(),
        subscription: subscription.to_string(),
        label: if env_token.is_some() {
            "setup-token".to_string()
        } else {
            "本機登入".to_string()
        },
        expires_at: None, // OS keychain manages token lifecycle
        api_key: String::new(),
        // `Some` ⇒ inject explicitly (setup-token deployments); `None` ⇒ let the
        // CLI read its own keychain via `credentials_dir`.
        oauth_token: env_token,
        credentials_dir: Some(claude_dir),
        is_healthy: true,
        consecutive_errors: 0,
        spent_this_month: 0,
        cooldown_until: None,
        last_used: None,
        total_requests: 0,
        // `claude auth status` said `loggedIn: true` — which, per this
        // function's own doc comment, proves only that *some* session exists.
        // Unverified until a real request (or a real probe) succeeds.
        credential_state: CredentialState::Unverified,
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    })
}

/// Resolve OAuth credentials directory for a named profile.
///
/// Modern Claude CLI versions no longer use `.credentials.json` — auth is
/// managed via OS keychain / internal storage. We check for the directory
/// itself (which still exists) and fall back to `.credentials.json` for
/// older versions.
fn resolve_oauth_credentials(profile: &str) -> Option<PathBuf> {
    let claude_dir = dirs::home_dir()?.join(".claude");

    let dir = if profile == "default" || profile.is_empty() {
        claude_dir.clone()
    } else {
        claude_dir.join("profiles").join(profile)
    };

    if !dir.exists() {
        return None;
    }

    // Accept if directory exists — modern CLI manages auth internally.
    // Legacy check (.credentials.json) is subsumed: if the file exists,
    // the directory also exists.
    Some(dir)
}

// ── API Key helpers ─────────────────────────────────────────

/// Load the `[secret_manager]` config from a top-level config table.
///
/// `table` here is a sub-table (e.g. `[api]` or an `[[accounts]]` entry), so we
/// cannot read `[secret_manager]` from it directly. The rotator only has the
/// per-account table at the call sites, not the full config, so we re-read the
/// top-level config to recover `[secret_manager]`. Absent / malformed →
/// `Default` (backend `local`), matching the gateway's fail-safe behavior.
async fn load_secret_manager_config(home_dir: &Path) -> SecretManagerConfig {
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(_) => return SecretManagerConfig::default(),
    };
    content
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| {
            t.get("secret_manager")
                .cloned()
                .and_then(|v| v.try_into().ok())
        })
        .unwrap_or_default()
}

/// Encrypted-field names an `[[accounts]] type = "api_key"` entry may carry.
/// Mirrors `resolve_api_key`'s `*_enc` precedence list.
const API_KEY_ENC_FIELDS: &[&str] = &["anthropic_api_key_enc", "api_key_enc"];

/// Encrypted-field name an `[[accounts]] type = "oauth"` entry may carry.
const OAUTH_TOKEN_ENC_FIELDS: &[&str] = &["oauth_token_enc"];

/// Whether the entry declares at least one of `fields` with a non-empty value.
///
/// The D4 detector's precondition: "the operator stored a credential here".
/// A field that is absent (or present but blank) is *not* a broken credential
/// — an OAuth entry with no `oauth_token_enc` legitimately relies on the OS
/// keychain, and must keep behaving exactly as before.
fn has_nonempty_field(table: &toml::Table, fields: &[&str]) -> bool {
    fields.iter().any(|name| {
        table
            .get(*name)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    })
}

/// Resolve an `[[accounts]]` entry's OAuth token from a TOML table.
///
/// WP-8A: goes through the shared [`SecretRef`] resolver instead of a
/// hand-rolled "decrypt keyfile, else resolve secret:// reference" pair that
/// duplicated `SecretRef`'s own logic.
///
/// Precedence:
/// 1. inline `oauth_token_enc` (decrypted via the per-machine keyfile)
/// 2. `oauth_token` plaintext that is a `secret://` reference
///
/// A non-reference plaintext `oauth_token` is intentionally NOT consumed
/// (preserving prior behavior, which only ever read `oauth_token_enc`) — the
/// plaintext candidate passed to [`SecretRef::classify`] is pre-filtered to
/// `None` unless it is itself a `secret://` reference, so a bare plaintext
/// token can never be picked up through this path.
async fn resolve_oauth_token(home_dir: &Path, table: &toml::Table) -> Option<String> {
    let enc = table.get("oauth_token_enc").and_then(|v| v.as_str());
    let plain = table
        .get("oauth_token")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("secret://"));
    if enc.is_none() && plain.is_none() {
        return None;
    }
    let sm_cfg = load_secret_manager_config(home_dir).await;
    SecretRef::classify(enc, plain)
        .resolve(&sm_cfg, home_dir)
        .await
        .map(|s| s.expose_owned())
}

/// Resolve API key from a TOML table.
///
/// WP-8A: goes through the shared [`SecretRef`] resolver (credentials
/// doctrine) instead of a hand-rolled "decrypt keyfile, else resolve
/// secret:// reference, else use literally" chain — this was the last of the
/// account_rotator dialects listed in `DESIGN-credentials-doctrine-2026-08.md`
/// §1.1 as reading `secret://` itself rather than sharing the canonical
/// classifier.
///
/// Resolution precedence (unchanged from before this consolidation, since
/// `anthropic_api_key_enc` / `api_key_enc` are two *alternative field names*
/// for the same slot, not an enc/plain pair — encrypted always wins over
/// plaintext regardless of which name holds it):
/// 1. Inline `*_enc` (decrypted via the per-machine keyfile) — tries
///    `anthropic_api_key_enc` then `api_key_enc`.
/// 2. A plaintext field that is a `secret://<backend>/<name>` reference →
///    resolved through the configured secret backend — tries
///    `anthropic_api_key` then `api_key`.
/// 3. A plaintext field used as-is (legacy / dev) — same two names.
async fn resolve_api_key(home_dir: &Path, table: &toml::Table) -> String {
    let sm_cfg = load_secret_manager_config(home_dir).await;

    for key_name in &["anthropic_api_key_enc", "api_key_enc"] {
        let enc = table.get(*key_name).and_then(|v| v.as_str());
        if let Some(secret) = SecretRef::classify(enc, None)
            .resolve(&sm_cfg, home_dir)
            .await
        {
            return secret.expose_owned();
        }
    }
    for key_name in &["anthropic_api_key", "api_key"] {
        let plain = table.get(*key_name).and_then(|v| v.as_str());
        if let Some(p) = plain
            && !p.is_empty()
            && !p.starts_with("secret://")
        {
            warn!("Using plaintext API key — run `duduclaw onboard` to encrypt");
        }
        if let Some(secret) = SecretRef::classify(None, plain)
            .resolve(&sm_cfg, home_dir)
            .await
        {
            return secret.expose_owned();
        }
        // A `secret://` reference that failed to resolve falls through to
        // the next key name (treated as unset), matching prior behavior.
    }
    String::new()
}

// ── Provider env-var map ────────────────────────────────────

/// Standard environment-variable name(s) for a provider's API key.
///
/// Thin delegate to `duduclaw_core::provider_env::provider_env_key_names`, the
/// single source of truth for this table (WP-8B, `commercial/docs/DESIGN-credentials-doctrine-2026-08.md`
/// §3 P3). `duduclaw-agent` already depends on `duduclaw-core` (see
/// `Cargo.toml`), so there is no new dependency edge — this used to be a third
/// hand-copied table that had already drifted in comments from the canonical
/// one; behavior (match arms) was verified byte-identical before collapsing.
/// The FIRST name is the canonical one emitted onto a subprocess; the
/// remaining names are accepted aliases when *reading* an env-var fallback
/// value. Unknown providers → empty slice.
fn provider_env_key_names(provider: &str) -> &'static [&'static str] {
    duduclaw_core::provider_env::provider_env_key_names(provider)
}

/// Human-facing catalogue of consumer subscription sources the rotator can
/// carry as OAuth pool members (`provider` id → display label).
///
/// Descriptive metadata for status / validation surfaces only — it does NOT
/// gate rotation (any `provider` string is accepted on an account). Codex
/// (`openai`) is live through the Codex runtime's host-login inheritance; the
/// remaining entries are PENDING-LIVE on provider-specific device-code flows.
pub fn known_subscription_providers() -> &'static [(&'static str, &'static str)] {
    &[
        ("anthropic", "Claude Pro/Max"),
        ("openai", "ChatGPT (Codex)"),
        ("github", "GitHub Copilot"),
        ("qwen", "Qwen Portal"),
    ]
}

// ── Account-pool matching (agent.toml [model] account_pool) ─────────

/// Whether an `account_pool` declaration carries at least one usable entry.
///
/// Blank / whitespace-only entries are ignored so a config like
/// `account_pool = ["", "  "]` behaves as "unset" rather than as a pool that
/// matches nothing (which would fail-open anyway, but with a misleading warn).
pub(crate) fn has_pool_entries(pool: &[String]) -> bool {
    pool.iter().any(|p| !p.trim().is_empty())
}

/// Result of narrowing a candidate set by an agent's `account_pool`.
///
/// Split out as a pure decision so the fail-open rule is unit-testable without
/// a rotator, a config file, or a tracing subscriber.
#[derive(Debug)]
pub(crate) enum PoolNarrowing<'a> {
    /// No pool declared (or only blank entries) — candidate set untouched.
    NotRequested,
    /// The pool matched at least one available account; rotate over these.
    Applied(Vec<&'a Account>),
    /// The pool matched no available account. The caller MUST keep the full
    /// candidate set (a stale pool must never brick an agent) and log a warn.
    FailedOpen,
}

/// Narrow `available` to the accounts named by `pool` (see [`PoolNarrowing`]).
///
/// Pure: no I/O, no logging, no rotator state. Applied *before* the rotation
/// strategy runs, so Priority / LeastCost / Failover / RoundRobin keep their
/// exact semantics over the narrowed set.
pub(crate) fn narrow_by_pool<'a>(available: &[&'a Account], pool: &[String]) -> PoolNarrowing<'a> {
    if available.is_empty() || !has_pool_entries(pool) {
        return PoolNarrowing::NotRequested;
    }
    let filtered: Vec<&Account> = available
        .iter()
        .copied()
        .filter(|a| account_in_pool(a, pool))
        .collect();
    if filtered.is_empty() {
        PoolNarrowing::FailedOpen
    } else {
        PoolNarrowing::Applied(filtered)
    }
}

/// Whether `account` is named by the agent's `account_pool`.
///
/// Matching is **exact** (after trimming, ASCII-case-insensitive) against the
/// account `id` and the user-visible `label` — operators reference either one,
/// since the dashboard picker shows the label. Deliberately NOT a substring
/// test (project convention 2: no unanchored `contains` for routing decisions);
/// `word_contains_ci`-style fuzziness would let a pool entry `main` capture an
/// unrelated `main-backup` account.
pub(crate) fn account_in_pool(account: &Account, pool: &[String]) -> bool {
    pool.iter().any(|entry| {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        if entry.eq_ignore_ascii_case(account.id.trim()) {
            return true;
        }
        let label = account.label.trim();
        !label.is_empty() && entry.eq_ignore_ascii_case(label)
    })
}

/// Build the subprocess env vars + direct-API metadata for a selected account.
///
/// Anthropic emission is unchanged from the original inline logic (API key vs.
/// OAuth token vs. keychain `CLAUDE_CONFIG_DIR`). Non-Anthropic providers emit
/// the provider's canonical key env var instead, and every API-key account
/// additionally exposes its raw key on `AccountEnv.raw_key`.
fn build_account_env(a: &Account) -> AccountEnv {
    let mut env_vars = HashMap::new();
    let mut raw_key = None;
    let mut seat_token = None;

    if a.provider == "anthropic" {
        match a.auth_method {
            AuthMethod::ApiKey => {
                env_vars.insert("ANTHROPIC_API_KEY".to_string(), a.api_key.clone());
                if !a.api_key.is_empty() {
                    raw_key = Some(a.api_key.clone());
                }
            }
            AuthMethod::OAuth => {
                if let Some(ref token) = a.oauth_token {
                    // setup-token account: inject token via env var
                    env_vars.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.clone());
                } else if let Some(dir) = &a.credentials_dir {
                    // OS keychain account: only set CLAUDE_CONFIG_DIR when it differs
                    // from the default `~/.claude`.
                    //
                    // CRITICAL: setting `CLAUDE_CONFIG_DIR=~/.claude` explicitly —
                    // even with the SAME value as the default — makes `claude` CLI
                    // stop looking at the OS keychain for credentials, producing
                    // "Not logged in · Please run /login" for every call. The CLI
                    // only uses the keychain when no `CLAUDE_CONFIG_DIR` is set.
                    //
                    // Leave the env var unset for the default session so claude
                    // CLI picks up keychain auth normally. Non-default profile
                    // directories (e.g. `~/.claude/profiles/work`) still get the
                    // env var because they need explicit pointing.
                    let is_default_home = dirs::home_dir()
                        .map(|h| h.join(".claude"))
                        .is_some_and(|default_dir| default_dir == *dir);
                    if !is_default_home {
                        env_vars.insert(
                            "CLAUDE_CONFIG_DIR".to_string(),
                            dir.to_string_lossy().to_string(),
                        );
                    }
                }
                // Ensure API key doesn't override OAuth
                env_vars.insert("ANTHROPIC_API_KEY".to_string(), String::new());
            }
        }
    } else {
        // Non-Anthropic provider.
        match a.auth_method {
            AuthMethod::ApiKey => {
                // Emit the provider's canonical env var so a subprocess sees the
                // right variable, and expose the raw key for direct-API callers.
                if let Some(name) = provider_env_key_names(&a.provider).first() {
                    env_vars.insert((*name).to_string(), a.api_key.clone());
                }
                if !a.api_key.is_empty() {
                    raw_key = Some(a.api_key.clone());
                }
            }
            AuthMethod::OAuth => {
                // Subscription OAuth for a non-Anthropic provider (ChatGPT Codex
                // / GitHub Copilot / Qwen Portal). Token acquisition + injection
                // is runtime-specific:
                //   - Codex (openai): inherits the host ChatGPT login — nothing
                //     to inject here (the runtime already sees it).
                //   - Copilot / Qwen: PENDING-LIVE (device-code flows need
                //     provider-specific credentials we do not fabricate).
                // We deliberately do NOT invent env-var names for tokens we
                // cannot verify, and do NOT expose the seat token as `raw_key`
                // (it is a subscription seat, not an API key — a direct-API
                // caller must not treat it as one). The account remains a
                // first-class rotation member: `provider` is carried below.
                //
                // When a persisted seat credential IS present (e.g. a GitHub
                // OAuth token minted by `duduclaw auth device --provider
                // copilot`, decrypted from `oauth_token_enc` at load time), it
                // is surfaced on `seat_token` so `duduclaw proxy` can exchange
                // it for a short-lived upstream token and forward the seat.
                if let Some(ref token) = a.oauth_token {
                    if !token.is_empty() {
                        seat_token = Some(token.clone());
                    }
                }
            }
        }
    }

    AccountEnv {
        id: a.id.clone(),
        auth_method: a.auth_method.clone(),
        provider: a.provider.clone(),
        raw_key,
        seat_token,
        env_vars,
    }
}

/// Synthesize a single ephemeral API-key selection from a provider's standard
/// env var, used when the config declares no accounts for that provider.
///
/// Returns `None` when the provider is unknown or its env var is unset/empty.
/// The ephemeral id (`<provider>-env`) intentionally does not correspond to any
/// stored account, so `on_success`/`on_error` for it are harmless no-ops — the
/// single ephemeral account has no persistent budget/cooldown state to track.
fn env_fallback_account_env(provider: &str) -> Option<AccountEnv> {
    let names = provider_env_key_names(provider);
    let emit_name = *names.first()?;
    let key = names
        .iter()
        .filter_map(|n| std::env::var(n).ok())
        .find(|v| !v.is_empty())?;

    let mut env_vars = HashMap::new();
    env_vars.insert(emit_name.to_string(), key.clone());
    info!(
        provider,
        "No configured accounts for provider — using ephemeral env-var account"
    );
    Some(AccountEnv {
        id: format!("{provider}-env"),
        auth_method: AuthMethod::ApiKey,
        provider: provider.to_string(),
        raw_key: Some(key),
        seat_token: None,
        env_vars,
    })
}

/// Create a rotator from config.toml rotation settings.
pub fn create_from_config(config: &toml::Table) -> AccountRotator {
    let rotation = config.get("rotation").and_then(|v| v.as_table());
    let strategy_str = rotation.and_then(|r| r.get("strategy")).and_then(|v| v.as_str()).unwrap_or("priority");
    let cooldown = rotation.and_then(|r| r.get("cooldown_after_rate_limit_seconds")).and_then(|v| v.as_integer()).unwrap_or(120) as u64;
    AccountRotator::new(RotationStrategy::from_str(strategy_str), cooldown)
}

#[cfg(test)]
mod select_env_tests {
    use super::*;

    fn account_with_credentials_dir(dir: PathBuf) -> Account {
        Account {
            id: "test".to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority: 1,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "max".to_string(),
            label: "test".to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: None,
            credentials_dir: Some(dir),
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// Regression test for the bug where the auto-detected default OAuth
    /// session would have `CLAUDE_CONFIG_DIR=~/.claude` injected into the
    /// subprocess env, which makes `claude` CLI stop looking at the OS
    /// keychain and return "Not logged in · Please run /login" forever.
    ///
    /// Fix: when `credentials_dir == ~/.claude` (the default location),
    /// `select()` must NOT set `CLAUDE_CONFIG_DIR` at all. Claude CLI then
    /// uses its normal default config + keychain lookup.
    #[tokio::test]
    async fn default_keychain_session_does_not_set_claude_config_dir() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        // Mimic what `detect_default_oauth_session()` produces.
        let default_dir = dirs::home_dir().expect("home").join(".claude");
        rotator
            .push_account_for_test(account_with_credentials_dir(default_dir))
            .await;

        let env = rotator.select().await.expect("should select account");
        assert!(
            !env.env_vars.contains_key("CLAUDE_CONFIG_DIR"),
            "CLAUDE_CONFIG_DIR must not be set for default keychain session; \
             setting it — even to the same path — breaks Claude CLI auth \
             lookup. Got env_vars: {:?}",
            env.env_vars
        );
        // ANTHROPIC_API_KEY must still be set empty to prevent ambient
        // api key from overriding OAuth.
        assert_eq!(env.env_vars.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
    }

    /// A non-default profile directory (e.g. `~/.claude/profiles/work`)
    /// MUST still have `CLAUDE_CONFIG_DIR` injected, otherwise claude CLI
    /// wouldn't know to pick up that profile's credentials.
    #[tokio::test]
    async fn non_default_profile_dir_still_sets_claude_config_dir() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let profile_dir = dirs::home_dir()
            .expect("home")
            .join(".claude/profiles/work");
        rotator
            .push_account_for_test(account_with_credentials_dir(profile_dir.clone()))
            .await;

        let env = rotator.select().await.expect("should select account");
        assert_eq!(
            env.env_vars.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(profile_dir.to_string_lossy().as_ref())
        );
    }

    /// v1.61.0 regression guard: a `setup-token` session must put the token ON
    /// the account, not rely on the child inheriting it.
    ///
    /// The P3 env scrub drops `*_TOKEN` from the spawn environment, so an
    /// account with `oauth_token: None` and no real keychain hands the spawned
    /// CLI nothing — every dispatch failed `authentication_failed` while a
    /// manual `claude -p` in the same container still worked. Asserting on the
    /// built env (not on the detection function, which shells out to `claude`)
    /// keeps this hermetic.
    #[test]
    fn setup_token_account_injects_the_token_into_spawn_env() {
        let mut acct = oauth_account("oauth-default");
        acct.oauth_token = Some("sk-ant-oat01-test".to_string());
        acct.credentials_dir = Some(std::path::PathBuf::from("/home/x/.claude"));

        let env = build_account_env(&acct);

        assert_eq!(
            env.env_vars.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
            Some("sk-ant-oat01-test"),
            "a setup-token account must inject its token explicitly — the spawn \
             env allowlist will not carry it ambiently"
        );
    }

    /// Build an available OAuth account with an explicit setup-token so it
    /// passes `is_available()` without touching the OS keychain.
    fn oauth_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority: 1,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "max".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: Some(format!("token-{id}")),
            credentials_dir: None,
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// L4 regression: the `LeastCost` strategy must rotate fairly among
    /// equal-cost (equal-spend) OAuth accounts instead of always returning
    /// the first one. We simulate the realistic flow where each selection is
    /// followed by `on_success`, which stamps `last_used` and makes the
    /// least-recently-used tiebreaker advance to the next account.
    #[tokio::test]
    async fn least_cost_rotates_among_equal_cost_oauth_accounts() {
        let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
        rotator.push_account_for_test(oauth_account("a")).await;
        rotator.push_account_for_test(oauth_account("b")).await;
        rotator.push_account_for_test(oauth_account("c")).await;

        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            let env = rotator.select().await.expect("should select account");
            seen.insert(env.id.clone());
            // Report success with zero cost so all accounts stay equal-cost;
            // this updates `last_used` so the next select picks a different one.
            rotator.on_success(&env.id, 0).await;
        }

        assert_eq!(
            seen.len(),
            3,
            "LeastCost should rotate across all three equal-cost OAuth accounts, \
             not repeatedly pick the first; saw {seen:?}"
        );
    }

    /// HIGH-C regression: a foreign-provider OAuth seat (added via
    /// `duduclaw auth device`, e.g. copilot/qwen) must NOT suppress the
    /// Anthropic host-login auto-detect — otherwise the anthropic pool is
    /// empty and every channel reply fails NoAccounts.
    #[test]
    fn foreign_oauth_seat_does_not_suppress_anthropic_autodetect() {
        let mut seat = oauth_account("copilot-seat");
        seat.provider = "github".to_string();
        assert!(
            should_autodetect_anthropic_oauth(&[seat]),
            "a github OAuth seat alone must still trigger anthropic auto-detect"
        );

        let mut qwen = oauth_account("qwen-seat");
        qwen.provider = "qwen".to_string();
        let mut codex = oauth_account("codex-seat");
        codex.provider = "openai".to_string();
        assert!(
            should_autodetect_anthropic_oauth(&[qwen, codex]),
            "multiple foreign seats must still trigger anthropic auto-detect"
        );
    }

    /// The auto-detect gate closes only when an Anthropic OAuth account is
    /// already configured; an Anthropic API-key account does not close it
    /// (API-key and OAuth are distinct pools by design).
    #[test]
    fn anthropic_oauth_account_suppresses_autodetect() {
        let anth = oauth_account("anthropic-oauth"); // provider = "anthropic"
        assert!(!should_autodetect_anthropic_oauth(&[anth]));

        // Empty pool → detect.
        assert!(should_autodetect_anthropic_oauth(&[]));

        // Mixed: foreign seat + anthropic OAuth → no detect needed.
        let mut seat = oauth_account("copilot-seat");
        seat.provider = "github".to_string();
        let anth2 = oauth_account("anthropic-oauth-2");
        assert!(!should_autodetect_anthropic_oauth(&[seat, anth2]));
    }
}

#[cfg(test)]
mod provider_rotation_tests {
    use super::*;

    /// Build an available API-key account for a given provider.
    fn api_account(id: &str, provider: &str, key: &str) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::ApiKey,
            provider: provider.to_string(),
            priority: 10,
            monthly_budget_cents: 5000,
            tags: vec![],
            profile: String::new(),
            email: String::new(),
            subscription: String::new(),
            label: id.to_string(),
            expires_at: None,
            api_key: key.to_string(),
            oauth_token: None,
            credentials_dir: None,
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// An account parsed WITHOUT a `provider` field must default to "anthropic"
    /// so existing configs behave byte-identically.
    #[test]
    fn absent_provider_defaults_to_anthropic() {
        let toml_src = r#"
            id = "a"
            type = "api_key"
            api_key = "sk-test"
        "#;
        let table: toml::Table = toml_src.parse().unwrap();
        // Round-trip the default via serde: an Account deserialized from a table
        // missing `provider` gets the default.
        #[derive(serde::Deserialize)]
        struct Probe {
            #[serde(default = "default_provider")]
            provider: String,
        }
        let p: Probe = table.clone().try_into().unwrap();
        assert_eq!(p.provider, "anthropic");
    }

    /// A `provider = "openai"` field is parsed and preserved.
    #[test]
    fn present_provider_is_parsed() {
        let toml_src = r#"
            provider = "openai"
        "#;
        let table: toml::Table = toml_src.parse().unwrap();
        #[derive(serde::Deserialize)]
        struct Probe {
            #[serde(default = "default_provider")]
            provider: String,
        }
        let p: Probe = table.try_into().unwrap();
        assert_eq!(p.provider, "openai");
    }

    /// `select_for_provider` only considers accounts of the requested provider.
    #[tokio::test]
    async fn select_for_provider_filters_by_provider() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(api_account("anthropic-1", "anthropic", "sk-ant"))
            .await;
        rotator
            .push_account_for_test(api_account("openai-1", "openai", "sk-openai"))
            .await;

        let sel = rotator
            .select_for_provider("openai")
            .await
            .expect("should select the openai account");
        assert_eq!(sel.id, "openai-1");
        assert_eq!(sel.provider, "openai");
        // The openai account emits OPENAI_API_KEY (not ANTHROPIC_API_KEY).
        assert_eq!(
            sel.env_vars.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-openai")
        );
        assert!(!sel.env_vars.contains_key("ANTHROPIC_API_KEY"));
        // Raw key is exposed for direct-API callers.
        assert_eq!(sel.raw_key.as_deref(), Some("sk-openai"));
    }

    /// Back-compat: `select()` == `select_for_provider("anthropic")` and emits
    /// the unchanged ANTHROPIC_API_KEY var for an anthropic API-key account.
    #[tokio::test]
    async fn select_is_anthropic_back_compat() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(api_account("anthropic-1", "anthropic", "sk-ant"))
            .await;
        rotator
            .push_account_for_test(api_account("openai-1", "openai", "sk-openai"))
            .await;

        let sel = rotator.select().await.expect("should select anthropic");
        assert_eq!(sel.id, "anthropic-1");
        assert_eq!(sel.provider, "anthropic");
        assert_eq!(
            sel.env_vars.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant")
        );
        assert!(!sel.env_vars.contains_key("OPENAI_API_KEY"));
    }

    /// When a provider has configured accounts that are all unavailable,
    /// selection returns None (does NOT fall through to env-var fallback).
    #[tokio::test]
    async fn unavailable_configured_accounts_do_not_env_fallback() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        // Budget-exhausted API key → unavailable, but still "configured".
        let mut acc = api_account("openai-1", "openai", "sk-openai");
        acc.monthly_budget_cents = 100;
        acc.spent_this_month = 200;
        rotator.push_account_for_test(acc).await;

        assert!(rotator.select_for_provider("openai").await.is_none());
    }

    /// env-var fallback synthesizes exactly one ephemeral account when the
    /// config declares no accounts for the requested provider.
    #[tokio::test]
    async fn env_var_fallback_synthesizes_single_account() {
        // groq is not referenced by any other test in this crate, so mutating
        // its env var here is isolated within this test binary.
        unsafe { std::env::set_var("GROQ_API_KEY", "gsk-test") };
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);

        let sel = rotator
            .select_for_provider("groq")
            .await
            .expect("env var fallback should synthesize an account");
        assert_eq!(sel.id, "groq-env");
        assert_eq!(sel.provider, "groq");
        assert_eq!(
            sel.env_vars.get("GROQ_API_KEY").map(String::as_str),
            Some("gsk-test")
        );
        assert_eq!(sel.raw_key.as_deref(), Some("gsk-test"));

        unsafe { std::env::remove_var("GROQ_API_KEY") };
    }

    /// Unknown provider with no env var → no fallback, returns None.
    #[tokio::test]
    async fn unknown_provider_with_no_env_returns_none() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        assert!(rotator
            .select_for_provider("not-a-real-provider")
            .await
            .is_none());
    }

    /// Gemini env-var fallback accepts the GOOGLE_API_KEY alias but always
    /// emits the canonical GEMINI_API_KEY name.
    #[tokio::test]
    async fn gemini_alias_env_emits_canonical_name() {
        unsafe { std::env::set_var("GOOGLE_API_KEY", "goog-test") };
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);

        let sel = rotator
            .select_for_provider("gemini")
            .await
            .expect("GOOGLE_API_KEY alias should satisfy gemini fallback");
        assert_eq!(
            sel.env_vars.get("GEMINI_API_KEY").map(String::as_str),
            Some("goog-test"),
            "canonical GEMINI_API_KEY must be emitted even when read via alias"
        );
        assert!(!sel.env_vars.contains_key("GOOGLE_API_KEY"));

        unsafe { std::env::remove_var("GOOGLE_API_KEY") };
    }
}

// ── Subscription-OAuth breadth (G2 Part A) ──────────────────
//
// The rotator carries consumer subscription seats from providers OTHER than
// Anthropic (ChatGPT Codex / GitHub Copilot / Qwen Portal) as OAuth pool
// members, selectable under any strategy within their provider pool.
#[cfg(test)]
mod subscription_oauth_tests {
    use super::*;

    /// A subscription OAuth seat for an arbitrary provider. No explicit token
    /// and no credentials dir — mirrors the Codex "host login inherited" case.
    fn oauth_seat(id: &str, provider: &str) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: provider.to_string(),
            priority: 1,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "pro".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: None,
            credentials_dir: None,
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// A non-Anthropic subscription seat is available with neither an explicit
    /// token nor a credentials dir (host-login inheritance) — unlike an
    /// Anthropic OAuth account, which requires one of the two.
    #[test]
    fn non_anthropic_oauth_seat_available_without_token_or_dir() {
        let codex = oauth_seat("codex-1", "openai");
        assert!(
            codex.is_available(),
            "non-Anthropic subscription seat should be available on health alone"
        );
        // Contrast: an Anthropic OAuth account with no token/dir is unavailable.
        let anth = oauth_seat("anth-1", "anthropic");
        assert!(
            !anth.is_available(),
            "Anthropic OAuth needs an explicit token or credentials dir"
        );
    }

    /// `select_for_provider` isolates by provider across a mixed OAuth pool and
    /// carries the provider on the selection.
    #[tokio::test]
    async fn select_isolates_by_provider_across_oauth_pool() {
        let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
        rotator.push_account_for_test(oauth_seat("codex-1", "openai")).await;
        rotator.push_account_for_test(oauth_seat("copilot-1", "github")).await;

        let sel = rotator
            .select_for_provider("openai")
            .await
            .expect("should select the openai subscription seat");
        assert_eq!(sel.id, "codex-1");
        assert_eq!(sel.provider, "openai");
        // Codex inherits host login — no fabricated token env var is emitted,
        // and (critically) no ANTHROPIC_API_KEY leaks onto the seat.
        assert!(sel.env_vars.is_empty(), "no env vars for host-login-inherited seat");
        // The seat token is NOT exposed as an API key.
        assert!(sel.raw_key.is_none());

        let sel2 = rotator
            .select_for_provider("github")
            .await
            .expect("should select the copilot seat");
        assert_eq!(sel2.id, "copilot-1");
        assert_eq!(sel2.provider, "github");
    }

    /// `select()` (the Anthropic back-compat shim) never returns a
    /// non-Anthropic subscription seat — provider isolation holds.
    #[tokio::test]
    async fn anthropic_shim_ignores_non_anthropic_seats() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth_seat("codex-1", "openai")).await;
        // No anthropic account present → the anthropic pool is empty.
        assert!(
            rotator.select().await.is_none(),
            "anthropic selection must not fall through to an openai seat"
        );
    }

    /// LeastCost prefers an OAuth seat (subscription, zero per-token cost) over
    /// an API-key account within the SAME provider pool.
    #[tokio::test]
    async fn least_cost_prefers_oauth_seat_within_provider() {
        let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
        // API-key openai account…
        let mut key_acc = oauth_seat("openai-key", "openai");
        key_acc.auth_method = AuthMethod::ApiKey;
        key_acc.api_key = "sk-openai".to_string();
        key_acc.monthly_budget_cents = 5000;
        rotator.push_account_for_test(key_acc).await;
        // …and an OAuth seat for the same provider.
        rotator.push_account_for_test(oauth_seat("openai-seat", "openai")).await;

        let sel = rotator
            .select_for_provider("openai")
            .await
            .expect("should select within the openai pool");
        assert_eq!(
            sel.id, "openai-seat",
            "LeastCost should prefer the zero-cost subscription seat"
        );
    }

    /// A stored seat credential (decrypted from `oauth_token_enc` at load) is
    /// surfaced on `AccountEnv.seat_token` for a non-Anthropic OAuth seat, but
    /// never as `raw_key` (it is not an API key). `has_seat_for_provider`
    /// reports it as available.
    #[tokio::test]
    async fn stored_seat_credential_surfaces_on_seat_token_not_raw_key() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut seat = oauth_seat("copilot-seat", "github");
        seat.oauth_token = Some("gho_stored_token".to_string());
        rotator.push_account_for_test(seat).await;

        assert!(rotator.has_seat_for_provider("github").await);
        assert!(!rotator.has_seat_for_provider("qwen").await);

        let sel = rotator
            .select_for_provider("github")
            .await
            .expect("should select the copilot seat");
        assert_eq!(sel.seat_token.as_deref(), Some("gho_stored_token"));
        assert!(sel.raw_key.is_none(), "seat token must NOT be an API key");
    }

    /// A non-Anthropic OAuth seat WITHOUT a stored token (host-login-inherited,
    /// e.g. Codex) has no `seat_token` and is not reported by
    /// `has_seat_for_provider` (nothing to forward through the proxy).
    #[tokio::test]
    async fn host_login_seat_has_no_seat_token() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(oauth_seat("codex-1", "openai"))
            .await;
        let sel = rotator.select_for_provider("openai").await.unwrap();
        assert!(sel.seat_token.is_none());
        assert!(!rotator.has_seat_for_provider("openai").await);
    }

    /// The subscription catalogue exposes the four consumer sources.
    #[test]
    fn known_subscription_providers_catalogue() {
        let cat = known_subscription_providers();
        let ids: Vec<&str> = cat.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&"anthropic"));
        assert!(ids.contains(&"openai"));
        assert!(ids.contains(&"github"));
        assert!(ids.contains(&"qwen"));
    }
}

// ── WP10 (2026-08-04 field incident) regression tests ────────────────
#[cfg(test)]
mod wp10_on_error_recovery_tests {
    use super::*;

    fn oauth_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority: 1,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "max".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            // An anthropic OAuth account is only "available" with a setup
            // token or an OS-keychain credentials dir — mirror the real
            // single-account install (keychain OAuth, no explicit token).
            oauth_token: None,
            credentials_dir: Some(PathBuf::from("/tmp/wp10-fake-credentials")),
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// The incident shape: ONE OAuth account. Three generic errors used to
    /// mark it unhealthy with `cooldown_until = None`, and `is_available()`
    /// only forgives an unhealthy account whose cooldown has EXPIRED — so a
    /// `None` cooldown meant permanently unavailable, and every later message
    /// died with "All accounts exhausted".
    #[tokio::test]
    async fn single_account_recovers_after_generic_errors() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth_account("oauth-default")).await;

        for _ in 0..3 {
            rotator.on_error("oauth-default").await;
        }

        // Unhealthy right now — that part is intended.
        assert!(
            rotator.select().await.is_none(),
            "3 consecutive errors should take the account out of rotation"
        );

        // ...but the outage must be BOUNDED. A cooldown has to exist, or the
        // account can never come back on its own.
        {
            let accounts = rotator.accounts.read().await;
            let acc = accounts.iter().find(|a| a.id == "oauth-default").unwrap();
            assert!(!acc.is_healthy);
            let cd = acc
                .cooldown_until
                .expect("on_error must attach a cooldown so recovery is automatic");
            assert!(cd > Utc::now(), "cooldown should be in the future");
        }

        // Simulate the cooldown elapsing: the account becomes available again
        // with no operator intervention and no gateway restart.
        {
            let mut accounts = rotator.accounts.write().await;
            let acc = accounts.iter_mut().find(|a| a.id == "oauth-default").unwrap();
            acc.cooldown_until = Some(Utc::now() - chrono::Duration::seconds(1));
        }
        assert!(
            rotator.select().await.is_some(),
            "an expired cooldown must return the sole account to rotation"
        );
    }

    /// WP10 M4 — the tier must follow the actual cooldown horizon, because
    /// "a few minutes" and "up to 24 hours" are what the user plans around.
    #[tokio::test]
    async fn unavailable_reason_tiers_by_cooldown_length() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth_account("acc")).await;

        // Healthy ⇒ nothing to attribute.
        assert_eq!(
            rotator.unavailable_reason().await,
            UnavailableReason::Unknown
        );

        // Rate limit books `cooldown_seconds` (120 s) ⇒ short.
        rotator.on_rate_limited("acc").await;
        assert_eq!(
            rotator.unavailable_reason().await,
            UnavailableReason::ShortCooldown
        );

        // Billing books 24 h ⇒ long, and must win over the short window.
        rotator.on_billing_exhausted("acc").await;
        assert_eq!(
            rotator.unavailable_reason().await,
            UnavailableReason::LongCooldown
        );
    }

    /// Unhealthy with no cooldown at all is NOT attributable — the caller must
    /// hedge rather than promise a horizon it cannot know.
    #[tokio::test]
    async fn unavailable_reason_is_unknown_without_a_cooldown() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut acc = oauth_account("acc");
        acc.is_healthy = false;
        rotator.push_account_for_test(acc).await;
        assert_eq!(
            rotator.unavailable_reason().await,
            UnavailableReason::Unknown
        );
    }

    /// A generic error must never shorten a longer billing cooldown.
    #[tokio::test]
    async fn on_error_never_shortens_an_existing_longer_cooldown() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth_account("acc")).await;

        rotator.on_billing_exhausted("acc").await; // 24 h
        let billing_until = {
            let accounts = rotator.accounts.read().await;
            accounts.iter().find(|a| a.id == "acc").unwrap().cooldown_until.unwrap()
        };

        for _ in 0..3 {
            rotator.on_error("acc").await; // 120 s — must not win
        }

        let accounts = rotator.accounts.read().await;
        let cd = accounts.iter().find(|a| a.id == "acc").unwrap().cooldown_until.unwrap();
        assert_eq!(
            cd, billing_until,
            "a 120s generic cooldown must not override the 24h billing cooldown"
        );
    }
}

// ── G1: agent.toml [model] account_pool → rotation candidate set ─────
//
// Before this, `account_pool` was serialized, editable in the dashboard, and
// read by nobody — a dead setting. These tests pin the three semantics that
// make it safe to turn on: it narrows, it never bricks, and an unset pool is a
// zero-behavior-change no-op.
#[cfg(test)]
mod account_pool_tests {
    use super::*;

    fn oauth(id: &str, priority: u32) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "max".to_string(),
            label: String::new(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: Some(format!("token-{id}")),
            credentials_dir: None,
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    fn labeled(id: &str, priority: u32, label: &str) -> Account {
        let mut a = oauth(id, priority);
        a.label = label.to_string();
        a
    }

    fn pool(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    // ── pure narrowing decision ──────────────────────────────────────

    /// An unset / blank-only pool must not even be considered — that is what
    /// makes "no pool configured" a byte-identical no-op.
    #[test]
    fn blank_pool_is_not_requested() {
        let a = oauth("a", 1);
        let avail = vec![&a];
        assert!(matches!(
            narrow_by_pool(&avail, &[]),
            PoolNarrowing::NotRequested
        ));
        assert!(matches!(
            narrow_by_pool(&avail, &pool(&["", "   "])),
            PoolNarrowing::NotRequested
        ));
    }

    #[test]
    fn pool_narrows_by_id() {
        let (a, b, c) = (oauth("a", 1), oauth("b", 2), oauth("c", 3));
        let avail = vec![&a, &b, &c];
        match narrow_by_pool(&avail, &pool(&["b", "c"])) {
            PoolNarrowing::Applied(v) => {
                let ids: Vec<&str> = v.iter().map(|a| a.id.as_str()).collect();
                assert_eq!(ids, vec!["b", "c"]);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    /// Operators reference accounts by the label the dashboard shows them, not
    /// only by the internal id — both must resolve. Labels are frequently CJK.
    #[test]
    fn pool_matches_label_including_cjk() {
        let a = labeled("acc-1785771258", 1, "工作帳號");
        let b = oauth("b", 2);
        let avail = vec![&a, &b];
        match narrow_by_pool(&avail, &pool(&["工作帳號"])) {
            PoolNarrowing::Applied(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].id, "acc-1785771258");
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        // ASCII case folding applies to ids/labels too.
        assert!(matches!(
            narrow_by_pool(&avail, &pool(&["  B  "])),
            PoolNarrowing::Applied(_)
        ));
    }

    /// Project convention 2: no unanchored substring matching for routing.
    /// A pool entry `main` must not capture `main-backup`.
    #[test]
    fn pool_match_is_exact_not_substring() {
        let backup = oauth("main-backup", 1);
        let avail = vec![&backup];
        assert!(matches!(
            narrow_by_pool(&avail, &pool(&["main"])),
            PoolNarrowing::FailedOpen
        ));
    }

    /// A pool naming only accounts that do not exist (renamed / deleted /
    /// copied from a template) must fail OPEN, not empty the candidate set.
    #[test]
    fn stale_pool_fails_open() {
        let a = oauth("a", 1);
        let avail = vec![&a];
        assert!(matches!(
            narrow_by_pool(&avail, &pool(&["ghost", "another-ghost"])),
            PoolNarrowing::FailedOpen
        ));
    }

    // ── end-to-end selection ─────────────────────────────────────────

    /// Priority strategy: the pool changes WHICH accounts compete, the
    /// strategy still picks the lowest priority number among them.
    #[tokio::test]
    async fn priority_selects_best_within_pool() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth("a", 1)).await; // best globally
        rotator.push_account_for_test(oauth("b", 5)).await;
        rotator.push_account_for_test(oauth("c", 9)).await;

        let sel = rotator
            .select_with_pool(&pool(&["b", "c"]))
            .await
            .expect("pooled account must be selectable");
        assert_eq!(sel.id, "b", "lowest priority *within the pool*, not globally");
    }

    /// Failover shares Priority's ordering but is a distinct configured
    /// strategy — pin it explicitly so a future divergence is caught.
    #[tokio::test]
    async fn failover_selects_within_pool() {
        let rotator = AccountRotator::new(RotationStrategy::Failover, 120);
        rotator.push_account_for_test(oauth("primary", 1)).await;
        rotator.push_account_for_test(oauth("secondary", 2)).await;

        let sel = rotator.select_with_pool(&pool(&["secondary"])).await.unwrap();
        assert_eq!(sel.id, "secondary");
    }

    /// RoundRobin must rotate *inside* the pool and never hand out a
    /// non-pooled account while a pooled one is available.
    #[tokio::test]
    async fn round_robin_stays_within_pool() {
        let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 120);
        rotator.push_account_for_test(oauth("a", 1)).await;
        rotator.push_account_for_test(oauth("b", 1)).await;
        rotator.push_account_for_test(oauth("c", 1)).await;

        let p = pool(&["a", "c"]);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..6 {
            let sel = rotator.select_with_pool(&p).await.unwrap();
            assert_ne!(sel.id, "b", "non-pooled account leaked into rotation");
            seen.insert(sel.id);
        }
        assert_eq!(seen.len(), 2, "both pooled accounts should be used: {seen:?}");
    }

    /// LeastCost prefers OAuth then least-spent; the pool must gate the field
    /// it chooses from without changing that preference order.
    #[tokio::test]
    async fn least_cost_stays_within_pool() {
        let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
        rotator.push_account_for_test(oauth("cheap", 1)).await;
        rotator.push_account_for_test(oauth("pooled", 9)).await;

        for _ in 0..3 {
            let sel = rotator.select_with_pool(&pool(&["pooled"])).await.unwrap();
            assert_eq!(sel.id, "pooled");
            rotator.on_success(&sel.id, 0).await;
        }
    }

    /// The load-bearing safety property: a pool that resolves to nothing must
    /// still produce an account. A stale `account_pool` copied from a template
    /// (`["main"]`) is the common real-world shape — it must degrade to the
    /// full set, not to "no accounts available".
    #[tokio::test]
    async fn stale_pool_falls_back_to_full_account_set() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(oauth("claude-oauth-1785771258", 1))
            .await;

        let sel = rotator
            .select_with_pool(&pool(&["main"]))
            .await
            .expect("a stale pool must never leave the agent with no account");
        assert_eq!(sel.id, "claude-oauth-1785771258");
    }

    /// Same fail-open rule when the pooled accounts EXIST but are all
    /// unavailable (rate-limited / cooling down): availability wins, and the
    /// non-pooled account answers instead of the reply failing.
    #[tokio::test]
    async fn exhausted_pool_falls_back_to_non_pooled_account() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth("pooled", 1)).await;
        rotator.push_account_for_test(oauth("spare", 2)).await;

        // Pool is honored while its member is healthy.
        assert_eq!(
            rotator.select_with_pool(&pool(&["pooled"])).await.unwrap().id,
            "pooled"
        );

        rotator.on_rate_limited("pooled").await;

        let sel = rotator
            .select_with_pool(&pool(&["pooled"]))
            .await
            .expect("fail-open must survive an exhausted pool");
        assert_eq!(sel.id, "spare");
    }

    /// Zero-behavior-change guarantee: an agent with no pool selects exactly
    /// what the legacy `select()` selects.
    #[tokio::test]
    async fn empty_pool_is_identical_to_legacy_select() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(oauth("a", 1)).await;
        rotator.push_account_for_test(oauth("b", 2)).await;

        let legacy = rotator.select().await.unwrap();
        let pooled = rotator.select_with_pool(&[]).await.unwrap();
        assert_eq!(legacy.id, pooled.id);
        assert_eq!(legacy.env_vars, pooled.env_vars);
    }

    /// The pool is provider-scoped like every other rotator filter: an
    /// anthropic pool must not reach into another provider's accounts, and a
    /// provider with no configured accounts still gets its env-var fallback.
    #[tokio::test]
    async fn pool_does_not_cross_provider_boundaries() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut foreign = oauth("shared-name", 1);
        foreign.provider = "github".to_string();
        rotator.push_account_for_test(foreign).await;
        rotator.push_account_for_test(oauth("anthropic-1", 1)).await;

        // Pool names the github account, but we are selecting for anthropic:
        // it must NOT be reachable — fail-open hands back anthropic's own set.
        let sel = rotator.select_with_pool(&pool(&["shared-name"])).await.unwrap();
        assert_eq!(sel.id, "anthropic-1");
        assert_eq!(sel.provider, "anthropic");
    }
}

/// WP-8A / credentials doctrine P2 (item #5): `resolve_api_key` and
/// `resolve_oauth_token` used to be two independent hand-rolled "decrypt
/// `<field>_enc` via keyfile, else resolve a `secret://` reference, else use
/// the plaintext literally" chains. This module pins down that the
/// consolidation onto `duduclaw_security::secret_ref::SecretRef` is
/// behavior-preserving: encrypted still wins over plaintext, a `secret://`
/// reference still resolves through the configured backend (here: `env`,
/// the only local, no-network backend that's practical to exercise without a
/// live Vault/1Password/Infisical), and a bare plaintext `oauth_token` is
/// still never consumed.
#[cfg(test)]
mod wp8a_secret_ref_consolidation_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A throwaway home directory carrying its own `.keyfile`, mirroring the
    /// helper `duduclaw-security/src/secret_ref.rs` uses for the same purpose
    /// — kept local here since `duduclaw-agent` cannot depend on
    /// `duduclaw-security`'s `#[cfg(test)]`-only items across the crate
    /// boundary.
    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-rotator-secretref-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn encrypt(&self, plain: &str) -> String {
            use duduclaw_security::crypto::CryptoEngine;
            let keyfile = self.0.join(".keyfile");
            let key = if keyfile.exists() {
                let bytes = std::fs::read(&keyfile).unwrap();
                let mut k = [0u8; 32];
                k.copy_from_slice(&bytes);
                k
            } else {
                let k = CryptoEngine::generate_key().unwrap();
                std::fs::write(&keyfile, k).unwrap();
                k
            };
            CryptoEngine::new(&key).unwrap().encrypt_string(plain).unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── resolve_api_key ──────────────────────────────────────────

    #[tokio::test]
    async fn enc_field_wins_over_a_differently_named_plaintext_field() {
        let home = TempHome::new();
        let enc = home.encrypt("from-api-key-enc");
        let table: toml::Table = format!(
            "api_key_enc = \"{enc}\"\nanthropic_api_key = \"plaintext-should-lose\"\n"
        )
        .parse()
        .unwrap();
        // Encrypted always wins over plaintext, even though `api_key_enc`
        // and `anthropic_api_key` are different field names — precedence is
        // "any enc field" before "any plaintext field", preserved from
        // before the WP-8A consolidation.
        assert_eq!(resolve_api_key(home.path(), &table).await, "from-api-key-enc");
    }

    #[tokio::test]
    async fn secret_env_reference_resolves_through_the_shared_resolver() {
        let home = TempHome::new();
        let var = format!("DUDUCLAW_ROTATOR_APIKEY_TEST_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "from-env-var") };
        let table: toml::Table = format!("anthropic_api_key = \"secret://env/{var}\"\n")
            .parse()
            .unwrap();
        let got = resolve_api_key(home.path(), &table).await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got, "from-env-var");
    }

    #[tokio::test]
    async fn plaintext_literal_still_works_as_legacy_fallback() {
        let home = TempHome::new();
        let table: toml::Table = "api_key = \"sk-legacy-literal\"\n".parse().unwrap();
        assert_eq!(resolve_api_key(home.path(), &table).await, "sk-legacy-literal");
    }

    #[tokio::test]
    async fn unresolvable_secret_reference_falls_through_to_the_next_field_name() {
        let home = TempHome::new();
        // anthropic_api_key points at an unset env var (unresolvable);
        // api_key carries a usable literal. Must not just give up on the
        // first field's failure.
        let table: toml::Table =
            "anthropic_api_key = \"secret://env/DUDUCLAW_DEFINITELY_UNSET_ROTATOR_XYZ\"\napi_key = \"sk-fallback\"\n"
                .parse()
                .unwrap();
        assert_eq!(resolve_api_key(home.path(), &table).await, "sk-fallback");
    }

    #[tokio::test]
    async fn nothing_configured_resolves_to_empty_string() {
        let home = TempHome::new();
        let table: toml::Table = "".parse().unwrap();
        assert_eq!(resolve_api_key(home.path(), &table).await, "");
    }

    // ── resolve_oauth_token ──────────────────────────────────────

    #[tokio::test]
    async fn oauth_token_enc_decrypts_via_keyfile() {
        let home = TempHome::new();
        let enc = home.encrypt("real-oauth-token");
        let table: toml::Table = format!("oauth_token_enc = \"{enc}\"\n").parse().unwrap();
        assert_eq!(
            resolve_oauth_token(home.path(), &table).await.as_deref(),
            Some("real-oauth-token")
        );
    }

    #[tokio::test]
    async fn oauth_token_secret_reference_resolves() {
        let home = TempHome::new();
        let var = format!("DUDUCLAW_ROTATOR_OAUTH_TEST_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "oauth-via-env") };
        let table: toml::Table = format!("oauth_token = \"secret://env/{var}\"\n")
            .parse()
            .unwrap();
        let got = resolve_oauth_token(home.path(), &table).await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got.as_deref(), Some("oauth-via-env"));
    }

    /// The behavior-preservation guarantee this whole consolidation exists to
    /// keep intact: a bare plaintext `oauth_token` (not a `secret://`
    /// reference) is NEVER consumed, even though `resolve_api_key`'s
    /// plaintext-field precedence tier would happily accept the equivalent
    /// shape for an API key. `oauth_token` and `api_key` are not
    /// interchangeable dialects — only `oauth_token_enc` and a `secret://`
    /// reference are legitimate sources for an OAuth token.
    #[tokio::test]
    async fn bare_plaintext_oauth_token_is_never_consumed() {
        let home = TempHome::new();
        let table: toml::Table = "oauth_token = \"sk-ant-oat01-literal-not-a-reference\"\n"
            .parse()
            .unwrap();
        assert_eq!(resolve_oauth_token(home.path(), &table).await, None);
    }

    #[tokio::test]
    async fn oauth_token_enc_wins_over_a_secret_reference_plaintext() {
        let home = TempHome::new();
        let enc = home.encrypt("enc-wins");
        let table: toml::Table = format!(
            "oauth_token_enc = \"{enc}\"\noauth_token = \"secret://env/DUDUCLAW_UNUSED_ROTATOR\"\n"
        )
        .parse()
        .unwrap();
        assert_eq!(
            resolve_oauth_token(home.path(), &table).await.as_deref(),
            Some("enc-wins")
        );
    }

    #[tokio::test]
    async fn nothing_configured_oauth_token_is_none() {
        let home = TempHome::new();
        let table: toml::Table = "".parse().unwrap();
        assert_eq!(resolve_oauth_token(home.path(), &table).await, None);
    }
}

/// WP-A (TODO-ai-runtimes-2026-09.md §3 WP-A item 2) — end-to-end coverage
/// for the exact `[[accounts]]` shape `duduclaw-gateway`'s `build_account_entry`
/// now writes for a non-Anthropic provider: `provider = "<id>"` +
/// `api_key` / `api_key_enc` (never `anthropic_api_key*`, which stays
/// Anthropic-only). `resolve_api_key`'s field-name precedence and
/// `select_for_provider`'s provider filter were already covered separately
/// (`wp8a_secret_ref_consolidation_tests`, `provider_rotation_tests`); this
/// module pins that `load_from_config` — the actual gateway startup / account
/// reload path `claude_runner::resolve_provider_key` relies on — reads that
/// combination correctly end to end, so an OpenAI/Gemini/xAI/DeepSeek key
/// added via `accounts.add` is really found by the Direct-API key resolution
/// path, not just by its lower-level pieces in isolation.
#[cfg(test)]
mod wpa_load_from_config_provider_tests {
    use super::*;

    #[tokio::test]
    async fn non_anthropic_provider_account_is_loaded_and_selectable() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            r#"
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key = "sk-openai-test-key"
priority = 1
monthly_budget_cents = 5000
"#,
        )
        .unwrap();

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let loaded = rotator.load_from_config(home.path()).await.unwrap();
        // `>= 1`, not `== 1`: step 2 of `load_from_config` auto-detects a
        // host `claude` login (`claude auth status`) whenever the config has
        // no Anthropic OAuth row, so on a developer machine that is signed
        // in to Claude Code this legitimately loads a second account. The
        // assertions below pin the row this test is about by id.
        assert!(loaded >= 1, "the [[accounts]] entry must be loaded (got {loaded})");

        let sel = rotator
            .select_for_provider("openai")
            .await
            .expect("the openai account must be selectable by provider");
        assert_eq!(sel.id, "openai-prod");
        assert_eq!(
            sel.raw_key.as_deref(),
            Some("sk-openai-test-key"),
            "raw_key must come from the `api_key` field (not `anthropic_api_key`)"
        );
        // Cross-provider filtering itself (an openai-only config must not
        // surface under a different provider's pool) is already covered by
        // `provider_rotation_tests::select_for_provider_filters_by_provider`;
        // not re-asserted here to avoid depending on whether ANTHROPIC_API_KEY
        // happens to be set in the environment running this test (the
        // no-configured-accounts path falls back to synthesizing an ephemeral
        // env-var account — see `env_fallback_account_env` — which is correct
        // behavior but would make an `is_none()` assertion here environment-
        // dependent).
    }

    /// The `_enc` twin of the same non-Anthropic field-name shape, going
    /// through the real per-machine keyfile encryption path (not a plaintext
    /// literal), since `build_account_entry` writes `api_key_enc` whenever
    /// encryption succeeds.
    #[tokio::test]
    async fn non_anthropic_provider_account_resolves_encrypted_key() {
        use duduclaw_security::crypto::CryptoEngine;

        let home = tempfile::tempdir().unwrap();
        let keyfile = home.path().join(".keyfile");
        let key = CryptoEngine::generate_key().unwrap();
        std::fs::write(&keyfile, key).unwrap();
        let enc = CryptoEngine::new(&key)
            .unwrap()
            .encrypt_string("sk-deepseek-enc-key")
            .unwrap();

        std::fs::write(
            home.path().join("config.toml"),
            format!(
                r#"
[[accounts]]
id = "deepseek-prod"
type = "api_key"
provider = "deepseek"
api_key_enc = "{enc}"
priority = 1
monthly_budget_cents = 5000
"#
            ),
        )
        .unwrap();

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.load_from_config(home.path()).await.unwrap();

        let sel = rotator
            .select_for_provider("deepseek")
            .await
            .expect("the deepseek account must be selectable by provider");
        assert_eq!(sel.raw_key.as_deref(), Some("sk-deepseek-enc-key"));
    }
}

/// WP-10C: `provider_env_key_names` here is now a thin delegate to
/// `duduclaw_core::provider_env::provider_env_key_names` — the third
/// hand-copied table collapsed onto the WP-8B single source of truth. These
/// tests pin the delegation itself so a future edit to either side can't
/// silently re-diverge without a red test.
#[cfg(test)]
mod wp10c_provider_env_delegation_tests {
    use super::provider_env_key_names;

    /// Every known provider must resolve to exactly the same name list as the
    /// canonical `duduclaw-core` table — this is the whole point of the
    /// delegation (not just "non-empty", but byte-identical).
    #[test]
    fn matches_core_table_for_every_known_provider() {
        for provider in duduclaw_core::provider_env::KNOWN_PROVIDER_IDS {
            assert_eq!(
                provider_env_key_names(provider),
                duduclaw_core::provider_env::provider_env_key_names(provider),
                "agent-crate delegate diverged from duduclaw-core for provider `{provider}`"
            );
        }
    }

    /// Spot-check the two multi-name providers plus the alias pair, matching
    /// the pre-consolidation table's asserted shape.
    #[test]
    fn known_provider_shapes_are_unchanged() {
        assert_eq!(provider_env_key_names("anthropic"), &["ANTHROPIC_API_KEY"]);
        assert_eq!(provider_env_key_names("openai"), &["OPENAI_API_KEY"]);
        assert_eq!(
            provider_env_key_names("gemini"),
            &["GEMINI_API_KEY", "GOOGLE_API_KEY"]
        );
        assert_eq!(
            provider_env_key_names("gemini"),
            provider_env_key_names("google"),
            "gemini/google must remain aliases"
        );
        assert_eq!(
            provider_env_key_names("qwen"),
            &["DASHSCOPE_API_KEY", "QWEN_API_KEY"]
        );
    }

    /// Unknown provider ids must still return an empty slice, never a guess.
    #[test]
    fn unknown_provider_is_empty() {
        assert!(provider_env_key_names("totally-unknown-vendor").is_empty());
    }
}

// ── 2026-09 credential-hardening regression tests (D2 / D3 / D4 / D5) ───
//
// The incident these pin down: a setup-token started returning
// `403 oauth_not_allowed_for_organization`; the rotator benched the account
// after three generic errors and a fake health probe (`claude auth status` →
// `loggedIn: true`) resurrected it 60 seconds later, forever — burning one
// scheduled dispatch per resurrection for 18 hours. Separately, an
// `oauth_token_enc` that decrypted to an empty string loaded as a perfectly
// normal account and spawned credential-less children with zero warnings.
#[cfg(test)]
mod credential_hardening_tests {
    use super::*;
    use crate::credential_probe::CredentialKind;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ── fixtures ────────────────────────────────────────────────────

    /// An Anthropic OAuth account carrying an explicit setup-token — the shape
    /// the credential probe can actually authenticate.
    fn token_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority: 1,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "default".to_string(),
            email: String::new(),
            subscription: "max".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: Some(format!("sk-ant-oat01-{id}")),
            credentials_dir: None,
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            credential_state: CredentialState::Unverified,
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// An Anthropic OAuth account with NO explicit token — an OS-keychain
    /// session, which has no secret we can present to the API.
    fn keychain_account(id: &str) -> Account {
        let mut a = token_account(id);
        a.oauth_token = None;
        a.credentials_dir = Some(PathBuf::from("/tmp/duduclaw-fake-credentials"));
        a
    }

    async fn snapshot(rotator: &AccountRotator, id: &str) -> Account {
        let accounts = rotator.accounts.read().await;
        accounts
            .iter()
            .find(|a| a.id == id)
            .expect("account present")
            .clone()
    }

    /// Fixed-response HTTP server that keeps answering until the test drops
    /// its handle. Dependency-free on purpose (this crate carries no test
    /// HTTP-server dependency and does not need one).
    async fn spawn_repeating_server(
        response: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    /// Like [`spawn_repeating_server`], but answers the FIRST request with
    /// `first` and every later one with `rest`, and hands back a counter of
    /// how many requests actually reached the wire.
    ///
    /// The counter is the only honest way to assert a probe was *skipped*:
    /// account state alone cannot tell "we asked and nothing changed" apart
    /// from "we never asked". The sequencing lets one account walk from a
    /// conclusive rejection into a recovery without rebuilding the rotator
    /// (`with_probe_base_url` is a constructor-time builder).
    async fn spawn_sequenced_server(
        first: &'static str,
        rest: &'static str,
    ) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&hits);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                // Counted after the request head is in hand, and before the
                // reply — so a probe that has returned to its caller is
                // always already counted (no flaky ordering).
                let nth = counter.fetch_add(1, Ordering::SeqCst);
                let body = if nth == 0 { first } else { rest };
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), hits, handle)
    }

    /// Every request gets the same answer; the counter still records them.
    async fn spawn_counting_server(
        response: &'static str,
    ) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
        spawn_sequenced_server(response, response).await
    }

    const RESP_200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    const RESP_401: &str =
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const RESP_403: &str =
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const RESP_500: &str =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    // ── (a) auth-dead backoff ladder ────────────────────────────────

    /// 15 min → 30 → 60 → 120 → 240 → capped at 360 (6 h) and never beyond.
    #[test]
    fn auth_dead_backoff_doubles_then_caps_at_six_hours() {
        let expect = [
            (1u32, 15i64),
            (2, 30),
            (3, 60),
            (4, 120),
            (5, 240),
            (6, 360),
            (7, 360),
            (50, 360),
            (u32::MAX, 360),
        ];
        for (strikes, minutes) in expect {
            assert_eq!(
                auth_dead_backoff(strikes).num_minutes(),
                minutes,
                "strike {strikes} should book {minutes} minutes"
            );
        }
        // Defensive: a zero strike count is treated as the first one, never as
        // "no cooldown at all".
        assert_eq!(auth_dead_backoff(0).num_minutes(), 15);
    }

    /// `on_auth_failed` walks that ladder on the live account, marks it
    /// auth-dead, and takes it out of rotation immediately (no three-strike
    /// grace period — a dead token does not become alive by being retried).
    #[tokio::test]
    async fn on_auth_failed_marks_dead_and_escalates_the_cooldown() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(token_account("acct")).await;

        assert!(rotator.select().await.is_some(), "healthy account selects");

        for (nth, expected_minutes) in [(1u32, 15i64), (2, 30), (3, 60), (4, 120)] {
            let before = Utc::now();
            rotator
                .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
                .await;
            let acc = snapshot(&rotator, "acct").await;
            assert!(!acc.is_healthy, "strike {nth}: account must go unhealthy");
            assert_eq!(acc.auth_dead_strikes, nth);
            assert_eq!(
                acc.credential_state,
                CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
            );
            let booked = (acc.cooldown_until.expect("cooldown") - before).num_minutes();
            assert!(
                (expected_minutes - 1..=expected_minutes).contains(&booked),
                "strike {nth}: expected ~{expected_minutes} min, got {booked}"
            );
            assert!(
                rotator.select().await.is_none(),
                "strike {nth}: an auth-dead account must not be selectable"
            );
        }
    }

    /// `on_success` is the reset: strikes back to zero, state back to `Ok`.
    /// Without it the ladder would ratchet forever across unrelated incidents.
    #[tokio::test]
    async fn on_success_resets_the_auth_dead_ladder() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(token_account("acct")).await;

        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        assert_eq!(snapshot(&rotator, "acct").await.auth_dead_strikes, 2);

        // A success can only follow the cooldown elapsing (that is what puts
        // the account back in rotation), so simulate that first. `on_success`
        // deliberately does not clear `cooldown_until` itself — the
        // never-shorten rule predates this work and guards against a stale
        // success overriding a concurrent rate-limit.
        {
            let mut accounts = rotator.accounts.write().await;
            let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
            a.cooldown_until = Some(Utc::now() - chrono::Duration::seconds(1));
        }
        rotator.on_success("acct", 0).await;
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.auth_dead_strikes, 0);
        assert_eq!(acc.credential_state, CredentialState::Ok);

        // The ladder restarts from the bottom, not from where it left off.
        let before = Utc::now();
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.auth_dead_strikes, 1);
        let booked = (acc.cooldown_until.expect("cooldown") - before).num_minutes();
        assert!(
            (14..=15).contains(&booked),
            "expected ~15 min, got {booked}"
        );
    }

    /// A `Broken` credential is never "successful", so a stray `on_success`
    /// for its id must not launder it back into rotation.
    #[tokio::test]
    async fn on_success_never_un_breaks_a_broken_credential() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut acc = token_account("acct");
        acc.credential_state = CredentialState::Broken;
        acc.is_healthy = false;
        rotator.push_account_for_test(acc).await;

        rotator.on_success("acct", 0).await;
        assert_eq!(
            snapshot(&rotator, "acct").await.credential_state,
            CredentialState::Broken
        );
        assert!(rotator.select().await.is_none());
    }

    // ── (b) health probe drives real credential state ───────────────

    /// A 200 from `/v1/models` is the ONLY thing that restores an auth-dead
    /// account — and it does so completely (healthy, no cooldown, state `Ok`,
    /// ladder reset).
    #[tokio::test]
    async fn probe_200_restores_an_auth_dead_token_account() {
        let (base, server) = spawn_repeating_server(RESP_200).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        assert!(rotator.select().await.is_none());

        assert_eq!(rotator.probe_and_restore().await, 1);

        let acc = snapshot(&rotator, "acct").await;
        assert!(acc.is_healthy);
        assert_eq!(acc.cooldown_until, None);
        assert_eq!(acc.credential_state, CredentialState::Ok);
        assert_eq!(acc.auth_dead_strikes, 0);
        assert!(
            rotator.select().await.is_some(),
            "a verified account is selectable again"
        );

        server.abort();
    }

    /// The incident itself: a 403 must keep the account dead and push the
    /// cooldown OUT, not resurrect it. Same for a 401.
    #[tokio::test]
    async fn probe_401_and_403_keep_the_account_dead_and_double_the_cooldown() {
        for (response, expected_kind) in [
            (RESP_401, AuthFailureKind::InvalidToken),
            (RESP_403, AuthFailureKind::OrgDisabled),
        ] {
            let (base, server) = spawn_repeating_server(response).await;
            let rotator =
                AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
            rotator.push_account_for_test(token_account("acct")).await;
            rotator
                .on_auth_failed("acct", AuthFailureKind::InvalidToken)
                .await;
            let before = snapshot(&rotator, "acct")
                .await
                .cooldown_until
                .expect("cooldown");

            assert_eq!(
                rotator.probe_and_restore().await,
                0,
                "a rejected credential must never count as restored"
            );

            let acc = snapshot(&rotator, "acct").await;
            assert!(!acc.is_healthy);
            assert_eq!(
                acc.credential_state,
                CredentialState::AuthDead(expected_kind)
            );
            let after = acc.cooldown_until.expect("cooldown still booked");
            let grew = (after - before).num_minutes();
            assert!(
                grew >= 13,
                "cooldown should roughly double (15 → ~30 min); grew only {grew} min"
            );
            assert!(
                (after - Utc::now()).num_minutes() <= AUTH_DEAD_CAP_MINUTES,
                "cooldown must stay under the 6 h cap"
            );
            assert!(rotator.select().await.is_none());

            server.abort();
        }
    }

    /// Repeated 403s escalate but never blow past the 6 h ceiling.
    #[tokio::test]
    async fn repeated_probe_failures_saturate_at_the_six_hour_cap() {
        let (base, hits, server) = spawn_counting_server(RESP_403).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        let mut acc = token_account("acct");
        // Start already dead with a short *live* cooldown. It must stay in the
        // future: an expired cooldown makes `is_available()` true, and the
        // candidate filter skips available accounts — which is how this test
        // used to run zero probes and assert the cap vacuously. `doubled_cooldown`
        // doubles whatever remains, so the ladder still climbs to the cap
        // without a second of wall clock.
        acc.is_healthy = false;
        acc.credential_state = CredentialState::AuthDead(AuthFailureKind::OrgDisabled);
        acc.cooldown_until = Some(Utc::now() + chrono::Duration::minutes(1));
        rotator.push_account_for_test(acc).await;

        for _ in 0..12 {
            {
                // Clear only the probe schedule, so this stays a test about
                // *repeated* probe failures rather than silently becoming a
                // single-probe test under the new backoff.
                let mut accounts = rotator.accounts.write().await;
                let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
                a.next_probe_at = None;
            }
            rotator.probe_and_restore().await;
            let a = snapshot(&rotator, "acct").await;
            let minutes = (a.cooldown_until.expect("cooldown") - Utc::now()).num_minutes();
            assert!(
                minutes <= AUTH_DEAD_CAP_MINUTES,
                "cooldown {minutes} min exceeded the 6 h cap"
            );
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            12,
            "every round must actually have reached the probe — otherwise the \
             cap assertion above passes vacuously"
        );
        server.abort();
    }

    // ── (b2) probe schedule backoff ─────────────────────────────────
    //
    // A conclusively-dead credential used to be re-asked every 60 s forever.
    // Free in dollars, but pointless traffic and one alarming log line a
    // minute. These pin the widening schedule that replaced it.

    /// 1 min → 2 → 4 → 8 → 16 → capped at 30 and never beyond.
    #[test]
    fn probe_backoff_doubles_then_caps_at_thirty_minutes() {
        let expect = [
            (1u32, 1i64),
            (2, 2),
            (3, 4),
            (4, 8),
            (5, 16),
            (6, 30),
            (7, 30),
            (50, 30),
            (u32::MAX, 30),
        ];
        for (failures, minutes) in expect {
            assert_eq!(
                probe_backoff(failures).num_minutes(),
                minutes,
                "failure {failures} should book {minutes} minutes"
            );
        }
        // Defensive: a zero count is treated as the first failure, never as
        // "probe again immediately".
        assert_eq!(probe_backoff(0).num_minutes(), 1);
        // The probe ceiling is deliberately far below the rotation ceiling —
        // a probe is free, so only the noise is being rationed.
        assert!(PROBE_BACKOFF_CAP_MINUTES < AUTH_DEAD_CAP_MINUTES);
    }

    /// (a) One 401 books a ~1-minute schedule, and the very next tick must not
    /// reach the wire at all. The request counter is the assertion that
    /// matters: account state alone cannot tell "asked and nothing changed"
    /// apart from "never asked".
    #[tokio::test]
    async fn a_conclusive_probe_failure_suppresses_the_next_tick() {
        let (base, hits, server) = spawn_counting_server(RESP_401).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;

        let before = Utc::now();
        assert_eq!(rotator.probe_and_restore().await, 0);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "the first tick probes");

        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.probe_failures, 1);
        let booked = (acc.next_probe_at.expect("probe schedule booked") - before).num_seconds();
        assert!(
            (60..=65).contains(&booked),
            "expected ~1 min until the next probe, got {booked}s"
        );

        // Second tick, immediately — this is the once-a-minute loop the
        // schedule exists to break.
        assert_eq!(rotator.probe_and_restore().await, 0);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a scheduled account must not be re-probed before its time"
        );
        assert_eq!(
            snapshot(&rotator, "acct").await.probe_failures,
            1,
            "a skipped tick must not count as a failure"
        );

        server.abort();
    }

    /// (b) One integration step on the ladder: consecutive conclusive
    /// failures book 1, then 2, then 4 minutes on the live account.
    #[tokio::test]
    async fn consecutive_conclusive_failures_walk_the_probe_ladder() {
        let (base, hits, server) = spawn_counting_server(RESP_403).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;

        for (nth, expected_minutes) in [(1u32, 1i64), (2, 2), (3, 4)] {
            {
                // Let the previous booking elapse without burning wall clock.
                let mut accounts = rotator.accounts.write().await;
                let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
                a.next_probe_at = None;
            }
            let before = Utc::now();
            assert_eq!(rotator.probe_and_restore().await, 0);
            let acc = snapshot(&rotator, "acct").await;
            assert_eq!(acc.probe_failures, nth);
            let booked = (acc.next_probe_at.expect("booked") - before).num_seconds();
            let want = expected_minutes * 60;
            assert!(
                (want..=want + 5).contains(&booked),
                "failure {nth}: expected ~{expected_minutes} min, got {booked}s"
            );
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        server.abort();
    }

    /// (c) A 200 (the operator re-issued the token) clears the schedule
    /// completely, so recovery is never held back by a stale backoff.
    #[tokio::test]
    async fn a_valid_probe_clears_the_probe_schedule() {
        // 401 first (books the backoff), 200 afterwards.
        let (base, hits, server) = spawn_sequenced_server(RESP_401, RESP_200).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;

        assert_eq!(rotator.probe_and_restore().await, 0);
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.probe_failures, 1);
        assert!(acc.next_probe_at.is_some());

        {
            let mut accounts = rotator.accounts.write().await;
            let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
            a.next_probe_at = Some(Utc::now() - chrono::Duration::seconds(1));
        }
        assert_eq!(rotator.probe_and_restore().await, 1);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.probe_failures, 0);
        assert_eq!(acc.next_probe_at, None);
        assert_eq!(acc.credential_state, CredentialState::Ok);
        assert!(rotator.select().await.is_some());

        server.abort();
    }

    /// …and so does a completed request, which is even stronger evidence than
    /// a probe.
    #[tokio::test]
    async fn on_success_clears_the_probe_schedule() {
        let (base, _hits, server) = spawn_counting_server(RESP_401).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        rotator.probe_and_restore().await;
        assert!(snapshot(&rotator, "acct").await.next_probe_at.is_some());

        rotator.on_success("acct", 0).await;
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.probe_failures, 0);
        assert_eq!(acc.next_probe_at, None);

        server.abort();
    }

    /// (d) A real spawn failure clears the schedule so the next tick can
    /// classify *that* failure — and must not itself bump the probe counter
    /// (it is a spawn outcome, not a probe verdict).
    #[tokio::test]
    async fn on_auth_failed_reopens_the_probe_schedule() {
        let (base, hits, server) = spawn_counting_server(RESP_403).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;

        assert_eq!(rotator.probe_and_restore().await, 0);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(
            snapshot(&rotator, "acct")
                .await
                .next_probe_at
                .is_some_and(|t| t > Utc::now()),
            "a conclusive failure books a future probe"
        );

        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(
            acc.next_probe_at, None,
            "a fresh spawn failure must let the next tick re-classify"
        );
        assert_eq!(
            acc.probe_failures, 1,
            "on_auth_failed counts spawn failures, not probe verdicts"
        );

        assert_eq!(rotator.probe_and_restore().await, 0);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "the next tick must actually probe again"
        );
        assert_eq!(snapshot(&rotator, "acct").await.probe_failures, 2);

        server.abort();
    }

    /// (e) An inconclusive answer (500) must not slow the probe down: an API
    /// outage saying nothing about the credential is the mirror image of the
    /// original bug, and delaying recovery on it would be a real cost.
    #[tokio::test]
    async fn an_inconclusive_probe_schedules_no_backoff() {
        let (base, hits, server) = spawn_counting_server(RESP_500).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;

        assert_eq!(rotator.probe_and_restore().await, 0);
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.next_probe_at, None);
        assert_eq!(acc.probe_failures, 0);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // …so the very next tick probes again, exactly as before this change.
        assert_eq!(rotator.probe_and_restore().await, 0);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        server.abort();
    }

    /// `status()` (and therefore `accounts.list`) carries the schedule, so an
    /// operator can see a dead token is being re-checked on a backoff rather
    /// than silently forgotten.
    #[tokio::test]
    async fn status_surfaces_the_probe_schedule() {
        let (base, _hits, server) = spawn_counting_server(RESP_401).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;

        // Never probed → nothing scheduled.
        let row = rotator.status().await.remove(0);
        assert_eq!(row.next_probe_at, None);
        assert_eq!(row.probe_failures, 0);

        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        rotator.probe_and_restore().await;

        let row = rotator.status().await.remove(0);
        assert_eq!(row.probe_failures, 1);
        let stamp = row.next_probe_at.expect("schedule surfaced");
        let parsed = stamp
            .parse::<DateTime<Utc>>()
            .expect("next_probe_at must be RFC 3339");
        assert!(parsed > Utc::now());

        server.abort();
    }

    /// A 500 (or any inconclusive answer) says nothing about the credential:
    /// account state must be byte-identical afterwards. Reading a server
    /// outage as a dead token would be the mirror image of the original bug.
    #[tokio::test]
    async fn probe_500_leaves_the_account_untouched() {
        let (base, server) = spawn_repeating_server(RESP_500).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;
        let before = snapshot(&rotator, "acct").await;

        assert_eq!(rotator.probe_and_restore().await, 0);

        let after = snapshot(&rotator, "acct").await;
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert_eq!(after.credential_state, before.credential_state);
        assert_eq!(after.auth_dead_strikes, before.auth_dead_strikes);
        assert_eq!(after.is_healthy, before.is_healthy);

        server.abort();
    }

    /// An unreachable API (transport error → `Unknown`) is inconclusive too.
    #[tokio::test]
    async fn probe_transport_failure_leaves_the_account_untouched() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120)
            .with_probe_base_url(format!("http://{addr}"));
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        let before = snapshot(&rotator, "acct").await;

        assert_eq!(rotator.probe_and_restore().await, 0);

        let after = snapshot(&rotator, "acct").await;
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert_eq!(after.credential_state, before.credential_state);
    }

    /// An API-key account is probed with its own key (`x-api-key`), same
    /// three-way verdict.
    #[tokio::test]
    async fn api_key_account_is_probed_with_its_own_key() {
        let (base, server) = spawn_repeating_server(RESP_200).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        let mut acc = token_account("api-acct");
        acc.auth_method = AuthMethod::ApiKey;
        acc.oauth_token = None;
        acc.api_key = "sk-ant-api03-test".to_string();
        acc.monthly_budget_cents = 5000;
        rotator.push_account_for_test(acc).await;
        rotator
            .on_auth_failed("api-acct", AuthFailureKind::InvalidToken)
            .await;

        assert_eq!(rotator.probe_and_restore().await, 1);
        assert_eq!(
            snapshot(&rotator, "api-acct").await.credential_state,
            CredentialState::Ok
        );

        server.abort();
    }

    /// A foreign-provider seat's credential means nothing to
    /// `api.anthropic.com` — probing it there would produce a confident, wrong
    /// verdict, so it must not be probe-able at all.
    #[test]
    fn non_anthropic_seats_are_never_probed_against_anthropic() {
        let mut seat = token_account("copilot-seat");
        seat.provider = "github".to_string();
        assert!(probe_secret_for(&seat).is_none());

        let mut key = token_account("openai-key");
        key.provider = "openai".to_string();
        key.auth_method = AuthMethod::ApiKey;
        key.api_key = "sk-openai".to_string();
        assert!(probe_secret_for(&key).is_none());

        // …while the Anthropic equivalents are.
        assert!(matches!(
            probe_secret_for(&token_account("anth")),
            Some((CredentialKind::OAuthToken, _))
        ));
        let mut anth_key = token_account("anth-key");
        anth_key.auth_method = AuthMethod::ApiKey;
        anth_key.oauth_token = None;
        anth_key.api_key = "sk-ant-api03".to_string();
        assert!(matches!(
            probe_secret_for(&anth_key),
            Some((CredentialKind::ApiKey, _))
        ));
        // An empty credential is not probe-able (and never was usable).
        let mut empty = token_account("empty");
        empty.oauth_token = Some("   ".to_string());
        assert!(probe_secret_for(&empty).is_none());
    }

    // ── (c) the weak `claude auth status` signal ────────────────────

    /// The heart of the incident: `loggedIn: true` must never resurrect an
    /// account we have watched fail authentication.
    #[test]
    fn claude_auth_status_may_not_restore_auth_dead_or_broken() {
        assert!(legacy_status_probe_may_restore(CredentialState::Unverified));
        assert!(legacy_status_probe_may_restore(CredentialState::Ok));
        assert!(!legacy_status_probe_may_restore(CredentialState::Broken));
        assert!(!legacy_status_probe_may_restore(CredentialState::AuthDead(
            AuthFailureKind::InvalidToken
        )));
        assert!(!legacy_status_probe_may_restore(CredentialState::AuthDead(
            AuthFailureKind::OrgDisabled
        )));
    }

    /// End-to-end version: a keychain account (no probe-able secret) that has
    /// been marked auth-dead stays dead across a probe tick, even on a machine
    /// where `claude auth status` happily reports `loggedIn: true`.
    #[tokio::test]
    async fn auth_dead_keychain_account_is_not_restored_by_the_status_probe() {
        // Unroutable probe base: if this account were ever routed to the
        // credential probe (it must not be — it has no secret), the test would
        // notice via a changed state rather than a silent pass.
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120)
            .with_probe_base_url("http://127.0.0.1:1");
        rotator
            .push_account_for_test(keychain_account("keychain"))
            .await;
        rotator
            .on_auth_failed("keychain", AuthFailureKind::OrgDisabled)
            .await;
        let before = snapshot(&rotator, "keychain").await;

        assert_eq!(
            rotator.probe_and_restore().await,
            0,
            "`claude auth status` must not resurrect an auth-dead account"
        );

        let after = snapshot(&rotator, "keychain").await;
        assert!(!after.is_healthy);
        assert_eq!(
            after.credential_state,
            CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
        );
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert!(rotator.select().await.is_none());
    }

    // ── (d) load-time broken-credential detection ───────────────────

    static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_home(label: &str) -> tempfile::TempDir {
        let _ = HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
        tempfile::Builder::new()
            .prefix(&format!("duduclaw-cred-{label}-"))
            .tempdir()
            .expect("tempdir")
    }

    /// An `oauth_token_enc` that cannot be decrypted (wrong / regenerated
    /// `.keyfile`) must load as BROKEN and never be selected — instead of
    /// quietly spawning children with no credential at all. An OAuth entry
    /// with no `_enc` field at all is untouched (it relies on the OS keychain).
    #[tokio::test]
    async fn undecryptable_oauth_token_loads_as_broken_and_is_never_selected() {
        let home = temp_home("broken-oauth");
        // A real keyfile exists — the ciphertext is simply not ours.
        std::fs::write(
            home.path().join(".keyfile"),
            duduclaw_security::crypto::CryptoEngine::generate_key().unwrap(),
        )
        .unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            r#"
[[accounts]]
id = "broken-oauth"
type = "oauth"
label = "壞掉的帳號"
oauth_token_enc = "this-is-not-valid-ciphertext"

[[accounts]]
id = "keychain-oauth"
type = "oauth"
profile = "default"
label = "鑰匙圈帳號"
"#,
        )
        .unwrap();

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.load_from_config(home.path()).await.unwrap();

        let broken = snapshot(&rotator, "broken-oauth").await;
        assert_eq!(broken.credential_state, CredentialState::Broken);
        assert!(!broken.is_healthy, "a broken credential is not healthy");
        assert!(
            !broken.is_available(),
            "a broken credential is never available"
        );
        assert!(
            broken.oauth_token.is_none(),
            "an unusable token must not reach the spawn env"
        );
        assert!(broken.credential_state.credential_detail().is_some());

        // The no-`_enc` sibling keeps its previous behavior exactly.
        let keychain = snapshot(&rotator, "keychain-oauth").await;
        assert_eq!(
            keychain.credential_state,
            CredentialState::Unverified,
            "an OAuth entry with no `_enc` field must NOT be judged broken"
        );

        // Whatever else is selectable, it is never the broken account.
        for _ in 0..5 {
            if let Some(sel) = rotator.select().await {
                assert_ne!(sel.id, "broken-oauth");
            }
        }
    }

    /// Same rule on the API-key side: an `api_key_enc` that resolves to
    /// nothing loads as BROKEN (previously the row was silently skipped, so a
    /// wrong `.keyfile` just made the pool quietly smaller).
    #[tokio::test]
    async fn undecryptable_api_key_loads_as_broken() {
        let home = temp_home("broken-apikey");
        std::fs::write(
            home.path().join(".keyfile"),
            duduclaw_security::crypto::CryptoEngine::generate_key().unwrap(),
        )
        .unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            r#"
[[accounts]]
id = "broken-key"
type = "api_key"
provider = "anthropic"
api_key_enc = "not-real-ciphertext"
"#,
        )
        .unwrap();

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.load_from_config(home.path()).await.unwrap();

        let acc = snapshot(&rotator, "broken-key").await;
        assert_eq!(acc.credential_state, CredentialState::Broken);
        assert!(!acc.is_healthy);
        assert!(!acc.is_available());
    }

    /// The precondition helper: only a *declared, non-empty* encrypted field
    /// makes an entry a broken-credential candidate.
    #[test]
    fn has_nonempty_field_requires_a_real_declaration() {
        let with_enc: toml::Table = "oauth_token_enc = \"abc\"\n".parse().unwrap();
        assert!(has_nonempty_field(&with_enc, OAUTH_TOKEN_ENC_FIELDS));

        let blank: toml::Table = "oauth_token_enc = \"   \"\n".parse().unwrap();
        assert!(!has_nonempty_field(&blank, OAUTH_TOKEN_ENC_FIELDS));

        let absent: toml::Table = "profile = \"default\"\n".parse().unwrap();
        assert!(!has_nonempty_field(&absent, OAUTH_TOKEN_ENC_FIELDS));

        // Either api-key field name counts.
        let alt: toml::Table = "anthropic_api_key_enc = \"abc\"\n".parse().unwrap();
        assert!(has_nonempty_field(&alt, API_KEY_ENC_FIELDS));
        assert!(!has_nonempty_field(&alt, OAUTH_TOKEN_ENC_FIELDS));
    }

    // ── (e) Broken is a hard exclusion under every strategy ─────────

    #[tokio::test]
    async fn broken_accounts_are_excluded_under_priority_and_round_robin() {
        for strategy in [RotationStrategy::Priority, RotationStrategy::RoundRobin] {
            let rotator = AccountRotator::new(strategy, 120);
            let mut broken = token_account("broken");
            broken.priority = 1; // would win on Priority
            broken.credential_state = CredentialState::Broken;
            // Deliberately left `is_healthy = true`: the exclusion must come
            // from the credential state alone, not from a health side effect.
            rotator.push_account_for_test(broken).await;
            let mut good = token_account("good");
            good.priority = 9;
            rotator.push_account_for_test(good).await;

            for _ in 0..6 {
                let sel = rotator
                    .select()
                    .await
                    .expect("the healthy account must answer");
                assert_eq!(sel.id, "good", "a Broken account leaked into rotation");
            }
        }
    }

    /// An agent whose `account_pool` names ONLY the broken account must still
    /// not get it — the pool's fail-open rule widens the candidate set, it
    /// never re-admits a hard-excluded account.
    #[tokio::test]
    async fn broken_account_is_not_reachable_through_the_account_pool() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut broken = token_account("broken");
        broken.credential_state = CredentialState::Broken;
        rotator.push_account_for_test(broken).await;
        rotator.push_account_for_test(token_account("good")).await;

        let sel = rotator
            .select_with_pool(&["broken".to_string()])
            .await
            .expect("fail-open must still hand back a usable account");
        assert_eq!(sel.id, "good");
    }

    /// With nothing but a broken account configured, selection returns None
    /// rather than falling through to the ambient `ANTHROPIC_API_KEY`.
    #[tokio::test]
    async fn only_broken_accounts_means_no_selection() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut broken = token_account("broken");
        broken.credential_state = CredentialState::Broken;
        rotator.push_account_for_test(broken).await;
        assert!(rotator.select().await.is_none());
    }

    // ── (D5) the shapes the dashboard / gateway wave codes against ──

    #[test]
    fn credential_state_display_and_serialization_are_stable() {
        assert_eq!(CredentialState::Ok.to_string(), "ok");
        assert_eq!(CredentialState::Unverified.to_string(), "unverified");
        assert_eq!(CredentialState::Broken.to_string(), "broken");
        assert_eq!(
            CredentialState::AuthDead(AuthFailureKind::InvalidToken).to_string(),
            "auth_dead:invalid_token"
        );
        assert_eq!(
            CredentialState::AuthDead(AuthFailureKind::OrgDisabled).to_string(),
            "auth_dead:org_disabled"
        );

        let json = |s: CredentialState| serde_json::to_string(&s).unwrap();
        assert_eq!(json(CredentialState::Ok), "\"ok\"");
        assert_eq!(json(CredentialState::Unverified), "\"unverified\"");
        assert_eq!(json(CredentialState::Broken), "\"broken\"");
        assert_eq!(
            json(CredentialState::AuthDead(AuthFailureKind::OrgDisabled)),
            "\"auth_dead\""
        );
        assert_eq!(
            serde_json::to_string(&AuthFailureKind::OrgDisabled).unwrap(),
            "\"org_disabled\""
        );
        assert_eq!(
            serde_json::to_string(&AuthFailureKind::InvalidToken).unwrap(),
            "\"invalid_token\""
        );

        // `Unverified` is the honest default — a credential we have not seen
        // work is neither Ok nor dead.
        assert_eq!(CredentialState::default(), CredentialState::Unverified);
        assert!(CredentialState::Broken.is_blocking());
        assert!(!CredentialState::AuthDead(AuthFailureKind::OrgDisabled).is_blocking());
    }

    /// `accounts.list` (via `status()`) must carry the new fields, or the
    /// dashboard badge has nothing to render.
    #[tokio::test]
    async fn status_surfaces_credential_state_and_detail() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;

        let rows = rotator.status().await;
        let row = rows.iter().find(|r| r.id == "acct").expect("row");
        assert_eq!(
            row.credential_state,
            CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
        );
        assert_eq!(row.auth_dead_strikes, 1);
        let detail = row
            .credential_detail
            .expect("a bad state must explain itself");
        assert!(
            detail.contains("403"),
            "detail should name the failure: {detail}"
        );
        assert!(!row.is_available);

        // Serialized shape (what the gateway forwards to the dashboard).
        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["credential_state"], serde_json::json!("auth_dead"));
        assert_eq!(json["auth_dead_strikes"], serde_json::json!(1));
    }

    /// `doubled_cooldown` never returns something shorter than the base and
    /// never exceeds the cap — including from an absent / already-expired
    /// cooldown, where "double nothing" would mean "retry immediately".
    #[test]
    fn doubled_cooldown_restarts_at_base_and_respects_the_cap() {
        let now = Utc::now();

        let from_none = (doubled_cooldown(None) - now).num_minutes();
        assert!((14..=15).contains(&from_none), "got {from_none}");

        let expired =
            (doubled_cooldown(Some(now - chrono::Duration::hours(3))) - now).num_minutes();
        assert!((14..=15).contains(&expired), "got {expired}");

        let doubled =
            (doubled_cooldown(Some(now + chrono::Duration::minutes(30))) - now).num_minutes();
        assert!((59..=60).contains(&doubled), "got {doubled}");

        let capped = (doubled_cooldown(Some(now + chrono::Duration::hours(5))) - now).num_minutes();
        assert_eq!(capped, AUTH_DEAD_CAP_MINUTES);
    }
}
