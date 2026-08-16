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

/// Abstraction over the single "call the agent and get its reply" step of a
/// cron task's execution. Extracting this seam lets the whole dispatch path
/// ([`dispatch_cron_task`] → [`execute_cron_task`] → run recording) be
/// unit-tested with a mock in place of the real Claude CLI call, so a test
/// can prove that a **run-now** goes through the exact same code as a
/// scheduled fire. Production always uses [`RealAgentInvoker`].
#[async_trait::async_trait]
pub trait AgentInvoker: Send + Sync {
    async fn invoke(
        &self,
        home_dir: &Path,
        registry: &Arc<RwLock<AgentRegistry>>,
        agent_id: &str,
        prompt: &str,
    ) -> Result<String, String>;
}

/// Production [`AgentInvoker`]: routes through the normal per-agent Claude
/// dispatch (`call_claude_for_agent_with_type`) tagged as a `Cron` request.
/// Zero-sized, so it is cheap to construct per spawned task.
pub struct RealAgentInvoker;

#[async_trait::async_trait]
impl AgentInvoker for RealAgentInvoker {
    async fn invoke(
        &self,
        home_dir: &Path,
        registry: &Arc<RwLock<AgentRegistry>>,
        agent_id: &str,
        prompt: &str,
    ) -> Result<String, String> {
        call_claude_for_agent_with_type(
            home_dir,
            registry,
            agent_id,
            prompt,
            crate::cost_telemetry::RequestType::Cron,
        )
        .await
    }
}

/// Maximum concurrent cron task executions.
const MAX_CONCURRENT_CRON: usize = 4;

/// Baseline reload/fire-check interval. Hot reload via [`CronScheduler::reload_now`]
/// will wake the loop earlier; this interval is the safety net that picks up
/// changes made by *other processes* (e.g. the MCP subprocess writing directly
/// to the shared SQLite DB).
const TICK_INTERVAL_SECS: u64 = 30;

/// Wall-clock unix seconds of the scheduler loop's most recent wake-up
/// (0 = the loop has not started yet). Written on every tick and read by the
/// `/healthz` staleness probe, so a silently-dead scheduler flips the
/// container unhealthy instead of staying "Up (healthy)" forever while every
/// cron task is missed (2026-08 LWM incident,
/// docs/todo/TODO-cron-scheduler-sleep-drift.md).
pub static LAST_TICK_UNIX: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// In-memory representation with parsed schedule and last-run tracking.
struct LiveTask {
    task: CronTaskRow,
    schedule: Schedule,
    /// Parsed `cron_timezone` from the DB row. `None` means UTC (legacy).
    /// An invalid IANA name becomes `None` with a warn-level log at load
    /// time — the task continues to fire in UTC instead of going silent.
    cron_tz: Option<chrono_tz::Tz>,
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

    /// Trigger a **single immediate execution** of the task with `task_id`
    /// ("run now" / "test execution"). The run goes through the exact same
    /// [`dispatch_cron_task`] path a scheduled fire uses — same trigger gate,
    /// same delegation env, same channel delivery, same run recording — so
    /// the outcome lands in the store's run history just like any other fire.
    ///
    /// Works regardless of the task's `enabled` state (so a routine can be
    /// tested before being switched on) and respects the same concurrency
    /// semaphore as scheduled fires. The execution is spawned in the
    /// background and this returns immediately with the task's display name;
    /// the caller (dashboard RPC) polls the run history to see the result.
    /// Returns an error only when the id is unknown or the store is
    /// unreadable.
    pub async fn run_now(&self, task_id: &str) -> Result<String, String> {
        let row = self
            .store
            .get(task_id)
            .await?
            .ok_or_else(|| format!("找不到排程任務：{task_id}"))?;
        let name = row.name.clone();
        let home = self.home_dir.clone();
        let registry = self.registry.clone();
        let store = self.store.clone();
        let sem = self.semaphore.clone();
        tokio::spawn(async move {
            let _permit = sem.acquire().await;
            info!(id = %row.id, name = %row.name, "cron task run-now triggered (manual test execution)");
            dispatch_cron_task(&home, &store, &registry, &row, &RealAgentInvoker).await;
        });
        Ok(name)
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
                    let cron_tz = resolve_task_cron_tz(&task);
                    new_live.push(LiveTask {
                        task,
                        schedule,
                        cron_tz,
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

        LAST_TICK_UNIX.store(Utc::now().timestamp(), std::sync::atomic::Ordering::Relaxed);
        info!("cron scheduler loop started ({TICK_INTERVAL_SECS}s tick)");
        self.reload().await;

        loop {
            // Wake on timer or on explicit reload request — whichever fires first.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS)) => {}
                _ = self.reload_notify.notified() => {
                    info!("cron scheduler hot-reload signal received");
                }
            }
            LAST_TICK_UNIX.store(Utc::now().timestamp(), std::sync::atomic::Ordering::Relaxed);

            self.reload().await;

            // ── WP2.6 P1: scheduler-level daily skill-gap digest ──
            // Rides the existing 30s tick (no new scheduler, mirroring the
            // heartbeat task-board pull precedent). Internally gated: default
            // OFF via `config.toml [skills] gap_digest_enabled`, and a
            // persisted daily cursor makes at most one attempt per 24h.
            {
                let home = self.home_dir.clone();
                tokio::spawn(async move {
                    crate::skill_gap_digest::maybe_send_daily_digest(&home).await;
                });
            }

            let now = Utc::now();
            let mut to_spawn = Vec::new();
            {
                let mut tasks = self.tasks.write().await;
                for lt in tasks.iter_mut() {
                    let should_fire = duduclaw_core::should_fire_in_tz(
                        &lt.schedule,
                        lt.last_run,
                        now,
                        lt.cron_tz,
                    );

                    if should_fire {
                        // Cadence is due. For `time` tasks this fires directly;
                        // for `condition` / `on_exit` tasks the gate runs inside
                        // `dispatch_cron_task`. `last_run` advances regardless of
                        // the gate outcome so the condition is re-evaluated only
                        // at the next scheduled slot (prevents per-tick runaway).
                        info!(
                            id = %lt.task.id,
                            name = %lt.task.name,
                            agent = %lt.task.agent_id,
                            trigger_kind = %lt.task.trigger_kind,
                            "cron task cadence due — dispatching"
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
                    dispatch_cron_task(&home, &store, &registry, &task, &RealAgentInvoker).await;
                });
            }
        }
    }
}

/// Route a due cron task through its trigger gate (G3), then execute it if the
/// gate passes.
///
/// - `time`      → execute unconditionally (legacy behaviour).
/// - `condition` → run the sandboxed condition script; persist any returned
///   state (≤16 KiB); execute only when the script reports `fire:true`,
///   injecting its `message` into the prompt.
/// - `on_exit`   → run the sandboxed watch command; execute only on exit-0.
///
/// Every gate is fail-closed: a misconfiguration or evaluation failure skips
/// execution and logs, it never fires the task.
async fn dispatch_cron_task(
    home_dir: &std::path::Path,
    store: &Arc<CronStore>,
    registry: &Arc<RwLock<AgentRegistry>>,
    task: &CronTaskRow,
    invoker: &dyn AgentInvoker,
) {
    use crate::condition_eval::{evaluate_condition, evaluate_on_exit, TriggerKind};

    // ── P3 runaway guard: cross-process circuit breaker on the cron feedback
    //    path (paper 2607.01641). A misconfigured cron (e.g. `* * * * *` that
    //    re-arms faster than the task completes) or a cron→delegate→cron cycle
    //    is bounded per target agent. Tripped ⇒ skip this fire and log
    //    (fail-visible), never silently; the breaker self-closes after cooldown.
    {
        let cfg = duduclaw_core::DispatchGuardConfig::from_home(home_dir);
        let decision =
            duduclaw_core::dispatch_guard_check(home_dir, "cron", &task.agent_id, &cfg);
        if let duduclaw_core::DispatchGuardDecision::Trip { reason, retry_after_secs } = decision {
            warn!(
                id = %task.id,
                name = %task.name,
                agent = %task.agent_id,
                retry_after_secs,
                "cron 派工斷路器跳閘,跳過本次觸發(防失控迴圈):{reason}"
            );
            return;
        }
    }

    match TriggerKind::from_db(&task.trigger_kind) {
        TriggerKind::Time => {
            execute_cron_task(home_dir, store, registry, task, None, invoker).await;
        }
        TriggerKind::Condition => {
            let script = match task.condition_script.as_deref() {
                Some(s) if !s.trim().is_empty() => s,
                _ => {
                    warn!(
                        id = %task.id,
                        name = %task.name,
                        "condition trigger 缺少 condition_script（fail-closed，跳過）"
                    );
                    return;
                }
            };

            let outcome = evaluate_condition(script, task.condition_state.as_deref()).await;

            // Persist any new state regardless of the fire decision — a script
            // may advance its cursor while choosing not to fire. Oversize state
            // was already rejected by the parser (fail-closed); this writeback
            // is best-effort and never blocks the fire decision.
            if let Some(ref new_state) = outcome.new_state {
                if let Err(e) = store
                    .update_condition_state(&task.id, Some(new_state))
                    .await
                {
                    warn!(id = %task.id, "condition state 回寫失敗（略過）：{e}");
                }
            }

            if outcome.fire {
                info!(id = %task.id, name = %task.name, "condition trigger 觸發，執行任務");
                execute_cron_task(
                    home_dir,
                    store,
                    registry,
                    task,
                    outcome.message.as_deref(),
                    invoker,
                )
                .await;
            } else {
                info!(id = %task.id, name = %task.name, "condition trigger 評估未觸發（跳過）");
            }
        }
        TriggerKind::OnExit => {
            let watch = match task.watch_command.as_deref() {
                Some(s) if !s.trim().is_empty() => s,
                _ => {
                    warn!(
                        id = %task.id,
                        name = %task.name,
                        "on_exit trigger 缺少 watch_command（fail-closed，跳過）"
                    );
                    return;
                }
            };

            if evaluate_on_exit(watch).await {
                info!(id = %task.id, name = %task.name, "on_exit trigger 觸發（監看指令退出 0），執行任務");
                execute_cron_task(home_dir, store, registry, task, None, invoker).await;
            } else {
                info!(id = %task.id, name = %task.name, "on_exit trigger 未觸發（跳過）");
            }
        }
    }
}

/// Execute a cron task by calling the Claude CLI for the target agent, then
/// persist the run outcome to the store.
///
/// `trigger_message` (G3) is optional context from a `condition` script's
/// `message` field; when present it is appended to the prompt so the agent
/// sees why the task fired.
async fn execute_cron_task(
    home_dir: &std::path::Path,
    store: &Arc<CronStore>,
    registry: &Arc<RwLock<AgentRegistry>>,
    task: &CronTaskRow,
    trigger_message: Option<&str>,
    invoker: &dyn AgentInvoker,
) {
    let prompt = match trigger_message {
        Some(m) if !m.trim().is_empty() => {
            format!(
                "[Scheduled Task: {}] {}\n\n[Trigger context] {}",
                task.name, task.task, m
            )
        }
        _ => format!("[Scheduled Task: {}] {}", task.name, task.task),
    };

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

    // v1.8.25: when the task has a notify_channel target, scope REPLY_CHANNEL
    // around the dispatch so that any `send_to_agent` the cron agent makes
    // inherits the channel context. Without this, nested delegations from
    // cron (e.g. a daily-report agent calling send_to_agent("agnes", …))
    // registered no delegation_callback — their replies landed in
    // message_queue.response and were silently dropped at
    // `forward_delegation_response`'s no-callback branch. The cron agent's
    // own top-level response still goes through `deliver_cron_result`
    // (a direct channel POST) as before — this scope only affects nested
    // sub-agent replies.
    let reply_channel_override = cron_reply_channel_string(task);

    // Best-effort typing indicator on the notify target for the duration
    // of the CLI call — cron tasks can run for minutes with no visible
    // sign of life otherwise. Cosmetic only: any resolution failure
    // (no notify target, non-Telegram channel, missing token) yields
    // `None` and does not affect dispatch.
    let typing_guard = build_cron_typing_guard(home_dir, task).await;

    let dispatch_fut = crate::claude_runner::DELEGATION_ENV
        .scope(delegation_env, async {
            invoker
                .invoke(home_dir, registry, &task.agent_id, &prompt)
                .await
        });
    let result = match reply_channel_override {
        Some(rc) => crate::claude_runner::REPLY_CHANNEL.scope(rc, dispatch_fut).await,
        None => dispatch_fut.await,
    };
    drop(typing_guard);

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

    // W3-1 D5: a scheduled result must not land in a conversation a human is
    // running. Dropped rather than deferred: a routine's output is tied to the
    // moment it ran (「今天的待辦」at 09:00), and the next occurrence supersedes
    // it — replaying stale routine output an hour later is noise, not recovery.
    // The run itself already recorded its result, so nothing is lost.
    if crate::takeover::is_target_paused(home_dir, channel, &effective_chat_id) {
        crate::takeover::log_skip("cron.deliver", channel, &effective_chat_id, &task.id);
        return Ok(());
    }

    // WeCom / DingTalk have no single `<channel>_bot_token` — their senders
    // resolve corp/app credentials from global config at send time, so a
    // token-cascade miss must not block delivery. We only verify the channel
    // section is genuinely configured so a missing setup still fails with a
    // clear error instead of a cryptic send failure.
    let token = if crate::channel_sender::sender_self_configures(channel) {
        let marker = crate::channel_sender::self_config_marker_field(channel)
            .expect("self-configuring channel must declare a marker field");
        let present =
            crate::config_crypto::read_encrypted_config_field(home_dir, "channels", marker)
                .await
                .map(|v| !v.is_empty())
                .unwrap_or(false);
        if !present {
            return Err(format!(
                "channel {channel} is not configured (missing `{marker}` in config.toml [channels])"
            ));
        }
        String::new() // factory-built {channel} sender ignores the token
    } else {
        let token = resolve_channel_token(home_dir, &task.agent_id, channel).await;
        if token.is_empty() {
            return Err(format!(
                "no bot token configured for channel {channel} (tried agent {} and global config)",
                task.agent_id
            ));
        }
        token
    };

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

/// Resolve a channel bot token for cron delivery with `reports_to` cascade.
///
/// Order of preference (v1.8.28):
///   1. The agent's own `agent.toml [channels.<ch>]`.
///   2. Walk up the `reports_to` chain until an ancestor with a token is
///      found (cycle-safe, bounded by `MAX_REPORTS_TO_HOPS`).
///   3. Global `config.toml [channels] <ch>_bot_token(_enc)` as last resort.
///
/// Step 2 is new in v1.8.28. It fixes the cron "Discord 401 Unauthorized"
/// loop that hit multi-bot setups: when a cron-fired agent (e.g. a
/// sub-agent under a TL) has no per-agent Discord token, the old cascade
/// fell straight to the global token. If the global token is a different
/// bot from the one that opened the notify thread, Discord returned 401
/// on every chunk. Walking `reports_to` lets the sub-agent inherit the
/// team root's bot token automatically.
async fn resolve_channel_token(home_dir: &Path, agent_id: &str, channel: &str) -> String {
    if let Some(tok) = crate::config_crypto::resolve_agent_channel_token_via_reports_to(
        home_dir, agent_id, channel,
    ).await {
        return tok.expose_owned();
    }

    // Global config fallback — only reached when nobody on the chain has
    // a per-agent token configured. Field names are NOT uniformly
    // `{channel}_bot_token` (LINE's is `line_channel_token`), so consult the
    // canonical mapping first and keep the pattern only as a last resort for
    // channels outside it.
    let field_base = crate::otp_delivery::token_field(channel)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{channel}_bot_token"));
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", &field_base)
        .await
        .unwrap_or_default()
}

/// Best-effort typing indicator for a cron task's notify target.
/// Telegram-only for now (see `channel_typing::typing_guard_for`); returns
/// `None` for every other case (no notify target configured, non-Telegram
/// channel, empty resolved token) without affecting task execution.
async fn build_cron_typing_guard(
    home_dir: &Path,
    task: &CronTaskRow,
) -> Option<crate::channel_typing::TypingGuard> {
    if task.notify_channel.as_deref() != Some("telegram") {
        return None;
    }
    let chat_id = task.notify_chat_id.as_deref()?;
    let token = resolve_channel_token(home_dir, &task.agent_id, "telegram").await;
    crate::channel_typing::typing_guard_for(
        reqwest::Client::new(), "telegram", chat_id, task.notify_thread_id.as_deref(), &token,
    )
}

/// Build the `DUDUCLAW_REPLY_CHANNEL` string from a cron task's notify_*
/// fields, or `None` if the task has no notification target (so the
/// caller should skip the `REPLY_CHANNEL.scope`).
///
/// The grammar matches `mcp.rs::send_to_agent`'s callback parser:
///
///   `<channel_type>:<chat_id>[:<thread_id>]`
///
/// where trailing `thread_id` is only included when present. For Discord
/// threads stored as `notify_chat_id=<thread_id>, notify_thread_id=NULL`
/// (our v1.8.24 UPDATE shape), this emits `discord:<thread_id>` — the
/// MCP parser treats the id as the channel directly, matching Discord's
/// "thread IS a channel" API semantics. For `(chat_id=<channel_id>,
/// thread_id=<thread_id>)` both components are included.
///
/// Introduced in v1.8.25 so nested `send_to_agent` calls inside cron
/// agents register delegation callbacks and their replies get
/// forwarded + session-appended instead of silently dropped.
fn cron_reply_channel_string(task: &CronTaskRow) -> Option<String> {
    let channel = task.notify_channel.as_deref().filter(|s| !s.is_empty())?;
    let chat = task.notify_chat_id.as_deref().filter(|s| !s.is_empty())?;
    match task.notify_thread_id.as_deref().filter(|s| !s.is_empty()) {
        Some(thread) => Some(format!("{channel}:{chat}:{thread}")),
        None => Some(format!("{channel}:{chat}")),
    }
}

/// Parse a task's `cron_timezone` column. Returns `None` for absent /
/// empty values (UTC / legacy behaviour) and for unknown IANA names —
/// the latter also emits a warn log once, at load time, so a typo is
/// visible in the scheduler output without spamming the per-tick loop.
fn resolve_task_cron_tz(task: &CronTaskRow) -> Option<chrono_tz::Tz> {
    let tz_name = task.cron_timezone.as_deref().unwrap_or("").trim();
    if tz_name.is_empty() {
        return None;
    }
    match duduclaw_core::parse_timezone(tz_name) {
        Some(tz) => Some(tz),
        None => {
            warn!(
                id = %task.id,
                name = %task.name,
                cron_timezone = tz_name,
                "Unknown cron_timezone on cron task — falling back to UTC. \
                 Use an IANA name like \"Asia/Taipei\"."
            );
            None
        }
    }
}

/// Normalise a cron expression to 6-field format (with seconds) and
/// translate numeric day-of-week from the Unix crontab convention
/// (`0`/`7` = Sunday, `1-5` = Mon–Fri) to the `cron` crate's Quartz
/// ordinals (`1` = Sunday). Delegates to the shared
/// [`duduclaw_core::cron_tz::normalise_cron`] so the scheduler, heartbeat,
/// and MCP validation all read expressions identically.
pub fn normalise_cron(expr: &str) -> String {
    duduclaw_core::cron_tz::normalise_cron(expr)
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

/// Standalone one-shot execution of a cron task by id **or** name, for
/// callers that do not hold a live [`CronScheduler`] handle — chiefly the
/// out-of-process MCP subprocess (`duduclaw mcp-server`), which shares the
/// same `<home>/cron_tasks.db` but has no in-memory scheduler.
///
/// Builds its own [`CronStore`] and a freshly-scanned [`AgentRegistry`] from
/// `home_dir`, then runs the **same** [`dispatch_cron_task`] path a
/// scheduled fire uses (trigger gate + execute + run recording). Unlike
/// [`CronScheduler::run_now`] this **blocks until the task completes** so the
/// MCP tool can report the outcome inline; the run is bounded by the shared
/// cross-process `dispatch_guard` breaker exactly like a scheduled fire.
///
/// Returns a short human-readable summary of the recorded run on success.
pub async fn run_cron_task_now_standalone(
    home_dir: &Path,
    id_or_name: &str,
) -> Result<String, String> {
    run_cron_task_now_with(home_dir, id_or_name, &RealAgentInvoker).await
}

/// Injectable core of [`run_cron_task_now_standalone`] — parameterised over
/// the [`AgentInvoker`] so tests can substitute a mock for the real CLI.
async fn run_cron_task_now_with(
    home_dir: &Path,
    id_or_name: &str,
    invoker: &dyn AgentInvoker,
) -> Result<String, String> {
    let store = Arc::new(CronStore::open(home_dir)?);

    // Resolve by id first, then by name (mirrors the MCP lookup convention).
    let row = match store.get(id_or_name).await? {
        Some(r) => r,
        None => store
            .get_by_name(id_or_name)
            .await?
            .ok_or_else(|| format!("找不到排程任務：{id_or_name}"))?,
    };

    // Scan the agents directory so the invoker can resolve the target agent's
    // config exactly as the gateway would. Best-effort: a scan error still
    // lets the dispatch attempt proceed (and fail with a clear per-agent
    // error) rather than silently swallowing the run request.
    let mut registry = AgentRegistry::new(home_dir.join("agents"));
    if let Err(e) = registry.scan().await {
        warn!("run_cron_task_now_standalone: agent registry scan failed: {e}");
    }
    let registry = Arc::new(RwLock::new(registry));

    dispatch_cron_task(home_dir, &store, &registry, &row, invoker).await;

    // Re-read the row to report the recorded outcome (dispatch_cron_task may
    // have skipped execution at a fail-closed trigger gate, in which case the
    // run counters are unchanged — surface that honestly).
    let updated = store.get(&row.id).await?.unwrap_or(row);
    let status = updated.last_status.as_deref().unwrap_or("未執行");
    Ok(format!(
        "已觸發排程「{name}」（id: {id}）。狀態：{status}｜累計執行 {runs} 次（失敗 {fails} 次）",
        name = updated.name,
        id = updated.id,
        runs = updated.run_count,
        fails = updated.failure_count,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Records every `(agent_id, prompt)` it is asked to invoke and returns a
    /// canned reply — a stand-in for the real Claude CLI call so the dispatch
    /// path can be exercised deterministically in a test.
    struct RecordingInvoker {
        calls: Arc<StdMutex<Vec<(String, String)>>>,
        reply: String,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl AgentInvoker for RecordingInvoker {
        async fn invoke(
            &self,
            _home_dir: &Path,
            _registry: &Arc<RwLock<AgentRegistry>>,
            agent_id: &str,
            prompt: &str,
        ) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((agent_id.to_string(), prompt.to_string()));
            if self.fail {
                Err("mock failure".to_string())
            } else {
                Ok(self.reply.clone())
            }
        }
    }

    /// run-now goes through the SAME `dispatch_cron_task` path a scheduled
    /// fire uses: the mock invoker receives the canonical
    /// `[Scheduled Task: <name>] <task>` prompt and the run is recorded in
    /// the store (run_count bumped, last_status = success) — proving the
    /// manual trigger reuses the scheduled execution path end-to-end.
    #[tokio::test]
    async fn run_now_dispatch_uses_same_path_and_records_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(CronStore::open(dir.path()).unwrap());
        let row = CronTaskRow::new(
            "rn1".into(),
            "Daily Digest".into(),
            "agnes".into(),
            "0 9 * * *".into(),
            "彙整今日郵件".into(),
        );
        store.insert(&row).await.unwrap();

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let invoker = RecordingInvoker {
            calls: calls.clone(),
            reply: "done".into(),
            fail: false,
        };
        let registry = Arc::new(RwLock::new(AgentRegistry::new(dir.path().join("agents"))));

        dispatch_cron_task(dir.path(), &store, &registry, &row, &invoker).await;

        // The invoker saw exactly one call with the scheduled-fire prompt shape.
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "invoker should be called exactly once");
        assert_eq!(recorded[0].0, "agnes");
        assert_eq!(recorded[0].1, "[Scheduled Task: Daily Digest] 彙整今日郵件");

        // The run was recorded like any scheduled fire.
        let got = store.get("rn1").await.unwrap().unwrap();
        assert_eq!(got.run_count, 1);
        assert_eq!(got.failure_count, 0);
        assert_eq!(got.last_status.as_deref(), Some("success"));
    }

    /// A failing invoke is recorded as a failed run through the same path.
    #[tokio::test]
    async fn run_now_dispatch_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(CronStore::open(dir.path()).unwrap());
        let row = CronTaskRow::new(
            "rn2".into(),
            "Boom".into(),
            "agnes".into(),
            "0 9 * * *".into(),
            "do stuff".into(),
        );
        store.insert(&row).await.unwrap();

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let invoker = RecordingInvoker {
            calls: calls.clone(),
            reply: String::new(),
            fail: true,
        };
        let registry = Arc::new(RwLock::new(AgentRegistry::new(dir.path().join("agents"))));

        dispatch_cron_task(dir.path(), &store, &registry, &row, &invoker).await;

        assert_eq!(calls.lock().unwrap().len(), 1);
        let got = store.get("rn2").await.unwrap().unwrap();
        assert_eq!(got.run_count, 1);
        assert_eq!(got.failure_count, 1);
        assert_eq!(got.last_status.as_deref(), Some("failure"));
    }

    /// The standalone (MCP-facing) entry point resolves by name, runs the same
    /// dispatch path (mock invoker), and reports the recorded outcome. Uses
    /// the injectable core so the real CLI is never spawned in a test.
    #[tokio::test]
    async fn standalone_run_now_resolves_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let store = CronStore::open(dir.path()).unwrap();
        let row = CronTaskRow::new(
            "sr1".into(),
            "Weekly".into(),
            "agnes".into(),
            "0 17 * * 5".into(),
            "週報".into(),
        );
        store.insert(&row).await.unwrap();
        drop(store);

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let invoker = RecordingInvoker {
            calls: calls.clone(),
            reply: "ok".into(),
            fail: false,
        };

        // Unknown id → clear error, no dispatch.
        let err = run_cron_task_now_with(dir.path(), "nope", &invoker)
            .await
            .unwrap_err();
        assert!(err.contains("找不到"), "unexpected error: {err}");
        assert!(calls.lock().unwrap().is_empty());

        // Resolve by name → runs the dispatch path and reports a success summary.
        let summary = run_cron_task_now_with(dir.path(), "Weekly", &invoker)
            .await
            .unwrap();
        assert!(summary.contains("Weekly"), "summary: {summary}");
        assert!(summary.contains("success"), "summary: {summary}");
        assert_eq!(calls.lock().unwrap().len(), 1);
        let store = CronStore::open(dir.path()).unwrap();
        let got = store.get("sr1").await.unwrap().unwrap();
        assert_eq!(got.run_count, 1, "standalone run should record exactly one run");
    }

    fn task_with_notify(channel: Option<&str>, chat: Option<&str>, thread: Option<&str>) -> CronTaskRow {
        let mut row = CronTaskRow::new(
            "test-id".to_string(),
            "test-name".to_string(),
            "agnes".to_string(),
            "0 * * * *".to_string(),
            "do stuff".to_string(),
        );
        row.notify_channel = channel.map(str::to_string);
        row.notify_chat_id = chat.map(str::to_string);
        row.notify_thread_id = thread.map(str::to_string);
        row
    }

    #[test]
    fn reply_channel_none_when_notify_unset() {
        assert_eq!(cron_reply_channel_string(&task_with_notify(None, None, None)), None);
        // Partial — channel but no chat — also None (deliver_cron_result
        // would reject this anyway).
        assert_eq!(cron_reply_channel_string(&task_with_notify(Some("discord"), None, None)), None);
        // Empty strings treated same as missing.
        assert_eq!(cron_reply_channel_string(&task_with_notify(Some(""), Some(""), None)), None);
    }

    #[test]
    fn reply_channel_discord_thread_as_chat_id() {
        // Our v1.8.24 UPDATE shape: thread_id baked into chat_id, separate
        // thread_id column left NULL. Discord's API treats the thread id
        // as a channel id for POST purposes, so this matches what
        // deliver_cron_result already does.
        let task = task_with_notify(Some("discord"), Some("1495935398852038686"), None);
        assert_eq!(
            cron_reply_channel_string(&task).as_deref(),
            Some("discord:1495935398852038686"),
        );
    }

    #[test]
    fn reply_channel_with_separate_thread_id() {
        // Alternative shape: chat_id is parent channel, thread_id is
        // separate. Emits the 3-field form mcp.rs knows how to parse.
        let task = task_with_notify(Some("discord"), Some("parent-channel"), Some("thread-42"));
        assert_eq!(
            cron_reply_channel_string(&task).as_deref(),
            Some("discord:parent-channel:thread-42"),
        );
    }

    #[test]
    fn reply_channel_telegram_without_thread() {
        let task = task_with_notify(Some("telegram"), Some("12345"), None);
        assert_eq!(
            cron_reply_channel_string(&task).as_deref(),
            Some("telegram:12345"),
        );
    }

    #[test]
    fn reply_channel_telegram_with_topic_thread() {
        // Telegram forum topics use a numeric thread_id.
        let task = task_with_notify(Some("telegram"), Some("12345"), Some("6789"));
        assert_eq!(
            cron_reply_channel_string(&task).as_deref(),
            Some("telegram:12345:6789"),
        );
    }

    // ── W3-1 (D5): a routine's result must not land in a conversation a
    //    human has taken over. ──

    fn begin_takeover(home: &Path, channel: &str, chat_id: &str) {
        duduclaw_core::takeover_state::begin(
            home,
            &duduclaw_core::takeover_state::BeginRequest {
                conversation: format!("{channel}:{chat_id}"),
                agent_id: "agnes".into(),
                holder_user_id: "555".into(),
                holder_display: "王小明".into(),
            },
            &duduclaw_core::takeover_state::TakeoverConfig::default(),
            chrono::Utc::now(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn cron_delivery_is_skipped_while_a_human_holds_the_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let task = task_with_notify(Some("telegram"), Some("12345"), None);

        // Baseline: with no takeover and no configured token, delivery fails
        // loudly — that is the signal we are actually reaching the send path.
        let err = deliver_cron_result(dir.path(), &task, "結果").await;
        assert!(err.is_err(), "control: unconfigured delivery must error");

        begin_takeover(dir.path(), "telegram", "12345");
        assert!(
            deliver_cron_result(dir.path(), &task, "結果").await.is_ok(),
            "a held conversation short-circuits before any send attempt"
        );
    }

    #[tokio::test]
    async fn cron_delivery_to_another_conversation_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        begin_takeover(dir.path(), "telegram", "12345");
        let other = task_with_notify(Some("telegram"), Some("99999"), None);
        assert!(
            deliver_cron_result(dir.path(), &other, "結果").await.is_err(),
            "a takeover must not mute unrelated conversations"
        );
    }

    #[tokio::test]
    async fn cron_discord_thread_delivery_respects_a_thread_takeover() {
        let dir = tempfile::tempdir().unwrap();
        // Discord threads are addressed by the thread id, so the takeover is
        // keyed on it — not on the parent channel.
        begin_takeover(dir.path(), "discord", "thread-42");
        let task = task_with_notify(Some("discord"), Some("parent"), Some("thread-42"));
        assert!(deliver_cron_result(dir.path(), &task, "結果").await.is_ok());
    }
}
