use axum::{
    Json, Router,
    extract::{Query, State},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::ConnectInfo,
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use duduclaw_auth::{JwtConfig, UserContext, UserDb};

static WS_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_ws_rate_limit(ip: IpAddr) -> bool {
    let mut map = WS_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    // Cleanup stale entries every time the map grows large
    if map.len() > 1000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 120);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 60 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 30 // max 30 WS connections per minute per IP
}

/// Login attempt rate limiter: max 5 attempts per email per 15 minutes.
static LOGIN_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_login_rate_limit(email: &str) -> bool {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 900);
    }
    let entry = map.entry(email.to_string()).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 900 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 5
}

use crate::auth::AuthManager;
use crate::extension::{GatewayExtension, NullExtension};
use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;

/// Configuration for the WebSocket RPC gateway.
pub struct GatewayConfig {
    /// Bind address (e.g. `"0.0.0.0"`).
    pub bind: String,
    /// Port to listen on.
    pub port: u16,
    /// Optional authentication token.  When `None`, authentication is
    /// disabled.
    pub auth_token: Option<String>,
    /// Path to the DuDuClaw home directory (e.g. `~/.duduclaw`).
    pub home_dir: std::path::PathBuf,
    /// Plugin extension point. Defaults to [`NullExtension`].
    pub extension: Arc<dyn GatewayExtension>,
}

/// Internal shared state for the Axum application.
struct AppState {
    auth: AuthManager,
    handler: MethodHandler,
    tx: broadcast::Sender<String>,
    /// Broadcast channel for real-time events (channel status, etc.) pushed to clients.
    event_tx: broadcast::Sender<String>,
    /// User database for multi-user authentication.
    user_db: Arc<UserDb>,
    /// JWT configuration for token issuance and verification.
    jwt_config: Arc<JwtConfig>,
}

/// Start the WebSocket RPC gateway and block until it shuts down.
pub async fn start_gateway(config: GatewayConfig) -> duduclaw_core::error::Result<()> {
    // Initialise the log broadcast channel (must happen before subscribers connect).
    let log_tx = crate::log::init_log_broadcaster();
    let tx = log_tx;

    let home_dir = config.home_dir.clone();
    let extension = config.extension.clone();

    // ── BUG-2 fix: anchor EvolutionEvents audit log to home_dir, not cwd ──
    //
    // EvolutionEventLogger::from_env() falls back to cwd-relative
    // "data/evolution/events" if neither EVOLUTION_EVENTS_DIR nor DUDUCLAW_HOME
    // is set. When the gateway runs with cwd=$HOME, audit events are silently
    // dropped because the path doesn't exist. We pin both env vars before any
    // emitter is constructed so every component sees the same target.
    {
        let events_dir = home_dir.join("evolution").join("events");
        // SAFETY: process is single-threaded at this point in start_gateway
        // (no other tasks have been spawned yet). Setting env vars here is
        // safe; later threads only read.
        if std::env::var_os("EVOLUTION_EVENTS_DIR").is_none() {
            unsafe { std::env::set_var("EVOLUTION_EVENTS_DIR", &events_dir); }
            info!(
                "EVOLUTION_EVENTS_DIR defaulted to {}",
                events_dir.display()
            );
        }
        if std::env::var_os("DUDUCLAW_HOME").is_none() {
            unsafe { std::env::set_var("DUDUCLAW_HOME", &home_dir); }
        }
        // Run a synchronous-ish self-test so a misconfigured path surfaces at
        // boot rather than after the first prediction error.
        let logger = crate::evolution_events::logger::EvolutionEventLogger::from_env();
        if let Err(e) = logger.self_test().await {
            warn!(
                "EvolutionEvents audit log path {} is not writable: {e} — \
                 audit events will be silently dropped until this is fixed",
                events_dir.display()
            );
        }
    }

    let handler = MethodHandler::with_extension(config.home_dir, extension.clone()).await;

    // Initialize cost telemetry (must happen before any Claude CLI calls)
    if let Err(e) = crate::cost_telemetry::init_telemetry(&home_dir) {
        tracing::warn!(error = %e, "Failed to initialize cost telemetry — continuing without it");
    }

    // Initialize wiki trust store (Phase 2 of wiki RL trust feedback).
    // Best-effort: if open fails, the rest of the system still works — RAG
    // simply falls back to frontmatter trust and trust feedback is skipped.
    {
        let trust_db = home_dir.join("wiki_trust.db");
        let pre_existing = trust_db.exists();

        // Phase 7: read [wiki.trust_feedback] + [wiki.trust_feedback.janitor]
        // from config.toml. Missing/malformed → safe defaults.
        let (trust_cfg, janitor_cfg, federation_cfg) = {
            let raw = std::fs::read_to_string(home_dir.join("config.toml")).unwrap_or_default();
            let table: toml::Table = raw.parse().unwrap_or_default();
            (
                duduclaw_memory::trust_store::TrustStoreConfig::from_toml(&table),
                duduclaw_memory::JanitorConfig::from_toml(&table),
                crate::wiki_trust_federation::FederationConfig::from_toml(&table),
            )
        };

        // R4 DEBT-3: propagate the configured tracker cap to the
        // process-global feedback module before any traffic arrives.
        duduclaw_memory::feedback::set_max_active_conversations(
            trust_cfg.max_active_conversations,
        );
        match duduclaw_memory::trust_store::init_global_trust_store_with_config(
            &trust_db, trust_cfg,
        ) {
            Ok(store) => {
                info!(
                    path = %trust_db.display(),
                    cap = trust_cfg.per_conversation_cap,
                    archive_threshold = trust_cfg.archive_threshold,
                    daily_limit = trust_cfg.daily_signal_limit,
                    "Wiki trust store initialized"
                );

                // Phase 7 migration: on first creation of the trust DB, seed
                // rows from existing wiki frontmatter so `trust_audit` shows
                // a meaningful baseline immediately. Idempotent for re-runs.
                if !pre_existing {
                    let agents_dir = home_dir.join("agents");
                    if agents_dir.exists() {
                        match store.bootstrap_from_wiki(&agents_dir) {
                            Ok((inserted, skipped)) => info!(
                                inserted, skipped,
                                "Wiki trust store bootstrapped from frontmatter"
                            ),
                            Err(e) => warn!(error = %e, "Wiki trust bootstrap failed"),
                        }
                    }
                }

                // Phase 3 / R2-4: restart-aware daily janitor.
                // Reads `last_janitor_run_at` from the trust DB on boot;
                // fires immediately if more than a full interval has elapsed
                // since the last run, otherwise sleeps until the next 24-h
                // boundary. Persists the timestamp after every successful
                // pass so a crash-then-restart cycle never skips retention.
                let agents_dir = home_dir.join("agents");
                let janitor_store = store.clone();
                tokio::spawn(async move {
                    const INTERVAL: std::time::Duration =
                        std::time::Duration::from_secs(24 * 3600);

                    let last_run = janitor_store
                        .meta_get("last_janitor_run_at")
                        .ok()
                        .flatten()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                        .map(|d| d.with_timezone(&chrono::Utc));

                    // If we've never run OR more than one interval has passed,
                    // run immediately.
                    let initial_delay = match last_run {
                        Some(t) => {
                            let elapsed = chrono::Utc::now()
                                .signed_duration_since(t)
                                .to_std()
                                .unwrap_or(INTERVAL);
                            INTERVAL.saturating_sub(elapsed)
                        }
                        None => std::time::Duration::ZERO,
                    };
                    if !initial_delay.is_zero() {
                        tokio::time::sleep(initial_delay).await;
                    }

                    loop {
                        run_wiki_janitor_pass(&agents_dir, &janitor_store, &janitor_cfg);
                        let now_str = chrono::Utc::now().to_rfc3339();
                        if let Err(e) = janitor_store.meta_set("last_janitor_run_at", &now_str) {
                            warn!(error = %e, "failed to persist janitor last-run timestamp");
                        }
                        tokio::time::sleep(INTERVAL).await;
                    }
                });

                // Phase 7: federation transport — periodic export to peers.
                // Skipped silently when no peers configured.
                if !federation_cfg.peers.is_empty() {
                    crate::wiki_trust_federation::spawn_federation_pusher(
                        store.clone(),
                        federation_cfg,
                    );
                }
            }
            Err(e) => warn!(
                path = %trust_db.display(),
                error = %e,
                "Wiki trust store init failed — trust feedback disabled"
            ),
        }
    }

    // ── Initialize user database & JWT ───────────────────────
    let user_db_path = home_dir.join("users.db");
    let user_db = Arc::new(
        UserDb::new(&user_db_path)
            .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to initialize user database: {e}")
            ))?,
    );
    // Ensure a default admin exists on first run
    match user_db.ensure_default_admin() {
        Ok(Some(_password)) => {
            // Password already printed by ensure_default_admin
        }
        Ok(None) => {} // Admin already exists
        Err(e) => {
            // C2 fix: fail hard if we can't create admin — don't silently continue
            return Err(duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to initialize user database: {e}")
            ));
        }
    }
    let jwt_config = Arc::new(
        JwtConfig::load_or_generate(&home_dir)
            .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to initialize JWT: {e}")
            ))?,
    );
    info!("User authentication system initialized");

    // Initialize session manager
    let session_db_path = home_dir.join("sessions.db");
    let session_manager = Arc::new(
        crate::session::SessionManager::new(&session_db_path)
            .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to initialize session manager: {e}")
            ))?,
    );

    // Start periodic session cleanup (every 6 hours, remove sessions older than 72 hours)
    {
        let sm = session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                match sm.cleanup_inactive(72).await {
                    Ok(n) if n > 0 => info!("Cleaned up {} inactive sessions", n),
                    Ok(_) => {}
                    Err(e) => warn!("Session cleanup error: {}", e),
                }
            }
        });
    }

    // ── Cost telemetry: periodic cleanup + adaptive routing ────
    {
        let hd = home_dir.clone();
        tokio::spawn(async move {
            // Wait 10 minutes before first check
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                crate::cost_telemetry::adaptive_routing_check(&hd).await;
            }
        });
    }

    // ── Initialize prediction engine (Phase 1) ────────────────
    // Embedding provider: None for now (Tier 2 vocabulary_novelty fallback).
    // When BGE-small-zh is available at ~/.duduclaw/models/embedding/bge-small-zh/,
    // pass Some(Arc::new(OnnxEmbeddingProvider::load(...))) here.
    let prediction_db_path = home_dir.join("prediction.db");
    let metacognition_path = home_dir.join("metacognition.json");
    let prediction_engine = Arc::new(
        crate::prediction::engine::PredictionEngine::new(
            prediction_db_path,
            Some(metacognition_path.clone()),
        )
    );
    info!("Prediction engine initialized (embedding: none, using vocabulary_novelty fallback)");

    // ── Initialize GVU loop (Phase 2) ────────────────────────
    let gvu_db_path = home_dir.join("evolution.db");
    // Load encryption key for rollback_diff at rest (reuses existing keyfile)
    let gvu_encryption_key = crate::config_crypto::load_keyfile_public(&home_dir);
    let gvu_loop = Arc::new(crate::gvu::loop_::GvuLoop::with_encryption(
        &gvu_db_path,
        None, // observation_hours — will be set per-agent from config
        None, // max_generations — will be set per-agent from config
        gvu_encryption_key.as_ref(),
    ));
    info!("GVU evolution loop initialized (encryption: {})", if gvu_encryption_key.is_some() { "enabled" } else { "disabled" });

    // ── BUG-1 fix: schedule ObservationFinalizer (30 min ticks) ───────────
    // Closes expired SOUL.md observation windows (confirmed / rolled_back /
    // extended). Without this, the very first applied SOUL change blocks all
    // subsequent GVU proposals indefinitely.
    {
        let finalizer = Arc::new(
            crate::gvu::observation_finalizer::ObservationFinalizer::new(
                crate::gvu::version_store::VersionStore::with_crypto(
                    &gvu_db_path,
                    gvu_encryption_key.as_ref(),
                ),
                home_dir.join("prediction.db"),
                home_dir.join("feedback.jsonl"),
                home_dir.join("agents"),
                gvu_encryption_key,
            ),
        );
        tokio::spawn(finalizer.run(std::time::Duration::from_secs(1800)));
        info!("ObservationFinalizer scheduled — 30 min interval");
    }

    // Event broadcast channel for pushing real-time updates (e.g. channel status) to dashboard
    let (event_tx, _) = broadcast::channel::<String>(64);
    handler.set_event_tx(event_tx.clone()).await;

    // Start channel bots if configured
    let reply_ctx = Arc::new(
        crate::channel_reply::ReplyContext::new(
            handler.registry().clone(),
            home_dir.clone(),
            session_manager.clone(),
            handler.channel_status().clone(),
            event_tx.clone(),
        )
        .with_prediction_engine(prediction_engine.clone())
        .with_gvu_loop(gvu_loop.clone())
        .with_memory_db(home_dir.join("memory.db"))
    );
    // Inject reply context into handler for channel hot-start/stop
    handler.set_reply_ctx(reply_ctx.clone()).await;

    // Store background task handles for graceful shutdown (BE-L4)
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Start channel bots — per-agent where supported
    for (label, h) in crate::telegram::start_telegram_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    // Slack: per-agent support ready in slack.rs but module has unresolved
    // dependencies (shared_http_client, url crate). Slack bot started via
    // existing mechanism when those are resolved.
    // for (label, h) in crate::slack::start_slack_bots(&home_dir, reply_ctx.clone()).await {
    //     handler.register_channel_handle(&label, h).await;
    // }
    for (label, h) in crate::discord::start_discord_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    // Webhook channels (LINE, WhatsApp, Feishu) — global only for now
    // Per-agent webhook routing requires multi-path routers (TODO-per-agent-channels.md)
    let line_router = crate::line::start_line_bot(&home_dir, reply_ctx.clone()).await;
    let webchat_ctx = reply_ctx.clone();

    // Start unified heartbeat scheduler (per-agent: evolution + cron + monitoring)
    // Replaces the old start_evolution_timers — each agent's HeartbeatConfig
    // now drives meso/macro reflections at its own interval or cron schedule.
    //
    // BUG-3 fix: wire a SilenceBreakerEvent channel so silence detection in
    // the scheduler turns into a real `silence_breaker` evolution event in
    // prediction.db (gated by a 4h per-agent cool-down).
    let (silence_tx, silence_rx) =
        tokio::sync::mpsc::unbounded_channel::<duduclaw_agent::SilenceBreakerEvent>();
    let heartbeat = duduclaw_agent::heartbeat::start_heartbeat_scheduler_with(
        home_dir.clone(),
        handler.registry().clone(),
        Some(silence_tx),
    );
    handler.set_heartbeat(heartbeat).await;
    info!("Heartbeat scheduler started (per-agent evolution + monitoring)");

    // P1 (2026-05-09): build the GvuTriggerCtx once and share it across the
    // silence-event consumer and the dispatcher so both code paths fire GVU
    // through the same plumbing (loop / notebook / home dir). Constructed
    // before the silence consumer spawn — see #3.3 in
    // commercial/docs/TODO-runtime-health-fixes-202605.md for context.
    let shared_gvu_ctx =
        Arc::new(crate::prediction::subagent_prediction::GvuTriggerCtx {
            gvu_loop: gvu_loop.clone(),
            notebook: Some(Arc::new(
                crate::gvu::mistake_notebook::MistakeNotebook::new(
                    &home_dir.join("evolution.db"),
                ),
            )),
            home_dir: home_dir.clone(),
        });

    // Consume SilenceBreakerEvent → forced reflection event → optional GVU
    {
        let cooldown = Arc::new(
            crate::prediction::forced_reflection::SilenceBreakerCooldown::default_4h(),
        );
        crate::prediction::forced_reflection::spawn_silence_event_consumer(
            silence_rx,
            prediction_engine.clone(),
            cooldown,
            Some(shared_gvu_ctx.clone()),
        );
    }

    // ── Memory decay: archive old entries daily ───────────────
    // Archives entries older than 30 days (low-importance) and permanently
    // deletes archived entries older than 90 days.
    {
        let hd = home_dir.clone();
        tokio::spawn(async move {
            // Wait 5 minutes after startup before first run
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
            let policy = duduclaw_memory::decay::MemoryDecayPolicy {
                archive_after_days: 30,
                delete_after_days: 90,
                ..duduclaw_memory::decay::MemoryDecayPolicy::default()
            };
            loop {
                interval.tick().await;
                let db_path = hd.join("memory.db");
                let p = policy.clone();
                tokio::task::spawn_blocking(move || {
                    let engine = match duduclaw_memory::SqliteMemoryEngine::new(&db_path) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!("Memory decay: failed to open memory.db: {e}");
                            return;
                        }
                    };
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(duduclaw_memory::decay::run_decay(&engine, &p));
                });
            }
        });
    }

    // Start cron scheduler (reads from SQLite cron_tasks.db, fires on schedule)
    let cron_store = Arc::new(
        crate::cron_store::CronStore::open(&home_dir)
            .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to open cron store: {e}")
            ))?,
    );
    handler.set_cron_store(cron_store.clone()).await;

    // Initialize task board store (SQLite tasks.db + activity feed)
    let task_store_opt: Option<Arc<crate::task_store::TaskStore>> =
        match crate::task_store::TaskStore::open(&home_dir) {
            Ok(ts) => {
                let arc = Arc::new(ts);
                handler.set_task_store(arc.clone()).await;
                // Share the same Arc with claude_runner so system-prompt
                // task injection reuses this connection rather than
                // opening a new SQLite handle per agent invocation.
                crate::claude_runner::set_shared_task_store(arc.clone());
                info!("Task board store initialized");
                Some(arc)
            }
            Err(e) => {
                warn!("Failed to open task store: {e}");
                None
            }
        };

    // Initialize autopilot rule store (SQLite autopilot.db)
    let autopilot_store_opt: Option<Arc<crate::autopilot_store::AutopilotStore>> =
        match crate::autopilot_store::AutopilotStore::open(&home_dir) {
            Ok(ap) => {
                let arc = Arc::new(ap);
                handler.set_autopilot_store(arc.clone()).await;
                info!("Autopilot store initialized");
                Some(arc)
            }
            Err(e) => {
                warn!("Failed to open autopilot store: {e}");
                None
            }
        };

    let (cron_handle, cron_scheduler) = crate::cron_scheduler::start_cron_scheduler(
        home_dir.clone(),
        cron_store.clone(),
        handler.registry().clone(),
    );
    handler.set_cron_scheduler(cron_scheduler).await;
    bg_handles.push(cron_handle);
    info!("Cron scheduler started (SQLite-backed with hot reload)");

    // Account health probe — periodically tests unhealthy CLI accounts and restores
    // them by priority when they recover (e.g. rate-limit cooldown expired).
    {
        let probe_interval = std::fs::read_to_string(home_dir.join("config.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| t.get("rotation")?.as_table()?.get("health_check_interval_seconds")?.as_integer())
            .unwrap_or(60) as u64;
        crate::claude_runner::spawn_health_probe(home_dir.clone(), probe_interval);
        info!(interval_secs = probe_interval, "Account health probe started");
    }

    // Ensure every agent has a `.mcp.json` with the duduclaw MCP server entry.
    //
    // Claude CLI in `-p --dangerously-skip-permissions` mode does NOT read
    // global `~/.claude/settings.json` MCP servers — it only reads project-level
    // `.mcp.json` from the working directory. So per-agent `.mcp.json` is required.
    //
    // `ensure_duduclaw_absolute_path()` handles 3 cases:
    // 1. No `.mcp.json` → creates one with the resolved duduclaw binary
    // 2. Relative command → resolves to absolute path
    // 3. Non-existent binary (e.g., stale `duduclaw-pro`) → fixes it
    {
        let agents_dir = home_dir.join("agents");
        let fixed = duduclaw_agent::mcp_template::ensure_mcp_absolute_paths_all(&agents_dir);
        if fixed > 0 {
            info!(count = fixed, "Fixed/created .mcp.json for agent MCP server discovery");
        }
    }

    // Initialize SQLite message queue (Phase 3 Hybrid TaskPipeline)
    let message_queue = match crate::message_queue::MessageQueue::open(&home_dir) {
        Ok(mq) => {
            info!("SQLite message queue initialized");
            Some(std::sync::Arc::new(mq))
        }
        Err(e) => {
            warn!("Failed to open SQLite message queue: {e} — falling back to JSONL only");
            None
        }
    };

    // Start agent dispatcher (consumes bus_queue.jsonl + SQLite queue, spawns sub-agents).
    // Clone the Arc so AutopilotEngine can share the same MessageQueue (delegate action).
    let mq_for_autopilot = message_queue.clone();
    // P1 fix (2026-05-09): reuse the shared GvuTriggerCtx built earlier so
    // dispatcher + silence consumer share the same GvuLoop / MistakeNotebook
    // — keeps post-GVU bookkeeping consistent across the two trigger paths.
    bg_handles.push(crate::dispatcher::start_agent_dispatcher_with_crypto(
        home_dir.clone(),
        handler.registry().clone(),
        None,
        message_queue,
        Some(prediction_engine.clone()),
        Some(shared_gvu_ctx.clone()),
    ));
    info!("Agent dispatcher started ({} background tasks)", bg_handles.len());

    // ── Autopilot trigger engine (Multica-inspired event-driven automation) ──
    // Subscribes to a typed broadcast bus. Events come from:
    //   1) WebSocket handlers (in-process, via `set_autopilot_event_tx`)
    //   2) MCP subprocess (out-of-process) through the SQLite event bus
    //      at `events.db` — replaces the legacy `events.jsonl` file bus.
    if let (Some(ap_store), Some(ts)) = (autopilot_store_opt, task_store_opt.clone()) {
        // Capacity 8192: covers a burst of ~4000 events/hr without
        // dropping under a slow DB. Beyond this, `RecvError::Lagged`
        // surfaces in both the error log and the Activity Feed so the
        // drop isn't silent.
        let (ap_tx, ap_rx) =
            tokio::sync::broadcast::channel::<crate::autopilot_engine::AutopilotEvent>(8192);
        handler.set_autopilot_event_tx(ap_tx.clone()).await;

        // Poll SQLite event bus for events appended by MCP subprocesses.
        match crate::events_store::EventBusStore::open(&home_dir) {
            Ok(bus) => {
                let bus = Arc::new(bus);
                bg_handles.push(crate::autopilot_engine::spawn_events_db_poll(
                    bus,
                    ap_tx.clone(),
                ));
                info!("Event bus (events.db) poll task started");
            }
            Err(e) => {
                warn!("events.db open failed: {e} — MCP-originated events will not reach Autopilot");
            }
        }

        // One-shot cleanup of legacy file bus. Any in-flight events
        // during the upgrade window are lost; this is a one-time cost.
        let _ = tokio::fs::remove_file(home_dir.join("events.jsonl")).await;
        let _ = tokio::fs::remove_file(home_dir.join("events.jsonl.1")).await;

        // Spawn the engine loop
        let engine = crate::autopilot_engine::AutopilotEngine::new(
            home_dir.clone(),
            ap_store,
            ts,
            mq_for_autopilot,
            ap_rx,
        );
        bg_handles.push(tokio::spawn(async move { engine.run().await }));
        info!("Autopilot trigger engine started");
    } else {
        info!("Autopilot engine disabled (missing task or autopilot store)");
    }

    // ── Periodic update check (every 6 hours) — broadcast to dashboard ──
    // Pro edition: auto-download + install + graceful restart (unless disabled).
    // CE edition: notify dashboard only.
    let auto_update = crate::updater::auto_update_enabled(&home_dir);
    {
        let etx = event_tx.clone();
        let home_for_update = home_dir.clone();
        tokio::spawn(async move {
            // First check after 30 seconds (let gateway finish startup)
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            loop {
                match crate::updater::check_update().await {
                    Ok(info) if info.available => {
                        let event = WsFrame::Event {
                            event: "system.update_available".to_string(),
                            payload: serde_json::json!({
                                "available": true,
                                "current_version": info.current_version,
                                "latest_version": info.latest_version,
                                "release_notes": info.release_notes,
                                "published_at": info.published_at,
                                "install_method": info.install_method,
                                "auto_update": auto_update,
                            }),
                            seq: None,
                            state_version: None,
                        };
                        if let Ok(json) = serde_json::to_string(&event) {
                            let _ = etx.send(json);
                        }

                        if auto_update {
                            // Pro auto-update: download, verify, install, restart
                            info!(
                                latest = %info.latest_version,
                                "Auto-update: downloading v{}...",
                                info.latest_version,
                            );

                            // Audit log
                            duduclaw_security::audit::append_audit_event(
                                &home_for_update,
                                &duduclaw_security::audit::AuditEvent::new(
                                    "auto_update_start",
                                    "system",
                                    duduclaw_security::audit::Severity::Info,
                                    serde_json::json!({
                                        "from": info.current_version,
                                        "to": info.latest_version,
                                    }),
                                ),
                            );

                            match crate::updater::apply_update(
                                &info.download_url,
                                &info.checksum_url,
                            ).await {
                                Ok(result) if result.success => {
                                    info!("Auto-update installed v{}", info.latest_version);

                                    // Notify dashboard before restart
                                    let done_event = WsFrame::Event {
                                        event: "system.update_installed".to_string(),
                                        payload: serde_json::json!({
                                            "version": info.latest_version,
                                            "needs_restart": result.needs_restart,
                                            "message": result.message,
                                        }),
                                        seq: None,
                                        state_version: None,
                                    };
                                    if let Ok(json) = serde_json::to_string(&done_event) {
                                        let _ = etx.send(json);
                                    }

                                    duduclaw_security::audit::append_audit_event(
                                        &home_for_update,
                                        &duduclaw_security::audit::AuditEvent::new(
                                            "auto_update_success",
                                            "system",
                                            duduclaw_security::audit::Severity::Info,
                                            serde_json::json!({
                                                "version": info.latest_version,
                                                "needs_restart": result.needs_restart,
                                            }),
                                        ),
                                    );

                                    if result.needs_restart {
                                        // Graceful shutdown after 3s to let WebSocket
                                        // clients receive the notification
                                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                        info!("Auto-update: restarting for v{}", info.latest_version);
                                        duduclaw_core::platform::self_interrupt();
                                    }
                                }
                                Ok(result) => {
                                    // apply_update returned success=false (e.g. Homebrew)
                                    warn!(
                                        msg = %result.message,
                                        "Auto-update skipped"
                                    );
                                }
                                Err(e) => {
                                    warn!(error = %e, "Auto-update failed — will retry next cycle");

                                    duduclaw_security::audit::append_audit_event(
                                        &home_for_update,
                                        &duduclaw_security::audit::AuditEvent::new(
                                            "auto_update_failed",
                                            "system",
                                            duduclaw_security::audit::Severity::Warning,
                                            serde_json::json!({
                                                "target_version": info.latest_version,
                                                "error": e.replace('\n', " "),
                                            }),
                                        ),
                                    );
                                }
                            }
                        } else {
                            info!(
                                latest = %info.latest_version,
                                "New version available — notified dashboard clients"
                            );
                        }
                    }
                    Ok(_) => { /* up to date, no broadcast */ }
                    Err(e) => {
                        tracing::debug!(error = %e, "Periodic update check failed (will retry)");
                    }
                }
                // Check every 6 hours
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
        info!(
            auto_update,
            "Periodic update checker started (every 6h, auto_update={})",
            auto_update,
        );
    }

    // Start reminder scheduler (time-wheel based, 10s disk polling for cross-process pickup)
    bg_handles.push(crate::reminder_scheduler::start_reminder_scheduler(
        home_dir.clone(),
        handler.registry().clone(),
    ));
    info!("Reminder scheduler started");

    // #13 (2026-05-12): async session summarizer task.
    // Every 10 min, scan sessions that have ≥ 10 new turns since their
    // last summary (or never summarized) and run Haiku to fold the older
    // turns into a bullet summary. channel_reply reads this summary in
    // lieu of the verbatim slice, keeping the hot conversation context tight.
    bg_handles.push(crate::session_summarizer_task::spawn_summarizer(
        session_manager.clone(),
        home_dir.clone(),
        crate::session_summarizer::SummarizeParams::default(),
    ));
    info!("Session summarizer task started (10-min cadence)");

    // Inject user_db into handler for user management RPC methods
    handler.set_user_db(user_db.clone(), jwt_config.clone()).await;

    let state = Arc::new(AppState {
        auth: AuthManager::new(config.auth_token),
        handler,
        tx,
        event_tx,
        user_db,
        jwt_config,
    });

    // WebChat endpoint — separate state from main /ws (different auth model)
    let webchat_state = Arc::new(crate::webchat::WebChatState::new(webchat_ctx));
    let webchat_router = Router::new()
        .route("/ws/chat", get(crate::webchat::ws_chat_handler))
        .with_state(webchat_state);

    // ── REST API endpoints for authentication ────────────────
    let auth_router = Router::new()
        .route("/api/login", post(handle_login))
        .route("/api/refresh", post(handle_refresh))
        .route("/api/me", get(handle_me))
        .with_state(state.clone());

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(crate::metrics::metrics_handler))
        .route("/api/mcp/oauth/callback", get(handle_mcp_oauth_callback))
        .route("/api/reliability/summary", get(handle_reliability_summary_http))
        .with_state(state)
        .merge(auth_router)
        .merge(webchat_router);

    // Wiki trust federation inbound endpoint — only mounted when the trust
    // store is initialised. Fails closed by returning 503 from a stub when
    // not initialised, so peers get a clear error instead of a 404.
    //
    // CRITICAL (review C2): the federation route lives outside auth_router
    // (peers don't have user JWTs), so it must enforce its own body size
    // limit. 1 MiB caps the JSON body well before any reasonable batch
    // bumps against MAX_FEDERATION_UPDATES_PER_PUSH (5k × ~150 bytes).
    if let Some(store) = duduclaw_memory::trust_store::global_trust_store() {
        let federation_state = crate::wiki_trust_federation::FederationServerState {
            store,
            shared_secret: {
                let raw = std::fs::read_to_string(home_dir.join("config.toml")).unwrap_or_default();
                let table: toml::Table = raw.parse().unwrap_or_default();
                crate::wiki_trust_federation::FederationConfig::from_toml(&table).shared_secret
            },
        };
        app = app.merge(
            Router::new()
                .route(
                    "/api/v1/wiki_trust/federation",
                    post(crate::wiki_trust_federation::handle_federation_push)
                        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)),
                )
                .with_state(federation_state),
        );
    }

    // ── .well-known endpoints for protocol discovery ──────────────
    app = app
        .route("/.well-known/mcp-server.json", get(well_known_mcp_server_card))
        .route("/.well-known/agent.json", get(well_known_agent_card));

    // Mount LINE webhook endpoint
    if let Some(line) = line_router {
        app = app.merge(line);
    }

    // Merge plugin extension routes (if any)
    if let Some(extra) = extension.extra_routes() {
        app = app.merge(extra);
    }

    #[cfg(feature = "dashboard")]
    {
        app = app.merge(duduclaw_dashboard::dashboard_router());
    }

    let app = app;

    let addr = format!("{}:{}", config.bind, config.port);
    info!("Gateway starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    // Serve with graceful shutdown on Ctrl+C
    let pe_for_shutdown = prediction_engine.clone();
    let meta_path_for_shutdown = metacognition_path.clone();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received, flushing state...");
            pe_for_shutdown.flush_all().await;
            pe_for_shutdown.persist_metacognition(&meta_path_for_shutdown).await;
            info!("Prediction engine state flushed");
        })
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    Ok(())
}

// ── REST Auth Handlers ───────────────────────────────────────

#[derive(serde::Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(serde::Serialize)]
struct LoginResponse {
    access_token: String,
    refresh_token: String,
    user: duduclaw_auth::User,
}

#[derive(serde::Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(serde::Serialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    error: String,
}

/// POST /api/login — Authenticate with email + password, return JWT tokens.
async fn handle_login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    // Rate limit login attempts
    if !check_login_rate_limit(&body.email) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many login attempts, try again in 15 minutes"})),
        ).into_response();
    }

    // Verify credentials
    let user = match state.user_db.verify_password(&body.email, &body.password) {
        Ok(u) => u,
        Err(e) => {
            warn!(email = %body.email, "Login failed: {e}");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid email or password"})),
            ).into_response();
        }
    };

    // Get agent bindings for this user
    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    // Issue tokens
    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            ).into_response();
        }
    };

    let refresh_token = match state.jwt_config.issue_refresh_token(&user.id) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue refresh token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            ).into_response();
        }
    };

    // Update last login
    let _ = state.user_db.update_last_login(&user.id);

    // Audit log
    let _ = state.user_db.log_action(
        Some(&user.id),
        "login",
        None,
        None,
        None,
    );

    Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "user": user,
    })).into_response()
}

/// Iterate every agent's wiki under `agents_dir` and run the Phase 3
/// janitor (auto-correct, archive, snapshot sync). Best-effort — failures
/// are logged and the loop continues.
fn run_wiki_janitor_pass(
    agents_dir: &std::path::Path,
    store: &Arc<duduclaw_memory::WikiTrustStore>,
    janitor_cfg: &duduclaw_memory::JanitorConfig,
) {
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %agents_dir.display(), error = %e, "wiki janitor: agents dir unreadable");
            return;
        }
    };
    let janitor = duduclaw_memory::WikiJanitor::with_config(store.clone(), *janitor_cfg);

    // (review HIGH-DB N3) Run global retention pruning ONCE per cycle, not
    // per agent. Doing it per agent meant the pruning budget was multiplied
    // by agent count, and rate / conv_cap deletes did the same work N times.
    match janitor.run_global_retention() {
        Ok((h, r, c)) => info!(
            history_pruned = h,
            rate_pruned = r,
            conv_cap_pruned = c,
            "wiki trust retention pruned"
        ),
        Err(e) => warn!(error = %e, "wiki trust retention pruning failed"),
    }

    for entry in entries.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        let agent_id = match agent_dir.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let wiki_dir = agent_dir.join("wiki");
        if !wiki_dir.exists() {
            continue;
        }
        let report = janitor.run_once(&wiki_dir, &agent_id);
        if !report.corrected_pages.is_empty()
            || !report.archived_pages.is_empty()
            || report.snapshot_synced > 0
        {
            info!(
                agent = %agent_id,
                corrected = report.corrected_pages.len(),
                archived = report.archived_pages.len(),
                snapshots = report.snapshot_synced,
                "wiki janitor pass produced changes"
            );
        }
    }
}

/// Refresh endpoint rate limiter: max 10 per IP per 5 minutes (H9 fix).
static REFRESH_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_refresh_rate_limit(ip: IpAddr) -> bool {
    let mut map = REFRESH_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 300);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 300 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 10
}

/// POST /api/refresh — Exchange a refresh token for a new access token.
async fn handle_refresh(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<RefreshRequest>,
) -> impl IntoResponse {
    // H9 fix: rate limit refresh endpoint
    if !check_refresh_rate_limit(addr.ip()) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many refresh attempts"})),
        ).into_response();
    }

    // Verify refresh token — generic error messages to prevent info leakage
    let claims = match state.jwt_config.verify_refresh_token(&body.refresh_token) {
        Ok(c) => c,
        Err(_) => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired refresh token"})),
            ).into_response();
        }
    };

    // Fetch fresh user data and bindings
    let user = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) if u.status == duduclaw_auth::UserStatus::Active => u,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "user not found or inactive"})),
            ).into_response();
        }
    };

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();
    let agent_access: Vec<(String, duduclaw_auth::AccessLevel)> = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect();

    let access_token = match state.jwt_config.issue_access_token(&user, &agent_access) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue access token: {e}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "token generation failed"})),
            ).into_response();
        }
    };

    Json(serde_json::json!({"access_token": access_token})).into_response()
}

/// GET /api/me — Return the current user's info from the Authorization header.
async fn handle_me(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing Authorization header"})),
            ).into_response();
        }
    };

    let claims = match state.jwt_config.verify_access_token(token) {
        Ok(c) => c,
        _ => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid or expired token"})),
            ).into_response();
        }
    };

    let user = match state.user_db.get_user(&claims.sub) {
        Ok(Some(u)) => u,
        _ => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "user not found"})),
            ).into_response();
        }
    };

    let bindings = state.user_db.get_user_agents(&user.id).unwrap_or_default();

    Json(serde_json::json!({
        "user": user,
        "bindings": bindings,
    })).into_response()
}

/// Extract Bearer token from Authorization header.
fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

// ── WebSocket Handlers ───────────────────────────────────────

/// Axum handler that upgrades HTTP to WebSocket.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    // Rate limit: max 30 WS connections per minute per IP.
    if !check_ws_rate_limit(addr.ip()) {
        warn!(ip = %addr.ip(), "WebSocket connection rejected: rate limit exceeded");
        return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // Validate Origin header to prevent cross-site WebSocket hijacking.
    // Non-browser clients (curl, SDK) don't send Origin, so absent is OK.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let is_safe = origin.starts_with("http://127.0.0.1")
            || origin.starts_with("http://localhost")
            || origin.starts_with("https://127.0.0.1")
            || origin.starts_with("https://localhost");
        if !is_safe {
            warn!(origin, "WebSocket connection rejected: invalid origin");
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.max_message_size(1024 * 1024) // 1MB max WebSocket message
      .on_upgrade(move |socket| handle_socket(socket, state))
}

/// Process a single WebSocket connection.
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    info!("New WebSocket connection established");

    // --- Authentication gate ---
    // Resolve a UserContext from the first "connect" message.
    // Supports 3 modes:
    //   1. JWT token: { "method": "connect", "params": { "jwt": "..." } }
    //   2. Legacy token: { "method": "connect", "params": { "token": "..." } }
    //   3. Ed25519 challenge-response (existing flow)
    //   4. No auth configured: admin fallback

    let user_ctx: UserContext = if state.auth.is_auth_required() || has_users(&state.user_db) {
        // Timeout auth handshake to prevent Slowloris-style resource exhaustion (BE-C4)
        let auth_timeout = std::time::Duration::from_secs(10);
        let result = match tokio::time::timeout(auth_timeout, socket.recv()).await {
            Err(_) => {
                warn!("WebSocket auth timeout — closing connection");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
            Ok(recv_result) => match recv_result {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<WsFrame>(&text) {
                    Ok(WsFrame::Request { id, method, params }) if method == "connect" => {
                        // ── JWT authentication (new) ─────────────────────
                        if let Some(jwt_str) = params.get("jwt").and_then(|v| v.as_str()) {
                            match authenticate_jwt(&state, jwt_str) {
                                Ok(ctx) => {
                                    let ok = WsFrame::ok_response(
                                        &id,
                                        serde_json::json!({
                                            "status": "authenticated",
                                            "user": {
                                                "id": ctx.user_id,
                                                "email": ctx.email,
                                                "role": ctx.role.to_string(),
                                            }
                                        }),
                                    );
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                    )).await;
                                    Ok(ctx)
                                }
                                Err(e) => {
                                    let err = WsFrame::error_response(&id, &format!("JWT authentication failed: {e}"));
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                    )).await;
                                    Err(())
                                }
                            }
                        }
                        // ── Ed25519 challenge-response ──────────────────────
                        else if state.auth.is_ed25519() {
                            let challenge = state.auth.issue_challenge();
                            let resp = WsFrame::ok_response(
                                &id,
                                serde_json::json!({ "challenge": challenge }),
                            );
                            let _ = socket.send(Message::Text(
                                serde_json::to_string(&resp).unwrap_or_default().into(),
                            )).await;

                            // Wait for the `authenticate` message (with timeout)
                            match tokio::time::timeout(auth_timeout, socket.recv()).await.unwrap_or(None) {
                                Some(Ok(Message::Text(auth_text))) => {
                                    match serde_json::from_str::<WsFrame>(&auth_text) {
                                        Ok(WsFrame::Request { id: auth_id, method: auth_method, params: auth_params })
                                            if auth_method == "authenticate" =>
                                        {
                                            let sig = auth_params
                                                .get("signature")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            match state.auth.verify_ed25519(sig) {
                                                Ok(()) => {
                                                    let ok = WsFrame::ok_response(
                                                        &auth_id,
                                                        serde_json::json!({"status": "authenticated"}),
                                                    );
                                                    let _ = socket.send(Message::Text(
                                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                                    )).await;
                                                    // Ed25519 users get admin context (backward compat)
                                                    Ok(UserContext::admin_fallback())
                                                }
                                                Err(_) => {
                                                    let err = WsFrame::error_response(&auth_id, "Ed25519 authentication failed");
                                                    let _ = socket.send(Message::Text(
                                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                                    )).await;
                                                    Err(())
                                                }
                                            }
                                        }
                                        _ => {
                                            let err = WsFrame::error_response("", "expected authenticate message");
                                            let _ = socket.send(Message::Text(
                                                serde_json::to_string(&err).unwrap_or_default().into(),
                                            )).await;
                                            Err(())
                                        }
                                    }
                                }
                                _ => Err(()),
                            }
                        }
                        // ── Legacy token authentication ────────────────────
                        else if state.auth.is_auth_required() {
                            let token = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
                            match state.auth.validate(token) {
                                Ok(()) => {
                                    let ok = WsFrame::ok_response(
                                        &id,
                                        serde_json::json!({"status": "authenticated"}),
                                    );
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                    )).await;
                                    // Legacy token users get admin context (backward compat)
                                    Ok(UserContext::admin_fallback())
                                }
                                Err(_) => {
                                    let err = WsFrame::error_response(&id, "authentication failed");
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                    )).await;
                                    Err(())
                                }
                            }
                        }
                        // ── User DB exists but no legacy auth — require JWT ──
                        else {
                            let err = WsFrame::error_response(&id, "authentication required — provide jwt parameter");
                            let _ = socket.send(Message::Text(
                                serde_json::to_string(&err).unwrap_or_default().into(),
                            )).await;
                            Err(())
                        }
                    }
                    _ => {
                        let err = WsFrame::error_response("", "expected connect message");
                        let _ = socket.send(Message::Text(
                            serde_json::to_string(&err).unwrap_or_default().into(),
                        )).await;
                        Err(())
                    }
                }
            }
            _ => Err(()),
        } // match recv_result
        }; // match tokio::time::timeout

        match result {
            Ok(ctx) => ctx,
            Err(()) => {
                warn!("WebSocket auth failed – closing connection");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    } else {
        // No auth required and no users in DB — admin fallback (local-only dashboard)
        UserContext::admin_fallback()
    };

    info!(user = %user_ctx.email, role = %user_ctx.role, "WebSocket authenticated");

    // Split the socket so we can drive sending and receiving concurrently.
    let (mut sink, mut stream) = socket.split();
    let mut log_rx = state.tx.subscribe();
    let mut event_rx = state.event_tx.subscribe();
    let mut logs_subscribed = false;

    // Heartbeat: send ping every 30s, close if no pong in 60s
    let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut last_pong = std::time::Instant::now();

    loop {
        tokio::select! {
            // ── Heartbeat ping ─────────────────────────────
            _ = heartbeat_interval.tick() => {
                if last_pong.elapsed().as_secs() > 60 {
                    warn!("Dashboard WebSocket heartbeat timeout");
                    break;
                }
                if sink.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            // ── Incoming WebSocket frames ───────────────────
            msg_opt = stream.next() => {
                let msg = match msg_opt {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => { warn!("WebSocket receive error: {e}"); break; }
                    None => break,
                };

                #[allow(clippy::collapsible_match)]
                match msg {
                    Message::Text(text) => {
                        let frame = match serde_json::from_str::<WsFrame>(&text) {
                            Ok(f) => f,
                            Err(e) => {
                                error!("Failed to parse WsFrame: {e}");
                                let err_resp = WsFrame::error_response("", "invalid frame");
                                let resp_text = serde_json::to_string(&err_resp).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
                                continue;
                            }
                        };

                        match frame {
                            WsFrame::Request { id, method, params } => {
                                // Track log subscription state
                                if method == "logs.subscribe" {
                                    logs_subscribed = true;
                                } else if method == "logs.unsubscribe" {
                                    logs_subscribed = false;
                                }

                                let mut response = state.handler.handle(&method, params, &user_ctx).await;
                                if let WsFrame::Response { id: ref mut resp_id, .. } = response {
                                    *resp_id = id;
                                }
                                let resp_text = serde_json::to_string(&response).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
                            }
                            other => { warn!("Received non-request frame: {:?}", other); }
                        }
                    }
                    Message::Close(_) => { info!("WebSocket connection closed by client"); break; }
                    Message::Ping(data) => {
                        if sink.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    Message::Pong(_) => {
                        last_pong = std::time::Instant::now();
                    }
                    _ => {}
                }
            }

            // ── Outbound log broadcast (only when subscribed) ─
            log_line = log_rx.recv(), if logs_subscribed => {
                match log_line {
                    Ok(line) => {
                        // Send as WsFrame::Event so the frontend can parse it uniformly
                        let data = serde_json::from_str::<serde_json::Value>(&line)
                            .unwrap_or(serde_json::Value::String(line));
                        let push = WsFrame::Event {
                            event: "logs.entry".to_string(),
                            payload: data,
                            seq: None,
                            state_version: None,
                        };
                        let text = serde_json::to_string(&push).unwrap_or_default();
                        if sink.send(Message::Text(text.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }

            // ── Outbound event broadcast (always active for authenticated clients) ─
            event_line = event_rx.recv() => {
                match event_line {
                    Ok(json) => {
                        // Events are already serialized as WsFrame::Event JSON
                        if sink.send(Message::Text(json.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }
        }
    }

    info!("WebSocket connection terminated");
}

/// Verify a JWT access token and build a UserContext.
/// Single DB lookup, fail-closed on error (R2 fix for double-lookup + fail-open).
fn authenticate_jwt(state: &AppState, jwt_str: &str) -> Result<UserContext, String> {
    let claims = state.jwt_config.verify_access_token(jwt_str)?;

    // Single DB lookup — fail-closed: DB error = reject
    match state.user_db.get_user(&claims.sub) {
        Ok(Some(user)) if user.status == duduclaw_auth::UserStatus::Active => {}
        Ok(Some(_)) => return Err("account is suspended or offboarded".to_string()),
        Ok(None) => return Err("user not found".to_string()),
        Err(_) => return Err("authentication service unavailable".to_string()),
    }

    UserContext::from_claims(&claims)
}

/// Check if any users exist in the database (to decide whether auth is needed).
/// Fail-closed: if the DB query fails, assume users exist and require auth (C2 fix).
fn has_users(user_db: &UserDb) -> bool {
    user_db.list_users().map(|u| !u.is_empty()).unwrap_or(true)
}

/// Simple health-check endpoint.
async fn health_handler() -> &'static str {
    "ok"
}

// ── Reliability Dashboard HTTP endpoint (W20-P0) ─────────────

/// Query parameters for `GET /api/reliability/summary`.
#[derive(serde::Deserialize, Debug)]
struct ReliabilitySummaryParams {
    /// Agent ID to compute the summary for (required).
    agent_id: Option<String>,
    /// Measurement window in days (1–365, default 7).
    window_days: Option<u32>,
}

/// GET /api/reliability/summary — Agent Reliability Dashboard Phase 1.
///
/// Returns a JSON object with four reliability metrics for the requested agent
/// over a configurable time window backed by the EvolutionEvent audit trail.
///
/// **Authorization**: Not guarded at the HTTP layer (read-only, no sensitive
/// mutation). Sensitive audit queries require the MCP `reliability_summary`
/// tool which enforces Admin scope.
///
/// ## Query parameters
/// - `agent_id` (required) — Agent to query.
/// - `window_days` (optional, 1–365, default 7) — Measurement window.
///
/// ## Example
/// ```text
/// curl "http://localhost:8080/api/reliability/summary?agent_id=my-agent&window_days=7"
/// ```
async fn handle_reliability_summary_http(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ReliabilitySummaryParams>,
) -> impl IntoResponse {
    use crate::evolution_events::query::AuditEventIndex;

    let agent_id = match params.agent_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => {
            return Json(serde_json::json!({
                "error": "agent_id query parameter is required"
            }))
            .into_response();
        }
    };

    let window_days = params
        .window_days
        .unwrap_or(7)
        .clamp(1, 365);

    let home_dir = state.handler.home_dir().to_owned();

    let idx = match AuditEventIndex::open(&home_dir) {
        Ok(i) => i,
        Err(e) => {
            warn!("GET /api/reliability/summary: index open failed: {e}");
            return Json(serde_json::json!({
                "error": format!("audit index unavailable: {e}")
            }))
            .into_response();
        }
    };

    if let Err(e) = idx.sync_from_files().await {
        warn!("GET /api/reliability/summary: sync warning (stale index): {e}");
    }

    match idx.compute_reliability_summary(&agent_id, window_days).await {
        Ok(s) => Json(serde_json::json!({
            "agent_id":              s.agent_id,
            "window_days":           s.window_days,
            "consistency_score":     s.consistency_score,
            "task_success_rate":     s.task_success_rate,
            "skill_adoption_rate":   s.skill_adoption_rate,
            "fallback_trigger_rate": s.fallback_trigger_rate,
            "total_events":          s.total_events,
            "generated_at":          s.generated_at,
        }))
        .into_response(),
        Err(e) => {
            warn!("GET /api/reliability/summary: compute failed: {e}");
            Json(serde_json::json!({
                "error": format!("reliability computation failed: {e}")
            }))
            .into_response()
        }
    }
}

// ── MCP OAuth callback endpoint ─────────────────────────────

/// Query parameters from the OAuth provider redirect.
#[derive(serde::Deserialize)]
struct OAuthCallbackParams {
    code: String,
    state: String,
}

/// GET /api/mcp/oauth/callback — Handles the OAuth redirect from the provider.
async fn handle_mcp_oauth_callback(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<OAuthCallbackParams>,
) -> impl IntoResponse {
    // Look up the pending OAuth flow by state nonce
    let pending = {
        let mut map = state.handler.mcp_oauth_pending().write().await;
        crate::mcp_oauth::cleanup_pending(&mut map);
        map.remove(&params.state)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            warn!("MCP OAuth callback with unknown state parameter");
            return axum::response::Html(
                "<html><body><h2>Authentication failed</h2>\
                 <p>Unknown or expired OAuth state. Please try again from the dashboard.</p>\
                 </body></html>"
                    .to_string(),
            );
        }
    };

    // Exchange the authorization code for tokens
    let token = match crate::mcp_oauth::exchange_code(
        &pending.config,
        &params.code,
        &pending.code_verifier,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            warn!(provider = %pending.provider_id, error = %e, "MCP OAuth token exchange failed");
            return axum::response::Html(format!(
                "<html><body><h2>Authentication failed</h2>\
                 <p>Token exchange error: {e}</p>\
                 <p>Please close this window and try again.</p>\
                 </body></html>"
            ));
        }
    };

    // Save the token to disk
    let home_dir = state.handler.home_dir();
    if let Err(e) = crate::mcp_oauth::upsert_token(home_dir, token) {
        warn!(error = %e, "Failed to save MCP OAuth token");
        return axum::response::Html(format!(
            "<html><body><h2>Authentication failed</h2>\
             <p>Failed to save token: {e}</p>\
             </body></html>"
        ));
    }

    info!(provider = %pending.provider_id, "MCP OAuth authentication successful");

    axum::response::Html(
        "<html><body style=\"font-family: system-ui, sans-serif; display: flex; \
         justify-content: center; align-items: center; height: 100vh; margin: 0; \
         background: #fafaf9;\">\
         <div style=\"text-align: center;\">\
         <h2 style=\"color: #1c1917;\">Authentication Successful</h2>\
         <p style=\"color: #78716c;\">You can close this window and return to the dashboard.</p>\
         </div></body></html>"
            .to_string(),
    )
}

// ── .well-known endpoints for protocol discovery ──────────────

async fn well_known_mcp_server_card() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": "DuDuClaw MCP Server",
        "version": crate::updater::current_version(),
        "description": "Claude Code extension layer with channel routing, memory, agent orchestration, and local inference",
        "tools": [
            {"name": "send_message", "description": "Send message to channel"},
            {"name": "memory_search", "description": "Search agent memory"},
            {"name": "memory_store", "description": "Store memory entry"},
            {"name": "execute_program", "description": "Execute PTC script"},
            {"name": "skill_bank_search", "description": "Search skill bank"},
            {"name": "session_restore_context", "description": "Restore hidden context"},
            {"name": "create_agent", "description": "Create sub-agent"},
            {"name": "send_to_agent", "description": "Delegate to agent"},
        ],
        "capabilities": ["memory", "agents", "channels", "inference", "skills", "evolution"],
    }))
}

async fn well_known_agent_card() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name": "DuDuClaw Agent",
        "description": "AI agent with channel routing, memory, and self-evolution",
        "url": format!("http://localhost:{}", std::env::var("DUDUCLAW_PORT").unwrap_or_else(|_| "3000".to_string())),
        "version": crate::updater::current_version(),
        "capabilities": {
            "streaming": true,
            "multi_turn": true,
            "tool_use": true,
        },
        "skills": [
            {"name": "chat", "description": "Multi-turn conversation", "tags": ["conversation"]},
            {"name": "channel_messaging", "description": "Telegram/LINE/Discord messaging", "tags": ["messaging"]},
            {"name": "memory", "description": "Search and store memories", "tags": ["memory"]},
        ],
    }))
}
