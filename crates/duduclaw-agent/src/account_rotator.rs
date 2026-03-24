//! Unified account rotation for Claude Code SDK.
//!
//! Manages multiple API keys with rotation strategies, health tracking,
//! budget enforcement, and cooldown. Replaces the Python-only rotator.
//!
//! The selected key is passed to `claude` CLI via ANTHROPIC_API_KEY env var.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Types ───────────────────────────────────────────────────

/// An API account loaded from config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub account_type: String,
    pub priority: u32,
    pub monthly_budget_cents: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    // Runtime state (not persisted in config)
    #[serde(skip)]
    pub api_key: String,
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
        self.is_healthy
            && !self.api_key.is_empty()
            && self.spent_this_month < self.monthly_budget_cents
            && self.cooldown_until.map(|cd| Utc::now() >= cd).unwrap_or(true)
    }
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

/// Account selection result.
#[derive(Debug, Clone)]
pub struct SelectedAccount {
    pub id: String,
    pub api_key: String,
}

/// Public status for monitoring.
#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub id: String,
    pub account_type: String,
    pub priority: u32,
    pub is_healthy: bool,
    pub spent_this_month: u64,
    pub monthly_budget_cents: u64,
    pub total_requests: u64,
    pub is_available: bool,
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

    /// Load accounts from config.toml, decrypting API keys.
    pub async fn load_from_config(
        &self,
        home_dir: &Path,
    ) -> Result<usize, String> {
        let config_path = home_dir.join("config.toml");
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|e| format!("Read config: {e}"))?;

        let table: toml::Table = content.parse().map_err(|e| format!("Parse config: {e}"))?;

        let mut loaded = Vec::new();

        // Try [[accounts]] array format
        if let Some(accs) = table.get("accounts").and_then(|v| v.as_array()) {
            for acc in accs {
                if let Some(acc_table) = acc.as_table() {
                    let id = acc_table.get("id").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    let api_key = resolve_api_key(home_dir, acc_table).await;
                    if api_key.is_empty() { continue; }

                    loaded.push(Account {
                        id: id.to_string(),
                        account_type: acc_table.get("type").and_then(|v| v.as_str()).unwrap_or("api_key").to_string(),
                        priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(10) as u32,
                        monthly_budget_cents: acc_table.get("monthly_budget_cents").and_then(|v| v.as_integer()).unwrap_or(5000) as u64,
                        tags: Vec::new(),
                        api_key,
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

        // Fallback: single [api] key
        if loaded.is_empty() {
            if let Some(api) = table.get("api").and_then(|v| v.as_table()) {
                let api_key = resolve_api_key(home_dir, api).await;
                if !api_key.is_empty() {
                    loaded.push(Account {
                        id: "main".to_string(),
                        account_type: "api_key".to_string(),
                        priority: 1,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        api_key,
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

        // Last fallback: env var
        if loaded.is_empty() {
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                if !key.is_empty() {
                    loaded.push(Account {
                        id: "env".to_string(),
                        account_type: "api_key".to_string(),
                        priority: 1,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        api_key: key,
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

        let count = loaded.len();
        info!(count, strategy = ?self.strategy, "Accounts loaded");
        *self.accounts.write().await = loaded;
        Ok(count)
    }

    /// Select the best available account based on strategy.
    pub async fn select(&self) -> Option<SelectedAccount> {
        let accounts = self.accounts.read().await;
        let available: Vec<&Account> = accounts.iter().filter(|a| a.is_available()).collect();

        if available.is_empty() {
            warn!("No available accounts for rotation");
            return None;
        }

        let selected = match self.strategy {
            RotationStrategy::Priority => {
                available.iter().min_by_key(|a| a.priority).copied()
            }
            RotationStrategy::LeastCost => {
                available.iter().min_by_key(|a| a.spent_this_month).copied()
            }
            RotationStrategy::Failover => {
                available.iter().min_by_key(|a| a.priority).copied()
            }
            RotationStrategy::RoundRobin => {
                let mut idx = self.round_robin_index.write().await;
                let selected = available[*idx % available.len()];
                *idx = (*idx + 1) % available.len();
                Some(selected)
            }
        };

        selected.map(|a| {
            info!(account = %a.id, strategy = ?self.strategy, "Account selected");
            SelectedAccount {
                id: a.id.clone(),
                api_key: a.api_key.clone(),
            }
        })
    }

    /// Report success — reset error count, record usage.
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

    /// Report error — increment error count, mark unhealthy after 3 consecutive.
    pub async fn on_error(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors += 1;
            if acc.consecutive_errors >= 3 {
                warn!(account = account_id, "Account marked unhealthy after 3 consecutive errors");
                acc.is_healthy = false;
            }
        }
    }

    /// Report rate limit — put account on cooldown.
    pub async fn on_rate_limited(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.cooldown_until = Some(
                Utc::now() + chrono::Duration::seconds(self.cooldown_seconds as i64),
            );
            warn!(account = account_id, cooldown = self.cooldown_seconds, "Account rate-limited, entering cooldown");
        }
    }

    /// Monthly budget reset.
    pub async fn reset_monthly(&self) {
        let mut accounts = self.accounts.write().await;
        let now = Utc::now();
        for acc in accounts.iter_mut() {
            acc.spent_this_month = 0;
            info!(account = %acc.id, "Monthly budget reset");
        }
    }

    /// Get status of all accounts.
    pub async fn status(&self) -> Vec<AccountStatus> {
        let accounts = self.accounts.read().await;
        accounts.iter().map(|a| AccountStatus {
            id: a.id.clone(),
            account_type: a.account_type.clone(),
            priority: a.priority,
            is_healthy: a.is_healthy,
            spent_this_month: a.spent_this_month,
            monthly_budget_cents: a.monthly_budget_cents,
            total_requests: a.total_requests,
            is_available: a.is_available(),
        }).collect()
    }

    /// Get total account count.
    pub async fn count(&self) -> usize {
        self.accounts.read().await.len()
    }
}

// ── Helpers ─────────────────────────────────────────────────

/// Resolve API key from a TOML table, trying encrypted then plaintext.
async fn resolve_api_key(home_dir: &Path, table: &toml::Table) -> String {
    // Try encrypted key first
    for key_name in &["anthropic_api_key_enc", "api_key_enc"] {
        if let Some(enc) = table.get(*key_name).and_then(|v| v.as_str()) {
            if !enc.is_empty() {
                if let Some(decrypted) = decrypt_with_keyfile(home_dir, enc) {
                    return decrypted;
                }
            }
        }
    }

    // Plaintext fallback
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
    let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
    engine.decrypt_string(encrypted).ok().filter(|s| !s.is_empty())
}

/// Create a rotator from config.toml rotation settings.
pub fn create_from_config(config: &toml::Table) -> AccountRotator {
    let rotation = config.get("rotation").and_then(|v| v.as_table());
    let strategy_str = rotation.and_then(|r| r.get("strategy")).and_then(|v| v.as_str()).unwrap_or("priority");
    let cooldown = rotation.and_then(|r| r.get("cooldown_after_rate_limit_seconds")).and_then(|v| v.as_integer()).unwrap_or(120) as u64;

    AccountRotator::new(RotationStrategy::from_str(strategy_str), cooldown)
}
