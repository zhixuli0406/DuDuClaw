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
    /// IANA timezone for interpreting `cron` (empty = UTC, the legacy
    /// behaviour pre-v1.8.23).
    pub cron_timezone: String,
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

/// Emitted when an agent has gone `max_silence_hours` without any evolution
/// trigger.  The gateway subscribes to this channel and turns the event into
/// an actual forced reflection (writing to `prediction.db.evolution_events`
/// and optionally invoking the GVU loop).
///
/// The heartbeat scheduler used to handle silence detection by simply
/// resetting a timer and emitting a `warn!` — no evolution actually
/// happened. This event is the bridge from "we noticed silence" to "we did
/// something about it".
#[derive(Debug, Clone)]
pub struct SilenceBreakerEvent {
    pub agent_id: String,
    /// How long (in hours) the agent had been silent when this event fired.
    pub hours: f64,
    pub timestamp: chrono::DateTime<Utc>,
}

// ── Internal per-agent state ──────────────────────────────────

struct LiveAgent {
    agent_id: String,
    config: HeartbeatConfig,
    /// Max silence hours before forced reflection (from EvolutionConfig).
    max_silence_hours: f64,
    schedule: Option<Schedule>,
    /// Parsed `cron_timezone`. `None` means UTC (legacy). An invalid name in
    /// config (e.g. typo) resolves to `None` here and a single warn-level
    /// log line is emitted at load time — the cron continues to fire in UTC
    /// instead of going silent.
    cron_tz: Option<chrono_tz::Tz>,
    last_run: Option<chrono::DateTime<Utc>>,
    /// Timestamp of the last evolution trigger (from any source: prediction engine or silence breaker).
    last_evolution_trigger: Option<chrono::DateTime<Utc>>,
    total_runs: u64,
    active_runs: Arc<tokio::sync::Semaphore>,
}

/// Parse the `cron_timezone` field, logging a warning once when an IANA name
/// is present but invalid. Empty strings are the normal "use UTC" default
/// and are silent.
fn resolve_cron_tz(agent_id: &str, tz_name: &str) -> Option<chrono_tz::Tz> {
    let trimmed = tz_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    match duduclaw_core::parse_timezone(trimmed) {
        Some(tz) => Some(tz),
        None => {
            warn!(
                agent = agent_id,
                cron_timezone = trimmed,
                "Unknown cron_timezone — falling back to UTC. Use an IANA name like \"Asia/Taipei\"."
            );
            None
        }
    }
}

impl LiveAgent {
    fn from_loaded(agent: &LoadedAgent) -> Self {
        let schedule = parse_cron(&agent.config.heartbeat.cron);
        let max = agent.config.heartbeat.max_concurrent_runs.max(1);
        let cron_tz = resolve_cron_tz(
            &agent.config.agent.name,
            &agent.config.heartbeat.cron_timezone,
        );
        Self {
            agent_id: agent.config.agent.name.clone(),
            config: agent.config.heartbeat.clone(),
            max_silence_hours: agent.config.evolution.max_silence_hours,
            schedule,
            cron_tz,
            last_run: None,
            last_evolution_trigger: Some(Utc::now()), // prevent cold-start immediate trigger
            total_runs: 0,
            active_runs: Arc::new(tokio::sync::Semaphore::new(max as usize)),
        }
    }

    /// Determine whether this agent should fire now. When `cron_timezone`
    /// is set on the config, the cron expression is evaluated in that
    /// timezone's wall clock — so `"0 9 * * *"` with `cron_timezone =
    /// "Asia/Taipei"` fires at 09:00 Taipei every day, not 09:00 UTC.
    fn should_fire(&self, now: chrono::DateTime<Utc>) -> bool {
        if !self.config.enabled {
            return false;
        }

        // Cron takes precedence if present
        if let Some(sched) = &self.schedule {
            return duduclaw_core::should_fire_in_tz(
                sched, self.last_run, now, self.cron_tz,
            );
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
    /// Returned as a UTC instant regardless of `cron_timezone` — callers
    /// format it for display.
    fn next_fire(&self) -> Option<chrono::DateTime<Utc>> {
        let anchor = self.last_run.unwrap_or_else(Utc::now);
        if let Some(sched) = &self.schedule {
            return match self.cron_tz {
                Some(tz) => sched
                    .after(&anchor.with_timezone(&tz))
                    .next()
                    .map(|dt| dt.with_timezone(&Utc)),
                None => sched.after(&anchor).next(),
            };
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
    /// Optional channel for forwarding [`SilenceBreakerEvent`]s to the gateway.
    /// `None` means silence-breaker events stay informational (warn-only).
    silence_tx: Option<tokio::sync::mpsc::UnboundedSender<SilenceBreakerEvent>>,
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
            silence_tx: None,
        }
    }

    /// Builder: install a channel that receives [`SilenceBreakerEvent`]s. Gateway
    /// uses this to wire silence breaker → forced reflection.
    pub fn with_silence_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<SilenceBreakerEvent>,
    ) -> Self {
        self.silence_tx = Some(tx);
        self
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
                existing.cron_tz = resolve_cron_tz(
                    &la.config.agent.name,
                    &la.config.heartbeat.cron_timezone,
                );
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
                cron_timezone: a.config.cron_timezone.clone(),
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
                        // Reset the timer first so a slow downstream consumer
                        // can't cause us to fire repeatedly.
                        agent.last_evolution_trigger = Some(now);
                        // Forward to the gateway if a channel is wired up. Use
                        // `try_send`-style fail-fast: if the receiver is gone
                        // we don't want to block the scheduler.
                        if let Some(tx) = self.silence_tx.as_ref() {
                            let event = SilenceBreakerEvent {
                                agent_id: agent.agent_id.clone(),
                                hours: hours_since_last,
                                timestamp: now,
                            };
                            if let Err(e) = tx.send(event) {
                                debug!(
                                    agent = %agent.agent_id,
                                    "Silence breaker channel closed: {e}"
                                );
                            }
                        }
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
    start_heartbeat_scheduler_with(home_dir, registry, None)
}

/// Same as [`start_heartbeat_scheduler`] but allows the caller to install a
/// channel for [`SilenceBreakerEvent`]s. Used by the gateway to convert
/// silence detection into a real forced-reflection trigger.
pub fn start_heartbeat_scheduler_with(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    silence_tx: Option<tokio::sync::mpsc::UnboundedSender<SilenceBreakerEvent>>,
) -> Arc<HeartbeatScheduler> {
    let mut sched = HeartbeatScheduler::new(home_dir, registry);
    if let Some(tx) = silence_tx {
        sched = sched.with_silence_tx(tx);
    }
    let scheduler = Arc::new(sched);
    let s = scheduler.clone();
    tokio::spawn(async move {
        s.run().await;
    });
    scheduler
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_live_agent(
        cron: &str,
        cron_timezone: &str,
    ) -> LiveAgent {
        LiveAgent {
            agent_id: "test".into(),
            config: HeartbeatConfig {
                enabled: true,
                interval_seconds: 300,
                max_concurrent_runs: 1,
                cron: cron.into(),
                cron_timezone: cron_timezone.into(),
            },
            max_silence_hours: 4.0,
            schedule: parse_cron(cron),
            cron_tz: resolve_cron_tz("test", cron_timezone),
            last_run: None,
            last_evolution_trigger: None,
            total_runs: 0,
            active_runs: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    #[test]
    fn should_fire_respects_cron_timezone() {
        // "0 9 * * *" in Asia/Taipei = 01:00 UTC.
        let agent = make_live_agent("0 9 * * *", "Asia/Taipei");

        // 00:59 UTC (08:59 Taipei) → no fire.
        let before = Utc.with_ymd_and_hms(2026, 4, 22, 0, 59, 0).unwrap();
        assert!(!agent.should_fire(before));

        // 01:00 UTC (09:00 Taipei) → fire.
        let at = Utc.with_ymd_and_hms(2026, 4, 22, 1, 0, 0).unwrap();
        assert!(agent.should_fire(at));
    }

    #[test]
    fn should_fire_utc_fallback_when_tz_empty() {
        // Same cron expression, no timezone → UTC, so 09:00 UTC is the fire time.
        let agent = make_live_agent("0 9 * * *", "");

        // 01:00 UTC → NOT fire (Taipei 09:00, but we're in UTC mode).
        let at_tpe = Utc.with_ymd_and_hms(2026, 4, 22, 1, 0, 0).unwrap();
        assert!(!agent.should_fire(at_tpe));

        // 09:00 UTC → fire.
        let at_utc = Utc.with_ymd_and_hms(2026, 4, 22, 9, 0, 0).unwrap();
        assert!(agent.should_fire(at_utc));
    }

    #[test]
    fn should_fire_invalid_tz_falls_back_to_utc() {
        // Unknown IANA name should resolve to None and behave like UTC.
        let agent = make_live_agent("0 9 * * *", "Mars/Olympus");
        assert!(agent.cron_tz.is_none());

        let at_tpe = Utc.with_ymd_and_hms(2026, 4, 22, 1, 0, 0).unwrap();
        assert!(!agent.should_fire(at_tpe));
        let at_utc = Utc.with_ymd_and_hms(2026, 4, 22, 9, 0, 0).unwrap();
        assert!(agent.should_fire(at_utc));
    }

    #[test]
    fn should_fire_disabled_never_fires() {
        let mut agent = make_live_agent("* * * * *", "UTC");
        agent.config.enabled = false;
        let now = Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap();
        assert!(!agent.should_fire(now));
    }

    #[test]
    fn next_fire_returns_utc_instant_regardless_of_tz() {
        // "0 9 * * *" Asia/Taipei → next fire instant should be 01:00 UTC
        // on the next day when last_run anchors at 01:05 UTC same day.
        let mut agent = make_live_agent("0 9 * * *", "Asia/Taipei");
        agent.last_run = Some(Utc.with_ymd_and_hms(2026, 4, 22, 1, 5, 0).unwrap());
        let next = agent.next_fire().expect("cron is set so next_fire is Some");
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 4, 23, 1, 0, 0).unwrap());
    }
}
