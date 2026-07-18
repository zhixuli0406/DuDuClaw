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
use rusqlite::OptionalExtension;
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
    /// Master evolution kill-switch (`[evolution] enabled`). When `false`, the
    /// silence-breaker below stays inert — the previously-uncovered bypass path
    /// where an agent kept evolving despite evolution being switched off.
    evolution_enabled: bool,
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
            evolution_enabled: agent.config.evolution.enabled,
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
    /// U1 natural-timing deferral state (silence breaker + proactive checks).
    /// In-memory only — restart merely restarts the 6h deferral cap clock.
    timing_deferrals: Arc<crate::proactive_timing::DeferralLedger>,
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
            timing_deferrals: Arc::new(crate::proactive_timing::DeferralLedger::new()),
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
            // Terminated / Archived / Deleted agents are off the scheduler —
            // archived + soft-deleted (WP4) also halt heartbeat/evolution.
            if matches!(
                la.config.agent.status,
                AgentStatus::Terminated | AgentStatus::Archived | AgentStatus::Deleted
            ) {
                agents.remove(&la.config.agent.name);
                continue;
            }

            if let Some(existing) = agents.get_mut(&la.config.agent.name) {
                existing.config = la.config.heartbeat.clone();
                existing.max_silence_hours = la.config.evolution.max_silence_hours;
                existing.evolution_enabled = la.config.evolution.enabled;
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
            let timing = self.timing_deferrals.clone();
            tokio::spawn(async move {
                execute_heartbeat(&home, &aid, &sem, &ps, &timing).await;
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

            // ── Task board pull (runs for ALL agents regardless of heartbeat.enabled) ──
            //
            // The task board is a scheduler-level concern, not a per-agent
            // evolution concern: even agents with `heartbeat.enabled = false`
            // (the default for most agents) still need to be woken up when
            // work is assigned to them on the board. Without this, the
            // Multica "Agent-as-teammate" design degenerates to "agents only
            // act when a channel message arrives" — exactly the failure
            // mode observed at 12:27 2026-04-28 with 26 unrouted tasks.
            //
            // Throttled per agent to 1 pull per 60s so a 30s scheduler tick
            // doesn't double-fire.
            let pull_targets: Vec<(PathBuf, String)> = {
                let agents = self.agents.read().await;
                agents
                    .values()
                    .map(|a| (self.home_dir.clone(), a.agent_id.clone()))
                    .collect()
            };
            for (home, aid) in pull_targets {
                tokio::spawn(async move {
                    if let Err(e) = poll_assigned_tasks(&home, &aid).await {
                        debug!(agent = %aid, error = %e, "Task board poll skipped");
                    }
                });
            }

            // Collect tasks to spawn while holding the lock, then release before spawning
            let mut to_spawn: Vec<(PathBuf, String, Arc<tokio::sync::Semaphore>)> = Vec::new();
            // Silence-threshold crossings detected this tick. The actual
            // send/defer decision + commit happens AFTER the lock is released
            // (the U1 timing gate reads sessions.db — no I/O under the lock).
            let mut silence_candidates: Vec<(String, f64)> = Vec::new();
            {
                let mut agents = self.agents.write().await;
                for agent in agents.values_mut() {
                    if !agent.config.enabled {
                        continue;
                    }

                    // ── Silence breaker (detection only) ──
                    // Master evolution kill-switch gate: when `[evolution]
                    // enabled = false`, this agent's autonomous evolution is
                    // frozen, so the silence-breaker must NOT fire (this was a
                    // bypass path — the client-reported "evolution kept running
                    // after I turned it off" bug). Bus polling below is a
                    // separate, non-evolution concern and still runs regardless.
                    //
                    // U1 (arXiv:2602.00880 / 2509.24073): crossing the
                    // threshold no longer means "fire now" — it means "fire at
                    // the next natural moment". We only collect the candidate
                    // here; `last_evolution_trigger` stays untouched until the
                    // timing gate approves the send, so a deferred event is
                    // simply re-detected on the next 30s tick (the tick loop
                    // itself is the rescheduler — no separate timer needed).
                    if agent.evolution_enabled {
                        let hours_since_last = agent
                            .last_evolution_trigger
                            .map(|t| now.signed_duration_since(t).num_minutes() as f64 / 60.0)
                            .unwrap_or(agent.max_silence_hours + 1.0);

                        if hours_since_last > agent.max_silence_hours {
                            silence_candidates.push((agent.agent_id.clone(), hours_since_last));
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

            // ── Silence breaker: natural-timing gate + commit (U1) ──
            //
            // Runs without the agents lock: the gate reads sessions.db
            // (read-only). `SendDecision::Defer` leaves the agent's
            // `last_evolution_trigger` untouched, so the next 30s tick
            // re-detects and re-evaluates; the gate's ledger caps total
            // deferral at 6h, after which the event fires regardless. With
            // `[proactive] natural_timing = false` (kill switch) the gate
            // returns SendNow unconditionally — identical to pre-U1 behaviour.
            for (aid, hours) in silence_candidates {
                let key = format!("silence:{aid}");
                let decision = crate::proactive_timing::gate_keyed_send(
                    &self.home_dir,
                    &aid,
                    None,
                    &key,
                    &self.timing_deferrals,
                    now,
                    "silence-breaker",
                )
                .await;
                if let crate::proactive_timing::SendDecision::Defer { until, reason } = decision {
                    info!(
                        agent = %aid,
                        until = %until.to_rfc3339(),
                        reason = %reason,
                        "Silence breaker deferred to natural moment"
                    );
                    continue;
                }

                // Commit: re-acquire the lock, reset the timer, emit the event.
                let mut agents = self.agents.write().await;
                let Some(agent) = agents.get_mut(&aid) else {
                    continue; // agent removed between detection and commit
                };
                warn!(
                    agent = %agent.agent_id,
                    hours = format!("{hours:.1}"),
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
                        hours,
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

            // Now spawn tasks without holding any lock
            for (home, aid, sem) in to_spawn {
                let global_sem = self.global_semaphore.clone();
                let proactive_states = self.proactive_states.clone();
                let timing = self.timing_deferrals.clone();
                tokio::spawn(async move {
                    let _global_permit = match global_sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    execute_heartbeat(&home, &aid, &sem, &proactive_states, &timing).await;
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
/// 2. Task board pull (new): scan tasks.db for `todo`/stalled `in_progress`
///    rows assigned to this agent and enqueue a wake-up message — closes the
///    "Multica task board exists but agents never claim from it" gap where
///    agents only ever ran when a channel message arrived.
/// 3. Proactive check: if PROACTIVE.md exists, execute checks and route results
async fn execute_heartbeat(
    home_dir: &Path,
    agent_id: &str,
    semaphore: &tokio::sync::Semaphore,
    proactive_states: &tokio::sync::Mutex<HashMap<String, crate::proactive::ProactiveState>>,
    timing_deferrals: &crate::proactive_timing::DeferralLedger,
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

    // Note: Task board pull (`poll_assigned_tasks`) is invoked from
    // `HeartbeatScheduler::run` directly so it covers agents with
    // `heartbeat.enabled = false` too — see the comment block there.

    // ── SOUL.md integrity check (added 2026-05-20) ──
    //
    // `soul_guard::check_soul_integrity` was previously only called by the
    // `duduclaw test <agent>` CLI command. That meant any drift between
    // SOUL.md and the stored hash — whether legitimate (failed
    // `agent_update_soul` hash refresh) or malicious (out-of-band tampering) —
    // sat silently until an operator manually ran the red-team test.
    //
    // Wiring it into the heartbeat means drift surfaces in the gateway log
    // within one heartbeat interval (default 1h). We log at WARN and append a
    // `_soul_integrity_drift` audit row so operators can grep / alert on it.
    // No automatic recovery — that's a policy decision that needs human input
    // (was the change intentional? rollback or accept?).
    check_soul_integrity_with_audit(home_dir, agent_id).await;

    // ── Proactive check (new) ──
    execute_proactive_check(home_dir, agent_id, proactive_states, timing_deferrals).await;

    info!(agent = agent_id, "Heartbeat cycle complete");
}

/// Run `soul_guard::check_soul_integrity` and surface drift via WARN log +
/// audit row. Pure side-effect; the result is reported but not acted upon.
async fn check_soul_integrity_with_audit(home_dir: &Path, agent_id: &str) {
    let agent_dir = home_dir.join("agents").join(agent_id);
    if !agent_dir.join("SOUL.md").exists() {
        // Agent without SOUL.md — nothing to check. Don't warn; this is the
        // documented configuration for stub agents.
        return;
    }

    // Run on a blocking thread — `check_soul_integrity` does sync file I/O
    // and SHA-256. The work is tiny but we avoid blocking the heartbeat
    // executor in case the agent directory is on a slow disk.
    let agent_dir_owned = agent_dir.clone();
    let agent_id_owned = agent_id.to_string();
    let result = match tokio::task::spawn_blocking(move || {
        duduclaw_security::soul_guard::check_soul_integrity(&agent_id_owned, &agent_dir_owned)
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(agent = agent_id, "SOUL integrity check task panicked: {e}");
            return;
        }
    };

    if result.intact {
        debug!(agent = agent_id, "SOUL.md integrity OK");
        return;
    }

    warn!(
        agent = agent_id,
        current_hash = %result.current_hash,
        expected_hash = %result.expected_hash,
        "SOUL.md integrity drift detected: {}",
        result.message,
    );

    // Audit so external alerting can pick it up without grepping logs.
    duduclaw_security::audit::append_tool_call(
        home_dir,
        agent_id,
        "_soul_integrity_drift",
        &format!(
            "drift: current={}, expected={}, msg={}",
            &result.current_hash.chars().take(16).collect::<String>(),
            &result.expected_hash.chars().take(16).collect::<String>(),
            result.message,
        ),
        false,
    );
}

/// Poll `tasks.db` for work assigned to this agent and enqueue a wake-up
/// message into `message_queue.db`. The dispatcher's existing 5-second poll
/// loop will then route the message to the agent through the same code path
/// channel messages use.
///
/// Two trigger conditions:
///   1. **Highest-priority `todo`** task assigned to this agent — pulls one
///      per heartbeat to avoid stampedes. The agent is told to use
///      `tasks_claim` to formally take ownership.
///   2. **Stalled `in_progress`** task — `updated_at` older than 30 min.
///      The agent is asked to either resume work or surface a blocker.
///
/// We track which (task_id, kind) pairs have been nudged in `tasks.db`'s
/// `metadata` field of an injected `activity` row, so a single backlog item
/// doesn't generate one wake-up per heartbeat tick. Cooldown: 1 hour.
async fn poll_assigned_tasks(home_dir: &Path, agent_id: &str) -> Result<(), String> {
    let tasks_db = home_dir.join("tasks.db");
    let queue_db = home_dir.join("message_queue.db");
    if !tasks_db.exists() || !queue_db.exists() {
        return Ok(());
    }

    // Validate agent_id (defense in depth — same checks as proactive).
    if agent_id.contains("..")
        || agent_id.contains('/')
        || agent_id.contains('\\')
        || agent_id.contains('\0')
    {
        return Err("invalid agent_id".into());
    }

    let agent = agent_id.to_string();
    let tasks_path = tasks_db.clone();
    let queue_path = queue_db.clone();

    // All sqlite I/O on a blocking thread — rusqlite is sync.
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let tdb = rusqlite::Connection::open(&tasks_path)
            .map_err(|e| format!("open tasks.db: {e}"))?;
        tdb.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("set busy_timeout: {e}"))?;

        // Highest-priority unstarted task for this agent. `pending` = durable
        // dispatch-engine tasks awaiting a claim — without it here they are
        // never surfaced to anyone (MED finding, 2026-07 review).
        // Goal-mode tasks are driven by the gateway's goal loop driver
        // (`goal_loop.rs`), which dispatches them without the 1-hour cooldown so
        // the Generator-Verifier retry loop stays tight. Excluding them here
        // avoids a double nudge (goal loop + heartbeat) on the same task; the
        // heartbeat pull remains the fallback wake-up for ordinary tasks.
        let todo: Option<(String, String, String)> = tdb
            .query_row(
                "SELECT id, title, priority FROM tasks
                 WHERE assigned_to = ?1 AND status IN ('todo', 'pending')
                   AND COALESCE(goal_mode, 0) = 0
                 ORDER BY CASE priority
                     WHEN 'critical' THEN 0
                     WHEN 'urgent'   THEN 1
                     WHEN 'high'     THEN 2
                     WHEN 'medium'   THEN 3
                     ELSE 4
                   END, created_at ASC
                 LIMIT 1",
                rusqlite::params![&agent],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("query todo: {e}"))?;

        // Stalled in_progress: updated_at > 30 minutes ago.
        let stall_cutoff = (chrono::Utc::now() - chrono::Duration::minutes(30)).to_rfc3339();
        let stalled: Option<(String, String)> = tdb
            .query_row(
                "SELECT id, title FROM tasks
                 WHERE assigned_to = ?1 AND status = 'in_progress'
                   AND updated_at < ?2
                 ORDER BY updated_at ASC
                 LIMIT 1",
                rusqlite::params![&agent, stall_cutoff],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| format!("query stalled: {e}"))?;
        drop(tdb);

        if todo.is_none() && stalled.is_none() {
            return Ok(());
        }

        let qdb = rusqlite::Connection::open(&queue_path)
            .map_err(|e| format!("open message_queue.db: {e}"))?;
        qdb.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("set busy_timeout: {e}"))?;

        // Cooldown gate: skip if the same kind of nudge was sent to this
        // agent within the last hour. We use the `payload` LIKE marker on
        // existing pending/acked rows so there's no extra schema.
        let cooldown_cutoff = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        if let Some((task_id, title, priority)) = todo {
            let marker = format!("[heartbeat-pull task_id={task_id}]");
            let already: i64 = qdb
                .query_row(
                    "SELECT COUNT(*) FROM message_queue
                     WHERE target = ?1 AND payload LIKE ?2 AND created_at > ?3",
                    rusqlite::params![&agent, format!("%{marker}%"), &cooldown_cutoff],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if already == 0 {
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let payload = format!(
                    "{marker} 任務看板有一筆指派給你的待辦任務尚未開始：\n\
                     • Task ID: {task_id}\n\
                     • 標題: {title}\n\
                     • 優先級: {priority}\n\n\
                     請使用 MCP 工具 `tasks_claim` 接手這項任務並開始執行；\
                     若無法立即處理，使用 `tasks_block` 並說明阻塞原因。\n\
                     接手後若任務需要較長時間，請每隔幾分鐘呼叫 `tasks_renew` 續約，\
                     否則租約到期任務會被回收並重新派工。"
                );
                qdb.execute(
                    "INSERT INTO message_queue \
                     (id, sender, target, payload, status, retry_count, delegation_depth, \
                      created_at) \
                     VALUES (?1, 'heartbeat-scheduler', ?2, ?3, 'pending', 0, 0, ?4)",
                    rusqlite::params![&id, &agent, &payload, &now],
                )
                .map_err(|e| format!("enqueue todo wake-up: {e}"))?;
                info!(
                    agent = %agent,
                    task_id = %task_id,
                    priority = %priority,
                    "Heartbeat: enqueued todo wake-up"
                );
            }
        }

        if let Some((task_id, title)) = stalled {
            let marker = format!("[heartbeat-stall task_id={task_id}]");
            let already: i64 = qdb
                .query_row(
                    "SELECT COUNT(*) FROM message_queue
                     WHERE target = ?1 AND payload LIKE ?2 AND created_at > ?3",
                    rusqlite::params![&agent, format!("%{marker}%"), &cooldown_cutoff],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if already == 0 {
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let payload = format!(
                    "{marker} 你 claim 的任務已停滯超過 30 分鐘無進度：\n\
                     • Task ID: {task_id}\n\
                     • 標題: {title}\n\n\
                     請使用 MCP 工具 `activity_post` 回報目前進度，或在受阻時 \
                     `tasks_block` 標記並說明原因；若仍在處理中，記得呼叫 \
                     `tasks_renew` 續約以免任務被回收。"
                );
                qdb.execute(
                    "INSERT INTO message_queue \
                     (id, sender, target, payload, status, retry_count, delegation_depth, \
                      created_at) \
                     VALUES (?1, 'heartbeat-scheduler', ?2, ?3, 'pending', 0, 0, ?4)",
                    rusqlite::params![&id, &agent, &payload, &now],
                )
                .map_err(|e| format!("enqueue stall wake-up: {e}"))?;
                info!(
                    agent = %agent,
                    task_id = %task_id,
                    "Heartbeat: enqueued stalled-task wake-up"
                );
            }
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// Execute proactive checks for an agent if PROACTIVE.md exists and conditions allow.
async fn execute_proactive_check(
    home_dir: &Path,
    agent_id: &str,
    proactive_states: &tokio::sync::Mutex<HashMap<String, crate::proactive::ProactiveState>>,
    timing_deferrals: &crate::proactive_timing::DeferralLedger,
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

    // ── U1 natural-timing gate (learned rhythm, sessions.db read-only) ──
    //
    // Complements the static config quiet-hours check above with the
    // *learned* per-agent rhythm: historically-dead hours and mid-flow user
    // turns defer the whole check to the next heartbeat tick. Gating BEFORE
    // the LLM spawn also saves the check's token cost while deferred.
    // Nothing is ever dropped: the check re-runs every heartbeat while the
    // deferral is open, and the gate's 6h hard cap forces execution even if
    // the quiet window persists. Kill switch: `[proactive] natural_timing
    // = false` in the global config.toml restores pre-U1 behaviour.
    {
        let channel_filter = if agent_config.proactive.notify_channel.is_empty() {
            None
        } else {
            Some(agent_config.proactive.notify_channel.as_str())
        };
        let key = format!("proactive:{agent_id}");
        if let crate::proactive_timing::SendDecision::Defer { until, reason } =
            crate::proactive_timing::gate_keyed_send(
                home_dir,
                agent_id,
                channel_filter,
                &key,
                timing_deferrals,
                chrono::Utc::now(),
                "proactive-check",
            )
            .await
        {
            info!(
                agent = agent_id,
                until = %until.to_rfc3339(),
                reason = %reason,
                "Proactive check deferred to natural moment"
            );
            return;
        }
    }

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
            evolution_enabled: false,
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

    // ── SOUL.md integrity drift detection (2026-05-20, #5) ──
    //
    // Heartbeat now calls soul_guard::check_soul_integrity per agent every
    // tick. The tests below cover the three observable outcomes:
    //   1. No SOUL.md → silent skip, no audit row, no warn log
    //   2. SOUL.md matches stored hash → silent debug, no audit row
    //   3. SOUL.md differs from stored hash → WARN log + drift audit row
    // We assert via the audit log because that's the operator-visible artefact.

    fn read_audit_rows(home: &Path, tool: &str) -> Vec<serde_json::Value> {
        let path = home.join("tool_calls.jsonl");
        if !path.exists() {
            return vec![];
        }
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("tool_name").and_then(|t| t.as_str()) == Some(tool))
            .collect()
    }

    fn make_test_agent_dir(home: &Path, name: &str, soul: Option<&str>) -> std::path::PathBuf {
        let agent_dir = home.join("agents").join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        if let Some(soul_content) = soul {
            std::fs::write(agent_dir.join("SOUL.md"), soul_content).unwrap();
        }
        agent_dir
    }

    #[tokio::test]
    async fn soul_integrity_check_skips_agent_without_soul() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // No SOUL.md.
        make_test_agent_dir(home, "stub", None);

        check_soul_integrity_with_audit(home, "stub").await;

        let rows = read_audit_rows(home, "_soul_integrity_drift");
        assert!(rows.is_empty(), "agent without SOUL.md must not emit a drift audit row");
    }

    #[tokio::test]
    async fn soul_integrity_check_clean_when_hash_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let soul = "## Identity\n\nI am the test agent.\n";
        let agent_dir = make_test_agent_dir(home, "matchy", Some(soul));
        // Bootstrap soul_guard hash to match current content.
        duduclaw_security::soul_guard::accept_soul_change("matchy", &agent_dir).unwrap();

        check_soul_integrity_with_audit(home, "matchy").await;

        let rows = read_audit_rows(home, "_soul_integrity_drift");
        assert!(
            rows.is_empty(),
            "matching hash must not flag drift; rows={:?}",
            rows
        );
    }

    #[tokio::test]
    async fn soul_integrity_check_emits_audit_on_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let agent_dir = make_test_agent_dir(home, "drifty", Some("## v1\n\noriginal content\n"));
        // Bootstrap hash to v1.
        duduclaw_security::soul_guard::accept_soul_change("drifty", &agent_dir).unwrap();

        // Simulate out-of-band tampering: rewrite SOUL.md without updating the hash.
        std::fs::write(
            agent_dir.join("SOUL.md"),
            "## v2\n\nTAMPERED content — written outside MCP\n",
        )
        .unwrap();

        check_soul_integrity_with_audit(home, "drifty").await;

        let rows = read_audit_rows(home, "_soul_integrity_drift");
        assert_eq!(rows.len(), 1, "one drift audit row expected; got: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.get("agent_id").and_then(|v| v.as_str()), Some("drifty"));
        assert_eq!(row.get("success").and_then(|v| v.as_bool()), Some(false));
        let summary = row.get("params_summary").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            summary.contains("drift:") && summary.contains("current=") && summary.contains("expected="),
            "drift audit summary should include current + expected hash; got: {summary}"
        );
    }
}
