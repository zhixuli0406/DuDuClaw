use std::path::PathBuf;

use clap::{Parser, Subcommand};
use duduclaw_agent::AgentRunner;
use duduclaw_core::error::DuDuClawError;
use duduclaw_core::types::CheckStatus;
mod service;

#[derive(Parser)]
#[command(name = "duduclaw", about = "DuDuClaw - Multi-Agent Orchestration CLI")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize DuDuClaw environment and configuration
    Onboard {
        /// Skip interactive prompts and use defaults
        #[arg(long)]
        yes: bool,
    },

    /// Run an agent
    Run {
        /// Agent name or path
        agent: String,
    },

    /// Manage agents (or start interactive session with no subcommand)
    Agent {
        #[command(subcommand)]
        command: Option<AgentCommands>,
    },

    /// Start the WebSocket gateway server
    Gateway,

    /// Show system status
    Status,

    /// Run system diagnostics
    Doctor,

    /// Manage the DuDuClaw background service
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },

    /// Print version information
    Version,
}

#[derive(Subcommand)]
enum AgentCommands {
    /// List all registered agents
    List,

    /// Create a new agent from template
    Create {
        /// Agent name
        name: String,
    },

    /// Inspect agent details
    Inspect {
        /// Agent name or ID
        agent: String,
    },

    /// Pause a running agent
    Pause {
        /// Agent name or ID
        agent: String,
    },

    /// Resume a paused agent
    Resume {
        /// Agent name or ID
        agent: String,
    },

    /// Start interactive session with a specific agent
    Run {
        /// Agent name
        name: String,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    /// Install DuDuClaw as a system service
    Install,

    /// Start the background service
    Start,

    /// Stop the background service
    Stop,

    /// Show service status
    Status,

    /// Show service logs
    Logs {
        /// Number of lines to show
        #[arg(short, long, default_value_t = 50)]
        lines: usize,
    },

    /// Uninstall the system service
    Uninstall,
}

/// Resolve the DuDuClaw home directory (~/.duduclaw).
fn duduclaw_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".duduclaw")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let result = run(cli).await;
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> duduclaw_core::error::Result<()> {
    match cli.command {
        Commands::Onboard { yes } => cmd_onboard(yes).await,
        Commands::Run { agent } => cmd_run_agent(&agent).await,
        Commands::Agent { command } => match command {
            None => cmd_agent_interactive(None).await,
            Some(AgentCommands::List) => cmd_agent_list().await,
            Some(AgentCommands::Create { name }) => {
                println!("TODO: implement agent create '{name}'");
                Ok(())
            }
            Some(AgentCommands::Inspect { agent }) => cmd_agent_inspect(&agent).await,
            Some(AgentCommands::Pause { agent }) => {
                println!("TODO: implement agent pause '{agent}'");
                Ok(())
            }
            Some(AgentCommands::Resume { agent }) => {
                println!("TODO: implement agent resume '{agent}'");
                Ok(())
            }
            Some(AgentCommands::Run { name }) => cmd_agent_interactive(Some(&name)).await,
        },
        Commands::Gateway => {
            println!("TODO: implement gateway");
            Ok(())
        }
        Commands::Status => cmd_status().await,
        Commands::Doctor => cmd_doctor().await,
        Commands::Service { command } => {
            match command {
                ServiceCommands::Install => {
                    println!("TODO: implement service install");
                    service::detect_platform();
                }
                ServiceCommands::Start => {
                    println!("TODO: implement service start");
                }
                ServiceCommands::Stop => {
                    println!("TODO: implement service stop");
                }
                ServiceCommands::Status => {
                    println!("TODO: implement service status");
                }
                ServiceCommands::Logs { lines } => {
                    println!("TODO: implement service logs (lines: {lines})");
                }
                ServiceCommands::Uninstall => {
                    println!("TODO: implement service uninstall");
                }
            }
            Ok(())
        }
        Commands::Version => {
            println!("duduclaw {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

/// `duduclaw onboard [--yes]`
async fn cmd_onboard(_skip_prompts: bool) -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    println!("Initializing DuDuClaw environment at {}", home.display());

    // Create directory structure
    let dirs_to_create = [
        home.clone(),
        home.join("agents"),
        home.join("agents").join("dudu"),
        home.join("agents").join("dudu").join("SKILLS"),
    ];

    for dir in &dirs_to_create {
        tokio::fs::create_dir_all(dir).await.map_err(|e| {
            DuDuClawError::Config(format!(
                "Failed to create directory {}: {e}",
                dir.display()
            ))
        })?;
    }

    // Create config.toml if it doesn't exist
    let config_path = home.join("config.toml");
    if !config_path.exists() {
        let config_content = r#"# DuDuClaw configuration
# Generated by `duduclaw onboard`

[general]
default_agent = "dudu"
log_level = "info"

[api]
# Set via ANTHROPIC_API_KEY environment variable
anthropic_api_key = ""

[docker]
enabled = true
default_image = "duduclaw/agent:latest"
"#
        .to_string();
        tokio::fs::write(&config_path, config_content)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "Failed to write {}: {e}",
                    config_path.display()
                ))
            })?;
        println!("  Created {}", config_path.display());
        println!("  NOTE: Set your API key via the ANTHROPIC_API_KEY environment variable.");
    } else {
        println!("  Config already exists at {}", config_path.display());
    }

    // Create default "dudu" agent
    let agent_toml_path = home.join("agents").join("dudu").join("agent.toml");
    if !agent_toml_path.exists() {
        let agent_toml = r#"[agent]
name = "dudu"
display_name = "DuDu"
role = "main"
status = "active"
trigger = "@DuDu"
reports_to = "human"
icon = "paw"

[model]
preferred = "claude-sonnet-4-20250514"
fallback = "claude-haiku-4-20250514"
account_pool = ["default"]

[container]
timeout_ms = 300000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["cli"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#;
        tokio::fs::write(&agent_toml_path, agent_toml)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "Failed to write {}: {e}",
                    agent_toml_path.display()
                ))
            })?;
        println!("  Created {}", agent_toml_path.display());
    }

    // Create SOUL.md for the default agent
    let soul_path = home.join("agents").join("dudu").join("SOUL.md");
    if !soul_path.exists() {
        let soul_content = r#"# DuDu - Your Friendly AI Assistant

You are DuDu, a helpful and friendly AI assistant powered by DuDuClaw.
You assist users with coding, planning, and general tasks.

## Core Values

- Be helpful, honest, and harmless
- Write clean, maintainable code
- Explain your reasoning clearly
- Ask for clarification when needed
"#;
        tokio::fs::write(&soul_path, soul_content)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "Failed to write {}: {e}",
                    soul_path.display()
                ))
            })?;
        println!("  Created {}", soul_path.display());
    }

    println!("\nDuDuClaw environment initialized successfully!");
    println!("Run `duduclaw agent` to start chatting with DuDu.");
    Ok(())
}

/// `duduclaw run <agent>` - Run a named agent interactively.
async fn cmd_run_agent(agent_name: &str) -> duduclaw_core::error::Result<()> {
    cmd_agent_interactive(Some(agent_name)).await
}

/// `duduclaw agent` or `duduclaw agent run <name>` - Interactive session.
async fn cmd_agent_interactive(
    agent_name: Option<&str>,
) -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    let runner = AgentRunner::new(home).await?;
    runner.run_interactive(agent_name).await
}

/// `duduclaw agent list`
async fn cmd_agent_list() -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    let runner = match AgentRunner::new(home.clone()).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "No agents found. Run `duduclaw onboard` first.\n({})",
                e
            );
            return Ok(());
        }
    };

    let agents = runner.list_agents();
    if agents.is_empty() {
        println!("No agents found in {}", home.join("agents").display());
        println!("Run `duduclaw onboard` to create a default agent.");
        return Ok(());
    }

    println!("Registered agents:\n");
    println!(
        "{:<15} {:<20} {:<12} {:<10}",
        "NAME", "DISPLAY", "ROLE", "STATUS"
    );
    println!("{}", "-".repeat(57));

    for agent in &agents {
        let info = &agent.config.agent;
        println!(
            "{:<15} {:<20} {:<12?} {:<10?}",
            info.name, info.display_name, info.role, info.status
        );
    }

    println!("\n{} agent(s) total.", agents.len());
    Ok(())
}

/// `duduclaw agent inspect <name>`
async fn cmd_agent_inspect(name: &str) -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    let runner = AgentRunner::new(home).await?;
    let agents = runner.list_agents();

    let agent = agents
        .iter()
        .find(|a| a.config.agent.name == name)
        .ok_or_else(|| DuDuClawError::Agent(format!("Agent '{}' not found", name)))?;

    let info = &agent.config.agent;
    let model = &agent.config.model;
    let budget = &agent.config.budget;
    let _perms = &agent.config.permissions;

    println!("Agent: {}", info.display_name);
    println!("  Name:        {}", info.name);
    println!("  Role:        {:?}", info.role);
    println!("  Status:      {:?}", info.status);
    println!("  Trigger:     {}", info.trigger);
    println!("  Reports to:  {}", info.reports_to);
    println!("  Icon:        {}", info.icon);
    println!("  Directory:   {}", agent.dir.display());
    println!();
    println!("Model:");
    println!("  Preferred:   {}", model.preferred);
    println!("  Fallback:    {}", model.fallback);
    println!();
    println!("Budget:");
    println!("  Monthly:     {} cents", budget.monthly_limit_cents);
    println!("  Warn at:     {}%", budget.warn_threshold_percent);
    println!("  Hard stop:   {}", budget.hard_stop);
    println!();
    println!("Files:");
    println!(
        "  SOUL.md:     {}",
        if agent.soul.is_some() { "yes" } else { "no" }
    );
    println!(
        "  IDENTITY.md: {}",
        if agent.identity.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "  MEMORY.md:   {}",
        if agent.memory.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    println!("  Skills:      {}", agent.skills.len());
    for skill in &agent.skills {
        println!("    - {}", skill.name);
    }

    Ok(())
}

/// `duduclaw status`
async fn cmd_status() -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    println!("DuDuClaw Status");
    println!("{}", "=".repeat(40));
    println!("Home:    {}", home.display());
    println!(
        "Config:  {}",
        if home.join("config.toml").exists() {
            "found"
        } else {
            "not found"
        }
    );

    // Count agents
    let agent_count = match AgentRunner::new(home).await {
        Ok(runner) => runner.list_agents().len(),
        Err(_) => 0,
    };
    println!("Agents:  {}", agent_count);

    // Docker status
    match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => match docker.ping().await {
            Ok(_) => println!("Docker:  connected"),
            Err(e) => println!("Docker:  not reachable ({})", e),
        },
        Err(e) => println!("Docker:  not available ({})", e),
    }

    Ok(())
}

/// `duduclaw doctor`
async fn cmd_doctor() -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    println!("DuDuClaw Doctor");
    println!("{}", "=".repeat(40));

    let mut checks: Vec<(String, CheckStatus, String)> = Vec::new();

    // Check 1: config.toml exists
    let config_path = home.join("config.toml");
    if config_path.exists() {
        checks.push((
            "Config file".into(),
            CheckStatus::Pass,
            format!("Found at {}", config_path.display()),
        ));
    } else {
        checks.push((
            "Config file".into(),
            CheckStatus::Fail,
            "Missing. Run `duduclaw onboard` to create.".into(),
        ));
    }

    // Check 2: agents directory
    let agents_dir = home.join("agents");
    if agents_dir.exists() {
        match AgentRunner::new(home).await {
            Ok(runner) => {
                let count = runner.list_agents().len();
                if count > 0 {
                    checks.push((
                        "Agents".into(),
                        CheckStatus::Pass,
                        format!("{} agent(s) found", count),
                    ));
                } else {
                    checks.push((
                        "Agents".into(),
                        CheckStatus::Warn,
                        "Agents directory exists but no valid agents found.".into(),
                    ));
                }
            }
            Err(e) => {
                checks.push((
                    "Agents".into(),
                    CheckStatus::Warn,
                    format!("Could not scan agents: {e}"),
                ));
            }
        }
    } else {
        checks.push((
            "Agents".into(),
            CheckStatus::Fail,
            "Agents directory not found. Run `duduclaw onboard`.".into(),
        ));
    }

    // Check 3: Docker availability
    match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => match docker.ping().await {
            Ok(_) => {
                checks.push((
                    "Docker".into(),
                    CheckStatus::Pass,
                    "Docker daemon is reachable.".into(),
                ));
            }
            Err(e) => {
                checks.push((
                    "Docker".into(),
                    CheckStatus::Warn,
                    format!("Docker installed but not reachable: {e}"),
                ));
            }
        },
        Err(e) => {
            checks.push((
                "Docker".into(),
                CheckStatus::Warn,
                format!("Docker not available: {e}. Container mode won't work."),
            ));
        }
    }

    // Print results
    let mut has_failure = false;
    for (name, status, message) in &checks {
        let icon = match status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => {
                has_failure = true;
                "FAIL"
            }
        };
        println!("  [{icon}] {name}: {message}");
    }

    println!();
    if has_failure {
        println!("Some checks failed. Run `duduclaw onboard` to fix.");
    } else {
        println!("All checks passed!");
    }

    Ok(())
}
