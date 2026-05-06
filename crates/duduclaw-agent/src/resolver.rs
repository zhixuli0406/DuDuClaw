use duduclaw_core::types::Message;

use crate::registry::{AgentRegistry, LoadedAgent};

/// Routes incoming messages to the appropriate agent based on trigger words,
/// channel bindings, and fallback rules.
pub struct AgentResolver<'a> {
    registry: &'a AgentRegistry,
}

impl<'a> AgentResolver<'a> {
    /// Create a new resolver backed by the given registry.
    pub fn new(registry: &'a AgentRegistry) -> Self {
        Self { registry }
    }

    /// Resolve which agent should handle the given message.
    ///
    /// Resolution order:
    /// 1. Trigger word match (e.g. `@DuDu` at the start of the message text).
    /// 2. **Channel/Thread binding** (RFC-22 Decision 3-D, Phase 3 W3) —
    ///    `[[channels.discord.bindings]]` entries in `agent.toml`. Resolves
    ///    `discord:thread:<id>` and `discord:<channel_id>` directly to the
    ///    bound agent so sub-agents receive channel messages without
    ///    going through the root agent first (which previously caused
    ///    14-day SOUL stagnation for 16 of 17 sub-agents).
    /// 3. Coarse permission grant — the message channel name (e.g. "discord")
    ///    is in the agent's `permissions.allowed_channels` list.
    /// 4. Fall back to the main agent (role = Main).
    pub fn resolve(&self, message: &Message) -> Option<&'a LoadedAgent> {
        let agents = self.registry.list();

        // 1. Trigger word match
        for agent in &agents {
            let trigger = &agent.config.agent.trigger;
            if !trigger.is_empty() && self.match_trigger(&message.text, trigger) {
                return Some(agent);
            }
        }

        // 2. Channel/Thread binding (RFC-22 Decision 3-D)
        if let Some(agent) = self.match_channel_binding(message, &agents) {
            return Some(agent);
        }

        // 3. Coarse permission grant
        for agent in &agents {
            let allowed = &agent.config.permissions.allowed_channels;
            if allowed.iter().any(|ch| ch == &message.channel) {
                return Some(agent);
            }
        }

        // 4. Fall back to main agent
        self.registry.main_agent()
    }

    /// Check whether `text` contains the trigger word (case-insensitive).
    fn match_trigger(&self, text: &str, trigger: &str) -> bool {
        text.to_lowercase().contains(&trigger.to_lowercase())
    }

    /// RFC-22 Decision 3-D: walk every agent's `[[channels.discord.bindings]]`
    /// looking for a kind/id pair that matches the message's session shape.
    ///
    /// `message.chat_id` carries the full session id from `channel_reply`
    /// (e.g. `"discord:thread:1501..."` or `"discord:1495..."`).  We extract
    /// `(kind, id)` from it and compare to each binding.
    ///
    /// Returns `None` when no agent has a matching binding (caller falls
    /// through to coarse permission / main-agent rules — backwards-compat).
    fn match_channel_binding(
        &self,
        message: &Message,
        agents: &[&'a LoadedAgent],
    ) -> Option<&'a LoadedAgent> {
        let (binding_kind, binding_id) = parse_session_binding(&message.chat_id)?;

        for agent in agents {
            // Only Discord wiring exposed in v1.11.0; telegram/line bindings
            // can be added when their config types gain a `bindings` field.
            if let Some(channels) = agent.config.channels.as_ref() {
                if let Some(discord) = channels.discord.as_ref() {
                    for b in &discord.bindings {
                        if binding_matches(&b.kind, &b.id, binding_kind, binding_id) {
                            return Some(*agent);
                        }
                    }
                }
            }
        }
        None
    }
}

/// Parse a session id string into a `(kind, id)` pair for binding lookup.
///
/// Supported shapes:
/// - `discord:thread:<id>` → `("thread", "<id>")`
/// - `discord:<channel_id>` → `("channel", "<channel_id>")`
/// - `telegram:<chat_id>` / `line:<id>` → `("channel", "<id>")` (currently
///   discord-only at the resolver layer; included for forward compatibility).
///
/// Returns `None` for malformed inputs (no `:` at all).
fn parse_session_binding(session: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = session.splitn(3, ':').collect();
    match parts.len() {
        3 if parts[1] == "thread" => Some(("thread", parts[2])),
        3 => Some(("channel", parts[1])),
        2 => Some(("channel", parts[1])),
        _ => None,
    }
}

/// True when a configured binding (`cfg_kind`, `cfg_id`) matches the
/// session-derived (`msg_kind`, `msg_id`).  Unknown `cfg_kind` values are
/// treated as no-match (fail-closed).
fn binding_matches(cfg_kind: &str, cfg_id: &str, msg_kind: &str, msg_id: &str) -> bool {
    match cfg_kind {
        "thread" | "channel" => cfg_kind == msg_kind && cfg_id == msg_id,
        "guild" => false, // reserved for future expansion
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_thread_session() {
        assert_eq!(
            parse_session_binding("discord:thread:1501225251910979704"),
            Some(("thread", "1501225251910979704"))
        );
    }

    #[test]
    fn parse_channel_session() {
        assert_eq!(
            parse_session_binding("discord:1495730722156318901"),
            Some(("channel", "1495730722156318901"))
        );
    }

    #[test]
    fn parse_telegram_session_treats_as_channel() {
        // Forward-compat: when telegram bindings land, this should match.
        assert_eq!(
            parse_session_binding("telegram:12345"),
            Some(("channel", "12345"))
        );
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert_eq!(parse_session_binding(""), None);
        assert_eq!(parse_session_binding("nocolon"), None);
    }

    #[test]
    fn binding_matches_thread_exact() {
        assert!(binding_matches("thread", "abc", "thread", "abc"));
        assert!(!binding_matches("thread", "abc", "thread", "xyz"));
        assert!(!binding_matches("thread", "abc", "channel", "abc"));
    }

    #[test]
    fn binding_matches_channel_exact() {
        assert!(binding_matches("channel", "abc", "channel", "abc"));
        assert!(!binding_matches("channel", "abc", "thread", "abc"));
    }

    #[test]
    fn binding_matches_guild_reserved_no_match() {
        // guild bindings are reserved; do not yet match anything.
        assert!(!binding_matches("guild", "abc", "channel", "abc"));
        assert!(!binding_matches("guild", "abc", "thread", "abc"));
    }

    #[test]
    fn binding_matches_unknown_kind_fail_closed() {
        assert!(!binding_matches("nonsense", "abc", "channel", "abc"));
        assert!(!binding_matches("", "abc", "channel", "abc"));
    }
}
