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

use crate::handlers::ChannelState;
use crate::gvu::loop_::GvuLoop;
use crate::prediction::engine::PredictionEngine;
use crate::session::SessionManager;
use crate::skill_lifecycle::activation::SkillActivationController;
use crate::skill_lifecycle::compression::CompressedSkillCache;
use crate::skill_lifecycle::lift::LiftTrackerStore;

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
    /// Sessions with voice reply mode enabled (toggled by /voice command).
    pub voice_sessions: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ReplyContext {
    pub fn new(
        registry: Arc<RwLock<AgentRegistry>>,
        home_dir: PathBuf,
        session_manager: Arc<SessionManager>,
        channel_status: ChannelStatusMap,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            registry,
            home_dir,
            http,
            session_manager,
            channel_status,
            prediction_engine: None,
            gvu_loop: None,
            skill_cache: Arc::new(tokio::sync::Mutex::new(CompressedSkillCache::new())),
            skill_activation: Arc::new(tokio::sync::Mutex::new(SkillActivationController::new(5))),
            skill_lift: Arc::new(tokio::sync::Mutex::new(LiftTrackerStore::new())),
            voice_sessions: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
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
}

/// Helper to update a channel's connection state.
pub async fn set_channel_connected(status: &ChannelStatusMap, name: &str, connected: bool, error: Option<String>) {
    let mut map = status.write().await;
    map.insert(name.to_string(), ChannelState {
        connected,
        last_event: Some(chrono::Utc::now()),
        error,
    });
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

    // Load session and prepend history to system prompt
    let session_mgr = &ctx.session_manager;
    let _ = session_mgr.get_or_create(session_id, &agent_id).await;

    // Scan user input for prompt injection before processing
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

    // Sanitize role-prefix injection: strip any attempt to impersonate assistant/system role
    let sanitized_text = if text.starts_with("assistant:") || text.starts_with("system:") {
        format!("[user input] {text}")
    } else {
        text.to_string()
    };

    // Append user message to session using improved CJK-aware token estimate
    let user_tokens = estimate_tokens(&sanitized_text);
    if let Err(e) = session_mgr
        .append_message(session_id, "user", &sanitized_text, user_tokens)
        .await
    {
        warn!("Failed to save user message to session: {e}");
    }

    // Build conversation history from session
    let history = match session_mgr.get_messages(session_id).await {
        Ok(msgs) => {
            if msgs.len() > 1 {
                let history_text = msgs
                    .iter()
                    .rev()
                    .skip(1) // skip the message we just appended
                    .rev()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
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

    let full_system_prompt = if history.is_empty() {
        system_prompt
    } else {
        format!("{system_prompt}{history}")
    };

    // 1. Try `claude` CLI directly (Claude Code SDK — has built-in tools)
    let reply = match call_claude_cli(&sanitized_text, &model, &full_system_prompt, &ctx.home_dir, agent_dir.as_deref(), on_progress.as_ref(), capabilities.as_ref()).await {
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
            None
        }
    };

    // 2. Fallback: Python wrapper (with account rotator)
    let reply = match reply {
        Some(r) => Some(r),
        None => match call_python_sdk_v2(&sanitized_text, &model, &full_system_prompt, &ctx.home_dir).await {
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
                None
            }
        },
    };

    if let Some(reply) = reply {
        // Save assistant reply to session
        let reply_tokens = estimate_tokens(&reply);
        if let Err(e) = session_mgr
            .append_message(session_id, "assistant", &reply, reply_tokens)
            .await
        {
            warn!("Failed to save assistant message to session: {e}");
        }

        // ── Prediction-driven evolution ──────────────────────────────
        if ctx.prediction_engine.is_some() {
            let pe = ctx.prediction_engine.as_ref().unwrap().clone();
            let gvu = ctx.gvu_loop.clone();
            let user_id_for_pred = user_id.to_string();
            let agent_id_for_pred = agent_id.clone();
            let session_id_for_pred = session_id.to_string();
            let text_clone = text.to_string();
            let home_for_pred = ctx.home_dir.clone();
            let agent_dir_for_pred = agent_dir.clone();
            let sm_for_pred = ctx.session_manager.clone();
            let skill_cache_for_pred = ctx.skill_cache.clone();
            let skill_activation_for_pred = ctx.skill_activation.clone();
            let skill_lift_for_pred = ctx.skill_lift.clone();

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

                // 3. Calculate prediction error (< 1ms, zero LLM)
                let error = pe.calculate_error(&prediction, &metrics).await;

                // 4. Update user model (< 1ms)
                pe.update_model(&metrics).await;

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
                        // Report skill gap to evolution engine
                        if let Some(ref gap) = diagnosis.skill_gap {
                            crate::skill_lifecycle::gap::inject_skill_gap(gap, &home_for_pred, &agent_id_for_pred);
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
                    }
                }

                // 7. Route to evolution action
                let consecutive = pe.consecutive_significant_count(&agent_id_for_pred).await;
                let action = crate::prediction::router::route(&error, consecutive);

                match action {
                    crate::prediction::router::EvolutionAction::None => {}
                    crate::prediction::router::EvolutionAction::StoreEpisodic { content, importance: _ } => {
                        let preview: String = content.chars().take(80).collect();
                        debug!(agent = %agent_id_for_pred, "Storing episodic observation: {preview}");
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

                            let outcome = gvu.run(
                                &agent_id_for_pred,
                                dir,
                                &context,
                                pre_metrics,
                                &contract.boundaries.must_not,
                                &contract.boundaries.must_always,
                                call_llm,
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

        // Check if compression needed; generate Claude summary then compress in background
        let sm = ctx.session_manager.clone();
        let sid = session_id.to_string();
        let home_for_compress = ctx.home_dir.clone();
        tokio::spawn(async move {
            if sm.should_compress(&sid).await {
                // Gather last messages to summarise
                let msgs = sm.get_messages(&sid).await.unwrap_or_default();
                let transcript: String = msgs.iter()
                    .map(|m| format!("[{}] {}", m.role, &m.content[..m.content.len().min(300)]))
                    .collect::<Vec<_>>()
                    .join("\n");
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

    // 3. Fallback: static error
    let reg = ctx.registry.read().await;
    let name = reg
        .main_agent()
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");
    format!(
        "{name} 收到你的訊息，但目前無法回覆。\n\
        請確認 Claude Code 已安裝並登入：\n\
        $ claude auth status"
    )
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
    call_claude_cli(user_message, model, system_prompt, home_dir, None, None, None).await
}

/// Call the `claude` CLI (Claude Code SDK) with streaming output.
///
/// Uses `--output-format stream-json --verbose` to read incremental events.
/// Instead of killing on idle, sends keepalive progress to the channel via
/// `on_progress` callback. A hard max timeout (30 min) acts as safety net.
async fn call_claude_cli(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Find claude binary
    let claude_path = which_claude().ok_or_else(|| "claude CLI not found in PATH".to_string())?;

    // API key is optional — OAuth users authenticate via OS keychain.
    // Only set ANTHROPIC_API_KEY env var if we have one (as backup/override).
    let api_key = get_api_key(home_dir).await;

    let mut cmd = tokio::process::Command::new(&claude_path);
    cmd.args([
        "-p", user_message,
        "--model", model,
        "--output-format", "stream-json",
        "--verbose",
        // Channel subprocess has no TTY — auto-accept tool permissions.
        // Agent-level security is enforced by CONTRACT.toml + container sandbox,
        // not by Claude Code's interactive permission prompts.
        "--permission-mode", "auto",
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
        cmd.current_dir(dir);
    }
    if let Some(ref key) = api_key {
        cmd.env("ANTHROPIC_API_KEY", key);
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

    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let mut result_text = String::new();
    // Track last tool type to suppress duplicate progress messages
    let mut last_tool_reported: Option<String> = None;

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

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            match event.get("type").and_then(|t| t.as_str()) {
                                // Final result event — contains the complete response
                                Some("result") => {
                                    if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                                        result_text = text.to_string();
                                    }
                                }
                                // Assistant message with content blocks
                                Some("assistant") => {
                                    if let Some(content) = event
                                        .pointer("/message/content")
                                        .and_then(|c| c.as_array())
                                    {
                                        for block in content {
                                            let block_type = block.get("type").and_then(|t| t.as_str());
                                            match block_type {
                                                Some("text") => {
                                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                        result_text = text.to_string();
                                                    }
                                                }
                                                Some("tool_use") => {
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
                                                _ => {} // thinking, tool_result, etc.
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
    if !status.success() && result_text.is_empty() {
        return Err(format!("claude CLI exit {}", status.code().unwrap_or(-1)));
    }

    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err("Empty response from claude CLI".to_string());
    }

    Ok(result_text)
}

/// Find the `claude` binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
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
    let candidates = [
        // Installed via pip
        String::new(), // use system PYTHONPATH
        // Development: project root python/
        home_dir
            .parent()
            .unwrap_or(home_dir)
            .join("python")
            .to_string_lossy()
            .to_string(),
        // Homebrew / source install
        "/opt/duduclaw".to_string(),
    ];

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
    call_python_sdk_v2(prompt, model, system_prompt, home_dir).await
}

/// Call the Python Claude Code SDK via subprocess.
///
/// The Python SDK uses the `anthropic` package with the `AccountRotator`
/// for multi-account rotation, budget tracking, and error recovery.
async fn call_python_sdk_v2(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let prompt_file = home_dir.join(format!(".tmp_system_prompt_{}.md", uuid::Uuid::new_v4()));
    tokio::fs::write(&prompt_file, system_prompt)
        .await
        .map_err(|e| format!("Write prompt: {e}"))?;

    let config_path = home_dir.join("config.toml");
    let python_path = find_python_path(home_dir);

    let mut child = Command::new("python3")
        .args([
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
        .kill_on_drop(true)
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
