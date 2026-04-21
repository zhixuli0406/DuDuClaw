//! Unified Heartbeat Scheduler — per-agent periodic wake-up system.
//!
//! Each agent's `HeartbeatConfig` controls:
//! - `enabled`: whether this agent participates in heartbeat
//! - `interval_seconds`: fixed interval between beats (fallback if no cron)
//! - `cron`: cron expression for fine-grained scheduling (takes precedence)
//! - `max_concurrent_runs`: concurrency cap per agent
//!
//! Evolution is driven exclusively by the prediction engine (Phase 1) and
//! GVU self-play loop (Phase 2). The heartbeat scheduler handles:
//! 1. Pending IPC/bus message polling
//! 2. Silence breaker — forces a reflection if no evolution trigger for too long
//! 3. Emitting `HeartbeatEvent` for monitoring

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{self, Utc};
use cron::Schedule;
use duduclaw_core::types::{AgentStatus, HeartbeatConfig};
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
    /// Max silence hours before forced reflection (from EvolutionConfig).
    max_silence_hours: f64,
    schedule: Option<Schedule>,
    last_run: Option<chrono::DateTime<Utc>>,
    /// Timestamp of the last evolution trigger (from any source: prediction engine or silence breaker).
    last_evolution_trigger: Option<chrono::DateTime<Utc>>,
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
            max_silence_hours: agent.config.evolution.max_silence_hours,
            schedule,
            last_run: None,
            last_evolution_trigger: Some(Utc::now()), // prevent cold-start immediate trigger
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

}

// ── HeartbeatScheduler ────────────────────────────────────────

/// Unified heartbeat scheduler: reads agent configs from the registry,
/// fires per-agent heartbeats, and drives evolution reflections.
/// Maximum total concurrent evolution subprocesses across all agents (BE-H5).
const MAX_GLOBAL_CONCURRENT: usize = 8;

pub struct HeartbeatScheduler {
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    agents: Arc<RwLock<HashMap<String, LiveAgent>>>,
    running: Arc<AtomicBool>,
    /// Global concurrency limiter for evolution subprocesses.
    global_semaphore: Arc<tokio::sync::Semaphore>,
    /// Per-agent proactive rate limiting state (shared across heartbeat cycles).
    proactive_states: Arc<tokio::sync::Mutex<HashMap<String, crate::proactive::ProactiveState>>>,
}

impl HeartbeatScheduler {
    pub fn new(home_dir: PathBuf, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        Self {
            home_dir,
            registry,
            agents: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            global_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_GLOBAL_CONCURRENT)),
            proactive_states: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Sync internal state with the registry (picks up new/removed/changed agents).
    /// Avoids nested locks by collecting data from registry first, then updating agents.
    async fn sync_from_registry(&self) {
        // Step 1: Read registry and collect all data (release reg lock immediately)
        let snapshot: Vec<LoadedAgent> = {
            let reg = self.registry.read().await;
            reg.list().iter().map(|a| (*a).clone()).collect()
        }; // reg lock released here

        // Step 2: Now take agents write lock (no nested locks)
        let mut agents = self.agents.write().await;

        let current_names: Vec<String> = snapshot.iter().map(|a| a.config.agent.name.clone()).collect();
        agents.retain(|name, _| current_names.contains(name));

        for la in &snapshot {
            if la.config.agent.status == AgentStatus::Terminated {
                agents.remove(&la.config.agent.name);
                continue;
            }

            if let Some(existing) = agents.get_mut(&la.config.agent.name) {
                existing.config = la.config.heartbeat.clone();
                existing.max_silence_hours = la.config.evolution.max_silence_hours;
                existing.schedule = parse_cron(&la.config.heartbeat.cron);
            } else {
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
                    .saturating_sub(a.active_runs.available_permits().min(u32::MAX as usize) as u32),
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
            let sem = agent.active_runs.clone();
            let ps = self.proactive_states.clone();
            tokio::spawn(async move {
                execute_heartbeat(&home, &aid, &sem, &ps).await;
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
            if tick.is_multiple_of(10) {
                self.sync_from_registry().await;
            }

            let now = Utc::now();

            // Collect tasks to spawn while holding the lock, then release before spawning
            let mut to_spawn: Vec<(PathBuf, String, Arc<tokio::sync::Semaphore>)> = Vec::new();
            {
                let mut agents = self.agents.write().await;
                for agent in agents.values_mut() {
                    if !agent.config.enabled {
                        continue;
                    }

                    // ── Silence breaker ──
                    // If no evolution has occurred for max_silence_hours, mark for
                    // heartbeat so the prediction engine can pick it up on next conversation.
                    let hours_since_last = agent
                        .last_evolution_trigger
                        .map(|t| now.signed_duration_since(t).num_minutes() as f64 / 60.0)
                        .unwrap_or(agent.max_silence_hours + 1.0);

                    if hours_since_last > agent.max_silence_hours {
                        warn!(
                            agent = %agent.agent_id,
                            hours = format!("{hours_since_last:.1}"),
                            "Silence breaker: no evolution trigger for too long"
                        );
                        agent.last_evolution_trigger = Some(now);
                    }

                    // ── Normal heartbeat: bus polling ──
                    if !agent.should_fire(now) {
                        continue;
                    }
                    if agent.active_runs.available_permits() == 0 {
                        debug!(agent = %agent.agent_id, "Heartbeat skipped: max concurrent runs reached");
                        continue;
                    }

                    info!(agent = %agent.agent_id, run = agent.total_runs + 1, "Heartbeat firing");
                    agent.last_run = Some(now);
                    agent.total_runs += 1;

                    to_spawn.push((
                        self.home_dir.clone(),
                        agent.agent_id.clone(),
                        agent.active_runs.clone(),
                    ));
                }
            } // write lock released here

            // Now spawn tasks without holding any lock
            for (home, aid, sem) in to_spawn {
                let global_sem = self.global_semaphore.clone();
                let proactive_states = self.proactive_states.clone();
                tokio::spawn(async move {
                    let _global_permit = match global_sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    execute_heartbeat(&home, &aid, &sem, &proactive_states).await;
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
///
/// 1. Bus polling (existing): check for pending inter-agent messages
/// 2. Proactive check (new): if PROACTIVE.md exists, execute checks and route results
async fn execute_heartbeat(
    home_dir: &Path,
    agent_id: &str,
    semaphore: &tokio::sync::Semaphore,
    proactive_states: &tokio::sync::Mutex<HashMap<String, crate::proactive::ProactiveState>>,
) {
    let _permit = match semaphore.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            warn!(agent = agent_id, "Heartbeat concurrency limit reached, skipping");
            return;
        }
    };

    info!(agent = agent_id, "Heartbeat cycle start");

    // ── Bus polling (existing) ──
    let pending = count_pending_bus_messages(home_dir, agent_id).await;
    if pending > 0 {
        info!(agent = agent_id, pending, "Agent has pending bus messages");
    }

    // ── Proactive check (new) ──
    execute_proactive_check(home_dir, agent_id, proactive_states).await;

    info!(agent = agent_id, "Heartbeat cycle complete");
}

/// Execute proactive checks for an agent if PROACTIVE.md exists and conditions allow.
async fn execute_proactive_check(
    home_dir: &Path,
    agent_id: &str,
    proactive_states: &tokio::sync::Mutex<HashMap<String, crate::proactive::ProactiveState>>,
) {
    use crate::proactive;

    // Validate agent_id format (defense in depth)
    if agent_id.contains("..") || agent_id.contains('/') || agent_id.contains('\\') || agent_id.contains('\0') {
        warn!(agent = agent_id, "Invalid agent_id in proactive check, skipping");
        return;
    }

    let agent_dir = home_dir.join("agents").join(agent_id);

    // Load agent config for proactive settings
    let config_path = agent_dir.join("agent.toml");
    let config_content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(_) => return,
    };
    let agent_config: duduclaw_core::types::AgentConfig = match toml::from_str(&config_content) {
        Ok(c) => c,
        Err(_) => return,
    };

    if !agent_config.proactive.enabled {
        return;
    }

    // Check quiet hours
    if proactive::is_quiet_hour(&agent_config.proactive) {
        debug!(agent = agent_id, "Proactive check skipped: quiet hours");
        return;
    }

    // Load PROACTIVE.md
    let proactive_md = match proactive::load_proactive_md(&agent_dir) {
        Some(md) => md,
        None => return, // No PROACTIVE.md → nothing to do
    };

    info!(agent = agent_id, "Proactive check: executing");

    // Build prompt and execute via Claude CLI.
    //
    // Previously used `--print --no-input --system-prompt <inline>` without
    // `--mcp-config`. Three problems with that form:
    //   1. `--no-input` was removed in Claude CLI ≥2.1 (causes hard error).
    //   2. No `--mcp-config` means the agent cannot call Notion / Gmail /
    //      duduclaw MCP tools during a proactive check, so any check that
    //      depends on external state silently no-ops.
    //   3. `--max-turns 3` is too tight for checks that chain tool calls.
    //
    // Fix: mirror the channel_reply spawn path — pass system prompt via a
    // temp file (avoids /proc/PID/cmdline leak), attach the agent's
    // `.mcp.json` with `--strict-mcp-config`, and make max_turns config-
    // driven with a sensible default of 8.
    let prompt = proactive::build_proactive_prompt(&proactive_md, agent_id);
    let claude = duduclaw_core::which_claude();

    let result = match claude {
        Some(claude_path) => {
            // Write system prompt to a temp file — Claude CLI ≥2 supports
            // --system-prompt-file and this avoids cmdline length / leak issues.
            let prompt_file = match tempfile::NamedTempFile::new() {
                Ok(mut f) => {
                    use std::io::Write;
                    if let Err(e) = f.write_all(prompt.as_bytes()) {
                        warn!(agent = agent_id, "Proactive temp-file write failed: {e}");
                        return;
                    }
                    f
                }
                Err(e) => {
                    warn!(agent = agent_id, "Proactive temp-file create failed: {e}");
                    return;
                }
            };
            let prompt_path = prompt_file.path().to_path_buf();

            let max_turns_str = agent_config.proactive.max_turns.to_string();
            let mut cmd = duduclaw_core::platform::async_command_for(&claude_path);
            cmd.arg("--print")
                .args(["--system-prompt-file", &prompt_path.to_string_lossy()])
                .args(["--max-turns", &max_turns_str])
                .args(["-p", "Execute the proactive checks now."])
                .current_dir(&agent_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            // Attach agent's MCP server definitions so Notion/Gmail/etc tools
            // are available during the proactive run. `--strict-mcp-config`
            // prevents ambient global MCP from leaking in.
            let mcp_json = agent_dir.join(".mcp.json");
            if mcp_json.exists() {
                cmd.args(["--mcp-config", &mcp_json.to_string_lossy()]);
                cmd.arg("--strict-mcp-config");
            }

            let output = cmd.output().await;
            drop(prompt_file); // explicit: temp file survives until after spawn exits
            match output {
                Ok(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).to_string()
                }
                Ok(o) => {
                    let stderr_snippet: String = String::from_utf8_lossy(&o.stderr).chars().take(200).collect();
                    warn!(agent = agent_id, stderr = %stderr_snippet, "Proactive Claude failed");
                    return;
                }
                Err(e) => {
                    warn!(agent = agent_id, "Proactive exec error: {e}");
                    return;
                }
            }
        }
        None => {
            warn!(agent = agent_id, "Proactive check: claude CLI not found");
            return;
        }
    };

    // Parse result — silent or actionable?
    match proactive::parse_proactive_result(&result) {
        None => {
            debug!(agent = agent_id, "Proactive check: PROACTIVE_OK (silent)");
            let mut states = proactive_states.lock().await;
            states
                .entry(agent_id.to_string())
                .or_insert_with(crate::proactive::ProactiveState::new)
                .record_silent();
        }
        Some(message) => {
            // Rate limit check
            {
                let mut states = proactive_states.lock().await;
                let state = states
                    .entry(agent_id.to_string())
                    .or_insert_with(crate::proactive::ProactiveState::new);
                if !state.can_send(agent_config.proactive.max_messages_per_hour) {
                    debug!(agent = agent_id, "Proactive notification skipped: rate limit exceeded");
                    return;
                }
                state.record_sent();
            }

            info!(agent = agent_id, msg_len = message.len(), "Proactive check: notification to send");

            // Route notification to channel via bus_queue
            let notify = &agent_config.proactive;
            if !notify.notify_channel.is_empty() && !notify.notify_chat_id.is_empty() {
                let bus_entry = serde_json::json!({
                    "type": "proactive_notification",
                    "agent_id": agent_id,
                    "channel": notify.notify_channel,
                    "chat_id": notify.notify_chat_id,
                    "message": message,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                let queue_path = home_dir.join("bus_queue.jsonl");
                let line = format!("{}\n", bus_entry);
                // Use spawn_blocking + flock for safe concurrent JSONL append
                let qp = queue_path.clone();
                let aid = agent_id.to_string();
                let ch = notify.notify_channel.clone();
                tokio::task::spawn_blocking(move || {
                    use std::io::Write;
                    let file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&qp);
                    if let Ok(mut f) = file {
                        if let Err(e) = duduclaw_core::platform::flock_exclusive(&f) {
                            tracing::warn!("flock LOCK_EX failed, proceeding without lock: {e}");
                        }
                        let _ = f.write_all(line.as_bytes());
                        // Lock is released on drop
                        tracing::info!(agent = %aid, channel = %ch, "Proactive notification queued");
                    }
                }).await.ok();
            } else {
                warn!(agent = agent_id, "Proactive notification has no target channel configured");
            }
        }
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
