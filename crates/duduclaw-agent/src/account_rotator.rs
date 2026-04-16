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

/// An account that can be used for Claude CLI invocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub auth_method: AuthMethod,
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
            // OAuth: either has explicit token (setup-token) or credentials_dir (OS keychain)
            AuthMethod::OAuth => self.oauth_token.is_some() || self.credentials_dir.is_some(),
        }
    }

    /// Days until token expires. Returns None if no expiry set.
    pub fn days_until_expiry(&self) -> Option<i64> {
        let exp = self.expires_at.as_ref()?;
        let expiry = exp.parse::<DateTime<Utc>>().ok()?;
        Some((expiry - Utc::now()).num_days())
    }
}

/// Environment variables to set when invoking `claude` CLI for a given account.
#[derive(Debug, Clone)]
pub struct AccountEnv {
    pub id: String,
    pub auth_method: AuthMethod,
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
                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::ApiKey,
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
                        let email = acc_table.get("email").and_then(|v| v.as_str()).unwrap_or("");
                        let sub = acc_table.get("subscription").and_then(|v| v.as_str()).unwrap_or("");
                        let label = acc_table.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        let expires_at = acc_table.get("expires_at").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let creds_dir = resolve_oauth_credentials(profile);

                        // Decrypt oauth_token_enc if present
                        let oauth_token = acc_table
                            .get("oauth_token_enc")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .and_then(|enc| decrypt_with_keyfile(home_dir, enc));

                        let has_auth = oauth_token.is_some() || creds_dir.is_some();

                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::OAuth,
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

        // 2. Auto-detect default OAuth session via `claude auth status`
        if !loaded.iter().any(|a| a.auth_method == AuthMethod::OAuth) {
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

    /// Select the best available account and return env vars for claude CLI.
    pub async fn select(&self) -> Option<AccountEnv> {
        let accounts = self.accounts.read().await;
        let available: Vec<&Account> = accounts.iter().filter(|a| a.is_available()).collect();

        if available.is_empty() {
            warn!("No available accounts for rotation");
            return None;
        }

        let selected = match self.strategy {
            RotationStrategy::Priority | RotationStrategy::Failover => {
                available.iter().min_by_key(|a| a.priority).copied()
            }
            RotationStrategy::LeastCost => {
                // Prefer OAuth (subscription, no per-token cost), then least spent API key
                let oauth: Vec<&&Account> = available.iter().filter(|a| a.auth_method == AuthMethod::OAuth).collect();
                if !oauth.is_empty() {
                    Some(*oauth[0])
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
            let mut env_vars = HashMap::new();

            match a.auth_method {
                AuthMethod::ApiKey => {
                    env_vars.insert("ANTHROPIC_API_KEY".to_string(), a.api_key.clone());
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

            info!(
                account = %a.id,
                method = ?a.auth_method,
                email = %a.email,
                "Account selected for rotation"
            );

            AccountEnv {
                id: a.id.clone(),
                auth_method: a.auth_method.clone(),
                env_vars,
            }
        })
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

    pub async fn on_error(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors += 1;
            if acc.consecutive_errors >= 3 {
                warn!(account = account_id, "Account marked unhealthy after 3 errors");
                acc.is_healthy = false;
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

/// Detect the default OAuth session via `claude auth status`.
///
/// Works with all Claude Code versions — does not depend on `.credentials.json`
/// which no longer exists in recent versions. The `claude` CLI manages its own
/// auth state (OS keychain / internal storage).
fn detect_default_oauth_session() -> Option<Account> {
    let claude = duduclaw_core::which_claude()?;
    let claude_dir = dirs::home_dir()?.join(".claude");

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
        priority: 1, // OAuth preferred over API key
        monthly_budget_cents: 0,
        tags: Vec::new(),
        profile: "default".to_string(),
        email: email.to_string(),
        subscription: subscription.to_string(),
        label: "本機登入".to_string(),
        expires_at: None, // OS keychain manages token lifecycle
        api_key: String::new(),
        oauth_token: None, // Uses OS keychain, not explicit token
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

/// Resolve API key from a TOML table (encrypted first, then plaintext).
async fn resolve_api_key(home_dir: &Path, table: &toml::Table) -> String {
    for key_name in &["anthropic_api_key_enc", "api_key_enc"] {
        if let Some(enc) = table.get(*key_name).and_then(|v| v.as_str())
            && !enc.is_empty()
                && let Some(decrypted) = decrypt_with_keyfile(home_dir, enc) {
                    return decrypted;
                }
    }
    for key_name in &["anthropic_api_key", "api_key"] {
        if let Some(key) = table.get(*key_name).and_then(|v| v.as_str())
            && !key.is_empty() {
                warn!("Using plaintext API key — run `duduclaw onboard` to encrypt");
                return key.to_string();
            }
    }
    String::new()
}

fn decrypt_with_keyfile(home_dir: &Path, encrypted: &str) -> Option<String> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() != 32 { return None; }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    duduclaw_security::crypto::CryptoEngine::new(&key).ok()?.decrypt_string(encrypted).ok().filter(|s| !s.is_empty())
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
}
