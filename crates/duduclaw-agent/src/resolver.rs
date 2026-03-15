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
    /// 2. Channel binding — the message channel is in the agent's
    ///    `permissions.allowed_channels` list.
    /// 3. Fall back to the main agent (role = Main).
    pub fn resolve(&self, message: &Message) -> Option<&'a LoadedAgent> {
        let agents = self.registry.list();

        // 1. Trigger word match
        for agent in &agents {
            let trigger = &agent.config.agent.trigger;
            if !trigger.is_empty() && self.match_trigger(&message.text, trigger) {
                return Some(agent);
            }
        }

        // 2. Channel binding
        for agent in &agents {
            let allowed = &agent.config.permissions.allowed_channels;
            if allowed.iter().any(|ch| ch == &message.channel) {
                return Some(agent);
            }
        }

        // 3. Fall back to main agent
        self.registry.main_agent()
    }

    /// Check whether `text` contains the trigger word (case-insensitive).
    fn match_trigger(&self, text: &str, trigger: &str) -> bool {
        text.to_lowercase().contains(&trigger.to_lowercase())
    }
}
