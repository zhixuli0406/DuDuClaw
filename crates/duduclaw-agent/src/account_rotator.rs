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
    // Runtime state (not persisted in config)
    #[serde(skip)]
    pub api_key: String,
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

impl Account {
    pub fn is_available(&self) -> bool {
        if !self.is_healthy {
            return false;
        }
        // API key accounts have budget enforcement
        if self.auth_method == AuthMethod::ApiKey
            && self.spent_this_month >= self.monthly_budget_cents
        {
            return false;
        }
        // Check cooldown
        if self.cooldown_until.is_some_and(|cd| Utc::now() < cd) {
            return false;
        }
        match self.auth_method {
            AuthMethod::ApiKey => !self.api_key.is_empty(),
            AuthMethod::OAuth => self.credentials_dir.is_some(),
        }
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
                            api_key,
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
                        let creds_dir = resolve_oauth_credentials(profile);

                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::OAuth,
                            priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(5) as u32,
                            monthly_budget_cents: 0, // OAuth = subscription, no per-token budget
                            tags: Vec::new(),
                            profile: profile.to_string(),
                            email: email.to_string(),
                            subscription: sub.to_string(),
                            api_key: String::new(),
                            credentials_dir: creds_dir.clone(),
                            is_healthy: creds_dir.is_some(),
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

        // 2. Auto-detect default OAuth session from ~/.claude/.credentials.json
        if !loaded.iter().any(|a| a.auth_method == AuthMethod::OAuth) {
            if let Some(creds) = detect_default_oauth_session() {
                loaded.push(creds);
            }
        }

        // 3. Fallback: single API key from [api] or env var
        if loaded.is_empty() {
            if let Some(api) = table.get("api").and_then(|v| v.as_table()) {
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
                        api_key,
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
        }

        if loaded.is_empty() {
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                if !key.is_empty() {
                    loaded.push(Account {
                        id: "env".to_string(),
                        auth_method: AuthMethod::ApiKey,
                        priority: 99,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        profile: String::new(),
                        email: String::new(),
                        subscription: String::new(),
                        api_key: key,
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
        }

        let oauth_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::OAuth).count();
        let apikey_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::ApiKey).count();
        let count = loaded.len();

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
                    if let Some(dir) = &a.credentials_dir {
                        env_vars.insert("CLAUDE_CONFIG_DIR".to_string(), dir.to_string_lossy().to_string());
                    }
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
            acc.is_healthy = true;
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
            email: a.email.clone(),
            subscription: a.subscription.clone(),
        }).collect()
    }

    pub async fn count(&self) -> usize {
        self.accounts.read().await.len()
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

    let output = std::process::Command::new(&claude)
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
        api_key: String::new(),
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
fn resolve_oauth_credentials(profile: &str) -> Option<PathBuf> {
    let claude_dir = dirs::home_dir()?.join(".claude");

    if profile == "default" || profile.is_empty() {
        // Default profile: ~/.claude/
        let creds = claude_dir.join(".credentials.json");
        if creds.exists() { Some(claude_dir) } else { None }
    } else {
        // Named profile: ~/.claude/profiles/<name>/
        let profile_dir = claude_dir.join("profiles").join(profile);
        let creds = profile_dir.join(".credentials.json");
        if creds.exists() { Some(profile_dir) } else { None }
    }
}

// ── API Key helpers ─────────────────────────────────────────

/// Resolve API key from a TOML table (encrypted first, then plaintext).
async fn resolve_api_key(home_dir: &Path, table: &toml::Table) -> String {
    for key_name in &["anthropic_api_key_enc", "api_key_enc"] {
        if let Some(enc) = table.get(*key_name).and_then(|v| v.as_str()) {
            if !enc.is_empty() {
                if let Some(decrypted) = decrypt_with_keyfile(home_dir, enc) {
                    return decrypted;
                }
            }
        }
    }
    for key_name in &["anthropic_api_key", "api_key"] {
        if let Some(key) = table.get(*key_name).and_then(|v| v.as_str()) {
            if !key.is_empty() {
                warn!("Using plaintext API key — run `duduclaw onboard` to encrypt");
                return key.to_string();
            }
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
