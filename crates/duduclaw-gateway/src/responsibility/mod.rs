//! P2-A — continuous responsibilities, bounded wake-ups and reliable delivery
//! of in-flight operator directions (steering).
//!
//! Waiting is not a task state: between two bounded runs there is no task at
//! all. A responsibility is an index row; three sources (time, events,
//! decisions) only ever write deduplicated wake facts (`wakeup_fires`), and
//! the only component that turns a fact into a goal task — and a goal task
//! into a queue message — is the existing goal-loop driver tick
//! ([`crate::goal_loop::GoalLoopDriver::tick_at`]). No new background loop or
//! scheduler exists.
//!
//! Service-layer entry points for the RPC / MCP / CLI surfaces (not wired in
//! this round) live in [`service`], [`steering`] and [`stop`].

mod agent_wake;
pub mod cost;
pub mod events;
pub mod lane;
pub mod notify;
pub mod operator_gate;
pub mod service;
pub mod steering;
pub mod stop;
pub mod summary;
pub mod team_activity;
pub(crate) mod test_hooks;
pub mod usage_hint;
pub mod wake;

#[cfg(test)]
mod tests;

use std::path::Path;

pub use cost::{CostSource, EpisodeCost, TelemetryCostSource};
pub use wake::{WakeContext, WakeReport, wake_pass};

/// Default and ceiling for `stop_at` distance (D12).
pub const DEFAULT_MAX_STOP_AT_DAYS: i64 = 30;
pub const MAX_STOP_AT_DAYS_CEILING: i64 = 90;
/// Event rows read per driver tick.
pub const DEFAULT_EVENT_POLL_BATCH: i64 = 500;
/// Events the MCP server actually writes to `events.db` (§3.3). Anything else
/// is refused when a subscription is created.
/// M-4: `activity.new` is not offered — every activity row is written by the
/// employee it is about, so it could only ever be the owner's own event and
/// never wake anything.
///
/// `mcp.event` (2026-10-08) is written by the gateway's MCP Events receiver
/// (`crate::mcp_events`) with `agent_id` = the subscription's employee. Only
/// deliveries of a subscription the operator opted into `normal` mode wake a
/// responsibility (an occurrence runs as an ordinary goal task); events of an
/// explore-lane subscription are recorded as `dropped(explore_lane)`.
pub const EVENT_WHITELIST: [&str; 3] = ["task.created", "task.updated", "mcp.event"];
/// Error text prefix the dispatcher writes when it refuses a stale durable
/// `goal:` round. The driver frees the slot without counting a failure.
pub const FENCE_ERROR_PREFIX: &str = "stale_dispatch_fenced";
/// At most one live agent-armed subscription per responsibility (D4).
pub const AGENT_ARMED_LIMIT: i64 = 1;
/// Approval kind of a responsibility decision question (D5: unbound request).
pub const DECISION_KIND: &str = "responsibility_decision";
/// Event payload key the MCP server stamps with the emitting employee.
pub const EVENT_EMITTED_BY_KEY: &str = "_emitted_by";
/// Activity event types written by this module.
pub mod activity {
    pub const WOKE: &str = "responsibility.woke";
    pub const EXPIRED: &str = "responsibility.expired";
    pub const BUDGET_PAUSED: &str = "responsibility.budget_paused";
    pub const BUDGET_RESUMED: &str = "responsibility.budget_resumed";
    pub const FAILURE_PAUSED: &str = "responsibility.failure_paused";
    pub const SETTLED: &str = "responsibility.settled";
    pub const COST_UNAVAILABLE: &str = "responsibility.cost_unavailable";
    pub const EVENT_GAP: &str = "responsibility.event_gap";
    pub const STOP_REQUESTED: &str = "task.stop_requested";
    pub const STOP_RECONCILED: &str = "task.stop_reconciled";
    pub const STOP_STEP_FAILED: &str = "task.stop_step_failed";
    pub const OCCURRENCE_COST_CAP: &str = "responsibility.occurrence_cost_cap";
    pub const EVENT_WAKE_CAP: &str = "responsibility.event_wake_cap";
    /// Activity `event_type` prefixes only the gateway may write (S-M6):
    /// MCP `activity_post` refuses them.
    pub const RESERVED_PREFIXES: [&str; 2] = ["responsibility.", "task.stop"];
}

/// `config.toml [responsibilities]`. Absent ⇒ defaults (off). A section that
/// cannot be parsed reads as off (fail closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsibilityConfig {
    pub enabled: bool,
    pub max_stop_at_days: i64,
    pub event_poll_batch: i64,
    pub max_notifications_per_period: i64,
    /// E-M4: occurrences per budget window that events may start; further
    /// event facts in that window are dropped (`event_wake_cap`).
    pub max_event_wakes_per_period: i64,
}

impl Default for ResponsibilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_stop_at_days: DEFAULT_MAX_STOP_AT_DAYS,
            event_poll_batch: DEFAULT_EVENT_POLL_BATCH,
            max_notifications_per_period: 10,
            max_event_wakes_per_period: 12,
        }
    }
}

fn read_config_table(home: &Path) -> Option<toml::Table> {
    let content = std::fs::read_to_string(home.join("config.toml")).ok()?;
    content.parse::<toml::Table>().ok()
}

impl ResponsibilityConfig {
    /// Read per call (hot): every tick and every service call re-reads it.
    pub fn from_home(home: &Path) -> Self {
        let mut cfg = Self::default();
        let Some(table) = read_config_table(home) else {
            return cfg;
        };
        let Some(section) = table.get("responsibilities") else {
            return cfg;
        };
        let Some(section) = section.as_table() else {
            return cfg; // wrong shape ⇒ off
        };
        cfg.enabled = section
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(days) = section.get("max_stop_at_days").and_then(|v| v.as_integer()) {
            cfg.max_stop_at_days = days.clamp(1, MAX_STOP_AT_DAYS_CEILING);
        }
        if let Some(batch) = section.get("event_poll_batch").and_then(|v| v.as_integer()) {
            cfg.event_poll_batch = batch.clamp(1, 2000);
        }
        if let Some(n) = section
            .get("max_notifications_per_period")
            .and_then(|v| v.as_integer())
        {
            cfg.max_notifications_per_period = n.max(0);
        }
        if let Some(n) = section
            .get("max_event_wakes_per_period")
            .and_then(|v| v.as_integer())
        {
            cfg.max_event_wakes_per_period = n.max(0);
        }
        cfg
    }
}

/// `config.toml [goal_loop] steering_enabled` (default `false`). Gates new
/// submissions only: entries already pending are still delivered on the
/// next round, so nothing an operator already sent is lost.
pub fn steering_enabled(home: &Path) -> bool {
    read_config_table(home)
        .and_then(|t| t.get("goal_loop").cloned())
        .and_then(|s| s.get("steering_enabled").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// Dispatcher fence for a `goal:` message: `Ok(())` only when the store says
/// the round may still run. Any error (including an unopenable store) is a
/// refusal carrying [`FENCE_ERROR_PREFIX`].
pub async fn fence_goal_message(home: &Path, message_id: &str) -> Result<(), String> {
    let store = crate::task_store::TaskStore::open(home)
        .map_err(|e| format!("{FENCE_ERROR_PREFIX}: task store unavailable: {e}"))?;
    store.fence_goal_dispatch(message_id).await.map_err(|e| {
        if e.starts_with(FENCE_ERROR_PREFIX) {
            e
        } else {
            format!("{FENCE_ERROR_PREFIX}: {e}")
        }
    })
}

/// The sender every goal-loop round carries.
pub const GOAL_LOOP_SENDER: &str = "goal-loop-driver";
/// The sender of heartbeat task-board wake-ups (`duduclaw-agent`).
pub const HEARTBEAT_SENDER: &str = "heartbeat-scheduler";

/// M-2: a heartbeat wake-up for a task inside a stopped tree is refused
/// (fail closed when the store cannot be read).
pub async fn fence_heartbeat_message(home: &Path, task_id: &str) -> Result<(), HeartbeatFence> {
    let store = crate::task_store::TaskStore::open(home).map_err(|e| {
        HeartbeatFence::Unreadable(format!("{FENCE_ERROR_PREFIX}: task store unavailable: {e}"))
    })?;
    match store.in_stop_tree(task_id).await {
        Ok(false) => Ok(()),
        Ok(true) => Err(HeartbeatFence::Stopped(format!(
            "{FENCE_ERROR_PREFIX}: task stopped"
        ))),
        Err(e) => Err(HeartbeatFence::Unreadable(format!(
            "{FENCE_ERROR_PREFIX}: {e}"
        ))),
    }
}

/// Why a heartbeat wake-up was not started (third review L3-3): a stopped
/// task's wake-up is failed for good; an unreadable store only postpones it
/// (the message goes back to pending), so a busy database does not cost the
/// employee its wake-up for the heartbeat's one-hour cooldown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatFence {
    Stopped(String),
    Unreadable(String),
}

/// E-H2: fence a goal round with a random id (no intent row).
pub async fn fence_plain_goal_message(home: &Path, task_id: &str) -> Result<(), String> {
    let store = crate::task_store::TaskStore::open(home)
        .map_err(|e| format!("{FENCE_ERROR_PREFIX}: task store unavailable: {e}"))?;
    store.fence_plain_goal_dispatch(task_id).await.map_err(|e| {
        if e.starts_with(FENCE_ERROR_PREFIX) {
            e
        } else {
            format!("{FENCE_ERROR_PREFIX}: {e}")
        }
    })
}

/// M3-1: the durable "this round is being handed to a runtime" mark,
/// written before the agent is started. An error means the mark could not be
/// written; the caller must not start the round (its spend would read as 0).
pub async fn mark_round_started(home: &Path, message_id: &str) -> Result<(), String> {
    let store = crate::task_store::TaskStore::open(home)
        .map_err(|e| format!("task store unavailable: {e}"))?;
    // L4-4: no intent row (deleted between the fence and here) is a failure
    // too: a round started without its mark would read as costing nothing.
    match store
        .mark_round_started(message_id, chrono::Utc::now())
        .await?
    {
        true => Ok(()),
        false => Err("no durable round with this id".into()),
    }
}

/// E-M2 / H-1: a durable round that never reached the employee (fenced, or
/// its dispatch failed before any agent ran it) abandons its intent and
/// gives its directions back to `pending`. Best effort: failures are logged;
/// the round then stays counted as sent (the conservative side).
pub async fn return_unrun_round(home: &Path, message_id: &str) {
    match crate::task_store::TaskStore::open(home) {
        Ok(store) => {
            if let Err(e) = store
                .return_unrun_round(message_id, chrono::Utc::now())
                .await
            {
                tracing::warn!(message_id, error = %e, "could not return undelivered directions");
            }
        }
        Err(e) => tracing::warn!(message_id, error = %e, "could not return undelivered directions"),
    }
}

/// sha256 hex of a string (contract / steering body hashes).
pub(crate) fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// P5: whether a goal-loop round for `task_id` must run in the explore lane.
/// `Ok(false)` for any task that is not a responsibility occurrence (and when
/// no task store exists yet); an unreadable store or lane is an error, and
/// the caller refuses the round (fail closed).
pub async fn round_requires_explore_lane(home: &Path, task_id: &str) -> Result<bool, String> {
    if !home.join("tasks.db").exists() {
        return Ok(false);
    }
    let store = crate::task_store::TaskStore::open(home)
        .map_err(|e| format!("task store unavailable: {e}"))?;
    match store.occurrence_for_task(task_id).await? {
        None => Ok(false),
        Some((_, resp)) => Ok(lane::lane_of(&resp)? == lane::RespLane::Explore),
    }
}
