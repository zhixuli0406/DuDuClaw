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
    // Enforce admin requirement for destructive safety commands
    if cmd.requires_admin() && !is_admin {
        return "⚠️ Permission denied. Only admins can use safety stop/resume commands.".to_string();
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
            "Tokens: {}\nLast active: {}",
            session.total_tokens, session.last_active
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
        env!("CARGO_PKG_VERSION"),
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
     `/voice` — Toggle voice reply\n\n\
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

async fn handle_pair(_ctx: &ReplyContext, _session_id: &str, _code: &str) -> String {
    // Pairing verification via access controller.
    // TODO: wire to ctx.access_controller when field is added to ReplyContext.
    "ℹ️ Pairing verification is managed via the Dashboard → Security page.".to_string()
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
    fn test_parse_safety_words() {
        assert_eq!(parse_command("!STOP", None), Some(ChatCommand::SafetyStop));
        assert_eq!(parse_command("!STOP ALL", None), Some(ChatCommand::SafetyStopAll));
        assert_eq!(parse_command("!RESUME", None), Some(ChatCommand::SafetyResume));
        assert_eq!(parse_command("!STATUS", None), Some(ChatCommand::SafetyStatus));
        assert_eq!(parse_command("!停止", None), Some(ChatCommand::SafetyStop));
    }
}
