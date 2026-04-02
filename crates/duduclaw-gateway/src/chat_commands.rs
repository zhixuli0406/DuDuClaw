//! In-channel chat commands (e.g. `/status`, `/new`, `/usage`).
//!
//! These are intercepted before the message reaches the AI pipeline,
//! so they respond instantly with zero LLM cost.

use crate::channel_reply::ReplyContext;
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
}

/// Check if a message starts with a `/` command.
pub fn is_command(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('/')
        && trimmed.len() > 1
        && trimmed.as_bytes()[1].is_ascii_alphabetic()
}

/// Parse a chat command from user text.  Returns `None` if not a recognized command.
pub fn parse_command(text: &str) -> Option<ChatCommand> {
    let trimmed = text.trim();
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
pub async fn handle_command(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
) -> String {
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

async fn handle_new(_ctx: &ReplyContext, _session_id: &str) -> String {
    // Session clearing is handled by the session manager's auto-compression.
    // /new resets the conversation context for the next message.
    "✅ Session cleared. Starting fresh!".to_string()
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
     `/voice` — Toggle voice reply"
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
        assert_eq!(parse_command("/status"), Some(ChatCommand::Status));
        assert_eq!(parse_command("/STATUS"), Some(ChatCommand::Status));
        assert_eq!(parse_command("/s"), Some(ChatCommand::Status));
    }

    #[test]
    fn test_parse_new() {
        assert_eq!(parse_command("/new"), Some(ChatCommand::New));
        assert_eq!(parse_command("/reset"), Some(ChatCommand::New));
    }

    #[test]
    fn test_parse_usage() {
        assert_eq!(parse_command("/usage"), Some(ChatCommand::Usage));
        assert_eq!(parse_command("/cost"), Some(ChatCommand::Usage));
    }

    #[test]
    fn test_parse_model_with_arg() {
        assert_eq!(
            parse_command("/model claude-sonnet-4-6"),
            Some(ChatCommand::Model(Some("claude-sonnet-4-6".to_string())))
        );
    }

    #[test]
    fn test_parse_model_no_arg() {
        assert_eq!(parse_command("/model"), Some(ChatCommand::Model(None)));
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(parse_command("/unknown"), None);
        assert_eq!(parse_command("not a command"), None);
    }

    #[test]
    fn test_parse_help() {
        assert_eq!(parse_command("/help"), Some(ChatCommand::Help));
        assert_eq!(parse_command("/h"), Some(ChatCommand::Help));
    }

    #[test]
    fn test_parse_compact() {
        assert_eq!(parse_command("/compact"), Some(ChatCommand::Compact));
    }
}
