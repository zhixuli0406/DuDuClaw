//! Message activation filter — decides whether the agent should respond.
//!
//! In DMs, the agent always responds (unless `Manual` mode).
//! In groups, the agent only responds when @mentioned or trigger keyword is present
//! (when `MentionOnly` mode), or always (`Always` mode).

use duduclaw_core::types::ActivationMode;

/// Contextual information about an incoming message.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Raw text content of the message.
    pub text: String,
    /// Whether this message is from a group chat (vs DM).
    pub is_group: bool,
    /// Group/chat ID (for group session isolation).
    pub group_id: Option<String>,
    /// Whether the bot was explicitly @mentioned in the message.
    pub is_mentioned: bool,
    /// Channel name (telegram, discord, line).
    pub channel: String,
    /// Stable user ID from the channel.
    pub user_id: String,
    /// Display name of the sender (for logging).
    pub sender_display_name: Option<String>,
}

/// Determine whether the agent should respond to this message.
pub fn should_respond(
    msg: &IncomingMessage,
    mode: &ActivationMode,
    trigger: &str,
) -> bool {
    match mode {
        ActivationMode::Always => true,
        ActivationMode::MentionOnly => {
            if !msg.is_group {
                // DMs always respond in MentionOnly mode
                return true;
            }
            // In groups, respond only if @mentioned or trigger keyword found
            if msg.is_mentioned {
                return true;
            }
            if !trigger.is_empty() {
                let text_lower = msg.text.to_lowercase();
                let trigger_lower = trigger.to_lowercase();
                return text_lower.contains(&trigger_lower);
            }
            false
        }
        ActivationMode::Manual => {
            // Only respond to slash commands
            crate::chat_commands::is_command(&msg.text)
        }
    }
}

/// Build the session key for this message.
///
/// DMs: `"{channel}:{user_id}"`
/// Groups: `"{channel}:group:{group_id}"`
pub fn session_key(msg: &IncomingMessage) -> String {
    if msg.is_group {
        if let Some(group_id) = &msg.group_id {
            return format!("{}:group:{}", msg.channel, group_id);
        }
    }
    format!("{}:{}", msg.channel, msg.user_id)
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dm_msg(text: &str) -> IncomingMessage {
        IncomingMessage {
            text: text.to_string(),
            is_group: false,
            group_id: None,
            is_mentioned: false,
            channel: "telegram".to_string(),
            user_id: "user1".to_string(),
            sender_display_name: None,
        }
    }

    fn group_msg(text: &str, mentioned: bool) -> IncomingMessage {
        IncomingMessage {
            text: text.to_string(),
            is_group: true,
            group_id: Some("group123".to_string()),
            is_mentioned: mentioned,
            channel: "telegram".to_string(),
            user_id: "user1".to_string(),
            sender_display_name: None,
        }
    }

    #[test]
    fn test_always_mode() {
        assert!(should_respond(&dm_msg("hello"), &ActivationMode::Always, ""));
        assert!(should_respond(&group_msg("hello", false), &ActivationMode::Always, ""));
    }

    #[test]
    fn test_mention_only_dm() {
        // DMs always respond in MentionOnly
        assert!(should_respond(&dm_msg("hello"), &ActivationMode::MentionOnly, ""));
    }

    #[test]
    fn test_mention_only_group_no_mention() {
        assert!(!should_respond(
            &group_msg("hello", false),
            &ActivationMode::MentionOnly,
            "",
        ));
    }

    #[test]
    fn test_mention_only_group_with_mention() {
        assert!(should_respond(
            &group_msg("hello", true),
            &ActivationMode::MentionOnly,
            "",
        ));
    }

    #[test]
    fn test_mention_only_group_with_trigger() {
        assert!(should_respond(
            &group_msg("hey @dudu how are you", false),
            &ActivationMode::MentionOnly,
            "@dudu",
        ));
    }

    #[test]
    fn test_manual_mode() {
        assert!(!should_respond(&dm_msg("hello"), &ActivationMode::Manual, ""));
        assert!(should_respond(&dm_msg("/status"), &ActivationMode::Manual, ""));
    }

    #[test]
    fn test_session_key_dm() {
        let msg = dm_msg("hello");
        assert_eq!(session_key(&msg), "telegram:user1");
    }

    #[test]
    fn test_session_key_group() {
        let msg = group_msg("hello", false);
        assert_eq!(session_key(&msg), "telegram:group:group123");
    }
}
