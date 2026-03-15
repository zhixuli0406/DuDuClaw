use std::path::PathBuf;

use duduclaw_core::error::{DuDuClawError, Result};
use tracing::info;

use crate::registry::{AgentRegistry, LoadedAgent};

/// Runs an agent in CLI mode (direct execution, no container for Phase 2).
pub struct AgentRunner {
    agents_dir: PathBuf,
    registry: AgentRegistry,
}

impl AgentRunner {
    /// Create a new runner, scanning the agents directory under `home_dir`.
    pub async fn new(home_dir: PathBuf) -> Result<Self> {
        let agents_dir = home_dir.join("agents");
        let mut registry = AgentRegistry::new(agents_dir.clone());
        registry.scan().await?;
        Ok(Self {
            agents_dir,
            registry,
        })
    }

    /// Run an interactive CLI session with the specified agent (or the main agent).
    pub async fn run_interactive(&self, agent_name: Option<&str>) -> Result<()> {
        let agent = match agent_name {
            Some(name) => self
                .registry
                .get(name)
                .ok_or_else(|| DuDuClawError::Agent(format!("Agent '{}' not found", name)))?,
            None => self
                .registry
                .main_agent()
                .ok_or_else(|| DuDuClawError::Agent("No main agent configured".into()))?,
        };

        info!(
            agent = %agent.config.agent.name,
            display_name = %agent.config.agent.display_name,
            "Starting interactive session"
        );

        // Build the system prompt from agent files
        let system_prompt = self.build_system_prompt(agent);

        println!(
            "DuDuClaw: {} is ready! Type your message (Ctrl+D to exit)",
            agent.config.agent.display_name
        );
        println!("---");

        // Simple stdin read loop
        let stdin = tokio::io::stdin();
        let reader = tokio::io::BufReader::new(stdin);
        use tokio::io::AsyncBufReadExt;
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            // For Phase 2, echo what the agent would do.
            // Real Claude Code SDK integration will come in Phase 3.
            println!(
                "\n[{}]: Received: \"{}\"\n(Claude Code SDK integration pending - Phase 3)",
                agent.config.agent.display_name,
                line.trim()
            );
            println!("System prompt length: {} chars", system_prompt.len());
            println!("---");
        }

        println!("\nSession ended. Goodbye!");
        Ok(())
    }

    /// Build a system prompt from the agent's SOUL.md, IDENTITY.md, SKILLS/, etc.
    fn build_system_prompt(&self, agent: &LoadedAgent) -> String {
        let mut parts = Vec::new();

        if let Some(soul) = &agent.soul {
            parts.push(format!("# Soul\n{}", soul));
        }

        if let Some(identity) = &agent.identity {
            parts.push(format!("# Identity\n{}", identity));
        }

        for skill in &agent.skills {
            parts.push(format!("# Skill: {}\n{}", skill.name, skill.content));
        }

        if let Some(memory) = &agent.memory {
            parts.push(format!("# Memory\n{}", memory));
        }

        parts.join("\n\n---\n\n")
    }

    /// Return a list of all loaded agents.
    pub fn list_agents(&self) -> Vec<&LoadedAgent> {
        self.registry.list()
    }

    /// Return the agents directory path.
    pub fn agents_dir(&self) -> &PathBuf {
        &self.agents_dir
    }
}
