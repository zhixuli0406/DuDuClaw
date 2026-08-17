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
use tracing::{info, warn};

// ── Types ───────────────────────────────────────────────────

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
#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub id: String,
    pub auth_method: String,
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
}

// ── AccountRotator ──────────────────────────────────────────

pub struct AccountRotator {
    accounts: Arc<RwLock<Vec<Account>>>,
    strategy: RotationStrategy,
    round_robin_index: Arc<RwLock<usize>>,
    cooldown_seconds: u64,
}

impl AccountRotator {
    pub fn new(strategy: RotationStrategy, cooldown_seconds: u64) -> Self {
        Self {
            accounts: Arc::new(RwLock::new(Vec::new())),
            strategy,
            round_robin_index: Arc::new(RwLock::new(0)),
            cooldown_seconds,
        }
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
                        if api_key.is_empty() { continue; }
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
                            is_healthy: true,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
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
                            is_healthy: has_auth,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
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
            acc.spent_this_month += cost_cents;
            acc.total_requests += 1;
            acc.last_used = Some(Utc::now());
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

    /// Probe all unhealthy accounts and restore those that respond successfully.
    ///
    /// For OAuth accounts: runs `claude auth status` to verify the session is valid.
    /// For API key accounts: does a lightweight `/v1/messages` health check.
    /// Restored accounts are sorted back by priority — highest priority first.
    ///
    /// Call this periodically (e.g. every 60s) from a background task.
    pub async fn probe_and_restore(&self) -> usize {
        let unhealthy_ids: Vec<(String, AuthMethod)> = {
            let accounts = self.accounts.read().await;
            accounts.iter()
                .filter(|a| !a.is_healthy || a.cooldown_until.is_some_and(|cd| Utc::now() >= cd))
                .filter(|a| !a.is_available()) // truly unavailable, not just cooled-down-and-ready
                .map(|a| (a.id.clone(), a.auth_method.clone()))
                .collect()
        };

        if unhealthy_ids.is_empty() {
            return 0;
        }

        let mut restored = 0u64;

        for (id, method) in &unhealthy_ids {
            let ok = match method {
                AuthMethod::OAuth => {
                    // Probe by running `claude auth status` — if it succeeds, OAuth is valid
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
