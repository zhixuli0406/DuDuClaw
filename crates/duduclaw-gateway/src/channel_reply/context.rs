use super::*;

/// Lightweight sub-agent descriptor for system prompt injection.
#[derive(Debug, Clone)]
pub(crate) struct TeamMember {
    pub name: String,
    pub display_name: String,
    pub role: String,
}

// ── Shared state ────────────────────────────────────────────

/// Shared context for building replies, initialized once at gateway start.
/// Process-wide broadcast of agent-config changes (`agent_id` payload).
///
/// `handlers::update_agent_toml` sends here after a successful registry
/// re-scan; live WebChat sockets subscribe and re-send their `session_info`
/// frame so the header (name / icon / model) reflects the change immediately —
/// without this, an open dashboard tab shows the stale model until reconnect.
/// A lagged/closed receiver is harmless: the socket just misses one refresh
/// and re-syncs on the next event or reconnect.
pub fn agent_config_events() -> &'static tokio::sync::broadcast::Sender<String> {
    static TX: OnceLock<tokio::sync::broadcast::Sender<String>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(32).0)
}

pub struct ReplyContext {
    pub registry: Arc<RwLock<AgentRegistry>>,
    pub home_dir: PathBuf,
    pub http: reqwest::Client,
    pub session_manager: Arc<SessionManager>,
    pub channel_status: ChannelStatusMap,
    /// Broadcast sender for pushing events (e.g. channel status changes) to WebSocket clients.
    pub event_tx: tokio::sync::broadcast::Sender<String>,
    /// Prediction engine for event-driven evolution.
    pub prediction_engine: Option<Arc<PredictionEngine>>,
    /// GVU evolution loop (Phase 2).
    pub gvu_loop: Option<Arc<GvuLoop>>,
    /// Skill lifecycle: compressed skill cache.
    pub skill_cache: Arc<tokio::sync::Mutex<CompressedSkillCache>>,
    /// Skill lifecycle: activation controller.
    pub skill_activation: Arc<tokio::sync::Mutex<SkillActivationController>>,
    /// Skill lifecycle: lift tracker store.
    pub skill_lift: Arc<tokio::sync::Mutex<LiftTrackerStore>>,
    /// Skill lifecycle: gap accumulator for auto-synthesis triggering.
    pub gap_accumulator: Arc<tokio::sync::Mutex<GapAccumulator>>,
    /// Skill lifecycle: sandbox store for trial skills.
    pub sandbox_store: Arc<tokio::sync::Mutex<SandboxStore>>,
    /// Sessions with voice reply mode enabled (toggled by /voice command).
    pub voice_sessions: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    /// Per-channel, per-scope settings (mention_only, whitelist, auto_thread, etc.).
    pub channel_settings: Arc<ChannelSettingsManager>,
    /// User-level access control: allowlist / blocklist / pairing codes.
    pub access_control: Arc<crate::access_control::AccessController>,
    /// WP9: channel user → agent bindings + one-time bind tokens (shared bot).
    pub agent_binding: Arc<crate::agent_binding::AgentBindingStore>,
    /// Killswitch configuration (safety words, thresholds, escalation).
    pub killswitch: Arc<KillswitchConfig>,
    /// Failsafe degradation manager (per-scope level tracking).
    pub failsafe: Option<Arc<FailsafeManager>>,
    /// Circuit breaker registry (per-scope anomaly detection).
    pub circuit_breakers: Option<Arc<CircuitBreakerRegistry>>,
    /// Mistake notebook for grounded GVU evolution (Phase 1 GVU²).
    pub mistake_notebook: Option<Arc<crate::gvu::mistake_notebook::MistakeNotebook>>,
    /// Trajectory recorder for skill extraction (Phase 3).
    pub skill_recorder: Arc<tokio::sync::Mutex<TrajectoryRecorder>>,
    /// Persistent skill bank for extracted skills (Phase 3).
    pub skill_bank: Arc<tokio::sync::Mutex<SkillCache>>,
    /// Path to memory.db for key-fact accumulator (P2).
    /// Engine is created on-demand per operation due to SQLite thread safety.
    pub memory_db_path: Option<PathBuf>,
    /// EvolutionEvents audit-log emitter (Sprint N P0).
    ///
    /// Non-blocking: all emit calls fire-and-forget via tokio::spawn.
    pub evolution_emitter: Arc<EvolutionEventEmitter>,
    /// RFC-23 redaction pipeline. `None` ⇒ disabled (existing behaviour).
    pub redaction_manager: Option<Arc<duduclaw_redaction::RedactionManager>>,
}

impl ReplyContext {
    pub fn new(
        registry: Arc<RwLock<AgentRegistry>>,
        home_dir: PathBuf,
        session_manager: Arc<SessionManager>,
        channel_status: ChannelStatusMap,
        event_tx: tokio::sync::broadcast::Sender<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        // Register the channel-status snapshot path (idempotent; first call wins).
        let _ = CHANNEL_STATUS_PATH.set(home_dir.join("channel_status.json"));
        // Co-locate channel settings in the session database
        let db_path = home_dir.join("sessions.db");
        let channel_settings =
            ChannelSettingsManager::from_session_db(&db_path).unwrap_or_else(|e| {
                warn!("Channel settings init failed ({e}), using in-memory fallback");
                ChannelSettingsManager::new(Path::new(":memory:"))
                    .expect("in-memory DB should always succeed")
            });
        // Load killswitch config from ~/.duduclaw/KILLSWITCH.toml
        let ks_path = home_dir.join("KILLSWITCH.toml");
        let killswitch = KillswitchConfig::load(&ks_path);

        // User-level access control (pairing / allowlist / blocklist),
        // persisted across restarts.
        let access_control = Arc::new(crate::access_control::AccessController::with_persistence(
            home_dir.join("access_control.json"),
        ));

        // WP9: shared-bot user→agent bindings, persisted across restarts and
        // shared with the dashboard RPC that mints bind tokens.
        let agent_binding = Arc::new(crate::agent_binding::AgentBindingStore::with_persistence(
            home_dir.join("agent_bindings.json"),
        ));

        // Initialize failsafe manager and circuit breaker registry
        let failsafe = Arc::new(FailsafeManager::new(killswitch.failsafe.clone()));
        let circuit_breakers = Arc::new(CircuitBreakerRegistry::new(
            killswitch.circuit_breaker.clone(),
        ));

        Self {
            registry,
            home_dir,
            http,
            session_manager,
            channel_status,
            event_tx,
            prediction_engine: None,
            gvu_loop: None,
            skill_cache: Arc::new(tokio::sync::Mutex::new(CompressedSkillCache::new())),
            skill_activation: Arc::new(tokio::sync::Mutex::new(SkillActivationController::new(5))),
            skill_lift: Arc::new(tokio::sync::Mutex::new(LiftTrackerStore::new())),
            gap_accumulator: Arc::new(tokio::sync::Mutex::new(GapAccumulator::new(3, 24))),
            sandbox_store: Arc::new(tokio::sync::Mutex::new(SandboxStore::new())),
            voice_sessions: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
            access_control,
            agent_binding,
            channel_settings: Arc::new(channel_settings),
            killswitch: Arc::new(killswitch),
            failsafe: Some(failsafe),
            circuit_breakers: Some(circuit_breakers),
            mistake_notebook: None,
            skill_recorder: Arc::new(tokio::sync::Mutex::new(TrajectoryRecorder::new())),
            skill_bank: Arc::new(tokio::sync::Mutex::new(SkillCache::new())),
            memory_db_path: None,
            evolution_emitter: Arc::new(EvolutionEventEmitter::from_env()),
            redaction_manager: None,
        }
    }

    /// Inject the redaction manager. `None` (default) ⇒ no redaction.
    pub fn with_redaction_manager(
        mut self,
        manager: Option<Arc<duduclaw_redaction::RedactionManager>>,
    ) -> Self {
        self.redaction_manager = manager;
        self
    }

    /// Create with prediction engine enabled.
    pub fn with_prediction_engine(mut self, engine: Arc<PredictionEngine>) -> Self {
        self.prediction_engine = Some(engine);
        self
    }

    /// Create with GVU evolution loop enabled.
    pub fn with_gvu_loop(mut self, gvu: Arc<GvuLoop>) -> Self {
        self.gvu_loop = Some(gvu);
        self
    }

    /// Create with MistakeNotebook for grounded GVU evolution.
    pub fn with_mistake_notebook(
        mut self,
        nb: Arc<crate::gvu::mistake_notebook::MistakeNotebook>,
    ) -> Self {
        self.mistake_notebook = Some(nb);
        self
    }

    /// Set memory DB path for cross-session key-fact accumulator (P2).
    pub fn with_memory_db(mut self, path: PathBuf) -> Self {
        self.memory_db_path = Some(path);
        self
    }
}

/// Snapshot file for out-of-process readers (the `channel_status` MCP tool
/// runs in the `duduclaw mcp-server` process and cannot see the gateway's
/// in-memory map). Set once at gateway start via [`ReplyContext::new`].
pub(super) static CHANNEL_STATUS_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Persist the channel-status snapshot atomically (temp + rename).
/// Best-effort: a failed write only degrades the MCP `channel_status` view.
pub(super) fn persist_channel_status_snapshot(snapshot: serde_json::Value) {
    let Some(path) = CHANNEL_STATUS_PATH.get() else {
        return;
    };
    let path = path.clone();
    tokio::task::spawn_blocking(move || {
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(&snapshot).unwrap_or_default();
        if std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .is_err()
        {
            tracing::debug!(?path, "channel status snapshot write failed");
        }
    });
}

/// Helper to update a channel's connection state and broadcast the change to dashboard clients.
pub async fn set_channel_connected(
    status: &ChannelStatusMap,
    name: &str,
    connected: bool,
    error: Option<String>,
    event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
) {
    let now = chrono::Utc::now();
    // WP12: several channel APIs carry the credential IN THE URL (Telegram's
    // `/bot<token>/getMe`, WeCom `?corpsecret=`, DingTalk `?appsecret=`), so a
    // raw transport error prints a working bot token. This is the single choke
    // point for every channel's error text — it feeds the dashboard roster, the
    // `channels.status_changed` WS event AND `channel_status.json` on disk, so
    // redacting here covers all three sinks for all nine channels at once.
    let error = crate::secret_redact::redact_opt(error);
    let error_clone = error.clone();
    // M3 — state de-duplication. A poller in a retry loop calls this on every
    // tick; before WP12 that meant a WS broadcast and a `channel_status.json`
    // rewrite every 3 seconds forever during an outage. The observable state is
    // `(connected, error)`, so only a *change* in that pair is news. The
    // in-memory `last_event` timestamp is still refreshed either way.
    {
        let mut map = status.write().await;
        let state_changed = map
            .get(name)
            .map(|prev| prev.connected != connected || prev.error != error)
            .unwrap_or(true);
        map.insert(
            name.to_string(),
            ChannelState {
                connected,
                last_event: Some(now),
                error,
            },
        );
        if !state_changed {
            return;
        }
        // Snapshot for the out-of-process `channel_status` MCP tool.
        let snapshot = serde_json::json!({
            "updated_at": now.to_rfc3339(),
            "channels": map.iter().map(|(n, s)| {
                (n.clone(), serde_json::json!({
                    "connected": s.connected,
                    "last_event": s.last_event.map(|t| t.to_rfc3339()),
                    "error": s.error,
                }))
            }).collect::<serde_json::Map<String, serde_json::Value>>(),
        });
        persist_channel_status_snapshot(snapshot);
    }
    // Broadcast status change to WebSocket clients for real-time dashboard updates
    if let Some(tx) = event_tx {
        let event = crate::protocol::WsFrame::event(
            "channels.status_changed",
            serde_json::json!({
                "name": name,
                "connected": connected,
                "last_connected": now.to_rfc3339(),
                "error": error_clone,
            }),
        );
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = tx.send(json);
        }
    }
}

/// Best-effort activity-feed append + live `activity.new` broadcast for
/// conversation-side events (agent replies, key-fact distillation). Channel
/// conversations previously left zero trace in 紀錄/即時動態 — the feed only
/// knew about task lifecycle and a few MCP tools. Never affects reply
/// delivery: every failure is logged and swallowed.
pub(crate) async fn post_conversation_activity(
    home_dir: &std::path::Path,
    event_tx: &tokio::sync::broadcast::Sender<String>,
    agent_id: &str,
    event_type: &str,
    summary: String,
) {
    let store = match crate::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "conversation activity skipped: task store open failed");
            return;
        }
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: None,
        summary,
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!(error = %e, "conversation activity append failed");
        return;
    }
    // Same JSON shape as handlers::activity_row_to_json so the dashboard's
    // existing `activity.new` subscribers render it unchanged.
    let frame = crate::protocol::WsFrame::event(
        "activity.new",
        serde_json::json!({
            "id": row.id,
            "type": row.event_type,
            "agent_id": row.agent_id,
            "task_id": row.task_id,
            "summary": row.summary,
            "timestamp": row.timestamp,
            "metadata": serde_json::Value::Null,
        }),
    );
    if let Ok(json) = serde_json::to_string(&frame) {
        let _ = event_tx.send(json);
    }
}

