//! Shared AI reply builder for all channel bots.
//!
//! Calls the Claude Code SDK (Python) via subprocess for AI responses,
//! using the multi-account rotator for key management and budget tracking.
//! Falls back to direct Anthropic API if Python is unavailable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use duduclaw_core::MemoryEngine;
use duduclaw_security::circuit_breaker::CircuitBreakerRegistry;
use duduclaw_security::failsafe::FailsafeManager;
use duduclaw_security::killswitch::KillswitchConfig;

use crate::channel_settings::ChannelSettingsManager;
use crate::handlers::ChannelState;
use crate::gvu::loop_::GvuLoop;
use crate::prediction::engine::PredictionEngine;
use crate::session::SessionManager;
use crate::skill_extraction::recorder::{
    Sentiment, SkillCache, SkillExtractor, TrajectoryOutcome, TrajectoryRecorder,
};
use crate::skill_lifecycle::activation::SkillActivationController;
use crate::skill_lifecycle::compression::CompressedSkillCache;
use crate::skill_lifecycle::gap_accumulator::GapAccumulator;
use crate::skill_lifecycle::lift::LiftTrackerStore;
use crate::skill_lifecycle::sandbox_trial::SandboxStore;

/// Shared channel status map, accessible by both channel bots and the RPC handler.
pub type ChannelStatusMap = Arc<RwLock<std::collections::HashMap<String, ChannelState>>>;

// ── Shared state ────────────────────────────────────────────

/// Shared context for building replies, initialized once at gateway start.
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
    // ── MemGPT 3-layer memory (Phase 5) ──
    /// Core Memory (L1) — always in context window.
    pub core_memory: Option<Arc<duduclaw_memory::CoreMemoryManager>>,
    /// Recall Memory (L2) — cross-session conversation log.
    pub recall_memory: Option<Arc<duduclaw_memory::RecallMemoryManager>>,
    /// Archival Memory (L3) — long-term semantic knowledge.
    pub archival_memory: Option<Arc<duduclaw_memory::ArchivalMemoryBridge>>,
    /// Memory budget configuration.
    pub memory_budget: duduclaw_memory::MemoryBudgetConfig,
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
        // Co-locate channel settings in the session database
        let db_path = home_dir.join("sessions.db");
        let channel_settings = ChannelSettingsManager::from_session_db(&db_path)
            .unwrap_or_else(|e| {
                warn!("Channel settings init failed ({e}), using in-memory fallback");
                ChannelSettingsManager::new(Path::new(":memory:"))
                    .expect("in-memory DB should always succeed")
            });
        // Load killswitch config from ~/.duduclaw/KILLSWITCH.toml
        let ks_path = home_dir.join("KILLSWITCH.toml");
        let killswitch = KillswitchConfig::load(&ks_path);

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
            channel_settings: Arc::new(channel_settings),
            killswitch: Arc::new(killswitch),
            failsafe: Some(failsafe),
            circuit_breakers: Some(circuit_breakers),
            mistake_notebook: None,
            skill_recorder: Arc::new(tokio::sync::Mutex::new(TrajectoryRecorder::new())),
            skill_bank: Arc::new(tokio::sync::Mutex::new(SkillCache::new())),
            core_memory: None,
            recall_memory: None,
            archival_memory: None,
            memory_budget: duduclaw_memory::MemoryBudgetConfig::default(),
        }
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
    pub fn with_mistake_notebook(mut self, nb: Arc<crate::gvu::mistake_notebook::MistakeNotebook>) -> Self {
        self.mistake_notebook = Some(nb);
        self
    }

    /// Create with MemGPT 3-layer memory.
    pub fn with_memory(
        mut self,
        core: Arc<duduclaw_memory::CoreMemoryManager>,
        recall: Arc<duduclaw_memory::RecallMemoryManager>,
        archival: Arc<duduclaw_memory::ArchivalMemoryBridge>,
        budget: duduclaw_memory::MemoryBudgetConfig,
    ) -> Self {
        self.core_memory = Some(core);
        self.recall_memory = Some(recall);
        self.archival_memory = Some(archival);
        self.memory_budget = budget;
        self
    }
}

/// Helper to update a channel's connection state and broadcast the change to dashboard clients.
pub async fn set_channel_connected(status: &ChannelStatusMap, name: &str, connected: bool, error: Option<String>, event_tx: Option<&tokio::sync::broadcast::Sender<String>>) {
    let now = chrono::Utc::now();
    let error_clone = error.clone();
    {
        let mut map = status.write().await;
        map.insert(name.to_string(), ChannelState {
            connected,
            last_event: Some(now),
            error,
        });
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

// ── User sentiment detection ───────────────────────────────

/// Detect user satisfaction heuristic from message text (zero LLM cost).
///
/// Positive signals: gratitude, approval, emoji thumbs-up, CJK equivalents.
/// Negative signals: corrections, complaints, error reports, CJK equivalents.
/// Returns `None` if no clear signal detected (neutral message).
fn detect_user_sentiment(text: &str) -> Option<Sentiment> {
    let lower = text.to_lowercase();
    let positive_signals = [
        "thanks",
        "thank you",
        "great",
        "good",
        "perfect",
        "awesome",
        "nice",
        "\u{1f44d}", // 👍
        "\u{1f389}", // 🎉
        "\u{2705}",  // ✅
        "\u{8b1d}\u{8b1d}",     // 謝謝
        "\u{611f}\u{8b1d}",     // 感謝
        "\u{5b8c}\u{7f8e}",     // 完美
        "\u{597d}\u{7684}",     // 好的
        "\u{8b9a}",             // 讚
        "\u{592a}\u{597d}\u{4e86}", // 太好了
        "\u{5f88}\u{597d}",     // 很好
    ];
    let negative_signals = [
        "no",
        "wrong",
        "incorrect",
        "fix",
        "error",
        "bug",
        "\u{4e0d}\u{5c0d}",     // 不對
        "\u{932f}\u{4e86}",     // 錯了
        "\u{91cd}\u{4f86}",     // 重來
        "\u{4e0d}\u{884c}",     // 不行
        "\u{4fee}\u{6b63}",     // 修正
        "\u{6709}\u{554f}\u{984c}", // 有問題
    ];

    if positive_signals.iter().any(|s| lower.contains(s)) {
        Some(Sentiment::Positive)
    } else if negative_signals.iter().any(|s| lower.contains(s)) {
        Some(Sentiment::Negative)
    } else {
        None
    }
}

// ── Public API ──────────────────────────────────────────────

/// Build a reply for an incoming user message (no user tracking).
///
/// Strategy:
/// 1. Try Python Claude Code SDK (subprocess) — uses rotator + budget tracking
/// 2. Fallback to direct Anthropic API (Rust reqwest) — single key only
/// 3. Fallback to static error message
pub async fn build_reply(text: &str, ctx: &ReplyContext) -> String {
    build_reply_with_session(text, ctx, "default", "anonymous", None).await
}

/// Build a reply with progress streaming.
///
/// `on_progress` callback receives real-time progress events (keepalive,
/// tool-use details) that the channel handler can forward to the user.
pub async fn build_reply_with_progress(
    text: &str,
    ctx: &ReplyContext,
    on_progress: Option<ProgressCallback>,
) -> String {
    build_reply_with_session(text, ctx, "default", "anonymous", on_progress).await
}

/// Build a reply for a specific named agent (used by per-agent Discord bots).
///
/// Instead of reading `default_agent` from config.toml, this directly resolves
/// the agent by `agent_name` in the registry.
pub async fn build_reply_for_agent(
    text: &str,
    ctx: &ReplyContext,
    agent_name: &str,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> String {
    build_reply_with_session_inner(text, ctx, Some(agent_name), session_id, user_id, on_progress).await
}

/// Build a reply with session tracking and optional progress streaming.
///
/// `user_id` should be the stable per-user identifier from the channel
/// (e.g., Telegram chat_id, LINE sender ID, Discord user ID).
/// This feeds the prediction engine's per-user statistical models.
pub async fn build_reply_with_session(
    text: &str,
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> String {
    build_reply_with_session_inner(text, ctx, None, session_id, user_id, on_progress).await
}

/// Inner implementation shared by both default-agent and explicit-agent paths.
///
/// When `agent_override` is `Some(name)`, the named agent is looked up directly.
/// When `None`, the default agent resolution logic (config.toml → main_agent) is used.
async fn build_reply_with_session_inner(
    text: &str,
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> String {
    // Determine which agent to use
    let reg = ctx.registry.read().await;
    let agent = if let Some(name) = agent_override {
        // Explicit agent name (per-agent Discord bot)
        reg.get(name).or_else(|| reg.main_agent())
    } else {
        // Default: config.toml default_agent → main_agent() → fallback
        let default_agent_name = get_default_agent(&ctx.home_dir).await;
        if let Some(name) = &default_agent_name {
            reg.get(name).or_else(|| reg.main_agent())
        } else {
            reg.main_agent()
        }
    };

    if let Some(a) = agent {
        info!("Using agent: {} ({})", a.config.agent.display_name, a.config.agent.name);
    }

    let model = agent
        .map(|a| a.config.model.preferred.clone())
        .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

    let agent_id = agent.map(|a| a.config.agent.name.clone()).unwrap_or_default();
    let agent_dir = agent.map(|a| a.dir.clone());
    let capabilities = agent.map(|a| a.config.capabilities.clone());
    let skill_token_budget = agent
        .map(|a| a.config.evolution.skill_token_budget)
        .unwrap_or(2500);

    // Refresh compressed skill cache from agent's loaded skills
    {
        let skills_data: Vec<(String, String, Option<String>)> = agent
            .map(|a| {
                a.skills.iter().map(|s| {
                    (s.name.clone(), s.content.clone(), None)
                }).collect()
            })
            .unwrap_or_default();
        let mut cache = ctx.skill_cache.lock().await;
        cache.refresh(&skills_data);
    }

    // Get active skills for progressive injection
    let active_skills = {
        let ctrl = ctx.skill_activation.lock().await;
        ctrl.get_active(&agent_id)
    };

    // Build progressive system prompt
    let system_prompt = {
        let cache = ctx.skill_cache.lock().await;
        let compressed: Vec<_> = cache.all().into_iter().cloned().collect();
        if compressed.is_empty() {
            build_system_prompt(agent, None, None, None, skill_token_budget)
        } else {
            build_system_prompt(
                agent,
                Some(text),
                Some(&compressed),
                Some(&active_skills),
                skill_token_budget,
            )
        }
    };
    drop(reg);

    let session_mgr = &ctx.session_manager;

    // ── L0: Safety word check (highest priority, zero latency) ──
    // Runs BEFORE session creation to avoid unnecessary DB writes for !STOP etc.
    let safety_action = duduclaw_security::safety_word::check(text, &ctx.killswitch.safety_words);
    if !matches!(safety_action, duduclaw_security::safety_word::SafetyWordAction::None) {
        // Safety words are handled by chat_commands.rs, but if we reach here
        // (e.g., direct call without command parsing), handle inline
        match &safety_action {
            duduclaw_security::safety_word::SafetyWordAction::Stop(scope) => {
                if let Some(ref failsafe) = ctx.failsafe {
                    match scope {
                        duduclaw_security::safety_word::SafetyWordScope::CurrentScope => {
                            failsafe.force_halt(session_id, "safety word").await;
                            duduclaw_security::audit::log_safety_word(
                                &ctx.home_dir, &agent_id, session_id, user_id, "stop",
                            );
                            return duduclaw_security::safety_word::format_response(&safety_action, session_id);
                        }
                        duduclaw_security::safety_word::SafetyWordScope::Global => {
                            // Global stop requires admin — this inline path has no
                            // admin context, so only halt the current scope as a
                            // safeguard. The full !STOP ALL is handled via
                            // chat_commands::handle_command which enforces admin.
                            warn!(session_id, user_id, "!STOP ALL via inline path — halting scope only (admin check unavailable)");
                            failsafe.force_halt(session_id, "safety word: STOP ALL (scope-only)").await;
                            duduclaw_security::audit::log_safety_word(
                                &ctx.home_dir, &agent_id, session_id, user_id, "stop_all_downgraded",
                            );
                            return "🛑 Agent stopped (scope). Global stop requires admin — use chat command.".to_string();
                        }
                    }
                }
                return duduclaw_security::safety_word::format_response(&safety_action, session_id);
            }
            duduclaw_security::safety_word::SafetyWordAction::Resume => {
                if let Some(ref failsafe) = ctx.failsafe {
                    // Only resume the current scope — global halt requires
                    // explicit !STOP ALL scope to be cleared separately (via
                    // chat_commands handler which has user_id for admin check).
                    failsafe.resume(session_id).await;
                    duduclaw_security::audit::log_safety_word(
                        &ctx.home_dir, &agent_id, session_id, user_id, "resume",
                    );
                    return duduclaw_security::safety_word::format_response(&safety_action, session_id);
                }
                return "⚠️ Failsafe system not initialized.".to_string();
            }
            duduclaw_security::safety_word::SafetyWordAction::Status => {
                if let Some(ref failsafe) = ctx.failsafe {
                    let state = failsafe.get_state(session_id).await;
                    return duduclaw_security::failsafe::format_status(session_id, state.as_ref());
                }
                return "Failsafe: not initialized".to_string();
            }
            duduclaw_security::safety_word::SafetyWordAction::None => {}
        }
    }

    // ── L1: Failsafe state gate ──
    if let Some(ref failsafe) = ctx.failsafe {
        // Check global halt first
        let global_level = failsafe.get_level("__global__").await;
        let scope_level = failsafe.get_level(session_id).await;
        let effective_level = std::cmp::max(global_level, scope_level);

        use duduclaw_security::failsafe::FailsafeLevel;
        match effective_level {
            FailsafeLevel::L4Halted => {
                // Halted: reply with canned message
                return failsafe.canned_reply(effective_level)
                    .unwrap_or("Service paused.").to_string();
            }
            FailsafeLevel::L3Muted => {
                // Muted: silent drop, no reply
                return String::new();
            }
            FailsafeLevel::L2Restricted => {
                // Restricted: return canned reply, don't call AI
                return failsafe.canned_reply(effective_level)
                    .unwrap_or("Service restricted.").to_string();
            }
            FailsafeLevel::L1Degraded => {
                // Degraded: allow through but could prefer local model
                // (model routing is handled downstream)
            }
            FailsafeLevel::L0Normal => {}
        }
    }

    // ── L2: Circuit breaker check ──
    let mut breaker_state = duduclaw_security::circuit_breaker::BreakerState::Closed;
    if let Some(ref cb_registry) = ctx.circuit_breakers {
        let decision = cb_registry.check_inbound(session_id, text).await;
        match decision {
            duduclaw_security::circuit_breaker::BreakerDecision::Allow => {}
            duduclaw_security::circuit_breaker::BreakerDecision::Throttle => {
                breaker_state = duduclaw_security::circuit_breaker::BreakerState::HalfOpen;
                // Allow through but mark for defensive prompt injection later
            }
            duduclaw_security::circuit_breaker::BreakerDecision::Deny(_) => {
                debug!(session_id, "Circuit breaker denied — message dropped");
                return String::new(); // silent drop
            }
            duduclaw_security::circuit_breaker::BreakerDecision::Trip(reason) => {
                warn!(session_id, reason = %reason, "Circuit breaker tripped");
                // Audit log
                duduclaw_security::audit::log_circuit_breaker_trip(
                    &ctx.home_dir, &agent_id, session_id, &reason.to_string(),
                );
                // Escalate failsafe
                if let Some(ref failsafe) = ctx.failsafe {
                    failsafe.escalate(session_id, &format!("circuit breaker: {reason}")).await;
                }
                return String::new(); // silent drop for this message
            }
        }
    }

    // ── L3: Prompt injection scan (existing) ──
    let scan = duduclaw_security::input_guard::scan_input(
        text,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if scan.blocked {
        warn!(
            agent = %agent_id,
            score = scan.risk_score,
            rules = ?scan.matched_rules,
            "Prompt injection detected — blocking message"
        );
        return format!("⚠️ {}", scan.summary);
    }

    // ── All pre-filters passed — now create/load session ──
    let _ = session_mgr.get_or_create(session_id, &agent_id).await;

    // ── Phase 3: Check if previous trajectory should get feedback ──
    // The current user message may contain feedback (positive/negative) for
    // the assistant's previous reply, completing the "within 2 turns" window.
    {
        let sentiment = detect_user_sentiment(text);
        if let Some(sentiment) = sentiment {
            let session_key = format!("{session_id}:{agent_id}");
            let mut recorder = ctx.skill_recorder.lock().await;
            if recorder.is_recording(&session_key) {
                // Record this feedback turn, then finalize with detected sentiment
                recorder.record_turn(&session_key, "user", text, vec![]);
                let outcome = match sentiment {
                    Sentiment::Positive => TrajectoryOutcome::Success,
                    Sentiment::Negative => TrajectoryOutcome::Failure,
                };
                if let Some(trajectory) = recorder.finalize(&session_key, outcome, Some(sentiment)) {
                    // Extract skill heuristically (zero LLM cost)
                    if let Some(skill) = SkillExtractor::extract_heuristic(&trajectory) {
                        info!(
                            skill_name = %skill.name,
                            tools = ?skill.tools_used,
                            confidence = skill.confidence,
                            "Auto-extracted skill from trajectory (feedback-triggered)"
                        );

                        // Persist to SkillCache
                        {
                            let mut bank = ctx.skill_bank.lock().await;
                            bank.add(skill.clone());
                            debug!(bank_size = bank.len(), "Skill added to SkillCache");
                        }

                        // Log extraction event to audit log
                        let audit_entry = serde_json::json!({
                            "event": "skill_extracted",
                            "trigger": "user_feedback",
                            "skill_id": skill.id,
                            "skill_name": skill.name,
                            "tools_used": skill.tools_used,
                            "confidence": skill.confidence,
                            "sentiment": format!("{sentiment:?}"),
                            "source_session": session_key,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        });
                        if let Ok(audit_line) = serde_json::to_string(&audit_entry) {
                            let audit_path = ctx.home_dir.join("skill_extraction_audit.jsonl");
                            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&audit_path)
                                .await
                            {
                                use tokio::io::AsyncWriteExt;
                                let _ = f.write_all(format!("{audit_line}\n").as_bytes()).await;
                            }
                        }
                    }
                }
                debug!(
                    session = %session_key,
                    sentiment = ?sentiment,
                    "User feedback detected for active trajectory"
                );
            }
        }
    }

    // Sanitize role-prefix injection: strip any attempt to impersonate assistant/system role
    let sanitized_text = if text.starts_with("assistant:") || text.starts_with("system:") {
        format!("[user input] {text}")
    } else {
        text.to_string()
    };

    // Prepend sender metadata so the agent can identify who is talking
    let sanitized_text = if user_id != "anonymous" && !user_id.is_empty() {
        format!("[sender_id: {user_id}]\n{sanitized_text}")
    } else {
        sanitized_text
    };

    // Append user message to session using improved CJK-aware token estimate
    let user_tokens = estimate_tokens(&sanitized_text);
    if let Err(e) = session_mgr
        .append_message(session_id, "user", &sanitized_text, user_tokens)
        .await
    {
        warn!("Failed to save user message to session: {e}");
    }

    // ── Recall Memory (L2): record user inbound message ──
    if let Some(recall) = &ctx.recall_memory {
        let (channel, chat_id) = parse_session_id_parts(session_id);
        let entry = duduclaw_memory::RecallEntry {
            agent_id: agent_id.clone(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            session_id: session_id.to_string(),
            role: "user".to_string(),
            content: sanitized_text.clone(),
            source: "interactive".to_string(),
            source_agent: String::new(),
            token_count: user_tokens,
            ..Default::default()
        };
        if let Err(e) = recall.record(entry).await {
            warn!("Failed to record user message to recall log: {e}");
        }
    }

    // Build conversation history from session
    let history = match session_mgr.get_messages(session_id).await {
        Ok(msgs) => {
            if msgs.len() > 1 {
                let mut history_text = String::with_capacity(msgs.len() * 200);
                // All messages except the last one (which we just appended)
                for m in msgs.iter().take(msgs.len().saturating_sub(1)) {
                    if !history_text.is_empty() { history_text.push('\n'); }
                    use std::fmt::Write;
                    let _ = write!(history_text, "{}: {}", m.role, m.content);
                }
                format!("\n\n## Conversation History\n{history_text}")
            } else {
                String::new()
            }
        }
        Err(e) => {
            warn!("Failed to load session messages: {e}");
            String::new()
        }
    };

    // ── MemGPT 3-layer memory injection ──
    let memory_section = if ctx.memory_budget.enabled {
        if let (Some(core), Some(recall), Some(archival)) =
            (&ctx.core_memory, &ctx.recall_memory, &ctx.archival_memory)
        {
            let budget_mgr = duduclaw_memory::MemoryBudgetManager::new(
                Arc::clone(core),
                Arc::clone(recall),
                Arc::clone(archival),
            );
            let (channel, chat_id) = parse_session_id_parts(session_id);
            match budget_mgr
                .build_memory_prompt(
                    &ctx.memory_budget,
                    &agent_id,
                    channel,
                    chat_id,
                    &sanitized_text,
                )
                .await
            {
                Ok(section) => section,
                Err(e) => {
                    warn!("Failed to build memory prompt: {e}");
                    String::new()
                }
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let full_system_prompt = match (memory_section.is_empty(), history.is_empty()) {
        (true, true) => system_prompt,
        (true, false) => format!("{system_prompt}{history}"),
        (false, true) => format!("{system_prompt}\n\n{memory_section}"),
        (false, false) => format!("{system_prompt}\n\n{memory_section}{history}"),
    };

    // Track the last underlying failure so the fallback message can
    // accurately describe what went wrong (rate limit vs timeout vs
    // missing binary etc.) instead of always blaming "not installed".
    let mut last_cli_error: Option<String> = None;

    // Record the moment we dispatched the CLI call. This is the lower
    // time bound used by the action-claim verifier when scanning
    // tool_calls.jsonl for receipts that back up the agent's text
    // assertions — anything before this timestamp belongs to a
    // previous turn and must not be credited to this one.
    let dispatch_start_time = chrono::Utc::now().to_rfc3339();

    // ── L5 Computer Use: intercept if agent has computer_use enabled ──
    // Check for natural-language emergency stop first
    if crate::risk_detector::is_emergency_stop(text) {
        info!(session_id, "Emergency stop detected for computer use");
        // Stop ALL active computer use sessions via the global registry
        let sessions = crate::computer_use_orchestrator::list_sessions().await;
        for sid in &sessions {
            if let Some(ctl) = crate::computer_use_orchestrator::get_session_control(sid).await {
                ctl.stopped.store(true, std::sync::atomic::Ordering::Release);
            }
            crate::computer_use_orchestrator::unregister_session(sid).await;
        }
        let count = sessions.len();
        return if count > 0 {
            format!("🛑 已停止 {count} 個電腦操作 session")
        } else {
            "🛑 已停止電腦操作".to_string()
        };
    }

    // Check if this agent has computer_use enabled and the user's intent
    // suggests a computer use task (e.g., mentions screen, click, open app).
    let cu_enabled = capabilities
        .as_ref()
        .map(|c| c.computer_use)
        .unwrap_or(false);

    if cu_enabled && looks_like_computer_use_request(text) {
        // Build a ComputerUseConfig from the agent's capabilities
        let cap_cfg = capabilities
            .as_ref()
            .map(|c| &c.computer_use_config)
            .cloned()
            .unwrap_or_default();
        // Read execution_mode from capabilities
        let exec_mode = capabilities
            .as_ref()
            .map(|c| c.computer_use_mode)
            .unwrap_or_default();

        // Read CONTRACT.toml must_not rules (if the agent has a contract)
        let contract_must_not = agent_dir.as_ref().and_then(|d| {
            let contract_path = d.join("CONTRACT.toml");
            let content = std::fs::read_to_string(&contract_path).ok()?;
            let table: toml::Table = content.parse().ok()?;
            let must_not = table.get("must_not")?.as_table()?;
            let rules = must_not.get("rules")?.as_array()?;
            Some(rules.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect::<Vec<_>>())
        }).unwrap_or_default();

        let cu_config = crate::computer_use_orchestrator::ComputerUseConfig {
            max_session_minutes: cap_cfg.max_session_minutes,
            max_actions: cap_cfg.max_actions,
            display_width: cap_cfg.display_width,
            display_height: cap_cfg.display_height,
            auto_confirm_trusted: cap_cfg.auto_confirm_trusted,
            allowed_apps: cap_cfg.allowed_apps.clone(),
            blocked_actions: cap_cfg.blocked_actions.clone(),
            execution_mode: exec_mode,
            contract_must_not,
            ..Default::default()
        };

        // Resolve API key for the Claude Vision API (computer use needs direct API)
        if let Some(api_key) = get_api_key(&ctx.home_dir).await {
            let mut orchestrator = crate::computer_use_orchestrator::ComputerUseOrchestrator::new(
                agent_id.clone(),
                ctx.home_dir.clone(),
                cu_config,
            );

            // Build a real channel sender from the session_id (e.g., "telegram:12345")
            // so screenshots and confirmations are delivered to the user's channel.
            let sender: Box<dyn crate::channel_sender::ChannelSender> = {
                let (ch_type, ch_id) = parse_session_id_parts(session_id);
                if ch_type.is_empty() || ch_id.is_empty() {
                    Box::new(crate::channel_sender::NullSender)
                } else if ch_type == "webchat" {
                    // WebChat needs the broadcast tx for WebSocket delivery
                    crate::channel_sender::create_webchat_sender(
                        ch_id.to_string(),
                        ctx.event_tx.clone(),
                    )
                } else {
                    // Look up the channel token from config
                    let token = crate::config_crypto::read_encrypted_config_field(
                        &ctx.home_dir, ch_type, &format!("{ch_type}_bot_token"),
                    ).await.unwrap_or_default();

                    let target = crate::channel_sender::ChannelTarget {
                        channel_type: ch_type.to_string(),
                        chat_id: ch_id.to_string(),
                        token,
                        extra_id: Some(user_id.to_string()),
                    };
                    crate::channel_sender::create_sender(&target, ctx.http.clone())
                }
            };

            // Generate a session ID and register in the global registry
            let cu_session_id = format!("cu-{}", uuid::Uuid::new_v4().as_simple());
            let control = orchestrator.control_handle();

            match orchestrator.start_session(&api_key, &model).await {
                Ok(()) => {
                    // Register session so /stop, emergency stop, and MCP tools can find it
                    if let Err(e) = crate::computer_use_orchestrator::register_session(
                        &cu_session_id, control,
                    ).await {
                        warn!(error = %e, "Failed to register computer use session");
                        orchestrator.stop_session().await;
                        // Fall through to text reply
                    } else {
                        let result = orchestrator.run_loop(text, sender.as_ref()).await;

                        // Always unregister on completion
                        crate::computer_use_orchestrator::unregister_session(&cu_session_id).await;

                        match result {
                            Ok(reply_text) => return reply_text,
                            Err(e) => {
                                warn!(error = %e, "Computer use session failed, falling back to text");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to start computer use container, falling back to text");
                }
            }
        }
    }

    // 1. Try `claude` CLI with multi-account rotation (OAuth + API keys)
    // Wrap in REPLY_CHANNEL scope so `send_to_agent` MCP tool can register
    // delegation callbacks for sub-agent response forwarding.
    // Only set for sessions originating from a real channel (telegram/line/discord).
    let cli_future = call_claude_cli_rotated(
        &sanitized_text, &model, &full_system_prompt, &ctx.home_dir,
        agent_dir.as_deref(), on_progress.as_ref(), capabilities.as_ref(),
    );
    let is_channel_session = duduclaw_core::SUPPORTED_CHANNEL_TYPES.iter()
        .any(|t| session_id.starts_with(&format!("{t}:")));
    let reply = if is_channel_session {
        crate::claude_runner::REPLY_CHANNEL.scope(session_id.to_string(), cli_future).await
    } else {
        cli_future.await
    };
    let reply = match reply {
        Ok(reply) => {
            info!("Claude replied via Claude Code SDK ({} chars)", reply.len());
            Some(reply)
        }
        Err(e) => {
            let log_line = format!("[{}] claude CLI error: {e}\n", chrono::Utc::now());
            let _ = tokio::fs::OpenOptions::new()
                .create(true).append(true)
                .open(ctx.home_dir.join("debug.log")).await
                .map(|mut f| { use tokio::io::AsyncWriteExt; tokio::spawn(async move { let _ = f.write_all(log_line.as_bytes()).await; }); });
            warn!("claude CLI unavailable: {e}");
            last_cli_error = Some(e);
            None
        }
    };

    // 2. Fallback: Local model inference (if configured)
    let reply = match reply {
        Some(r) => Some(r),
        None => {
            // Resolve agent's local model config (if any)
            let local_model_id = agent_dir.as_ref().and_then(|d| {
                let toml_path = d.join("agent.toml");
                let content = std::fs::read_to_string(&toml_path).ok()?;
                let table: toml::Table = content.parse().ok()?;
                table.get("model")?.as_table()?
                    .get("local")?.as_table()?
                    .get("model")?.as_str().map(|s| s.to_string())
            });
            match crate::claude_runner::try_local_inference(
                &ctx.home_dir, &sanitized_text, &full_system_prompt, local_model_id.as_deref(),
            ).await {
                Ok(local_reply) => {
                    info!("Replied via local model ({} chars)", local_reply.len());
                    // Prepend a notice so the user knows CLI failed and local model is answering
                    let cli_err = last_cli_error.as_deref().unwrap_or("unknown");
                    let hint = classify_cli_error_hint(cli_err);
                    let notice = format!(
                        "⚠️ Claude CLI 暫時不可用（{hint}），本次由本地模型代為回應。\n\
                         系統會在背景自動偵測恢復。\n\n"
                    );
                    Some(format!("{notice}{local_reply}"))
                }
                Err(e) => {
                    if e != "ROUTER_ESCALATE_TO_CLOUD" {
                        warn!("Local inference unavailable: {e}");
                    }
                    None
                }
            }
        }
    };

    // 3. Fallback: Python wrapper (with account rotator)
    //
    // The Python SDK uses the `anthropic` package (Direct API) which requires
    // an API key — OAuth tokens are not supported. Only attempt this fallback
    // when an API key is available; skip entirely for OAuth-only setups to
    // avoid the misleading "未設定任何 API 帳號" error message.
    let fallback_api_key = get_api_key(&ctx.home_dir).await;
    let reply = match reply {
        Some(r) => Some(r),
        None if fallback_api_key.is_some() => {
            match call_python_sdk_v2(
                &sanitized_text, &model, &full_system_prompt, &ctx.home_dir,
                fallback_api_key.as_deref(),
            ).await {
                Ok(reply) => {
                    info!("Claude replied via Python SDK ({} chars)", reply.len());
                    Some(reply)
                }
                Err(e) => {
                    let log_line = format!("[{}] python SDK error: {e}\n", chrono::Utc::now());
                    let _ = tokio::fs::OpenOptions::new()
                        .create(true).append(true)
                        .open(ctx.home_dir.join("debug.log")).await
                        .map(|mut f| { use tokio::io::AsyncWriteExt; tokio::spawn(async move { let _ = f.write_all(log_line.as_bytes()).await; }); });
                    warn!("Python SDK unavailable: {e}");
                    // Only overwrite if we don't already have a more specific CLI error.
                    if last_cli_error.is_none() {
                        last_cli_error = Some(e);
                    }
                    None
                }
            }
        }
        None => {
            info!("Skipping Python SDK fallback — no API key available (OAuth-only setup)");
            None
        }
    };

    if let Some(mut reply) = reply {
        // ── Action-claim verifier (shadow mode) ─────────────────────
        //
        // Cross-reference factual assertions in `reply` against the
        // MCP tool-call audit trail (`tool_calls.jsonl`) that was
        // populated during this turn. Catches "Agnes-class" bugs where
        // the agent narrates having done something (created 12 agents,
        // sent a message, updated a SOUL file) without actually calling
        // the corresponding MCP tool.
        //
        // Currently runs in SHADOW MODE: detections are logged to the
        // security audit log and emitted as tracing events, but the
        // reply is NOT altered. This lets us gather a `ungrounded_claim_rate`
        // baseline before flipping to enforce mode.
        //
        // Zero LLM cost — pure regex + log diff.
        // Zero marginal latency — runs on a value we already have.
        if !agent_id.is_empty() {
            let hallucinations = duduclaw_security::action_claim_verifier::detect_hallucinations(
                &ctx.home_dir,
                &agent_id,
                &reply,
                &dispatch_start_time,
            );
            if !hallucinations.is_empty() {
                warn!(
                    agent = %agent_id,
                    session_id,
                    count = hallucinations.len(),
                    "🚨 Action-claim verifier flagged {} ungrounded claim(s) in reply (shadow mode — not blocking)",
                    hallucinations.len()
                );
                for h in &hallucinations {
                    if let duduclaw_security::action_claim_verifier::VerifyResult::Hallucination {
                        claim,
                        reason,
                    } = h
                    {
                        warn!(
                            agent = %agent_id,
                            claim_type = ?claim.claim_type,
                            target = %claim.target_id,
                            matched_text = %claim.matched_text,
                            reason = %reason,
                            "ungrounded claim"
                        );
                        // Append a structured entry to security_audit.jsonl
                        // so dashboards and forensic tooling can surface
                        // the event. One row per claim.
                        duduclaw_security::audit::log_tool_hallucination(
                            &ctx.home_dir,
                            &agent_id,
                            &claim.matched_text,
                            claim.claim_type.expected_tool(),
                        );
                    }
                }
            }
        }

        // Record outbound for circuit breaker echo detection
        let reply_tokens = estimate_tokens(&reply);
        if let Some(ref cb_registry) = ctx.circuit_breakers {
            cb_registry.record_outbound(session_id, &reply, reply_tokens as usize).await;
        }

        // Inject defensive prompt if circuit breaker is in HalfOpen (bot loop suspected)
        if crate::defensive_prompt::should_inject(breaker_state)
            && ctx.killswitch.defensive_prompt.enabled
        {
            // Extract channel type from session_id (e.g. "telegram:123" → "telegram")
            let channel_type = session_id.split(':').next().unwrap_or("unknown");
            reply = crate::defensive_prompt::inject_defensive_prompt(
                &reply,
                &ctx.killswitch.defensive_prompt.languages,
                channel_type,
            );
            debug!(session_id, "Defensive prompt injected (circuit breaker HalfOpen)");
        }

        // Save assistant reply to session
        if let Err(e) = session_mgr
            .append_message(session_id, "assistant", &reply, reply_tokens)
            .await
        {
            warn!("Failed to save assistant message to session: {e}");
        }

        // ── Recall Memory (L2): record assistant reply ──
        if let Some(recall) = &ctx.recall_memory {
            let (channel, chat_id) = parse_session_id_parts(session_id);
            let entry = duduclaw_memory::RecallEntry {
                agent_id: agent_id.clone(),
                channel: channel.to_string(),
                chat_id: chat_id.to_string(),
                session_id: session_id.to_string(),
                role: "assistant".to_string(),
                content: reply.clone(),
                source: "interactive".to_string(),
                source_agent: agent_id.clone(),
                token_count: reply_tokens,
                ..Default::default()
            };
            if let Err(e) = recall.record(entry).await {
                warn!("Failed to record assistant reply to recall log: {e}");
            }
        }

        // ── Prediction-driven evolution ──────────────────────────────
        if let Some(pe) = ctx.prediction_engine.as_ref() {
            let pe = pe.clone();
            let gvu = ctx.gvu_loop.clone();
            let user_id_for_pred = user_id.to_string();
            let agent_id_for_pred = agent_id.clone();
            let session_id_for_pred = session_id.to_string();
            let text_clone = text.to_string();
            let reply_clone_for_pred = reply.clone();
            let home_for_pred = ctx.home_dir.clone();
            let agent_dir_for_pred = agent_dir.clone();
            let sm_for_pred = ctx.session_manager.clone();
            let skill_cache_for_pred = ctx.skill_cache.clone();
            let skill_activation_for_pred = ctx.skill_activation.clone();
            let skill_lift_for_pred = ctx.skill_lift.clone();
            let gap_acc_for_pred = ctx.gap_accumulator.clone();
            let sandbox_for_pred = ctx.sandbox_store.clone();
            let notebook_for_pred = ctx.mistake_notebook.clone();

            tokio::spawn(async move {
                // 1. Generate prediction (< 1ms, zero LLM)
                let prediction = pe.predict(&user_id_for_pred, &agent_id_for_pred, &text_clone).await;
                debug!(
                    agent = %agent_id_for_pred,
                    satisfaction = format!("{:.2}", prediction.expected_satisfaction),
                    confidence = format!("{:.2}", prediction.confidence),
                    "Prediction generated"
                );

                // 2. Extract conversation metrics
                let messages = sm_for_pred.get_messages(&session_id_for_pred).await.unwrap_or_default();
                let metrics = crate::prediction::metrics::ConversationMetrics::extract(
                    &session_id_for_pred,
                    &agent_id_for_pred,
                    &user_id_for_pred,
                    &messages,
                    0,
                );

                // 3. Calculate prediction error (embedding ~5ms if available, otherwise < 1ms)
                let (error, embedding) = pe.calculate_error(&prediction, &metrics).await;

                // 3.5 Log evolution event: PredictionError (Sutskever Day 1)
                pe.log_evolution_event(
                    "prediction_error",
                    &agent_id_for_pred,
                    Some(error.composite_error),
                    Some(&format!("{:?}", error.category)),
                    None, None, None,
                );

                // 4. Update user model — pass pre-computed embedding to avoid redundant embed()
                pe.update_model_with_embedding(&metrics, embedding).await;

                // 4.5 Conversation outcome detection + MistakeNotebook (Phase 1 GVU²)
                // Skip for very short conversations (< 4 messages) to avoid false positives (review #28)
                let mut error = error;
                let conv_outcome = if messages.len() >= 4 {
                    Some(crate::prediction::outcome::ConversationOutcome::extract(
                        &session_id_for_pred, &agent_id_for_pred, &messages,
                    ))
                } else {
                    None
                };
                // Apply task completion signal to prediction error
                if let Some(ref outcome) = conv_outcome {
                    let meta = pe.metacognition.lock().await;
                    error.apply_outcome(outcome, &meta.thresholds);
                }
                // Record failure to MistakeNotebook for grounded GVU
                if let Some(ref outcome) = conv_outcome {
                    if outcome.is_failure() {
                        if let Some(ref nb) = notebook_for_pred {
                            let category = match outcome.task_type {
                                crate::prediction::outcome::TaskType::Coding => crate::gvu::mistake_notebook::MistakeCategory::Capability,
                                crate::prediction::outcome::TaskType::QA => crate::gvu::mistake_notebook::MistakeCategory::Factual,
                                _ => crate::gvu::mistake_notebook::MistakeCategory::Behavioral,
                            };
                            let what_wrong = match outcome.satisfaction {
                                crate::prediction::outcome::SatisfactionSignal::Negative => "User expressed dissatisfaction",
                                _ => "Task not completed",
                            };
                            let entry = crate::gvu::mistake_notebook::build_mistake_entry(
                                &agent_id_for_pred,
                                &session_id_for_pred,
                                category,
                                &text_clone,
                                &reply_clone_for_pred,
                                what_wrong,
                                None,
                            );
                            if let Err(e) = nb.record(&entry) {
                                warn!(agent = %agent_id_for_pred, "Failed to record mistake: {e}");
                            }
                        }
                    }
                }

                // 5. Skill lifecycle: diagnose + activate + track lift
                {
                    let compressed: Vec<_> = {
                        let cache = skill_cache_for_pred.lock().await;
                        cache.all().into_iter().cloned().collect()
                    };

                    // Diagnose error and suggest skills
                    if let Some(diagnosis) = crate::skill_lifecycle::diagnostician::diagnose(&error, &compressed) {
                        // Activate suggested skills
                        if !diagnosis.suggested_skills.is_empty() {
                            let mut ctrl = skill_activation_for_pred.lock().await;
                            for skill_name in &diagnosis.suggested_skills {
                                ctrl.activate(&agent_id_for_pred, skill_name, error.composite_error);
                            }
                        }
                        // Report skill gap to evolution engine + accumulate for synthesis
                        if let Some(ref gap) = diagnosis.skill_gap {
                            crate::skill_lifecycle::gap::inject_skill_gap(gap, &home_for_pred, &agent_id_for_pred);

                            // Accumulate gap for potential auto-synthesis
                            let trigger = {
                                let mut acc = gap_acc_for_pred.lock().await;
                                acc.record_gap(&agent_id_for_pred, gap, error.composite_error)
                            };
                            if let Some(trigger) = trigger {
                                info!(
                                    agent = %agent_id_for_pred,
                                    topic = %trigger.topic,
                                    gap_count = trigger.gap_count,
                                    "Skill synthesis trigger fired — queuing synthesis"
                                );
                                // Log synthesis trigger event to feedback.jsonl
                                // Use structured fields to prevent second-order injection via topic
                                let signal = serde_json::json!({
                                    "signal_type": "synthesis_trigger",
                                    "agent_id": &agent_id_for_pred,
                                    "topic": &trigger.topic,
                                    "gap_count": trigger.gap_count,
                                    "avg_composite_error": trigger.avg_composite_error,
                                    "channel": "skill_synthesis",
                                    "timestamp": chrono::Utc::now().to_rfc3339(),
                                });
                                let feedback_path = home_for_pred.join("feedback.jsonl");
                                let feedback_clone = feedback_path.clone();
                                let signal_str = signal.to_string();
                                // Non-blocking write to avoid stalling async runtime
                                tokio::task::spawn_blocking(move || {
                                    use std::io::Write;
                                    if let Err(e) = std::fs::OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open(&feedback_clone)
                                        .and_then(|mut f| writeln!(f, "{}", signal_str))
                                    {
                                        tracing::warn!(
                                            path = %feedback_clone.display(),
                                            error = %e,
                                            "Failed to write synthesis trigger to feedback.jsonl"
                                        );
                                    }
                                });

                                // Mark topic as pending to prevent re-triggering during
                                // async synthesis. Call confirm_synthesis() on success or
                                // cancel_pending() on failure to resume gap accumulation.
                                {
                                    let mut acc = gap_acc_for_pred.lock().await;
                                    acc.mark_pending(&agent_id_for_pred, &trigger.topic);
                                }
                            }
                        }
                    }

                    // Record conversation for activation effectiveness tracking
                    {
                        let mut ctrl = skill_activation_for_pred.lock().await;
                        ctrl.record_conversation(&agent_id_for_pred, error.composite_error);
                    }

                    // Track lift for each skill (active vs inactive)
                    {
                        let active = {
                            let ctrl = skill_activation_for_pred.lock().await;
                            ctrl.get_active(&agent_id_for_pred)
                        };
                        let mut lift_store = skill_lift_for_pred.lock().await;
                        for skill in &compressed {
                            let tracker = lift_store.get_or_create(&agent_id_for_pred, &skill.name);
                            if active.contains(&skill.name) {
                                tracker.record_with(error.composite_error);
                            } else {
                                tracker.record_without(error.composite_error);
                            }
                        }
                    }
                }

                // 6. Periodic: evaluate activations + scan distillation (every ~20 conversations)
                {
                    // Use prediction count as conversation counter (low overhead)
                    let should_evaluate = pe.metacognition.lock().await.total_predictions % 20 == 0;
                    if should_evaluate {
                        // Evaluate and prune ineffective skills
                        let deactivated = {
                            let mut ctrl = skill_activation_for_pred.lock().await;
                            ctrl.evaluate_all(&agent_id_for_pred)
                        };
                        for name in &deactivated {
                            info!(agent = %agent_id_for_pred, skill = %name, "Skill deactivated by effectiveness evaluation");
                        }

                        // Scan for distillation candidates
                        let candidates = {
                            let lift_store = skill_lift_for_pred.lock().await;
                            let trackers = lift_store.get_all(&agent_id_for_pred);
                            crate::skill_lifecycle::distillation::scan_for_distillation(&agent_id_for_pred, &trackers)
                        };
                        for candidate in &candidates {
                            info!(
                                agent = %agent_id_for_pred,
                                skill = %candidate.skill_name,
                                readiness = format!("{:.2}", candidate.readiness),
                                lift = format!("{:.3}", candidate.lift),
                                "Skill ready for distillation into SOUL.md"
                            );
                            // Distillation via GVU would be triggered here in production
                            // (requires async GVU call — deferred to dedicated distillation task)
                        }

                        // Scan for graduation candidates (cross-agent migration)
                        {
                            let lift_store = skill_lift_for_pred.lock().await;
                            let trackers = lift_store.get_all(&agent_id_for_pred);
                            let criteria = crate::skill_lifecycle::graduation::GraduationCriteria::default();
                            for tracker in &trackers {
                                if let Some(candidate) = crate::skill_lifecycle::graduation::check_graduation(tracker, &criteria) {
                                    info!(
                                        agent = %agent_id_for_pred,
                                        skill = %candidate.skill_name,
                                        lift = format!("{:.3}", candidate.lift),
                                        "Skill eligible for graduation to global scope"
                                    );
                                }
                            }
                        }

                        // Evaluate sandbox trials
                        // Lock ordering: collect data from each lock independently,
                        // never hold lift_store and sandbox_store simultaneously.
                        {
                            let sandbox_names = {
                                let store = sandbox_for_pred.lock().await;
                                store.active_names(&agent_id_for_pred)
                            };

                            // Collect tracker snapshots (lift data) — release lift_store before sandbox
                            let tracker_snapshots: Vec<_> = {
                                let lift_store = skill_lift_for_pred.lock().await;
                                sandbox_names.iter().filter_map(|name| {
                                    lift_store.get_all(&agent_id_for_pred)
                                        .into_iter()
                                        .find(|t| t.skill_name == *name)
                                        .map(|t| (name.clone(), t.clone()))
                                }).collect()
                            }; // lift_store released here

                            for (name, tracker) in &tracker_snapshots {
                                let sandboxed = {
                                    let store = sandbox_for_pred.lock().await;
                                    store.get(&agent_id_for_pred, name).cloned()
                                };
                                if let Some(sandboxed) = sandboxed {
                                    let outcome = crate::skill_lifecycle::sandbox_trial::evaluate_trial(tracker, &sandboxed);
                                    match outcome.decision {
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::Graduate => {
                                            info!(agent = %agent_id_for_pred, skill = %name, "Sandbox trial → GRADUATE");
                                            let mut store = sandbox_for_pred.lock().await;
                                            store.graduate(&agent_id_for_pred, name);
                                        }
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::Discard => {
                                            info!(agent = %agent_id_for_pred, skill = %name, reason = %outcome.reason, "Sandbox trial → DISCARD");
                                            let mut store = sandbox_for_pred.lock().await;
                                            store.discard(&agent_id_for_pred, name);
                                            let mut ctrl = skill_activation_for_pred.lock().await;
                                            ctrl.deactivate(&agent_id_for_pred, name);
                                        }
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::ExtendTrial(extra) => {
                                            if extra > 0 {
                                                let mut store = sandbox_for_pred.lock().await;
                                                store.extend_ttl(&agent_id_for_pred, name, extra);
                                            }
                                        }
                                    }
                                }
                            }
                            // Tick all sandbox TTLs
                            let mut store = sandbox_for_pred.lock().await;
                            store.tick_agent(&agent_id_for_pred);
                        }
                    }
                }

                // 7. Route to evolution action (with hardening: ε-floor + anti-sycophancy)
                // Snapshot consistency first, then lock exploration (audit #1: avoid dual mutex)
                let consecutive = pe.consecutive_significant_count(&agent_id_for_pred).await;
                let consistency_snapshot = pe.consistency.lock().await.clone();
                let action = {
                    let mut exploration = pe.exploration.lock().await;
                    crate::prediction::router::route(&error, consecutive, &mut exploration, &consistency_snapshot)
                };

                match action {
                    crate::prediction::router::EvolutionAction::None => {}
                    crate::prediction::router::EvolutionAction::StoreEpisodic { content, importance } => {
                        let preview: String = content.chars().take(80).collect();
                        debug!(agent = %agent_id_for_pred, "Storing episodic observation: {preview}");

                        // Persist to per-agent memory.db
                        let mem_dir = home_for_pred.join("agents").join(&agent_id_for_pred).join("state");
                        if let Err(e) = std::fs::create_dir_all(&mem_dir) {
                            warn!(agent = %agent_id_for_pred, "Failed to create memory state dir: {e}");
                        } else {
                            let db_path = mem_dir.join("memory.db");
                            match duduclaw_memory::engine::SqliteMemoryEngine::new(&db_path) {
                                Ok(engine) => {
                                    let entry = duduclaw_core::types::MemoryEntry {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        agent_id: agent_id_for_pred.clone(),
                                        content,
                                        timestamp: chrono::Utc::now(),
                                        tags: vec![],
                                        embedding: None,
                                        layer: duduclaw_core::types::MemoryLayer::Episodic,
                                        importance,
                                        access_count: 0,
                                        last_accessed: None,
                                        source_event: "prediction_episodic".to_string(),
                                    };
                                    if let Err(e) = engine.store(&agent_id_for_pred, entry).await {
                                        warn!(agent = %agent_id_for_pred, "Failed to store episodic memory: {e}");
                                    }
                                }
                                Err(e) => {
                                    warn!(agent = %agent_id_for_pred, "Failed to open memory db: {e}");
                                }
                            }
                        }
                    }
                    crate::prediction::router::EvolutionAction::TriggerReflection { ref context }
                    | crate::prediction::router::EvolutionAction::TriggerEmergencyEvolution { ref context } => {
                        let is_emergency = matches!(
                            action,
                            crate::prediction::router::EvolutionAction::TriggerEmergencyEvolution { .. }
                        );
                        if is_emergency {
                            warn!(agent = %agent_id_for_pred, error = format!("{:.3}", error.composite_error), "Critical prediction error → emergency evolution");
                        } else {
                            info!(agent = %agent_id_for_pred, error = format!("{:.3}", error.composite_error), "Prediction error → triggering reflection");
                        }

                        // Log evolution event: GVU trigger (Sutskever Day 1)
                        let etype = if context.contains("Epistemic Foraging") {
                            "epistemic_foraging"
                        } else if context.contains("Anti-Sycophancy") {
                            "sycophancy_alert"
                        } else {
                            "gvu_trigger"
                        };
                        pe.log_evolution_event(
                            etype,
                            &agent_id_for_pred,
                            Some(error.composite_error),
                            Some(&format!("{:?}", error.category)),
                            Some(&context.chars().take(500).collect::<String>()),
                            None, None,
                        );

                        // Run GVU loop if available
                        if let (Some(gvu), Some(dir)) = (&gvu, &agent_dir_for_pred) {
                            let contract = duduclaw_agent::contract::load_contract(dir);
                            let pre_metrics = crate::gvu::version_store::VersionMetrics::default();
                            let home = home_for_pred.clone();

                            // LLM caller: uses claude CLI
                            let call_llm = |prompt: String| {
                                let h = home.clone();
                                async move {
                                    crate::channel_reply::call_claude_cli_public(
                                        &prompt, "claude-haiku-4-5", "", &h,
                                    ).await
                                }
                            };

                            // Query MistakeNotebook for grounded generation context
                            let relevant_mistakes = notebook_for_pred
                                .as_ref()
                                .map(|nb| nb.query_by_agent(&agent_id_for_pred, 5))
                                .unwrap_or_default();

                            // Get MetaCognition snapshot for adaptive depth
                            let meta_snapshot = pe.metacognition.lock().await.clone();

                            let outcome = gvu.run_with_context(
                                &agent_id_for_pred,
                                dir,
                                &context,
                                pre_metrics,
                                &contract.boundaries.must_not,
                                &contract.boundaries.must_always,
                                call_llm,
                                Some(&meta_snapshot),
                                relevant_mistakes,
                            ).await;

                            // Log outcome and feed back to metacognition
                            match outcome {
                                crate::gvu::loop_::GvuOutcome::Applied(ref version) => {
                                    info!(
                                        agent = %agent_id_for_pred,
                                        version = %version.version_id,
                                        "GVU applied SOUL.md change"
                                    );
                                    let mut meta = pe.metacognition.lock().await;
                                    meta.record_outcome(error.category, true);
                                }
                                crate::gvu::loop_::GvuOutcome::Abandoned { ref last_gradient } => {
                                    warn!(
                                        agent = %agent_id_for_pred,
                                        critique = %last_gradient.critique,
                                        "GVU abandoned all attempts"
                                    );
                                    let mut meta = pe.metacognition.lock().await;
                                    meta.record_outcome(error.category, false);
                                }
                                crate::gvu::loop_::GvuOutcome::Skipped { ref reason } => {
                                    debug!(agent = %agent_id_for_pred, reason, "GVU skipped");
                                    if !reason.contains("observation") {
                                        let mut meta = pe.metacognition.lock().await;
                                        meta.record_outcome(error.category, false);
                                    }
                                }
                                crate::gvu::loop_::GvuOutcome::Deferred { retry_count, retry_after_hours, .. } => {
                                    info!(
                                        agent = %agent_id_for_pred,
                                        retry_count,
                                        retry_after_hours,
                                        "GVU deferred — will retry with accumulated gradients"
                                    );
                                    // Don't record as outcome yet — will be evaluated on retry
                                }
                                crate::gvu::loop_::GvuOutcome::TimedOut { elapsed, generations_completed, .. } => {
                                    warn!(
                                        agent = %agent_id_for_pred,
                                        elapsed_secs = elapsed.as_secs(),
                                        generations_completed,
                                        "GVU timed out — wall-clock budget exceeded"
                                    );
                                    // Treat as inconclusive — don't record outcome
                                }
                            }
                        } else {
                            warn!(
                                agent = %agent_id_for_pred,
                                "Evolution triggered but GVU loop not available — skipping"
                            );
                        }
                    }
                }
            });
        }

        // ── Wiki ingest (async, non-blocking) ────────────────────
        {
            let user_text_for_wiki = sanitized_text.clone();
            let reply_for_wiki = reply.clone();
            let agent_id_for_wiki = agent_id.clone();
            let session_for_wiki = session_id.to_string();
            let home_for_wiki = ctx.home_dir.clone();
            tokio::spawn(async move {
                crate::wiki_ingest::run_ingest(
                    &user_text_for_wiki,
                    &reply_for_wiki,
                    &agent_id_for_wiki,
                    &session_for_wiki,
                    &home_for_wiki,
                ).await;
            });
        }

        // ── Phase 3: Record trajectory for skill extraction ──────
        // Start or continue recording the conversation trajectory.
        // Recording is finalized when the next user message contains
        // positive/negative feedback (see "within 2 turns" check above).
        {
            let session_key = format!("{session_id}:{agent_id}");
            let mut recorder = ctx.skill_recorder.lock().await;
            if !recorder.is_recording(&session_key) {
                recorder.start(&session_key, &agent_id);
                recorder.record_turn(&session_key, "user", text, vec![]);
            }
            // Record the assistant reply turn
            // Tool names are not available here (streamed via CLI), so empty for now.
            // Future: parse tool_use events from streaming and pass them through.
            recorder.record_turn(&session_key, "assistant", &reply, vec![]);
        }

        // Check if compression needed; generate Claude summary then compress in background
        let sm = ctx.session_manager.clone();
        let sid = session_id.to_string();
        let home_for_compress = ctx.home_dir.clone();
        tokio::spawn(async move {
            if sm.should_compress(&sid).await {
                // Gather last messages to summarise
                let msgs = sm.get_messages(&sid).await.unwrap_or_default();
                let transcript = {
                    let mut buf = String::with_capacity(msgs.len() * 350);
                    for m in &msgs {
                        if !buf.is_empty() { buf.push('\n'); }
                        use std::fmt::Write;
                        let _ = write!(buf, "[{}] {}", m.role, &m.content[..m.content.len().min(300)]);
                    }
                    buf
                };
                let prompt = format!(
                    "Summarize the following conversation history concisely for use as context \
                     in future turns. Include key facts, decisions, and outcomes. Max 400 words.\n\n{transcript}"
                );
                let summary = match call_claude_cli(&prompt, "claude-haiku-4-5", "", &home_for_compress, None, None, None).await {
                    Ok(s) => s,
                    Err(_) => "[Session compressed — previous conversation summary omitted for brevity]".to_string(),
                };
                if let Err(e) = sm.compress(&sid, &summary).await {
                    warn!("Session compression failed: {e}");
                }
            }
        });

        return reply;
    }

    // 3. Fallback: classified error message
    let reg = ctx.registry.read().await;
    let name = reg
        .main_agent()
        .map(|a| a.config.agent.display_name.clone())
        .unwrap_or_else(|| "DuDuClaw".to_string());
    drop(reg);

    let err_str = last_cli_error.clone().unwrap_or_else(|| "No error info".to_string());
    let reason = classify_cli_failure(&err_str);
    warn!(
        agent = %name,
        reason = ?reason,
        last_error = %err_str.chars().take(200).collect::<String>(),
        "Channel reply fallback — all providers failed"
    );

    // Append a structured audit line so the dashboard can surface failure trends.
    let audit = serde_json::json!({
        "event": "channel_reply_fallback",
        "agent": name,
        "session_id": session_id,
        "reason": format!("{reason:?}"),
        "error": err_str.chars().take(300).collect::<String>(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Ok(line) = serde_json::to_string(&audit) {
        let path = ctx.home_dir.join("channel_failures.jsonl");
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            use tokio::io::AsyncWriteExt;
            let _ = f.write_all(format!("{line}\n").as_bytes()).await;
        }
    }

    format_fallback_message(&name, reason)
}

/// Classified failure category for `claude` CLI / Python SDK calls.
///
/// Drives the user-facing fallback message so we tell the user *why*
/// it actually failed (rate limit, timeout, etc.) rather than always
/// suggesting they re-run `claude auth status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureReason {
    /// `claude` binary was not found on the filesystem.
    BinaryMissing,
    /// All rotator accounts exhausted due to rate-limit / usage-limit / 429.
    RateLimited,
    /// Billing / credit exhausted (402, insufficient_quota).
    Billing,
    /// Claude CLI reported "Not logged in" / authentication failure.
    /// Distinct from BinaryMissing (binary exists, just not authenticated).
    AuthFailed,
    /// 30-minute hard timeout tripped.
    Timeout,
    /// Subprocess failed to spawn or exited non-zero without recognizable cause.
    SpawnError,
    /// CLI returned empty output after trimming.
    EmptyResponse,
    /// No rotator accounts configured.
    NoAccounts,
    /// CLI failed but local model replied successfully.
    /// Contains the original CLI error for user transparency.
    LocalModelFallback(String),
    /// Fallback — unrecognized error string.
    Unknown,
}

/// Classify an error string produced by `call_claude_cli_rotated` or `call_python_sdk_v2`.
pub(crate) fn classify_cli_failure(err: &str) -> FailureReason {
    let lower = err.to_lowercase();

    if lower.contains("claude cli not found") {
        return FailureReason::BinaryMissing;
    }
    // Auth failures come through the stream-json `is_error` branch as
    // "claude CLI stream error: Not logged in · Please run /login" or
    // "claude CLI assistant error: authentication_failed".
    if lower.contains("not logged in")
        || lower.contains("authentication_failed")
        || lower.contains("please run /login")
    {
        return FailureReason::AuthFailed;
    }
    if lower.contains("hard timeout") {
        return FailureReason::Timeout;
    }
    if lower.contains("empty response") {
        return FailureReason::EmptyResponse;
    }
    if lower.contains("no accounts") || lower.contains("no account configured") {
        return FailureReason::NoAccounts;
    }
    // Reuse the shared billing/rate classifiers so we stay in sync with claude_runner.
    if crate::claude_runner::is_billing_error(err) {
        return FailureReason::Billing;
    }
    if crate::claude_runner::is_rate_limit_error(err) {
        return FailureReason::RateLimited;
    }
    if lower.contains("spawn error")
        || lower.contains("no such file")
        || lower.contains("exit ")
        || lower.contains("read error")
    {
        return FailureReason::SpawnError;
    }
    FailureReason::Unknown
}

/// Build a zh-TW user-facing message for a classified failure.
///
/// Messages directly tell the user *why* CLI failed (rate limit, billing, etc.)
/// and whether a local model fallback was used.
pub(crate) fn format_fallback_message(agent_name: &str, reason: FailureReason) -> String {
    match reason {
        FailureReason::BinaryMissing => format!(
            "{agent_name} 暫時無法回應：系統找不到 Claude Code CLI。\n\
             請確認已安裝，並執行：\n\
             $ claude auth status"
        ),
        FailureReason::AuthFailed => format!(
            "{agent_name} 無法回應：Claude Code 未登入或認證失效。\n\
             請在終端執行：\n\
             $ claude /login\n\
             登入完成後，可繼續對我說話。"
        ),
        FailureReason::RateLimited => format!(
            "{agent_name} 暫時忙線中（API 使用量已達上限），請稍後再試。\n\
             系統會在背景自動偵測恢復，屆時將自動切回 Claude。\n\
             若持續發生，可在儀表板加入備用 OAuth 帳號以啟用自動輪替。"
        ),
        FailureReason::Billing => format!(
            "{agent_name} 無法回應：目前帳號額度已用完。\n\
             請於 Anthropic Console 儲值，或在儀表板切換到其他有效帳號。"
        ),
        FailureReason::Timeout => format!(
            "{agent_name} 這次處理超時（已達 30 分鐘安全上限）。\n\
             請重新送出訊息，或將任務拆成較小的步驟。"
        ),
        FailureReason::SpawnError => format!(
            "{agent_name} 啟動 Claude Code 子程序失敗。\n\
             請查看 ~/.duduclaw/debug.log 取得詳細錯誤。"
        ),
        FailureReason::EmptyResponse => format!(
            "{agent_name} 這次沒有回覆內容（空回應）。\n\
             請重送訊息；若持續發生請回報。"
        ),
        FailureReason::NoAccounts => format!(
            "{agent_name} 目前沒有可用的 Claude 帳號。\n\
             請先到儀表板設定 OAuth 或 API Key。"
        ),
        FailureReason::LocalModelFallback(ref cli_err) => {
            let reason_hint = classify_cli_error_hint(cli_err);
            format!(
                "⚠️ Claude CLI 暫時不可用（{reason_hint}），本次由本地模型代為回應。\n\
                 系統會在背景自動偵測 CLI 恢復，屆時將自動切回 Claude。"
            )
        }
        FailureReason::Unknown => format!(
            "{agent_name} 暫時無法回應。\n\
             請稍後再試，或查看 ~/.duduclaw/debug.log 取得詳細原因。"
        ),
    }
}

/// Translate a raw CLI error into a short zh-TW hint for the user.
fn classify_cli_error_hint(err: &str) -> &'static str {
    let reason = classify_cli_failure(err);
    match reason {
        FailureReason::RateLimited => "使用量已達上限",
        FailureReason::Billing => "帳號額度用完",
        FailureReason::AuthFailed => "認證失效",
        FailureReason::Timeout => "處理超時",
        FailureReason::EmptyResponse => "空回應",
        FailureReason::BinaryMissing => "CLI 未安裝",
        FailureReason::NoAccounts => "無可用帳號",
        FailureReason::SpawnError => "程序啟動失敗",
        _ => "連線異常",
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    fn classify_rate_limit_variants() {
        assert_eq!(classify_cli_failure("Error 429 rate limit reached"), FailureReason::RateLimited);
        assert_eq!(classify_cli_failure("usage limit exceeded"), FailureReason::RateLimited);
        assert_eq!(classify_cli_failure("All accounts exhausted. Last error: overloaded"), FailureReason::RateLimited);
    }

    #[test]
    fn classify_billing_variants() {
        assert_eq!(classify_cli_failure("insufficient_quota credit balance"), FailureReason::Billing);
        assert_eq!(classify_cli_failure("HTTP 402 payment required"), FailureReason::Billing);
    }

    #[test]
    fn classify_timeout() {
        assert_eq!(
            classify_cli_failure("claude CLI hard timeout (1800s, no output)"),
            FailureReason::Timeout
        );
    }

    #[test]
    fn classify_binary_missing() {
        assert_eq!(classify_cli_failure("claude CLI not found in PATH"), FailureReason::BinaryMissing);
    }

    #[test]
    fn classify_empty_response() {
        assert_eq!(classify_cli_failure("Empty response from claude CLI"), FailureReason::EmptyResponse);
    }

    /// Regression lock: v1.3.13 added diagnostic suffixes to Empty / exit
    /// errors. The classifier's substring match must still identify the
    /// reason so user-facing messages stay specific.
    #[test]
    fn classify_empty_response_with_diagnostic_suffix() {
        let err = "Empty response from claude CLI (exit=0 lines=42 events=30 \
                   assistant=2 text_blocks=0 thinking=1 tool_use=0 result_events=1 \
                   result_subtype=Some(\"success\") stop_reason=Some(\"tool_use\") \
                   last_line=\"{\\\"type\\\":\\\"result\\\"...}\" stderr_tail=\"\")";
        assert_eq!(classify_cli_failure(err), FailureReason::EmptyResponse);
    }

    #[test]
    fn classify_exit_code_with_diagnostic_suffix() {
        let err = "claude CLI exit 1 (exit=1 lines=3 events=2 \
                   assistant=0 text_blocks=0 thinking=0 tool_use=0 result_events=0 \
                   result_subtype=None stop_reason=None last_line=\"\" stderr_tail=\"\")";
        assert_eq!(classify_cli_failure(err), FailureReason::SpawnError);
    }

    #[test]
    fn classify_spawn_error() {
        assert_eq!(classify_cli_failure("claude CLI spawn error: No such file"), FailureReason::SpawnError);
        assert_eq!(classify_cli_failure("claude CLI exit 127"), FailureReason::SpawnError);
    }

    #[test]
    fn classify_unknown_fallthrough() {
        assert_eq!(classify_cli_failure("some weird unrelated thing"), FailureReason::Unknown);
    }

    #[test]
    fn classify_auth_failed_variants() {
        // Stream-json error path — what channel_reply surfaces after the fix.
        assert_eq!(
            classify_cli_failure("claude CLI stream error: Not logged in · Please run /login"),
            FailureReason::AuthFailed
        );
        // Assistant event error field path.
        assert_eq!(
            classify_cli_failure("claude CLI assistant error: authentication_failed"),
            FailureReason::AuthFailed
        );
        // Raw "please run /login" text without the prefix.
        assert_eq!(
            classify_cli_failure("Please run /login to authenticate"),
            FailureReason::AuthFailed
        );
    }

    #[test]
    fn message_auth_failed_tells_user_to_login() {
        let msg = format_fallback_message("Agnes", FailureReason::AuthFailed);
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("未登入") || msg.contains("認證失效"));
        assert!(msg.contains("/login"));
        // Must NOT say "claude auth status" (that's the BinaryMissing hint
        // and doesn't fix an auth problem on its own).
        assert!(!msg.contains("auth status"));
    }

    #[test]
    fn message_rate_limited_contains_busy_string_not_auth_status() {
        let msg = format_fallback_message("Agnes", FailureReason::RateLimited);
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("忙線中"));
        assert!(!msg.contains("auth status"));
    }

    #[test]
    fn message_binary_missing_keeps_auth_status_hint() {
        let msg = format_fallback_message("Agnes", FailureReason::BinaryMissing);
        assert!(msg.contains("找不到 Claude Code"));
        assert!(msg.contains("auth status"));
    }

    #[test]
    fn message_timeout_mentions_30_min() {
        let msg = format_fallback_message("Agnes", FailureReason::Timeout);
        assert!(msg.contains("30 分鐘"));
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use duduclaw_agent::account_rotator::{
        Account, AccountRotator, AuthMethod, RotationStrategy,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Build a synthetic OAuth account for testing.
    ///
    /// Sets `credentials_dir` to a fake path so `is_available()` returns true
    /// without needing real keychain state.
    fn fake_oauth_account(id: &str, priority: u32) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            priority,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "test".to_string(),
            email: format!("{id}@example.com"),
            subscription: "pro".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: Some(format!("tok_{id}")),
            credentials_dir: Some(PathBuf::from(format!("/tmp/fake/{id}"))),
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
        }
    }

    /// Scenario: first account rate-limited, second succeeds.
    ///
    /// Verifies:
    /// 1. rotate_cli_spawn advances to the second account after a rate-limit error
    /// 2. first account is placed in cooldown via on_rate_limited
    /// 3. successful result is returned from the second account
    #[tokio::test]
    async fn rotation_advances_past_rate_limited_account() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        // Lower priority number = selected first under Priority strategy.
        rotator.push_account_for_test(fake_oauth_account("first", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("second", 2)).await;
        assert_eq!(rotator.count().await, 2);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_cloned = call_count.clone();

        let result = rotate_cli_spawn(
            &rotator,
            move |env_vars| {
                let n = call_count_cloned.fetch_add(1, Ordering::SeqCst);
                // First attempt: simulate rate limit.
                // Second attempt: return success with a distinctive body.
                async move {
                    // Sanity: env_vars should contain OAuth token for the selected account.
                    assert!(env_vars.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
                    if n == 0 {
                        Err("Error 429 rate limit reached".to_string())
                    } else {
                        Ok("hello from second".to_string())
                    }
                }
            },
            100,
        )
        .await;

        assert_eq!(result.as_deref(), Ok("hello from second"));
        assert_eq!(call_count.load(Ordering::SeqCst), 2, "both accounts should be tried");

        // First account should now be unavailable (cooldown), second still healthy.
        let statuses = rotator.status().await;
        let first = statuses.iter().find(|s| s.id == "first").unwrap();
        let second = statuses.iter().find(|s| s.id == "second").unwrap();
        assert!(!first.is_available, "first account should be in cooldown after rate-limit");
        assert!(second.is_available, "second account should remain available");
        assert_eq!(second.total_requests, 1, "second account should have one success recorded");
    }

    /// Scenario: both accounts fail with the same error.
    ///
    /// Verifies:
    /// 1. Both accounts are exercised
    /// 2. Final Err carries the last underlying error string (not a generic message)
    /// 3. The error is classifiable (so the fallback message will be specific)
    #[tokio::test]
    async fn rotation_all_fail_propagates_last_error() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("a", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("b", 2)).await;

        let result = rotate_cli_spawn(
            &rotator,
            |_env_vars| async move {
                Err::<String, _>("claude CLI hard timeout (1800s, no output)".to_string())
            },
            100,
        )
        .await;

        let err = result.expect_err("should fail when all accounts fail");
        assert!(err.contains("All accounts exhausted"), "expected aggregator prefix, got: {err}");
        assert!(
            err.contains("hard timeout"),
            "expected last error to be propagated, got: {err}"
        );

        // Extracted error must still be classifiable as Timeout (not Unknown).
        assert_eq!(classify_cli_failure(&err), FailureReason::Timeout);
    }

    /// Scenario: billing-exhausted error places the account on a 24h cooldown.
    #[tokio::test]
    async fn rotation_billing_error_triggers_long_cooldown() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("broke", 1)).await;

        let result = rotate_cli_spawn(
            &rotator,
            |_env_vars| async move {
                Err::<String, _>("HTTP 402 insufficient_quota credit balance".to_string())
            },
            100,
        )
        .await;

        assert!(result.is_err());
        let statuses = rotator.status().await;
        let broke = &statuses[0];
        assert!(!broke.is_healthy, "billing-exhausted account should be marked unhealthy");
        assert!(!broke.is_available, "should be unavailable during 24h cooldown");
    }

    /// T4.7 smoke replacement: single good OAuth account — no regression.
    ///
    /// When exactly one healthy account exists and the spawn closure succeeds
    /// immediately, we should return that response on the first attempt and
    /// record success.
    #[tokio::test]
    async fn single_account_success_is_first_try() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("only", 1)).await;

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_cloned = attempts.clone();

        let result = rotate_cli_spawn(
            &rotator,
            move |_env_vars| {
                attempts_cloned.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<String, String>("OK".to_string()) }
            },
            50,
        )
        .await;

        assert_eq!(result.as_deref(), Ok("OK"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let status = &rotator.status().await[0];
        assert_eq!(status.total_requests, 1);
        assert!(status.is_available);
    }

    /// T4.9 smoke replacement: forced rate-limit → user sees 忙線中 message.
    ///
    /// End-to-end path from spawn failure → rotator exhaustion → error
    /// propagation → `classify_cli_failure` → `format_fallback_message`.
    /// Asserts the user-facing text is the RateLimited variant, not
    /// the misleading BinaryMissing "please install and auth" hint.
    #[tokio::test]
    async fn end_to_end_rate_limit_yields_busy_message() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("one", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("two", 2)).await;

        let result = rotate_cli_spawn(
            &rotator,
            |_env_vars| async move {
                Err::<String, _>("Error 429 rate limit: usage limit exceeded".to_string())
            },
            50,
        )
        .await;

        let err = result.expect_err("should fail");
        let reason = classify_cli_failure(&err);
        assert_eq!(reason, FailureReason::RateLimited);

        let user_msg = format_fallback_message("Agnes", reason);
        assert!(user_msg.contains("Agnes"));
        assert!(user_msg.contains("忙線中"), "must say busy: {user_msg}");
        assert!(
            !user_msg.contains("auth status"),
            "must NOT suggest re-running auth status on rate limit: {user_msg}"
        );
        assert!(
            !user_msg.contains("找不到"),
            "must NOT say 'binary not found' on rate limit: {user_msg}"
        );
    }

    /// Regression test for the v1.3.12 bug: stream parser used to
    /// swallow `is_error: true` result events as valid text, which led
    /// to "Not logged in · Please run /login" being delivered to users
    /// as Agnes's reply. After the fix, `spawn_claude_cli_with_env`
    /// returns `Err("claude CLI stream error: Not logged in ...")` and
    /// the classifier + message builder surface the AuthFailed reason.
    ///
    /// We exercise the rotator→classifier→message pipeline by having the
    /// spawn closure return exactly the error shape the new stream parser
    /// now produces.
    #[tokio::test]
    async fn end_to_end_not_logged_in_yields_auth_failed_message() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("broken", 1)).await;

        let result = rotate_cli_spawn(
            &rotator,
            |_env_vars| async move {
                Err::<String, _>(
                    "claude CLI stream error: Not logged in · Please run /login".to_string(),
                )
            },
            50,
        )
        .await;

        let err = result.expect_err("auth failure must surface as Err");
        let reason = classify_cli_failure(&err);
        assert_eq!(reason, FailureReason::AuthFailed);

        let msg = format_fallback_message("Agnes", reason);
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("/login"));
        assert!(
            !msg.contains("Not logged in · Please run /login"),
            "user-facing message must be our zh-TW explanation, not raw CLI text"
        );
    }

    /// T4.8 smoke replacement: empty-rotator → `call_claude_cli_rotated`
    /// fresh-install passthrough. We can't actually spawn `claude`, but the
    /// primitive behaviour of "empty rotator returns exhausted-Err" is
    /// verified below; the outer function's fall-through to
    /// `call_claude_cli` is a one-liner trivially correct by inspection.
    #[tokio::test]
    async fn rotation_empty_rotator_returns_empty_exhausted() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        assert_eq!(rotator.count().await, 0);

        let result = rotate_cli_spawn(
            &rotator,
            |_env_vars| async move { Ok::<String, String>("never called".to_string()) },
            100,
        )
        .await;

        let err = result.expect_err("empty rotator should return err from primitive");
        assert!(err.contains("All accounts exhausted"));
        // Last error is empty because no attempt was made
        assert!(err.ends_with("Last error: "));
    }
}

// ── Python SDK subprocess ───────────────────────────────────

// ── Claude Code SDK (claude CLI) ────────────────────────────

// ── Streaming progress types ───────────────────────────────

/// Progress events emitted during Claude CLI streaming.
///
/// Sent to the channel via callback so users see real-time progress
/// instead of silence during long-running agentic tasks.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// Periodic keepalive — no new stream-json events for `keepalive_interval`.
    Keepalive,
    /// Claude is using a tool (parsed from stream-json `tool_use` content block).
    ToolUse {
        tool: String,
        /// Optional file path or search pattern extracted from tool input.
        detail: Option<String>,
    },
}

impl ProgressEvent {
    /// Format as a user-facing progress message.
    pub fn to_display(&self) -> String {
        match self {
            Self::Keepalive => "⏳ 仍在處理中…".to_string(),
            Self::ToolUse { tool, detail } => {
                let action = match tool.as_str() {
                    "Read" | "read" => "正在讀取",
                    "Write" | "write" => "正在撰寫",
                    "Edit" | "edit" => "正在編輯",
                    "Grep" | "grep" | "search" => "正在搜尋",
                    "Glob" | "glob" => "正在搜尋檔案",
                    "Bash" | "bash" => "正在執行指令",
                    _ => "正在使用工具",
                };
                match detail {
                    Some(d) => format!("⏳ {action} {d}…"),
                    None => format!("⏳ {action}…"),
                }
            }
        }
    }
}

/// Callback type for sending progress events to the channel.
///
/// The callback is `Send + Sync` so it can be invoked from the streaming loop.
/// Implementations should be lightweight (just enqueue a message send).
pub type ProgressCallback = Box<dyn Fn(ProgressEvent) + Send + Sync>;

/// Keepalive interval — send progress if no stream-json events for this long.
pub(crate) const KEEPALIVE_INTERVAL_SECS: u64 = 90;

/// Hard max timeout — absolute safety net to kill truly hung processes.
const HARD_MAX_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes

/// Internal wrapper for GVU loop LLM calls.
/// Restricted to crate-internal use with model allowlist.
const ALLOWED_EVOLUTION_MODELS: &[&str] = &["claude-haiku-4-5", "claude-haiku-4-5-20250307"];

pub(crate) async fn call_claude_cli_public(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    if !ALLOWED_EVOLUTION_MODELS.contains(&model) {
        return Err(format!("Model '{model}' not allowed for evolution calls"));
    }
    // Use account-rotated path so GVU benefits from multi-account failover
    // instead of failing silently when the ambient account is rate-limited.
    call_claude_cli_rotated(user_message, model, system_prompt, home_dir, None, None, None).await
}

/// Call the `claude` CLI (Claude Code SDK) with streaming output.
///
/// Uses `--output-format stream-json --verbose` to read incremental events.
/// Instead of killing on idle, sends keepalive progress to the channel via
/// `on_progress` callback. A hard max timeout (30 min) acts as safety net.
///
/// Thin wrapper around [`spawn_claude_cli_with_env`] that uses the ambient
/// environment (and any configured `ANTHROPIC_API_KEY` as fallback). This is
/// the no-rotation path — used by compression and GVU reflection helpers.
/// The main channel-reply path goes through [`call_claude_cli_rotated`].
async fn call_claude_cli(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Result<String, String> {
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    spawn_claude_cli_with_env(
        user_message, model, system_prompt, home_dir, work_dir,
        on_progress, capabilities, &empty,
    ).await
}

/// Try the `claude` CLI with rotation across configured `AccountRotator` accounts.
///
/// On each attempt the rotator selects an account and yields its env vars
/// (`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`, or `ANTHROPIC_API_KEY`).
/// Classifies failures and feeds them back to the rotator so unhealthy
/// accounts cool down correctly. Falls through to the non-rotated path
/// when no accounts are configured (fresh-install passthrough).
pub(crate) async fn call_claude_cli_rotated(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Result<String, String> {
    let rotator = match crate::claude_runner::get_rotator_cached(home_dir).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Rotator unavailable — falling back to non-rotated CLI path");
            return call_claude_cli(
                user_message, model, system_prompt, home_dir, work_dir,
                on_progress, capabilities,
            ).await;
        }
    };

    let account_count = rotator.count().await;
    if account_count == 0 {
        // Fresh install — no accounts configured. Use ambient env.
        return call_claude_cli(
            user_message, model, system_prompt, home_dir, work_dir,
            on_progress, capabilities,
        ).await;
    }

    // Delegate to the testable primitive with a closure that actually spawns the CLI.
    let input_len = user_message.len();
    rotate_cli_spawn(&rotator, move |env_vars| {
        let user_message = user_message.to_string();
        let model = model.to_string();
        let system_prompt = system_prompt.to_string();
        let home_dir = home_dir.to_path_buf();
        let work_dir = work_dir.map(|p| p.to_path_buf());
        let on_progress = on_progress;
        let capabilities = capabilities.cloned();
        async move {
            spawn_claude_cli_with_env(
                &user_message, &model, &system_prompt, &home_dir,
                work_dir.as_deref(), on_progress, capabilities.as_ref(), &env_vars,
            ).await
        }
    }, input_len).await
}

/// Rotation-loop primitive, decoupled from the actual subprocess spawn.
///
/// Iterates `rotator.select()` up to `rotator.count()` times. For each
/// selected account, calls the provided `spawn` closure with the env-var
/// map. On success, records cost telemetry and returns. On failure,
/// classifies the error and feeds it back to the rotator (`on_billing_exhausted`,
/// `on_rate_limited`, or `on_error`). Returns the last error when all
/// accounts are exhausted.
///
/// `input_size_hint` is used for rough API-key cost accounting when the
/// spawn closure doesn't extract token usage from the CLI stream.
pub(crate) async fn rotate_cli_spawn<F, Fut>(
    rotator: &duduclaw_agent::account_rotator::AccountRotator,
    spawn: F,
    input_size_hint: usize,
) -> Result<String, String>
where
    F: Fn(std::collections::HashMap<String, String>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let account_count = rotator.count().await;
    let max_attempts = account_count.max(1);
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let Some(selected) = rotator.select().await else {
            break;
        };
        info!(account = %selected.id, attempt, "Channel CLI attempt");

        match spawn(selected.env_vars.clone()).await {
            Ok(text) => {
                // Channel calls don't extract token usage from streams, so cost
                // is 0 (OAuth subscription) or a rough estimate (API key).
                let cost = if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                    0
                } else {
                    ((input_size_hint + text.len()) / 1000).max(1) as u64
                };
                rotator.on_success(&selected.id, cost).await;
                return Ok(text);
            }
            Err(e) => {
                last_error = e.clone();
                if crate::claude_runner::is_billing_error(&e) {
                    warn!(account = %selected.id, error = %e, "Account billing exhausted — 24h cooldown");
                    rotator.on_billing_exhausted(&selected.id).await;
                } else if crate::claude_runner::is_rate_limit_error(&e) {
                    warn!(account = %selected.id, error = %e, "Account rate-limited — cooldown");
                    rotator.on_rate_limited(&selected.id).await;
                } else {
                    warn!(account = %selected.id, error = %e, "Account CLI attempt failed");
                    rotator.on_error(&selected.id).await;
                }
            }
        }
    }

    Err(format!("All accounts exhausted. Last error: {last_error}"))
}

/// Core primitive: spawn the `claude` CLI subprocess with a streaming JSON reader.
///
/// `env_vars` allows the caller to inject per-account credentials
/// (e.g. `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`, `ANTHROPIC_API_KEY`).
/// When `env_vars` is empty, falls back to the ambient env plus any
/// `ANTHROPIC_API_KEY` discovered via [`get_api_key`].
///
/// An empty-string value in `env_vars` is treated as a `remove` directive —
/// this matches `AccountRotator::select()` semantics (it emits an empty
/// `ANTHROPIC_API_KEY` to force OAuth paths not to leak an API key).
#[allow(clippy::too_many_arguments)] // pure extraction of existing call_claude_cli body
async fn spawn_claude_cli_with_env(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    env_vars: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Find claude binary
    let claude_path = duduclaw_core::which_claude().ok_or_else(|| "claude CLI not found in PATH".to_string())?;

    // API key is optional — OAuth users authenticate via OS keychain.
    // Only set ANTHROPIC_API_KEY env var if we have one (as backup/override).
    // Skipped when the caller provides explicit env_vars (rotator path).
    let api_key = if env_vars.is_empty() {
        get_api_key(home_dir).await
    } else {
        None
    };

    let mut cmd = duduclaw_core::platform::async_command_for(&claude_path);
    cmd.args([
        "-p", user_message,
        "--model", model,
        "--output-format", "stream-json",
        "--verbose",
        // Channel subprocess has no TTY — bypass all permission prompts.
        // Agent-level security is enforced by CONTRACT.toml + container sandbox
        // + duduclaw security hooks, not by Claude Code's interactive prompts.
        // "auto" mode still pauses for some high-risk ops (mkdir, write) which
        // causes Claude to tell Discord users "please click Allow in terminal".
        "--dangerously-skip-permissions",
        // Allow enough agentic turns for complex tasks (read files → think → write).
        // Default -p max-turns can be too low, causing Claude to stop mid-task
        // and return a text summary instead of completing the work.
        "--max-turns", "50",
    ]);

    // Apply tool restrictions based on agent capabilities (deny-by-default)
    {
        let caps = capabilities.cloned().unwrap_or_default();
        let denied = caps.disallowed_tools();
        if !denied.is_empty() {
            let denied_csv = denied.join(",");
            cmd.args(["--disallowedTools", &denied_csv]);
        }
        // Signal bash-gate.sh to allow browser automation commands
        if caps.browser_via_bash {
            cmd.env("DUDUCLAW_BROWSER_VIA_BASH", "1");
        }
    }
    // Set working directory to agent dir so Claude can access agent config
    // (.claude/, CLAUDE.md, .mcp.json) and project files (docs/, etc.)
    if let Some(dir) = work_dir {
        // Install the agent-file-guard PreToolUse hook into
        // <agent_dir>/.claude/settings.json before spawning. This blocks
        // the sub-agent from using raw Write/Edit to create agent-structure
        // files (agent.toml/SOUL.md/…) outside <home>/agents/<name>/.
        // Best-effort — logs warning on failure but does not abort spawn.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(dir, &bin).await {
            warn!(
                agent_dir = %dir.display(),
                error = %e,
                "Failed to install agent-file-guard hook — spawn continuing without enforcement"
            );
        }
        cmd.current_dir(dir);
    }
    if let Some(ref key) = api_key {
        cmd.env("ANTHROPIC_API_KEY", key);
    }

    // Apply rotator-provided env vars (overrides any ambient/api_key values).
    // Empty-string values mean "remove this env var" — used by AccountRotator
    // to force OAuth paths to not leak a stale ANTHROPIC_API_KEY.
    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    // Pass system prompt via temp file to avoid exposure in /proc/PID/cmdline (BE-C1)
    let _prompt_guard: Option<tempfile::TempPath> = if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(system_prompt.as_bytes());
                let path = f.into_temp_path();
                cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                Some(path)
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    // Inject channel reply context for delegation callback forwarding.
    // The MCP `send_to_agent` tool reads this env var to register a callback
    // so sub-agent responses are forwarded back to the originating channel.
    if let Ok(channel) = crate::claude_runner::REPLY_CHANNEL.try_with(|ch| ch.clone()) {
        cmd.env(duduclaw_core::ENV_REPLY_CHANNEL, &channel);
    }

    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // Drain stderr concurrently and keep the last ~2 KiB for error diagnostics.
    // Without draining, claude CLI may block if stderr pipe fills up (>64 KiB).
    let stderr_pipe = child.stderr.take();
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(pipe) = stderr_pipe {
        let buf = stderr_buf.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut reader = tokio::io::BufReader::new(pipe);
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut guard) = buf.lock() {
                    guard.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    // Keep only the last 2 KiB — we only need tail for diagnostics.
                    if guard.len() > 2048 {
                        let cut = guard.len() - 2048;
                        *guard = guard[cut..].to_string();
                    }
                }
            }
        });
    }

    // Optional raw-stream logging for deep debugging. Enable with
    // `DUDUCLAW_STREAM_DEBUG=1` in the gateway process environment — every
    // line from `claude`'s stdout is appended to `<home>/claude_stream.log`.
    // Intentionally off by default (can be large and contains prompts).
    let stream_debug = std::env::var("DUDUCLAW_STREAM_DEBUG")
        .map(|v| v == "1")
        .unwrap_or(false);
    let stream_debug_path = if stream_debug {
        Some(home_dir.join("claude_stream.log"))
    } else {
        None
    };

    let mut result_text = String::new();
    // Track last tool type to suppress duplicate progress messages
    let mut last_tool_reported: Option<String> = None;

    // Diagnostic counters — included in the "Empty response" error message
    // so the next occurrence is immediately actionable (no more needing to
    // reproduce manually in a shell).
    let mut lines_seen: u32 = 0;
    let mut events_parsed: u32 = 0;
    let mut assistant_events: u32 = 0;
    let mut text_blocks: u32 = 0;
    let mut thinking_blocks: u32 = 0;
    let mut tool_use_blocks: u32 = 0;
    let mut result_events: u32 = 0;
    let mut last_raw_line: String = String::new();
    let mut last_result_subtype: Option<String> = None;
    let mut last_stop_reason: Option<String> = None;

    // Keepalive timer — fires periodically when no stream events arrive
    let mut keepalive = tokio::time::interval(
        std::time::Duration::from_secs(KEEPALIVE_INTERVAL_SECS),
    );
    keepalive.reset(); // don't fire immediately

    // Hard max timeout — absolute safety net
    let hard_deadline = tokio::time::sleep(
        std::time::Duration::from_secs(HARD_MAX_TIMEOUT_SECS),
    );
    tokio::pin!(hard_deadline);

    loop {
        tokio::select! {
            // Priority 1: read stream-json events from CLI stdout
            line_result = reader.next_line() => {
                match line_result {
                    // Stream ended normally
                    Ok(None) => break,
                    // Read error
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(format!("claude CLI read error: {e}"));
                    }
                    // Got a line — parse stream-json event
                    Ok(Some(line)) => {
                        // Reset keepalive timer on every received line
                        keepalive.reset();

                        if line.trim().is_empty() {
                            continue;
                        }

                        lines_seen += 1;
                        // Keep only a truncated tail for diagnostics (full line
                        // can contain the user's prompt — we don't want it on disk).
                        last_raw_line = line.chars().take(400).collect();

                        // Optional raw-stream debug log.
                        if let Some(ref p) = stream_debug_path {
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(p)
                            {
                                use std::io::Write;
                                let _ = writeln!(f, "{line}");
                            }
                        }

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            events_parsed += 1;
                            match event.get("type").and_then(|t| t.as_str()) {
                                // Final result event — contains the complete response.
                                //
                                // CRITICAL: the stream-json schema signals terminal
                                // errors via `is_error: true` on the `result` event
                                // (e.g. "Not logged in · Please run /login", auth
                                // failures, rate limits surfaced as synthetic replies).
                                // Without this check we would swallow the error text
                                // into `result_text` and return Ok to the caller.
                                Some("result") => {
                                    result_events += 1;
                                    last_result_subtype = event
                                        .get("subtype")
                                        .and_then(|s| s.as_str())
                                        .map(String::from);
                                    let is_error = event
                                        .get("is_error")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if is_error {
                                        let err_text = event
                                            .get("result")
                                            .and_then(|r| r.as_str())
                                            .unwrap_or("Unknown stream-json error");
                                        let _ = child.kill().await;
                                        return Err(format!(
                                            "claude CLI stream error: {err_text}"
                                        ));
                                    }
                                    if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                                        // Only overwrite with the result event's text if it's
                                        // non-empty. When Claude uses tools, the final `result`
                                        // event often has `result: ""` because the real answer
                                        // was emitted in intermediate assistant text blocks.
                                        // Overwriting with "" would discard those responses and
                                        // trigger a false "Empty response" error.
                                        if !text.is_empty() {
                                            result_text = text.to_string();
                                        }
                                    }
                                }
                                // Assistant message with content blocks
                                Some("assistant") => {
                                    assistant_events += 1;
                                    // Also check the envelope-level `error` field that
                                    // newer claude-code versions emit alongside the
                                    // synthetic assistant message on auth failure.
                                    if let Some(err) = event.get("error").and_then(|e| e.as_str()) {
                                        let _ = child.kill().await;
                                        return Err(format!(
                                            "claude CLI assistant error: {err}"
                                        ));
                                    }
                                    // Capture stop_reason for diagnostics (max_tokens,
                                    // tool_use, end_turn, stop_sequence, ...).
                                    if let Some(sr) = event
                                        .pointer("/message/stop_reason")
                                        .and_then(|v| v.as_str())
                                    {
                                        last_stop_reason = Some(sr.to_string());
                                    }
                                    if let Some(content) = event
                                        .pointer("/message/content")
                                        .and_then(|c| c.as_array())
                                    {
                                        for block in content {
                                            let block_type = block.get("type").and_then(|t| t.as_str());
                                            match block_type {
                                                Some("text") => {
                                                    text_blocks += 1;
                                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                        result_text = text.to_string();
                                                    }
                                                }
                                                Some("thinking") => {
                                                    thinking_blocks += 1;
                                                }
                                                Some("tool_use") => {
                                                    tool_use_blocks += 1;
                                                    // Extract tool name and detail for progress
                                                    if let Some(cb) = on_progress {
                                                        let tool = block.get("name")
                                                            .and_then(|n| n.as_str())
                                                            .unwrap_or("unknown")
                                                            .to_string();
                                                        let detail = extract_tool_detail(block);

                                                        // Suppress duplicate: same tool consecutively
                                                        let dominated = last_tool_reported
                                                            .as_ref()
                                                            .is_some_and(|prev| *prev == tool && detail.is_none());
                                                        if !dominated {
                                                            cb(ProgressEvent::ToolUse {
                                                                tool: tool.clone(),
                                                                detail,
                                                            });
                                                            last_tool_reported = Some(tool);
                                                        }
                                                    }
                                                }
                                                _ => {} // tool_result, etc.
                                            }
                                        }
                                    }
                                }
                                _ => {} // system, rate_limit_event, etc.
                            }
                        }
                    }
                }
            }

            // Priority 2: keepalive timer — send progress if silent too long
            _ = keepalive.tick() => {
                if let Some(cb) = on_progress {
                    cb(ProgressEvent::Keepalive);
                }
            }

            // Priority 3: hard max timeout — kill truly hung processes
            _ = &mut hard_deadline => {
                warn!(
                    "claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s) — killing process"
                );
                let _ = child.kill().await;
                if result_text.is_empty() {
                    return Err(format!(
                        "claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s, no output)"
                    ));
                }
                warn!(
                    "claude CLI hard timeout — returning partial result ({} chars)",
                    result_text.len()
                );
                break;
            }
        }
    }

    // Wait for process to exit
    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;

    // Snapshot stderr tail for error diagnostics.
    let stderr_tail: String = stderr_buf
        .lock()
        .ok()
        .map(|g| g.chars().take(400).collect::<String>())
        .unwrap_or_default();

    // Compose the diagnostic summary that all error sites below embed.
    // With this in the error string, `channel_failures.jsonl` becomes
    // self-describing: we can tell whether the CLI produced any output
    // at all, whether it only produced thinking, whether stop_reason
    // was "max_tokens" / "tool_use", etc.
    let diag = format!(
        "exit={} lines={lines_seen} events={events_parsed} \
         assistant={assistant_events} text_blocks={text_blocks} \
         thinking={thinking_blocks} tool_use={tool_use_blocks} \
         result_events={result_events} \
         result_subtype={:?} stop_reason={:?} \
         last_line={:?} stderr_tail={:?}",
        status.code().unwrap_or(-1),
        last_result_subtype,
        last_stop_reason,
        last_raw_line,
        stderr_tail,
    );

    // Any non-zero exit is now a hard failure. Previously we only errored
    // when `result_text.is_empty()`, which hid synthetic error messages
    // (e.g. "Not logged in · Please run /login") that Claude CLI emits as
    // a real result event with `is_error: true` and exit code 1. The
    // stream-json error check above should have caught those before we
    // reach here, but the exit-code gate is a defensive backstop.
    if !status.success() {
        return Err(format!(
            "claude CLI exit {} ({diag})",
            status.code().unwrap_or(-1)
        ));
    }

    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err(format!("Empty response from claude CLI ({diag})"));
    }

    Ok(result_text)
}

/// Extract a human-readable detail from a `tool_use` content block's `input`.
///
/// Tries common field names: `file_path`, `path`, `command`, `pattern`, `query`.
/// Returns the first match (truncated to 60 chars for display).
pub(crate) fn extract_tool_detail(block: &serde_json::Value) -> Option<String> {
    let input = block.get("input")?;
    for key in &["file_path", "path", "command", "pattern", "query"] {
        if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
            let truncated: String = val.chars().take(60).collect();
            return Some(truncated);
        }
    }
    None
}

// ── Python SDK subprocess (fallback) ────────────────────────

/// Find the Python source path for `duduclaw.sdk.chat`.
fn find_python_path(home_dir: &Path) -> String {
    find_python_path_static(home_dir)
}

/// Public version usable from other modules (e.g. handlers).
pub fn find_python_path_static(home_dir: &Path) -> String {
    // Try common locations
    let mut candidates = vec![
        // Installed via pip
        String::new(), // use system PYTHONPATH
        // Development: project root python/
        home_dir
            .parent()
            .unwrap_or(home_dir)
            .join("python")
            .to_string_lossy()
            .to_string(),
    ];
    #[cfg(not(windows))]
    {
        // Homebrew / source install
        candidates.push("/opt/duduclaw".to_string());
        // Homebrew Cellar (Apple Silicon)
        candidates.push("/opt/homebrew/opt/duduclaw-pro/libexec/python".to_string());
        // Homebrew Cellar (Intel Mac)
        candidates.push("/usr/local/opt/duduclaw-pro/libexec/python".to_string());
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
            candidates.push(format!("{appdata}\\Programs\\duduclaw\\python"));
        }
    }
    // User-local fallback
    candidates.push(home_dir.join(".duduclaw").join("python").to_string_lossy().to_string());

    for path in &candidates {
        if !path.is_empty() && Path::new(path).join("duduclaw").exists() {
            return path.clone();
        }
    }

    // Fallback: return existing PYTHONPATH
    std::env::var("PYTHONPATH").unwrap_or_default()
}

/// Delegate execution — spawn Python subprocess with a given prompt and return the response.
pub async fn call_python_sdk_delegate(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    call_python_sdk_v2(prompt, model, system_prompt, home_dir, None).await
}

/// Call the Python Claude Code SDK via subprocess.
///
/// The Python SDK uses the `anthropic` package with the `AccountRotator`
/// for multi-account rotation, budget tracking, and error recovery.
///
/// When `api_key` is provided, it is injected as `ANTHROPIC_API_KEY` env var
/// into the subprocess so the Python SDK can authenticate even when
/// `config.toml` has no `[[accounts]]` entries.
async fn call_python_sdk_v2(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    api_key: Option<&str>,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let prompt_file = home_dir.join(format!(".tmp_system_prompt_{}.md", uuid::Uuid::new_v4()));
    tokio::fs::write(&prompt_file, system_prompt)
        .await
        .map_err(|e| format!("Write prompt: {e}"))?;

    let config_path = home_dir.join("config.toml");
    let python_path = find_python_path(home_dir);

    let mut cmd = Command::new(duduclaw_core::platform::python3_command());
    cmd.args([
            "-m",
            "duduclaw.sdk.chat",
            "--model",
            model,
            "--system-prompt-file",
            &prompt_file.to_string_lossy(),
            "--config",
            &config_path.to_string_lossy(),
        ])
        .env("PYTHONPATH", &python_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Inject API key if provided by caller (from rotator or config).
    if let Some(key) = api_key {
        cmd.env("ANTHROPIC_API_KEY", key);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Spawn python3: {e}"))?;

    // Write user message to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(user_message.as_bytes())
            .await
            .map_err(|e| format!("Write stdin: {e}"))?;
        drop(stdin); // close stdin to signal EOF
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("Wait: {e}"))?;

    let _ = tokio::fs::remove_file(&prompt_file).await;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "exit {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(200).collect::<String>()
        ));
    }

    if stdout.is_empty() {
        return Err("Empty response".to_string());
    }

    Ok(stdout)
}

// ── Helpers ─────────────────────────────────────────────────

/// Build system prompt with progressive skill injection.
///
/// When `compressed_skills` and `active_skills` are available, uses three-layer
/// progressive loading instead of full injection. Otherwise falls back to legacy
/// full injection.
fn build_system_prompt(
    agent: Option<&duduclaw_agent::registry::LoadedAgent>,
    user_message: Option<&str>,
    compressed_skills: Option<&[crate::skill_lifecycle::compression::CompressedSkill]>,
    active_skills: Option<&std::collections::HashSet<String>>,
    skill_token_budget: u32,
) -> String {
    let mut parts = Vec::new();

    if let Some(a) = agent {
        if let Some(soul) = &a.soul {
            parts.push(soul.clone());
        }
        if let Some(identity) = &a.identity {
            parts.push(identity.clone());
        }

        // Progressive skill injection (when available)
        if let (Some(skills), Some(msg)) = (compressed_skills, user_message) {
            if !skills.is_empty() {
                let active = active_skills.cloned().unwrap_or_default();

                // Layer 0: all skill names
                let index: Vec<&str> = skills.iter().map(|s| s.tag.as_str()).collect();
                parts.push(format!("Available skills: {}", index.join(", ")));

                // Rank and select layers
                let ranked = crate::skill_lifecycle::relevance::rank_skills(msg, skills);
                let config = crate::skill_lifecycle::relevance::RelevanceConfig::default();
                let selection = crate::skill_lifecycle::relevance::select_layers(
                    &ranked, &active, skills, &config,
                );

                let mut remaining_budget = skill_token_budget;

                // Layer 2: active + highly relevant — full content
                for &idx in &selection.layer2 {
                    let skill = &skills[idx];
                    if remaining_budget >= skill.tokens_layer2 {
                        parts.push(format!("## Skill: {}\n{}", skill.name, skill.full_content));
                        remaining_budget = remaining_budget.saturating_sub(skill.tokens_layer2);
                    }
                }

                // Layer 1: relevant — summary only
                for &idx in &selection.layer1 {
                    let skill = &skills[idx];
                    if remaining_budget >= skill.tokens_layer1 {
                        parts.push(format!("## {}: {}", skill.name, skill.summary));
                        remaining_budget = remaining_budget.saturating_sub(skill.tokens_layer1);
                    }
                }
            }
        } else {
            // Legacy: inject all skills fully (backward compat when progressive not enabled)
            for skill in &a.skills {
                parts.push(format!("## Skill: {}\n{}", skill.name, skill.content));
            }
        }
    }

    if parts.is_empty() {
        "You are DuDuClaw, a helpful AI assistant. Reply concisely in the user's language."
            .to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}

/// Read the default_agent from config.toml [general] section.
async fn get_default_agent(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let general = table.get("general")?.as_table()?;
    let name = general.get("default_agent")?.as_str()?;
    if name.is_empty() { None } else { Some(name.to_string()) }
}

/// Estimate the token count for a piece of text.
///
/// Uses a CJK-aware heuristic:
/// - CJK characters (U+3000–U+9FFF and supplementary ranges): ~1.5 chars/token
/// - ASCII words: ~4 chars/token
/// - Mixed: weighted average
///
/// This is significantly more accurate than the naive `len / 4` for Chinese,
/// Japanese, and Korean text, which is the primary language of this application.
fn estimate_tokens(text: &str) -> u32 {
    let mut cjk_chars: u32 = 0;
    let mut other_chars: u32 = 0;

    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2A6DF).contains(&cp)
            || (0x2A700..=0x2CEAF).contains(&cp)
        {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }

    // CJK: ~1.5 chars per token; other: ~4 chars per token
    let cjk_tokens = (cjk_chars as f32 / 1.5).ceil() as u32;
    let other_tokens = (other_chars as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1 // +1 minimum
}

/// Parse session_id "telegram:12345" or "telegram:12345:thread" into (channel, chat_id).
fn parse_session_id_parts(session_id: &str) -> (&str, &str) {
    let parts: Vec<&str> = session_id.splitn(3, ':').collect();
    match parts.len() {
        0 | 1 => ("", session_id),
        _ => (parts[0], parts[1]),
    }
}

async fn get_api_key(home_dir: &Path) -> Option<String> {
    // Environment variable takes precedence
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    // Try encrypted config field, fallback to plaintext
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key").await
}

/// Heuristic: does the user's message look like a computer use request?
///
/// Matches keywords in Chinese, English, and Japanese that indicate the
/// user wants the agent to interact with the desktop GUI.
fn looks_like_computer_use_request(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Chinese keywords
    let cn = ["打開", "開啟", "點擊", "截圖", "螢幕", "桌面", "滑鼠",
              "鍵盤", "操作電腦", "幫我開", "幫我點", "幫我按",
              "幫我打", "幫我填", "幫我輸入", "幫我關", "視窗",
              "列印", "下載", "安裝"];
    // English keywords
    let en = ["open app", "click on", "take screenshot", "on my screen",
              "on my desktop", "mouse", "keyboard", "type into",
              "fill the form", "close the window", "print the",
              "download the", "install the", "open the browser",
              "control my computer", "on my computer"];
    // Japanese keywords
    let jp = ["画面", "クリック", "開いて", "入力して", "スクリーンショット"];

    cn.iter().any(|kw| lower.contains(&kw.to_lowercase()))
        || en.iter().any(|kw| lower.contains(kw))
        || jp.iter().any(|kw| lower.contains(&kw.to_lowercase()))
}
