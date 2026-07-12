//! In-channel chat commands (e.g. `/status`, `/new`, `/usage`).
//!
//! These are intercepted before the message reaches the AI pipeline,
//! so they respond instantly with zero LLM cost.
//!
//! Safety words (`!STOP`, `!RESUME`, etc.) are also handled here as
//! a special class of commands that interact with the failsafe system.

use crate::channel_reply::ReplyContext;
use duduclaw_security::safety_word::{SafetyWordAction, SafetyWordScope};
use tracing::warn;

/// Parsed chat command from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `/status` — show agent name, status, token usage, last active time
    Status,
    /// `/new` — reset current session (clear history)
    New,
    /// `/usage` — show token usage, estimated cost, cache hit rate
    Usage,
    /// `/help` — list all available commands
    Help,
    /// `/compact` — force session compression
    Compact,
    /// `/model [name]` — show or switch current model
    Model(Option<String>),
    /// `/pair <code>` — verify a pairing code for access control
    Pair(String),
    /// `/voice` — toggle voice reply mode (next response as TTS audio)
    Voice,
    /// `/screenshot` — capture and send current screen
    Screenshot,
    /// `/computer on` — temporarily enable computer use for this session
    ComputerOn,
    /// `/computer off` — disable computer use
    ComputerOff,
    /// `/pause` — pause the active computer use session
    ComputerPause,
    /// `/resume` — resume a paused computer use session
    ComputerResume,
    /// `/stop` — stop the active computer use session
    ComputerStop,
    /// `/replay [n]` — show the last N screenshots from audit log
    Replay(u32),
    /// `/handoff <channel>` — move the current conversation to another channel (G4).
    /// `None` = missing argument → usage message.
    Handoff(Option<String>),
    /// `/undo [n]` — tombstone the last N user+assistant turn pairs (G4, default 1)
    Undo(u32),
    /// `/rollback` — undo back to the last checkpoint watermark (G4)
    Rollback,
    /// `!STOP` / `!停止` — stop the current agent/scope via failsafe
    SafetyStop,
    /// `!STOP ALL` / `!全部停止` — emergency stop all agents
    SafetyStopAll,
    /// `!RESUME` / `!恢復` — resume from failsafe halt
    SafetyResume,
    /// `!STATUS` / `!狀態` — query failsafe status
    SafetyStatus,
}

impl ChatCommand {
    /// Whether this command requires admin/owner privileges.
    ///
    /// Safety words that affect agent operation (`!STOP`, `!STOP ALL`, `!RESUME`)
    /// should only be usable by admins to prevent denial-of-service by
    /// arbitrary group members. `!STATUS` is read-only and allowed for all.
    pub fn requires_admin(&self) -> bool {
        matches!(
            self,
            ChatCommand::SafetyStop | ChatCommand::SafetyStopAll | ChatCommand::SafetyResume
        )
    }
}

/// Check if a message starts with a `/` command or `!` safety word.
pub fn is_command(text: &str) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with('/')
        && trimmed.len() > 1
        && trimmed.as_bytes()[1].is_ascii_alphabetic())
        || is_likely_safety_word(trimmed)
}

/// Fast heuristic: does this look like a safety word?
///
/// This uses only prefix checking (`!` followed by non-space) so it
/// doesn't need the full config. Actual validation happens in
/// `parse_safety_word_with_config`.
fn is_likely_safety_word(text: &str) -> bool {
    text.starts_with('!') && text.len() > 1 && text.as_bytes()[1] != b' '
}

/// Try to parse a safety word using the actual loaded config.
///
/// Call this instead of the hardcoded-default version.
pub fn parse_safety_word_with_config(
    text: &str,
    config: &duduclaw_security::killswitch::SafetyWordsConfig,
) -> Option<ChatCommand> {
    match duduclaw_security::safety_word::check(text, config) {
        SafetyWordAction::Stop(SafetyWordScope::CurrentScope) => Some(ChatCommand::SafetyStop),
        SafetyWordAction::Stop(SafetyWordScope::Global) => Some(ChatCommand::SafetyStopAll),
        SafetyWordAction::Resume => Some(ChatCommand::SafetyResume),
        SafetyWordAction::Status => Some(ChatCommand::SafetyStatus),
        SafetyWordAction::None => None,
    }
}

/// Parse a chat command from user text.  Returns `None` if not a recognized command.
///
/// Also checks for safety words (e.g., `!STOP`, `!RESUME`).
/// Pass the actual `SafetyWordsConfig` loaded from KILLSWITCH.toml.
pub fn parse_command(
    text: &str,
    safety_config: Option<&duduclaw_security::killswitch::SafetyWordsConfig>,
) -> Option<ChatCommand> {
    let trimmed = text.trim();

    // Check safety words first (they use `!` prefix)
    if let Some(config) = safety_config {
        if let Some(cmd) = parse_safety_word_with_config(trimmed, config) {
            return Some(cmd);
        }
    } else {
        // Fallback: use defaults if no config provided
        let default_config = duduclaw_security::killswitch::SafetyWordsConfig::default();
        if let Some(cmd) = parse_safety_word_with_config(trimmed, &default_config) {
            return Some(cmd);
        }
    }

    if !trimmed.starts_with('/') {
        return None;
    }

    // Split into command and args: "/model claude-sonnet" → ("model", Some("claude-sonnet"))
    let without_slash = &trimmed[1..];
    let (cmd, args) = match without_slash.find(char::is_whitespace) {
        Some(pos) => (&without_slash[..pos], Some(without_slash[pos..].trim())),
        None => (without_slash, None),
    };

    match cmd.to_ascii_lowercase().as_str() {
        "status" | "s" => Some(ChatCommand::Status),
        "new" | "reset" => Some(ChatCommand::New),
        "usage" | "cost" => Some(ChatCommand::Usage),
        "help" | "h" => Some(ChatCommand::Help),
        "compact" => Some(ChatCommand::Compact),
        "model" | "m" => Some(ChatCommand::Model(
            args.filter(|a| !a.is_empty()).map(|a| a.to_string()),
        )),
        "pair" => {
            let code = args.filter(|a| !a.is_empty()).map(|a| a.to_string())?;
            Some(ChatCommand::Pair(code))
        }
        "voice" | "v" => Some(ChatCommand::Voice),
        "screenshot" | "ss" => Some(ChatCommand::Screenshot),
        "computer" => match args.map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("on") => Some(ChatCommand::ComputerOn),
            Some("off") => Some(ChatCommand::ComputerOff),
            _ => Some(ChatCommand::Screenshot), // bare /computer → show status
        },
        "pause" => Some(ChatCommand::ComputerPause),
        "resume" => Some(ChatCommand::ComputerResume),
        "stop" => Some(ChatCommand::ComputerStop),
        "replay" => {
            let n = args
                .and_then(|a| a.parse::<u32>().ok())
                .unwrap_or(5);
            Some(ChatCommand::Replay(n))
        }
        "handoff" => Some(ChatCommand::Handoff(
            args.filter(|a| !a.is_empty()).map(|a| a.to_ascii_lowercase()),
        )),
        "undo" => {
            // Same laxity as /replay: non-numeric arg falls back to default.
            let n = args.and_then(|a| a.parse::<u32>().ok()).unwrap_or(1);
            Some(ChatCommand::Undo(n))
        }
        "rollback" => Some(ChatCommand::Rollback),
        _ => None,
    }
}

/// Execute a chat command and return the response text.
///
/// `is_admin` indicates whether the user has admin/owner privileges.
/// Safety words that modify agent state (`!STOP`, `!RESUME`) require admin.
pub async fn handle_command(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    is_admin: bool,
) -> String {
    // Enforce admin requirement for destructive safety commands.
    // Fail-closed: callers must compute real admin status per channel
    // (channel_reply::is_channel_admin) — never hardcode `true`.
    if cmd.requires_admin() && !is_admin {
        return "⚠️ 此指令僅限管理員使用。請管理員在該頻道設定 admin_users 後再試。".to_string();
    }

    match cmd {
        ChatCommand::Status => handle_status(ctx, session_id, agent_id).await,
        ChatCommand::New => handle_new(ctx, session_id).await,
        ChatCommand::Usage => handle_usage(ctx, agent_id).await,
        ChatCommand::Help => handle_help(),
        ChatCommand::Compact => handle_compact(ctx, session_id).await,
        ChatCommand::Model(name) => handle_model(ctx, agent_id, name.as_deref()).await,
        ChatCommand::Pair(code) => handle_pair(ctx, session_id, code).await,
        ChatCommand::Voice => {
            let mut voice_set = ctx.voice_sessions.lock().await;
            let key = session_id.to_string();
            if voice_set.contains(&key) {
                voice_set.remove(&key);
                "🔇 Voice mode OFF — replies will be sent as text.".to_string()
            } else {
                voice_set.insert(key);
                "🔊 Voice mode ON — next replies will be sent as audio.".to_string()
            }
        }
        ChatCommand::Screenshot => {
            // Handled by the caller — needs access to the orchestrator
            "📸 截圖指令已接收（需要 active computer use session）".to_string()
        }
        ChatCommand::ComputerOn => {
            "🖥️ Computer Use 已啟用（本次 session）".to_string()
        }
        ChatCommand::ComputerOff => {
            "🖥️ Computer Use 已關閉".to_string()
        }
        ChatCommand::ComputerPause => {
            "⏸ Computer Use session 已暫停。發送 /resume 繼續".to_string()
        }
        ChatCommand::ComputerResume => {
            "▶️ Computer Use session 已恢復".to_string()
        }
        ChatCommand::ComputerStop => {
            "🛑 Computer Use session 已終止".to_string()
        }
        ChatCommand::Replay(n) => {
            handle_replay(ctx, agent_id, *n).await
        }
        ChatCommand::Handoff(target) => {
            handle_handoff(ctx, session_id, agent_id, target.as_deref()).await
        }
        ChatCommand::Undo(n) => handle_undo(ctx, session_id, *n).await,
        ChatCommand::Rollback => handle_rollback(ctx, session_id).await,
        ChatCommand::SafetyStop => handle_safety_stop(ctx, session_id, agent_id).await,
        ChatCommand::SafetyStopAll => handle_safety_stop_all(ctx).await,
        ChatCommand::SafetyResume => handle_safety_resume(ctx, session_id).await,
        ChatCommand::SafetyStatus => handle_safety_status(ctx, session_id).await,
    }
}

// ── Individual handlers ─────────────────────────────────────────

async fn handle_status(ctx: &ReplyContext, session_id: &str, agent_id: &str) -> String {
    let reg = ctx.registry.read().await;
    let agent = reg.get(agent_id).or_else(|| reg.main_agent());

    let (display_name, role, status, model) = match agent {
        Some(a) => (
            a.config.agent.display_name.clone(),
            format!("{:?}", a.config.agent.role),
            format!("{:?}", a.config.agent.status),
            a.config.model.preferred.clone(),
        ),
        None => (
            "Unknown".to_string(),
            "N/A".to_string(),
            "N/A".to_string(),
            "N/A".to_string(),
        ),
    };
    drop(reg);

    // Session info
    let session_info = match ctx.session_manager.get_or_create(session_id, agent_id).await {
        Ok(session) => format!(
            "Session: #{}\nTokens: {}\nLast active: {}",
            session.lineage, session.total_tokens, session.last_active
        ),
        Err(_) => "Session: N/A".to_string(),
    };

    // Channel status
    let channel_map = ctx.channel_status.read().await;
    let channels: Vec<String> = channel_map
        .iter()
        .map(|(name, state)| {
            let icon = if state.connected { "🟢" } else { "🔴" };
            format!("{icon} {name}")
        })
        .collect();
    drop(channel_map);

    format!(
        "📊 *Status*\n\
         Agent: {display_name}\n\
         Role: {role}\n\
         Status: {status}\n\
         Model: `{model}`\n\
         {session_info}\n\
         Channels: {}\n\
         Version: v{}",
        if channels.is_empty() {
            "none".to_string()
        } else {
            channels.join(", ")
        },
        crate::updater::current_version(),
    )
}

async fn handle_new(ctx: &ReplyContext, session_id: &str) -> String {
    match ctx.session_manager.delete_session(session_id).await {
        Ok(()) => "✅ Session cleared. Starting fresh!".to_string(),
        Err(e) => {
            warn!(session_id, error = %e, "Failed to clear session");
            format!("⚠️ Failed to clear session: {e}")
        }
    }
}

async fn handle_usage(_ctx: &ReplyContext, _agent_id: &str) -> String {
    // Cost telemetry summary — reads from SQLite via CostTelemetry module.
    // TODO: wire to crate::cost_telemetry::get_summary when available.
    "💰 Usage tracking is available in the Dashboard → Reports page.".to_string()
}

fn handle_help() -> String {
    "📖 *Available Commands*\n\n\
     `/status` — Agent status and session info\n\
     `/new` — Clear session, start fresh\n\
     `/usage` — Token usage and cost summary\n\
     `/help` — Show this help message\n\
     `/compact` — Force session compression\n\
     `/model [name]` — Show or switch model\n\
     `/pair <code>` — Verify pairing code\n\
     `/voice` — Toggle voice reply\n\
     `/handoff <頻道>` — 將對話轉移到其他頻道\n\
     `/undo [次數]` — 撤銷最近 N 輪對話（預設 1，最多 20）\n\
     `/rollback` — 回退到最近一次檢查點（僅回退對話，不還原檔案）\n\n\
     *Safety Words*\n\
     `!STOP` / `!停止` — Stop current agent\n\
     `!STOP ALL` / `!全部停止` — Emergency stop all\n\
     `!RESUME` / `!恢復` — Resume agent\n\
     `!STATUS` / `!狀態` — Check safety status"
        .to_string()
}

async fn handle_compact(ctx: &ReplyContext, session_id: &str) -> String {
    match ctx.session_manager.force_compress(session_id).await {
        Ok(saved) => format!("🗜️ Session compressed. Saved ~{saved} tokens."),
        Err(e) => {
            warn!("Failed to compress session: {e}");
            format!("⚠️ Compression failed: {e}")
        }
    }
}

async fn handle_pair(ctx: &ReplyContext, session_id: &str, code: &str) -> String {
    // Channels that intercept commands here don't carry a user id, so the
    // pairing subject is the session id. The central access gate in
    // channel_reply checks BOTH user id and session id, so either form of
    // approval unlocks the conversation. Codes come from the operator via
    // the `pairing_generate` MCP tool.
    if ctx.access_control.verify_pairing_code(session_id, code).await {
        "✅ 配對成功，現在可以開始對話了。".to_string()
    } else {
        "❌ 配對碼錯誤或已過期，請向管理員索取新的配對碼。".to_string()
    }
}

async fn handle_model(ctx: &ReplyContext, agent_id: &str, new_model: Option<&str>) -> String {
    let reg = ctx.registry.read().await;
    let agent = reg.get(agent_id).or_else(|| reg.main_agent());

    match agent {
        Some(a) => {
            let current = &a.config.model.preferred;
            match new_model {
                Some(name) => {
                    // Show the requested model — actual switching requires config update
                    format!(
                        "🔄 Current model: `{current}`\n\
                         Requested: `{name}`\n\
                         ⚠️ Model switching via chat is read-only. \
                         Update `agent.toml [model] preferred` to change."
                    )
                }
                None => format!("🤖 Current model: `{current}`"),
            }
        }
        None => "⚠️ No agent found.".to_string(),
    }
}

// ── G4 session portability handlers (/handoff, /undo, /rollback) ──

/// `/handoff <channel>` — copy the conversation state onto the same agent's
/// session on the target channel. Fail-closed at every step: unknown
/// channel, unconfigured/disconnected channel, no existing session on the
/// target (we cannot invent a `<channel>:<chat_id>` key), or an AMBIGUOUS
/// target (several unrelated sessions — see `resolve_handoff_target`;
/// "most recent wins" would land the transcript in another user's chat).
async fn handle_handoff(
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    target: Option<&str>,
) -> String {
    let channel_list = duduclaw_core::SUPPORTED_CHANNEL_TYPES.join("、");
    let target = match target {
        Some(t) => t.trim().to_ascii_lowercase(),
        None => {
            return format!("用法：/handoff <頻道>\n可用頻道：{channel_list}");
        }
    };

    if !duduclaw_core::SUPPORTED_CHANNEL_TYPES.contains(&target.as_str()) {
        return format!("❌ 不支援的頻道「{target}」。\n可用頻道：{channel_list}");
    }

    let source_channel = session_id.split(':').next().unwrap_or("");
    if source_channel == target {
        return format!("目前對話已經在 {target} 頻道，不需要轉移。");
    }

    // The session row's agent is authoritative; fall back to the caller's.
    let owner = match ctx.session_manager.session_agent(session_id).await {
        Ok(Some(a)) => a,
        Ok(None) => agent_id.to_string(),
        Err(e) => {
            warn!(session_id, error = %e, "handoff: session_agent lookup failed");
            return format!("⚠️ 轉移失敗：{e}");
        }
    };

    // Fail closed: the target channel must be configured AND connected on
    // this gateway before we move a conversation onto it. Per-agent bots
    // register under `<channel>:<agent>` keys — check those too before
    // declaring the channel unconnected (exact keys, never substrings).
    let connected = {
        let status = ctx.channel_status.read().await;
        [
            target.clone(),
            format!("{target}:{owner}"),
            format!("{target}:{agent_id}"),
        ]
        .iter()
        .any(|key| status.get(key).map(|s| s.connected).unwrap_or(false))
    };
    if !connected {
        return format!("❌ {target} 頻道尚未設定或目前未連線，無法轉移。請先在儀表板完成該頻道設定。");
    }

    let candidates = match ctx
        .session_manager
        .active_sessions_for_channel(&owner, &target, 10)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(session_id, target = %target, error = %e, "handoff: target lookup failed");
            return format!("⚠️ 轉移失敗：{e}");
        }
    };
    let target_session = match crate::session_portability::resolve_handoff_target(
        &candidates,
        session_id,
    ) {
        crate::session_portability::HandoffTarget::Resolved(id) => id,
        crate::session_portability::HandoffTarget::NoSession => {
            // Design choice (documented in session_portability.rs): sessions
            // are keyed by <channel>:<chat_id>, so without an inbound message
            // we cannot know where to deliver. Ask the user to ping first.
            return format!(
                "❌ {target} 頻道還沒有任何對話。請先在 {target} 頻道傳一則訊息給代理建立會話，再回來執行 /handoff {target}。"
            );
        }
        crate::session_portability::HandoffTarget::Ambiguous(n) => {
            // Fail-closed: never guess among several users' conversations.
            return format!(
                "❌ 目標頻道有多個對話（{n} 個），無法確定要轉移到哪一個。\
                 請先在 {target} 頻道傳送一則訊息以指定對話，再回來執行 /handoff {target}。"
            );
        }
    };

    match ctx
        .session_manager
        .handoff_session(session_id, &target_session)
        .await
    {
        Ok(crate::session_portability::HandoffDecision::Done(report)) => format!(
            "✅ 已將對話轉移到 {target} 頻道（共 {} 則訊息）。\n到 {target} 頻道傳送訊息即可接續目前的對話；本頻道的紀錄仍會保留。",
            report.copied_messages
        ),
        Ok(crate::session_portability::HandoffDecision::NothingToHandoff) => {
            "目前對話沒有可轉移的內容。".to_string()
        }
        Err(e) => {
            warn!(session_id, target = %target, error = %e, "handoff failed");
            format!("⚠️ 轉移失敗：{e}")
        }
    }
}

/// `/undo [N]` — tombstone the last N turn pairs (default 1, max 20).
async fn handle_undo(ctx: &ReplyContext, session_id: &str, n: u32) -> String {
    use crate::session_portability::{UndoDecision, UNDO_MAX_PAIRS};
    if n == 0 || n > UNDO_MAX_PAIRS {
        return format!("❌ 次數必須介於 1 到 {UNDO_MAX_PAIRS} 之間。用法：/undo [次數]");
    }
    match ctx.session_manager.undo_last_turns(session_id, n).await {
        Ok(UndoDecision::Undone { pairs, messages, .. }) => {
            format!("↩️ 已撤銷最近 {pairs} 輪對話（共 {messages} 則訊息）。")
        }
        Ok(UndoDecision::NothingToUndo) => "目前沒有可撤銷的對話。".to_string(),
        Ok(UndoDecision::BoundaryBlocked { available }) => format!(
            "⚠️ 無法撤銷 {n} 輪：更早的對話已經過壓縮整理，無法再撤銷。目前最多可撤銷 {available} 輪（/undo {available}）。"
        ),
        Err(e) => {
            warn!(session_id, error = %e, "undo failed");
            format!("⚠️ 撤銷失敗：{e}")
        }
    }
}

/// `/rollback` — conversation-state rollback to the last checkpoint
/// watermark. File changes made by the agent are NOT reverted (the CLI
/// runtimes own file edits) and the reply says so honestly.
async fn handle_rollback(ctx: &ReplyContext, session_id: &str) -> String {
    use crate::session_portability::RollbackDecision;
    match ctx.session_manager.rollback_to_checkpoint(session_id).await {
        Ok(RollbackDecision::RolledBack { messages, .. }) => format!(
            "⏪ 已回退到最近一次檢查點（移除 {messages} 則訊息）。\n注意：這只會回退對話內容；代理先前對檔案所做的變更不會被還原。"
        ),
        Ok(RollbackDecision::NothingToRollback) => {
            "目前沒有可回退的檢查點內容。若要撤銷最近的對話，可改用 /undo。".to_string()
        }
        Err(e) => {
            warn!(session_id, error = %e, "rollback failed");
            format!("⚠️ 回退失敗：{e}")
        }
    }
}

// ── Safety word handlers ───────────────────────────────────────

async fn handle_safety_stop(ctx: &ReplyContext, session_id: &str, _agent_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        failsafe.force_halt(session_id, "safety word: !STOP").await;
        // Also reset the circuit breaker for this scope
        if let Some(ref cb) = ctx.circuit_breakers {
            cb.reset(session_id).await;
        }
        "🛑 Agent stopped. Use `!RESUME` to restart.".to_string()
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_stop_all(ctx: &ReplyContext) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        // Halt all active scopes + a global scope marker
        failsafe.force_halt("__global__", "safety word: !STOP ALL").await;
        for (scope, _) in failsafe.active_states().await {
            failsafe.force_halt(&scope, "safety word: !STOP ALL").await;
        }
        "🛑 EMERGENCY STOP — all agents halted. Use `!RESUME` to restart.".to_string()
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_resume(ctx: &ReplyContext, session_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        // Check if there's a global halt — need to resume that too
        let global_level = failsafe.get_level("__global__").await;
        failsafe.resume(session_id).await;

        if global_level != duduclaw_security::failsafe::FailsafeLevel::L0Normal {
            // Global halt is active — also resume it.
            // NOTE: requires_admin() should be checked by the channel handler
            // before calling this. We resume global here because chat_commands
            // is the only path where admin checks can happen.
            failsafe.resume("__global__").await;
            "✅ Agent resumed. Global halt also cleared.".to_string()
        } else {
            "✅ Agent resumed. Normal operation restored.".to_string()
        }
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_status(ctx: &ReplyContext, session_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        let state = failsafe.get_state(session_id).await;
        let scope_status = duduclaw_security::failsafe::format_status(session_id, state.as_ref());

        // Also check global halt
        let global_state = failsafe.get_state("__global__").await;
        let global_status = if global_state.is_some() {
            "\n\n⚠️ GLOBAL HALT is active.".to_string()
        } else {
            String::new()
        };

        // Circuit breaker status
        let cb_status = if let Some(ref cb) = ctx.circuit_breakers {
            let state = cb.state(session_id).await;
            let reason = cb.trip_reason(session_id).await;
            let reason_str = reason.map(|r| format!(" ({r})")).unwrap_or_default();
            format!("\n\nCircuit Breaker: {:?}{reason_str}", state)
        } else {
            String::new()
        };

        format!("🔒 *Safety Status*\n\n{scope_status}{global_status}{cb_status}")
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_replay(ctx: &ReplyContext, agent_id: &str, limit: u32) -> String {
    let audit = crate::screenshot_audit::BrowserAuditLog::new(&ctx.home_dir, 7);
    match audit.entries_for_agent(agent_id, limit as usize) {
        Ok(entries) if entries.is_empty() => {
            "📭 沒有找到截圖記錄".to_string()
        }
        Ok(entries) => {
            let mut lines = vec![format!("📸 最近 {} 筆操作記錄：", entries.len())];
            for entry in &entries {
                let ts = entry.timestamp.format("%H:%M:%S");
                let ss = entry
                    .screenshot_path
                    .as_ref()
                    .map(|p| format!(" [截圖: {}]", p.display()))
                    .unwrap_or_default();
                lines.push(format!("• {ts} [{tier}] {action}{ss}",
                    tier = entry.tier,
                    action = entry.action,
                ));
            }
            lines.join("\n")
        }
        Err(e) => {
            warn!(error = %e, "Failed to read audit log");
            format!("⚠️ 讀取審計記錄失敗：{e}")
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command() {
        assert!(is_command("/status"));
        assert!(is_command("/help"));
        assert!(is_command("  /status  "));
        assert!(is_command("/model claude-sonnet"));
        assert!(!is_command("hello"));
        assert!(!is_command("/ not a command"));
        assert!(!is_command("/123"));
        assert!(!is_command(""));
        assert!(!is_command("/"));
    }

    #[test]
    fn test_parse_status() {
        assert_eq!(parse_command("/status", None), Some(ChatCommand::Status));
        assert_eq!(parse_command("/STATUS", None), Some(ChatCommand::Status));
        assert_eq!(parse_command("/s", None), Some(ChatCommand::Status));
    }

    #[test]
    fn test_parse_new() {
        assert_eq!(parse_command("/new", None), Some(ChatCommand::New));
        assert_eq!(parse_command("/reset", None), Some(ChatCommand::New));
    }

    #[test]
    fn test_parse_usage() {
        assert_eq!(parse_command("/usage", None), Some(ChatCommand::Usage));
        assert_eq!(parse_command("/cost", None), Some(ChatCommand::Usage));
    }

    #[test]
    fn test_parse_model_with_arg() {
        assert_eq!(
            parse_command("/model claude-sonnet-4-6", None),
            Some(ChatCommand::Model(Some("claude-sonnet-4-6".to_string())))
        );
    }

    #[test]
    fn test_parse_model_no_arg() {
        assert_eq!(parse_command("/model", None), Some(ChatCommand::Model(None)));
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(parse_command("/unknown", None), None);
        assert_eq!(parse_command("not a command", None), None);
    }

    #[test]
    fn test_parse_help() {
        assert_eq!(parse_command("/help", None), Some(ChatCommand::Help));
        assert_eq!(parse_command("/h", None), Some(ChatCommand::Help));
    }

    #[test]
    fn test_parse_compact() {
        assert_eq!(parse_command("/compact", None), Some(ChatCommand::Compact));
    }

    #[test]
    fn test_parse_computer_commands() {
        assert_eq!(parse_command("/screenshot", None), Some(ChatCommand::Screenshot));
        assert_eq!(parse_command("/ss", None), Some(ChatCommand::Screenshot));
        assert_eq!(parse_command("/computer on", None), Some(ChatCommand::ComputerOn));
        assert_eq!(parse_command("/computer off", None), Some(ChatCommand::ComputerOff));
        assert_eq!(parse_command("/pause", None), Some(ChatCommand::ComputerPause));
        assert_eq!(parse_command("/resume", None), Some(ChatCommand::ComputerResume));
        assert_eq!(parse_command("/stop", None), Some(ChatCommand::ComputerStop));
        assert_eq!(parse_command("/replay", None), Some(ChatCommand::Replay(5)));
        assert_eq!(parse_command("/replay 10", None), Some(ChatCommand::Replay(10)));
    }

    #[test]
    fn test_parse_session_portability_commands() {
        // /handoff — arg is lowercased; missing arg → None payload (usage reply)
        assert_eq!(
            parse_command("/handoff telegram", None),
            Some(ChatCommand::Handoff(Some("telegram".to_string())))
        );
        assert_eq!(
            parse_command("/handoff Slack", None),
            Some(ChatCommand::Handoff(Some("slack".to_string())))
        );
        assert_eq!(parse_command("/handoff", None), Some(ChatCommand::Handoff(None)));

        // /undo — default 1, explicit N passes through (validated in handler)
        assert_eq!(parse_command("/undo", None), Some(ChatCommand::Undo(1)));
        assert_eq!(parse_command("/undo 3", None), Some(ChatCommand::Undo(3)));
        assert_eq!(parse_command("/undo abc", None), Some(ChatCommand::Undo(1)));

        assert_eq!(parse_command("/rollback", None), Some(ChatCommand::Rollback));

        // Non-admin commands, same gating as peers (/new, /compact).
        assert!(!ChatCommand::Handoff(Some("telegram".into())).requires_admin());
        assert!(!ChatCommand::Undo(1).requires_admin());
        assert!(!ChatCommand::Rollback.requires_admin());
    }

    #[test]
    fn test_parse_safety_words() {
        assert_eq!(parse_command("!STOP", None), Some(ChatCommand::SafetyStop));
        assert_eq!(parse_command("!STOP ALL", None), Some(ChatCommand::SafetyStopAll));
        assert_eq!(parse_command("!RESUME", None), Some(ChatCommand::SafetyResume));
        assert_eq!(parse_command("!STATUS", None), Some(ChatCommand::SafetyStatus));
        assert_eq!(parse_command("!停止", None), Some(ChatCommand::SafetyStop));
    }
}
