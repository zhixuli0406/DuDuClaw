use std::collections::HashMap;

use duduclaw_core::types::Message;
use tracing::{debug, info};

/// Message router that dispatches messages to agents based on channel and trigger words.
pub struct MessageRouter {
    routes: HashMap<String, Vec<String>>, // channel -> agent_ids
    default_agent: Option<String>,
}

impl MessageRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            default_agent: None,
        }
    }

    /// Register a route: messages from `channel` go to `agent_id`.
    pub fn add_route(&mut self, channel: &str, agent_id: &str) {
        info!(channel, agent_id, "Adding route");

        self.routes
            .entry(channel.to_string())
            .or_default()
            .push(agent_id.to_string());
    }

    /// Set the default agent for unrouted messages.
    pub fn set_default(&mut self, agent_id: &str) {
        info!(agent_id, "Setting default agent");
        self.default_agent = Some(agent_id.to_string());
    }

    /// Route a message to the appropriate agent(s).
    ///
    /// Resolution order:
    /// 1. Check if the message channel has specific routes
    /// 2. Check trigger words in the message text
    /// 3. Fall back to default agent
    pub fn route(&self, message: &Message) -> Vec<String> {
        // 1. Check if channel has specific routes
        if let Some(agents) = self.routes.get(&message.channel)
            && !agents.is_empty()
        {
            debug!(
                channel = %message.channel,
                agents = ?agents,
                "Routed by channel"
            );
            return agents.clone();
        }

        // 2. Check trigger words in message text — scan all routes for
        //    agent_ids that appear as whole trigger words in the message text.
        //    Use word-boundary matching (not raw substring) so one agent id does
        //    not mis-trigger when it is a substring of another id or word
        //    (e.g. "bot" must not match inside "robot" or agent id "bot-2").
        let mut triggered: Vec<String> = Vec::new();
        for agents in self.routes.values() {
            for agent_id in agents {
                if duduclaw_core::word_contains_ci(&message.text, agent_id)
                    && !triggered.contains(agent_id)
                {
                    triggered.push(agent_id.clone());
                }
            }
        }
        if !triggered.is_empty() {
            debug!(
                agents = ?triggered,
                "Routed by trigger word"
            );
            return triggered;
        }

        // 3. Fall back to default agent
        if let Some(ref default) = self.default_agent {
            debug!(agent = %default, "Routed to default agent");
            return vec![default.clone()];
        }

        debug!("No route found for message");
        Vec::new()
    }

    /// Remove all routes for an agent.
    pub fn remove_agent(&mut self, agent_id: &str) {
        info!(agent_id, "Removing all routes for agent");

        for agents in self.routes.values_mut() {
            agents.retain(|id| id != agent_id);
        }

        if self.default_agent.as_deref() == Some(agent_id) {
            self.default_agent = None;
        }
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::Message;

    // Build a Message via JSON to avoid pulling chrono in as a direct test dep
    // (serde_json is already a dependency).
    fn msg(channel: &str, text: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "id": "test",
            "message_type": "incoming",
            "channel": channel,
            "chat_id": "chat",
            "sender": "sender",
            "text": text,
            "timestamp": "2026-01-01T00:00:00Z",
            "agent_id": null,
        }))
        .expect("valid test Message")
    }

    #[test]
    fn trigger_word_does_not_match_substring_of_another_id() {
        // "bot" registered on a channel that won't directly route this message,
        // forcing the trigger-word path. It must NOT fire on the word "robot".
        let mut router = MessageRouter::new();
        router.add_route("telegram", "bot");
        let routed = router.route(&msg("discord", "I love my robot"));
        assert!(routed.is_empty(), "substring match leaked: {routed:?}");
    }

    #[test]
    fn trigger_word_matches_whole_word() {
        let mut router = MessageRouter::new();
        router.add_route("telegram", "bot");
        let routed = router.route(&msg("discord", "hey bot, help me"));
        assert_eq!(routed, vec!["bot".to_string()]);
    }

    #[test]
    fn trigger_word_is_case_insensitive() {
        let mut router = MessageRouter::new();
        router.add_route("telegram", "helper");
        let routed = router.route(&msg("discord", "Calling HELPER now"));
        assert_eq!(routed, vec!["helper".to_string()]);
    }

    #[test]
    fn channel_route_takes_precedence_over_trigger() {
        let mut router = MessageRouter::new();
        router.add_route("discord", "primary");
        router.add_route("telegram", "secondary");
        // "secondary" appears in text but channel route wins.
        let routed = router.route(&msg("discord", "ping secondary"));
        assert_eq!(routed, vec!["primary".to_string()]);
    }

    #[test]
    fn falls_back_to_default_when_no_trigger() {
        let mut router = MessageRouter::new();
        router.add_route("telegram", "bot");
        router.set_default("fallback");
        let routed = router.route(&msg("discord", "nothing relevant here"));
        assert_eq!(routed, vec!["fallback".to_string()]);
    }
}
