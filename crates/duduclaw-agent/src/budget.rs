use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Utc};
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::types::BudgetConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetTracker {
    pub agent_id: String,
    pub config: BudgetConfig,
    pub spent_cents: u64,
    pub last_reset: DateTime<Utc>,
    pub month: u32,
    pub year: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_cents: u64,
}

pub struct BudgetManager {
    trackers: Arc<RwLock<HashMap<String, BudgetTracker>>>,
}

impl BudgetManager {
    pub fn new() -> Self {
        Self {
            trackers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an agent with its budget config.
    pub async fn register(&self, agent_id: &str, config: BudgetConfig) {
        let now = Utc::now();
        let tracker = BudgetTracker {
            agent_id: agent_id.to_string(),
            config,
            spent_cents: 0,
            last_reset: now,
            month: now.month(),
            year: now.year(),
        };
        self.trackers
            .write()
            .await
            .insert(agent_id.to_string(), tracker);
        info!("Budget registered for agent: {}", agent_id);
    }

    /// Record usage and check budget.
    pub async fn record_usage(&self, agent_id: &str, cost_cents: u64) -> Result<()> {
        let mut trackers = self.trackers.write().await;
        let tracker = trackers.get_mut(agent_id).ok_or_else(|| {
            DuDuClawError::Agent(format!("No budget tracker for: {}", agent_id))
        })?;

        // Auto-reset if month changed
        let now = Utc::now();
        if now.month() != tracker.month || now.year() != tracker.year {
            info!("Monthly budget reset for agent: {}", agent_id);
            tracker.spent_cents = 0;
            tracker.month = now.month();
            tracker.year = now.year();
            tracker.last_reset = now;
        }

        tracker.spent_cents += cost_cents;

        // Check warn threshold
        let threshold = (tracker.config.monthly_limit_cents as f64
            * tracker.config.warn_threshold_percent as f64
            / 100.0) as u64;

        if tracker.spent_cents >= tracker.config.monthly_limit_cents {
            if tracker.config.hard_stop {
                error!(
                    "Budget EXCEEDED for agent {} ({}/{})",
                    agent_id, tracker.spent_cents, tracker.config.monthly_limit_cents
                );
                return Err(DuDuClawError::Agent(format!(
                    "Budget exceeded for agent: {} (spent: {} cents, limit: {} cents)",
                    agent_id, tracker.spent_cents, tracker.config.monthly_limit_cents
                )));
            }
            warn!("Budget exceeded (soft) for agent: {}", agent_id);
        } else if tracker.spent_cents >= threshold {
            warn!(
                "Budget warning for agent {} ({}/{})",
                agent_id, tracker.spent_cents, tracker.config.monthly_limit_cents
            );
        }

        Ok(())
    }

    /// Check if agent can still use budget.
    pub async fn can_spend(&self, agent_id: &str) -> bool {
        let trackers = self.trackers.read().await;
        match trackers.get(agent_id) {
            Some(tracker) if tracker.config.hard_stop => {
                tracker.spent_cents < tracker.config.monthly_limit_cents
            }
            _ => true, // no tracker or soft limit = allow
        }
    }

    /// Get budget status for an agent.
    pub async fn status(&self, agent_id: &str) -> Option<BudgetStatus> {
        let trackers = self.trackers.read().await;
        trackers.get(agent_id).map(budget_status_from_tracker)
    }

    /// Get all agents' budget status.
    pub async fn all_status(&self) -> Vec<BudgetStatus> {
        let trackers = self.trackers.read().await;
        trackers
            .values()
            .map(budget_status_from_tracker)
            .collect()
    }
}

/// Compute usage percentage safely, guarding against divide-by-zero and
/// capping at 100 for the `u8` return value.
fn compute_usage_percent(spent: u64, limit: u64) -> u8 {
    if limit == 0 {
        if spent > 0 { 100 } else { 0 }
    } else {
        let pct = (spent as f64 / limit as f64 * 100.0) as u64;
        pct.min(100) as u8
    }
}

/// Build a [`BudgetStatus`] from a [`BudgetTracker`].
fn budget_status_from_tracker(t: &BudgetTracker) -> BudgetStatus {
    BudgetStatus {
        agent_id: t.agent_id.clone(),
        spent_cents: t.spent_cents,
        limit_cents: t.config.monthly_limit_cents,
        remaining_cents: t.config.monthly_limit_cents.saturating_sub(t.spent_cents),
        usage_percent: compute_usage_percent(t.spent_cents, t.config.monthly_limit_cents),
        hard_stop: t.config.hard_stop,
        is_exceeded: t.spent_cents >= t.config.monthly_limit_cents,
    }
}

impl Default for BudgetManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub agent_id: String,
    pub spent_cents: u64,
    pub limit_cents: u64,
    pub remaining_cents: u64,
    pub usage_percent: u8,
    pub hard_stop: bool,
    pub is_exceeded: bool,
}
