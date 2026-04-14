//! ACP server — generates protocol discovery cards (`.well-known` endpoints).

use serde::{Deserialize, Serialize};

/// Skill descriptor within an Agent Card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// Capabilities advertised by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub multi_turn: bool,
    pub tool_use: bool,
}

/// A2A-compatible Agent Card returned at `/.well-known/agent.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub capabilities: AgentCapabilities,
    pub skills: Vec<AgentSkill>,
}

/// Minimal ACP server that can generate discovery metadata.
pub struct AcpServer;

impl AcpServer {
    /// Generate an A2A-compatible Agent Card.
    pub fn generate_agent_card(name: &str, description: &str, url: &str) -> AgentCard {
        AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            url: url.to_string(),
            version: duduclaw_gateway::updater::current_version().to_string(),
            capabilities: AgentCapabilities {
                streaming: true,
                multi_turn: true,
                tool_use: true,
            },
            skills: vec![
                AgentSkill {
                    name: "chat".to_string(),
                    description: "Multi-turn conversation".to_string(),
                    tags: vec!["conversation".to_string()],
                },
                AgentSkill {
                    name: "channel_messaging".to_string(),
                    description: "Telegram/LINE/Discord messaging".to_string(),
                    tags: vec!["messaging".to_string()],
                },
                AgentSkill {
                    name: "memory".to_string(),
                    description: "Search and store memories".to_string(),
                    tags: vec!["memory".to_string()],
                },
            ],
        }
    }
}
