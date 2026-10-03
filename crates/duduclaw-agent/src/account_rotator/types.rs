//! Account / credential value types shared by the whole rotator: the
//! credential state machine, one [`Account`], its env projection and the
//! backoff maths. Moved verbatim out of `account_rotator.rs`.

use super::*;

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
pub(super) fn default_provider() -> String {
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
    /// `[[accounts]] tags` (v1.68.0) — shown and edited in the dashboard,
    /// matched by `agent.toml [model] account_pool`.
    pub tags: Vec<String>,
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
pub(super) const AUTH_DEAD_BASE_MINUTES: i64 = 15;

/// Ceiling for any auth-dead / probe-failure cooldown (6 hours).
pub(super) const AUTH_DEAD_CAP_MINUTES: i64 = 6 * 60;

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
pub(super) const PROBE_BACKOFF_CAP_MINUTES: i64 = 30;

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
