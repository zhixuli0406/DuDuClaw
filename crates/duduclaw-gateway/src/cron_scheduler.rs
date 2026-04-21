//! Cron task scheduler — reads tasks from [`CronStore`], evaluates cron
//! expressions, and executes due tasks by calling the Claude CLI for the
//! target agent. Supports **hot reload** via a `tokio::sync::Notify` signal
//! so that dashboard/MCP edits take effect immediately instead of waiting
//! for the next baseline poll.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule;
use tokio::sync::{Notify, RwLock};
use tracing::{info, warn};

use crate::claude_runner::call_claude_for_agent_with_type;
use crate::cron_store::{CronStore, CronTaskRow};
use duduclaw_agent::registry::AgentRegistry;

/// Re-export the DB row type under the historical name so external callers
/// that used `cron_scheduler::CronTask` keep compiling. Prefer
/// [`crate::cron_store::CronTaskRow`] in new code.
pub type CronTask = CronTaskRow;

/// Maximum concurrent cron task executions.
const MAX_CONCURRENT_CRON: usize = 4;

/// Baseline reload/fire-check interval. Hot reload via [`CronScheduler::reload_now`]
/// will wake the loop earlier; this interval is the safety net that picks up
/// changes made by *other processes* (e.g. the MCP subprocess writing directly
/// to the shared SQLite DB).
const TICK_INTERVAL_SECS: u64 = 30;

/// In-memory representation with parsed schedule and last-run tracking.
struct LiveTask {
    task: CronTaskRow,
    schedule: Schedule,
    last_run: Option<chrono::DateTime<Utc>>,
}

/// Cron scheduler that loads tasks from a [`CronStore`] and fires them on time.
pub struct CronScheduler {
    home_dir: PathBuf,
    store: Arc<CronStore>,
    registry: Arc<RwLock<AgentRegistry>>,
    tasks: Arc<RwLock<Vec<LiveTask>>>,
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Fired by [`CronScheduler::reload_now`] to wake the run loop for an
    /// immediate reload. Consumed inside a `tokio::select!`.
    reload_notify: Arc<Notify>,
}

impl CronScheduler {
    pub fn new(
        home_dir: PathBuf,
        store: Arc<CronStore>,
        registry: Arc<RwLock<AgentRegistry>>,
    ) -> Self {
        Self {
            home_dir,
            store,
            registry,
            tasks: Arc::new(RwLock::new(Vec::new())),
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CRON)),
            reload_notify: Arc::new(Notify::new()),
        }
    }

    /// Signal the run loop to reload from the store **immediately**. Safe to
    /// call from any thread; returns instantly and never blocks.
    pub fn reload_now(&self) {
        self.reload_notify.notify_one();
    }

    /// Reload tasks from the store into memory, preserving `last_run` for
    /// tasks that already existed. Tasks with invalid cron expressions are
    /// logged and dropped.
    async fn reload(&self) {
        let raw_tasks = match self.store.list_enabled().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("failed to load cron tasks from store: {e}");
                return;
            }
        };

        let mut live = self.tasks.write().await;

        // Preserve in-memory last_run for tasks that already existed so we
        // don't re-fire the same minute after a reload triggered mid-cycle.
        let old_runs: std::collections::HashMap<String, chrono::DateTime<Utc>> = live
            .iter()
            .filter_map(|lt| lt.last_run.map(|lr| (lt.task.id.clone(), lr)))
            .collect();

        let mut new_live = Vec::with_capacity(raw_tasks.len());
        for task in raw_tasks {
            let expr = normalise_cron(&task.cron);
            match expr.parse::<Schedule>() {
                Ok(schedule) => {
                    // Start from whichever is newer: in-memory cache, or the DB's persisted last_run_at.
                    let db_last_run = task
                        .last_run_at
                        .as_deref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&Utc));
                    let last_run = match (old_runs.get(&task.id).copied(), db_last_run) {
                        (Some(mem), Some(db)) => Some(mem.max(db)),
                        (Some(mem), None) => Some(mem),
                        (None, db) => db,
                    };
                    new_live.push(LiveTask {
                        task,
                        schedule,
                        last_run,
                    });
                }
                Err(e) => {
                    warn!(id = %task.id, cron = %task.cron, "invalid cron expression: {e}");
                }
            }
        }

        info!(count = new_live.len(), "cron tasks loaded");
        *live = new_live;
    }

    /// Start the scheduler loop. Wakes on a 30-second timer OR on
    /// [`Self::reload_now`] (whichever comes first). On every wake:
    ///   1. reload from the store (cheap)
    ///   2. scan for due tasks and spawn them
    pub async fn run(self: Arc<Self>) {
        // One-shot migration from the legacy JSONL file. Idempotent — if the
        // file is absent or already archived, this is a no-op.
        if let Err(e) = self.store.migrate_from_jsonl(&self.home_dir).await {
            warn!("cron JSONL migration failed: {e}");
        }

        self.reload().await;

        loop {
            // Wake on timer or on explicit reload request — whichever fires first.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS)) => {}
                _ = self.reload_notify.notified() => {
                    info!("cron scheduler hot-reload signal received");
                }
            }

            self.reload().await;

            let now = Utc::now();
            let mut to_spawn = Vec::new();
            {
                let mut tasks = self.tasks.write().await;
                for lt in tasks.iter_mut() {
                    let should_fire = match lt.last_run {
                        Some(last) => lt
                            .schedule
                            .after(&last)
                            .next()
                            .map(|next| next <= now)
                            .unwrap_or(false),
                        None => lt
                            .schedule
                            .after(&(now - chrono::Duration::hours(1)))
                            .next()
                            .map(|next| next <= now)
                            .unwrap_or(false),
                    };

                    if should_fire {
                        info!(
                            id = %lt.task.id,
                            name = %lt.task.name,
                            agent = %lt.task.agent_id,
                            "cron task firing"
                        );
                        lt.last_run = Some(now);
                        to_spawn.push(lt.task.clone());
                    }
                }
            } // write lock released

            for task in to_spawn {
                let home = self.home_dir.clone();
                let registry = self.registry.clone();
                let store = self.store.clone();
                let sem = self.semaphore.clone();
                tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    execute_cron_task(&home, &store, &registry, &task).await;
                });
            }
        }
    }
}

/// Execute a cron task by calling the Claude CLI for the target agent, then
/// persist the run outcome to the store.
async fn execute_cron_task(
    home_dir: &std::path::Path,
    store: &Arc<CronStore>,
    registry: &Arc<RwLock<AgentRegistry>>,
    task: &CronTaskRow,
) {
    let prompt = format!("[Scheduled Task: {}] {}", task.name, task.task);

    // Wrap cron execution in DELEGATION_ENV scope so the Claude CLI subprocess
    // receives delegation context. Cron tasks start at depth 0 with origin="cron".
    let mut delegation_env = std::collections::HashMap::new();
    delegation_env.insert(
        duduclaw_core::ENV_DELEGATION_DEPTH.to_string(),
        "0".to_string(),
    );
    delegation_env.insert(
        duduclaw_core::ENV_DELEGATION_ORIGIN.to_string(),
        "cron".to_string(),
    );
    delegation_env.insert(
        duduclaw_core::ENV_DELEGATION_SENDER.to_string(),
        task.agent_id.clone(),
    );

    // Record dispatch time *before* the CLI call — this is the lower
    // bound for action-claim verification below.
    let dispatch_start_time = chrono::Utc::now().to_rfc3339();

    let result = crate::claude_runner::DELEGATION_ENV
        .scope(delegation_env, async {
            call_claude_for_agent_with_type(
                home_dir,
                registry,
                &task.agent_id,
                &prompt,
                crate::cost_telemetry::RequestType::Cron,
            )
            .await
        })
        .await;

    match result {
        Ok(response) => {
            info!(
                id = %task.id,
                name = %task.name,
                response_len = response.len(),
                "cron task completed"
            );

            // ── Channel delivery (v1.8.22, issue #15) ───────────
            // Previously cron results stayed in the DB and the user had to
            // poll the dashboard to see them. When the task row carries a
            // notify_* target, forward the response to that channel so
            // Discord/Telegram/LINE/Slack users receive it automatically.
            if task.has_notify_target() {
                if let Err(e) = deliver_cron_result(home_dir, task, &response).await {
                    warn!(
                        id = %task.id,
                        name = %task.name,
                        channel = task.notify_channel.as_deref().unwrap_or(""),
                        "cron notification delivery failed: {e}"
                    );
                }
            }

            // ── Action-claim verifier (shadow mode) ─────────────
            // Same logic as channel_reply.rs: scan the response text
            // for factual assertions and cross-reference against the
            // MCP tool-call audit log. Cron tasks are particularly
            // vulnerable because (a) no user sees the reply in real
            // time, so fabrications go unchallenged longer, and
            // (b) the current success criterion is just "exit code
            // = 0", which a fabricating agent always satisfies.
            //
            // Shadow mode: log + audit, do not flip record_run to
            // failure. Enforce mode can be enabled later by setting
            // `last_status = "partial"` when hallucinations > 0.
            let hallucinations = duduclaw_security::action_claim_verifier::detect_hallucinations(
                home_dir,
                &task.agent_id,
                &response,
                &dispatch_start_time,
            );
            if !hallucinations.is_empty() {
                warn!(
                    id = %task.id,
                    name = %task.name,
                    agent = %task.agent_id,
                    count = hallucinations.len(),
                    "🚨 cron task produced {} ungrounded claim(s) (shadow mode)",
                    hallucinations.len()
                );
                for h in &hallucinations {
                    if let duduclaw_security::action_claim_verifier::VerifyResult::Hallucination {
                        claim,
                        reason,
                    } = h
                    {
                        warn!(
                            id = %task.id,
                            claim_type = ?claim.claim_type,
                            target = %claim.target_id,
                            matched_text = %claim.matched_text,
                            reason = %reason,
                            "cron ungrounded claim"
                        );
                        duduclaw_security::audit::log_tool_hallucination(
                            home_dir,
                            &task.agent_id,
                            &claim.matched_text,
                            claim.claim_type.expected_tool(),
                        );
                    }
                }
            }

            if let Err(e) = store.record_run(&task.id, true, None).await {
                warn!(id = %task.id, "failed to record successful run: {e}");
            }
        }
        Err(e) => {
            warn!(id = %task.id, name = %task.name, "cron task failed: {e}");
            if let Err(re) = store.record_run(&task.id, false, Some(&e)).await {
                warn!(id = %task.id, "failed to record failed run: {re}");
            }
        }
    }
}

/// Deliver a successful cron task's response text to the configured
/// notification channel. Resolves the bot token via the same cascade used
/// by channel_reply / dispatcher (per-agent token first, then global
/// `config.toml [channels]`), then calls the unified `ChannelSender`.
///
/// The response is clamped to a safe size per platform before sending —
/// Discord's 2000-char message cap is the tightest limit, so we truncate
/// at 3500 *chars* (not bytes) which works on every supported channel and
/// still fits well inside Telegram's 4096-char cap. Longer responses get
/// a `\n[…truncated]` suffix so the user knows there's more in the logs.
async fn deliver_cron_result(
    home_dir: &Path,
    task: &CronTaskRow,
    response: &str,
) -> Result<(), String> {
    let channel = task
        .notify_channel
        .as_deref()
        .ok_or_else(|| "notify_channel missing".to_string())?;
    let chat_id = task
        .notify_chat_id
        .as_deref()
        .ok_or_else(|| "notify_chat_id missing".to_string())?;

    // Discord threads are addressed as the channel_id — forwarding routes
    // a message to the thread by using the thread id as chat_id.
    let effective_chat_id = match (channel, task.notify_thread_id.as_deref()) {
        ("discord", Some(tid)) if !tid.is_empty() => tid.to_string(),
        _ => chat_id.to_string(),
    };

    let token = resolve_channel_token(home_dir, &task.agent_id, channel).await;
    if token.is_empty() {
        return Err(format!(
            "no bot token configured for channel {channel} (tried agent {} and global config)",
            task.agent_id
        ));
    }

    // Clamp by chars (not bytes) — CJK-safe because we already count code
    // points, and it stays under every channel's message size cap.
    const MAX_CHARS: usize = 3500;
    let message = if response.chars().count() > MAX_CHARS {
        let mut s: String = response.chars().take(MAX_CHARS).collect();
        s.push_str("\n[…truncated]");
        s
    } else {
        response.to_string()
    };

    // Prefix a one-line header so the user can tell scheduled messages
    // apart from interactive replies at a glance.
    let body = format!("⏰ [{}] {}", task.name, message);

    let target = crate::channel_sender::ChannelTarget {
        channel_type: channel.to_string(),
        chat_id: effective_chat_id,
        token,
        extra_id: None,
    };
    // `reqwest::Client` is cheap to construct for a per-task send; the
    // cron pipeline fires at most once per minute per task so we don't
    // need a shared pool.
    let http = reqwest::Client::new();
    let sender = crate::channel_sender::create_sender(&target, http);
    sender
        .send_text(&body)
        .await
        .map_err(|e| format!("send_text failed: {e}"))
}

/// Resolve a channel bot token for cron delivery, mirroring the dispatcher
/// cascade: per-agent `agent.toml [channels.<ch>]` → global
/// `config.toml [channels] <ch>_bot_token(_enc)`.
async fn resolve_channel_token(home_dir: &Path, agent_id: &str, channel: &str) -> String {
    // Per-agent encrypted / plaintext first.
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    if let Ok(content) = tokio::fs::read_to_string(&agent_toml).await {
        if let Ok(table) = content.parse::<toml::Value>() {
            if let Some(section) = table
                .get("channels")
                .and_then(|c| c.as_table())
                .and_then(|t| t.get(channel))
                .and_then(|v| v.as_table())
            {
                if let Some(enc) = section.get("bot_token_enc").and_then(|v| v.as_str()) {
                    if !enc.is_empty() {
                        if let Some(plain) = crate::config_crypto::decrypt_value(enc, home_dir) {
                            return plain;
                        }
                    }
                }
                if let Some(plain) = section.get("bot_token").and_then(|v| v.as_str()) {
                    if !plain.is_empty() {
                        return plain.to_string();
                    }
                }
            }
        }
    }

    // Global config fallback.
    let field_base = format!("{channel}_bot_token");
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", &field_base)
        .await
        .unwrap_or_default()
}

/// Normalise a cron expression to 6-field format (with seconds). If the
/// expression has 5 fields, prepend "0" for seconds.
pub fn normalise_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// Start the cron scheduler as a background task. Returns the join handle
/// **and** a shared handle to the scheduler so other components
/// (dashboard handlers, MCP bridge) can call [`CronScheduler::reload_now`].
pub fn start_cron_scheduler(
    home_dir: PathBuf,
    store: Arc<CronStore>,
    registry: Arc<RwLock<AgentRegistry>>,
) -> (tokio::task::JoinHandle<()>, Arc<CronScheduler>) {
    let scheduler = Arc::new(CronScheduler::new(home_dir, store, registry));
    let handle = {
        let sched = scheduler.clone();
        tokio::spawn(async move {
            sched.run().await;
        })
    };
    (handle, scheduler)
}
