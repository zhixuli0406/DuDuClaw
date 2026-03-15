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
        //    agent_ids that appear as trigger words in the message text
        let mut triggered: Vec<String> = Vec::new();
        for agents in self.routes.values() {
            for agent_id in agents {
                if message.text.contains(agent_id.as_str()) && !triggered.contains(agent_id) {
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
