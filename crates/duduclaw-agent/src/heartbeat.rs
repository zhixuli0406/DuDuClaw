//! Unified Heartbeat Scheduler — per-agent periodic wake-up system.
//!
//! Each agent's `HeartbeatConfig` controls:
//! - `enabled`: whether this agent participates in heartbeat
//! - `interval_seconds`: fixed interval between beats (fallback if no cron)
//! - `cron`: cron expression for fine-grained scheduling (takes precedence)
//! - `max_concurrent_runs`: concurrency cap per agent
//!
//! On each heartbeat tick the scheduler:
//! 1. Checks pending IPC/bus messages for the agent
//! 2. Triggers meso reflection (if `evolution.meso_reflection` is enabled)
//! 3. Triggers macro reflection once per day (if `evolution.macro_reflection` is enabled)
//! 4. Emits a `HeartbeatEvent` for monitoring

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{self, Utc};
use cron::Schedule;
use duduclaw_core::types::{AgentStatus, EvolutionConfig, HeartbeatConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, info, warn};

use crate::registry::{AgentRegistry, LoadedAgent};

// ── Public types ──────────────────────────────────────────────

/// Snapshot of one agent's heartbeat state, for monitoring / RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatStatus {
    pub agent_id: String,
    pub enabled: bool,
    pub interval_seconds: u64,
    pub cron: String,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub total_runs: u64,
    pub active_runs: u32,
    pub max_concurrent: u32,
}

/// Event emitted each time a heartbeat fires (for broadcast / logging).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatEvent {
    pub agent_id: String,
    pub timestamp: String,
    pub actions: Vec<String>,
}

// ── Internal per-agent state ──────────────────────────────────

struct LiveAgent {
    agent_id: String,
    config: HeartbeatConfig,
    evolution: EvolutionConfig,
    agent_dir: PathBuf,
    schedule: Option<Schedule>,
    last_run: Option<chrono::DateTime<Utc>>,
    last_macro: Option<chrono::DateTime<Utc>>,
    total_runs: u64,
    active_runs: Arc<tokio::sync::Semaphore>,
}

impl LiveAgent {
    fn from_loaded(agent: &LoadedAgent) -> Self {
        let schedule = parse_cron(&agent.config.heartbeat.cron);
        let max = agent.config.heartbeat.max_concurrent_runs.max(1);
        Self {
            agent_id: agent.config.agent.name.clone(),
            config: agent.config.heartbeat.clone(),
            evolution: agent.config.evolution.clone(),
            agent_dir: agent.dir.clone(),
            schedule,
            last_run: None,
            last_macro: None,
            total_runs: 0,
            active_runs: Arc::new(tokio::sync::Semaphore::new(max as usize)),
        }
    }

    /// Determine whether this agent should fire now.
    fn should_fire(&self, now: chrono::DateTime<Utc>) -> bool {
        if !self.config.enabled {
            return false;
        }

        // Cron takes precedence if present
        if let Some(sched) = &self.schedule {
            return match self.last_run {
                Some(last) => sched.after(&last).next().is_some_and(|next| next <= now),
                None => sched
                    .after(&(now - chrono::Duration::hours(1)))
                    .next()
                    .is_some_and(|next| next <= now),
            };
        }

        // Fallback: fixed interval
        match self.last_run {
            Some(last) => {
                let elapsed = now.signed_duration_since(last);
                let interval =
                    chrono::Duration::from_std(Duration::from_secs(self.config.interval_seconds))
                        .unwrap_or_default();
                elapsed >= interval
            }
            None => true,
        }
    }

    /// Compute the next expected fire time (for monitoring display).
    fn next_fire(&self) -> Option<chrono::DateTime<Utc>> {
        let anchor = self.last_run.unwrap_or_else(Utc::now);
        if let Some(sched) = &self.schedule {
            return sched.after(&anchor).next();
        }
        Some(anchor + chrono::Duration::seconds(self.config.interval_seconds as i64))
    }

    /// Whether macro reflection should run (once per 24h).
    fn should_macro(&self, now: chrono::DateTime<Utc>) -> bool {
        if !self.evolution.macro_reflection {
            return false;
        }
        match self.last_macro {
            Some(last) => now.signed_duration_since(last) >= chrono::Duration::hours(24),
            None => true,
        }
    }
}

// ── HeartbeatScheduler ────────────────────────────────────────

/// Unified heartbeat scheduler: reads agent configs from the registry,
/// fires per-agent heartbeats, and drives evolution reflections.
pub struct HeartbeatScheduler {
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    agents: Arc<RwLock<HashMap<String, LiveAgent>>>,
    running: Arc<AtomicBool>,
}

impl HeartbeatScheduler {
    pub fn new(home_dir: PathBuf, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        Self {
            home_dir,
            registry,
            agents: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Sync internal state with the registry (picks up new/removed/changed agents).
    async fn sync_from_registry(&self) {
        let reg = self.registry.read().await;
        let loaded: Vec<&LoadedAgent> = reg.list();

        let mut agents = self.agents.write().await;

        // Remove agents that no longer exist in registry
        let current_names: Vec<String> = loaded.iter().map(|a| a.config.agent.name.clone()).collect();
        agents.retain(|name, _| current_names.contains(name));

        // Add/update agents
        for la in loaded {
            // Skip terminated agents
            if la.config.agent.status == AgentStatus::Terminated {
                agents.remove(&la.config.agent.name);
                continue;
            }

            if let Some(existing) = agents.get_mut(&la.config.agent.name) {
                // Update config in-place (preserve last_run, total_runs)
                existing.config = la.config.heartbeat.clone();
                existing.evolution = la.config.evolution.clone();
                existing.schedule = parse_cron(&la.config.heartbeat.cron);
            } else {
                // New agent
                agents.insert(la.config.agent.name.clone(), LiveAgent::from_loaded(la));
            }
        }

        info!(
            active = agents.values().filter(|a| a.config.enabled).count(),
            total = agents.len(),
            "Heartbeat registry synced"
        );
    }

    /// Query the current heartbeat status for all agents.
    pub async fn status(&self) -> Vec<HeartbeatStatus> {
        let agents = self.agents.read().await;
        agents
            .values()
            .map(|a| HeartbeatStatus {
                agent_id: a.agent_id.clone(),
                enabled: a.config.enabled,
                interval_seconds: a.config.interval_seconds,
                cron: a.config.cron.clone(),
                last_run: a.last_run.map(|t| t.to_rfc3339()),
                next_run: a.next_fire().map(|t| t.to_rfc3339()),
                total_runs: a.total_runs,
                active_runs: a.config.max_concurrent_runs.max(1)
                    - a.active_runs.available_permits() as u32,
                max_concurrent: a.config.max_concurrent_runs.max(1),
            })
            .collect()
    }

    /// Manually trigger a heartbeat for a specific agent (on-demand).
    pub async fn trigger(&self, agent_id: &str) -> bool {
        let mut agents = self.agents.write().await;
        if let Some(agent) = agents.get_mut(agent_id) {
            agent.last_run = Some(Utc::now());
            agent.total_runs += 1;
            let home = self.home_dir.clone();
            let aid = agent.agent_id.clone();
            let dir = agent.agent_dir.clone();
            let evo = agent.evolution.clone();
            let sem = agent.active_runs.clone();
            tokio::spawn(async move {
                execute_heartbeat(&home, &aid, &dir, &evo, &sem).await;
            });
            true
        } else {
            false
        }
    }

    /// Start the main scheduler loop. Checks every 30 seconds.
    /// Syncs from registry every 5 minutes to pick up config changes.
    pub async fn run(self: Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        self.sync_from_registry().await;
        info!("Heartbeat scheduler started");

        let mut tick: u64 = 0;
        while self.running.load(Ordering::SeqCst) {
            // Wait before first check (give bots time to start)
            time::sleep(Duration::from_secs(30)).await;
            tick += 1;

            // Re-sync from registry every 5 minutes (10 * 30s)
            if tick % 10 == 0 {
                self.sync_from_registry().await;
            }

            let now = Utc::now();
            let mut agents = self.agents.write().await;

            for agent in agents.values_mut() {
                if !agent.should_fire(now) {
                    continue;
                }

                // Check concurrency limit
                if agent.active_runs.available_permits() == 0 {
                    debug!(
                        agent = %agent.agent_id,
                        "Heartbeat skipped: max concurrent runs reached"
                    );
                    continue;
                }

                info!(agent = %agent.agent_id, run = agent.total_runs + 1, "Heartbeat firing");
                agent.last_run = Some(now);
                agent.total_runs += 1;

                // Check if macro is also due
                let run_macro = agent.should_macro(now);
                if run_macro {
                    agent.last_macro = Some(now);
                }

                let home = self.home_dir.clone();
                let aid = agent.agent_id.clone();
                let dir = agent.agent_dir.clone();
                let evo = agent.evolution.clone();
                let sem = agent.active_runs.clone();

                tokio::spawn(async move {
                    execute_heartbeat(&home, &aid, &dir, &evo, &sem).await;
                    if run_macro {
                        execute_evolution("macro", &home, &aid, &dir).await;
                    }
                });
            }
        }

        warn!("Heartbeat scheduler stopped");
    }

    /// Stop the scheduler.
    pub fn stop(&self) {
        info!("Stopping heartbeat scheduler");
        self.running.store(false, Ordering::SeqCst);
    }
}

// ── Heartbeat execution ───────────────────────────────────────

/// Execute a single heartbeat cycle for one agent.
async fn execute_heartbeat(
    home_dir: &Path,
    agent_id: &str,
    agent_dir: &Path,
    evolution: &EvolutionConfig,
    semaphore: &tokio::sync::Semaphore,
) {
    // Acquire concurrency permit
    let _permit = match semaphore.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            warn!(agent = agent_id, "Heartbeat concurrency limit reached, skipping");
            return;
        }
    };

    info!(agent = agent_id, "Heartbeat cycle start");

    // 1. Process pending bus_queue messages for this agent
    let pending = count_pending_bus_messages(home_dir, agent_id).await;
    if pending > 0 {
        info!(agent = agent_id, pending, "Agent has pending bus messages");
    }

    // 2. Meso reflection (if enabled)
    if evolution.meso_reflection {
        execute_evolution("meso", home_dir, agent_id, agent_dir).await;
    }

    info!(agent = agent_id, "Heartbeat cycle complete");
}

/// Run an evolution reflection via Python subprocess.
async fn execute_evolution(
    reflection_type: &str,
    home_dir: &Path,
    agent_id: &str,
    agent_dir: &Path,
) {
    info!(agent = agent_id, r#type = reflection_type, "Evolution reflection start");

    let python_path = find_python_path(home_dir);
    let mut cmd = tokio::process::Command::new("python3");
    cmd.args([
        "-m",
        "duduclaw.evolution.run",
        reflection_type,
        "--agent-id",
        agent_id,
        "--agent-dir",
        &agent_dir.to_string_lossy(),
    ]);
    cmd.env("PYTHONPATH", &python_path);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let timeout_secs = match reflection_type {
        "macro" => 120,
        _ => 60,
    };

    match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            info!(agent = agent_id, r#type = reflection_type, "Evolution reflection completed");
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                agent = agent_id,
                r#type = reflection_type,
                "Evolution reflection failed: {}",
                &stderr[..stderr.len().min(200)]
            );
        }
        Ok(Err(e)) => warn!(agent = agent_id, "Evolution spawn error: {e}"),
        Err(_) => warn!(
            agent = agent_id,
            r#type = reflection_type,
            timeout_secs,
            "Evolution reflection timed out"
        ),
    }
}

// ── Helpers ───────────────────────────────────────────────────

/// Parse a cron expression, normalising 5-field to 6-field (prepend seconds=0).
fn parse_cron(expr: &str) -> Option<Schedule> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalised = if trimmed.split_whitespace().count() == 5 {
        format!("0 {trimmed}")
    } else {
        trimmed.to_string()
    };
    match normalised.parse::<Schedule>() {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(cron = trimmed, "Invalid cron expression: {e}");
            None
        }
    }
}

fn find_python_path(home_dir: &Path) -> String {
    let candidates = [
        home_dir
            .parent()
            .unwrap_or(home_dir)
            .join("python")
            .to_string_lossy()
            .to_string(),
        "/opt/duduclaw".to_string(),
    ];
    for path in &candidates {
        if !path.is_empty() && Path::new(path).join("duduclaw").exists() {
            return path.clone();
        }
    }
    std::env::var("PYTHONPATH").unwrap_or_default()
}

/// Count pending `agent_message` entries in bus_queue.jsonl for a specific agent.
async fn count_pending_bus_messages(home_dir: &Path, agent_id: &str) -> usize {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) => c,
        Err(_) => return 0,
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                && v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
        })
        .count()
}

// ── Public entry point ────────────────────────────────────────

/// Start the unified heartbeat scheduler as a background task.
///
/// Returns the `Arc<HeartbeatScheduler>` so callers can query status
/// or trigger on-demand heartbeats.
pub fn start_heartbeat_scheduler(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> Arc<HeartbeatScheduler> {
    let scheduler = Arc::new(HeartbeatScheduler::new(home_dir, registry));
    let s = scheduler.clone();
    tokio::spawn(async move {
        s.run().await;
    });
    scheduler
}
