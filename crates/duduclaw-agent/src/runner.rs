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
        let model_id = agent.config.model.preferred.clone();
        let api_key = get_api_key(&self.agents_dir).await;

        println!(
            "DuDuClaw: {} is ready! Type your message (Ctrl+D to exit)",
            agent.config.agent.display_name
        );
        if api_key.is_empty() {
            println!("  [warn] No API key found — set ANTHROPIC_API_KEY or run `duduclaw onboard`");
        }
        println!("---");

        // Simple stdin read loop
        let stdin = tokio::io::stdin();
        let reader = tokio::io::BufReader::new(stdin);
        use tokio::io::AsyncBufReadExt;
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let input = line.trim();
            if input.is_empty() {
                continue;
            }

            print!("\n[{}]: ", agent.config.agent.display_name);
            // Flush before blocking on subprocess
            use std::io::Write as _;
            let _ = std::io::stdout().flush();

            // Call Claude Code SDK (claude CLI)
            let response = call_claude(input, &model_id, &system_prompt, &api_key).await;
            println!("{response}");
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

// ── Standalone helpers ───────────────────────────────────────

/// Get the API key from env var or config.toml.
async fn get_api_key(agents_dir: &PathBuf) -> String {
    // 1. Environment variable
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // 2. config.toml in home dir (agents_dir parent is home/agents, parent of that is home)
    if let Some(home) = agents_dir.parent().and_then(|p| p.parent()) {
        let config_path = home.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
            if let Ok(table) = content.parse::<toml::Value>() {
                if let Some(key) = table.get("api")
                    .and_then(|v| v.get("anthropic_api_key"))
                    .and_then(|v| v.as_str())
                {
                    if !key.is_empty() {
                        return key.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// Call the claude CLI (Claude Code SDK) with a prompt and return the response.
async fn call_claude(prompt: &str, model: &str, system_prompt: &str, api_key: &str) -> String {
    // Find claude binary
    let claude = match which_claude() {
        Some(p) => p,
        None => return "(Claude CLI not found. Install: npm install -g @anthropic-ai/claude-code)".to_string(),
    };

    if api_key.is_empty() {
        return "(No API key configured. Run: export ANTHROPIC_API_KEY=sk-ant-...)".to_string();
    }

    let mut cmd = tokio::process::Command::new(&claude);
    cmd.args(["-p", prompt, "--model", model, "--output-format", "text"]);
    if !system_prompt.is_empty() {
        cmd.args(["--system-prompt", system_prompt]);
    }
    cmd.env("ANTHROPIC_API_KEY", api_key);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        cmd.output(),
    ).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() { "(empty response)".to_string() } else { text }
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            format!("(claude error: {})", stderr.chars().take(200).collect::<String>())
        }
        Ok(Err(e)) => format!("(spawn error: {e})"),
        Err(_) => "(timeout after 120s)".to_string(),
    }
}

/// Find the `claude` CLI binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
}
