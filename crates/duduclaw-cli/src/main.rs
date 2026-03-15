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

    /// Start DuDuClaw server (gateway + channels + heartbeat)
    Run {
        /// Skip interactive prompts
        #[arg(long)]
        yes: bool,
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
        Commands::Run { yes } => cmd_run_server(yes).await,
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
async fn cmd_onboard(skip_prompts: bool) -> duduclaw_core::error::Result<()> {
    use console::style;
    use dialoguer::{Input, Password, Select, Confirm};

    let home = duduclaw_home();

    // ── Welcome ──────────────────────────────────────────────
    println!();
    println!("  {} {}", style("🐾").bold(), style("歡迎使用 DuDuClaw v0.1.0").bold());
    println!("  {}", style("Multi-Agent AI Assistant Platform").dim());
    println!();

    // ── 1. Install mode ──────────────────────────────────────
    let quick_mode = if skip_prompts {
        true
    } else {
        let modes = &["快速啟動（推薦）— 使用預設值", "進階設定 — 完整互動式設定"];
        let sel = Select::new()
            .with_prompt("選擇安裝模式")
            .items(modes)
            .default(0)
            .interact()
            .unwrap_or(0);
        sel == 0
    };

    // ── 2. API Key ───────────────────────────────────────────
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let api_key = if !skip_prompts && api_key.is_empty() {
        println!();
        println!("  {} {}", style("▸").cyan(), style("Claude API 設定").bold());
        let auth_methods = &["API Key", "OAuth Token", "稍後設定"];
        let auth_sel = Select::new()
            .with_prompt("認證方式")
            .items(auth_methods)
            .default(0)
            .interact()
            .unwrap_or(2);

        match auth_sel {
            0 => {
                let key: String = Password::new()
                    .with_prompt("API Key")
                    .interact()
                    .unwrap_or_default();
                if !key.is_empty() {
                    println!("  {} API Key 已設定", style("✓").green());
                }
                key
            }
            1 => {
                let token: String = Password::new()
                    .with_prompt("OAuth Token")
                    .interact()
                    .unwrap_or_default();
                if !token.is_empty() {
                    println!("  {} OAuth Token 已設定", style("✓").green());
                }
                token
            }
            _ => {
                println!("  {} 稍後可透過環境變數 ANTHROPIC_API_KEY 設定", style("ℹ").blue());
                String::new()
            }
        }
    } else {
        if !api_key.is_empty() {
            println!("  {} 從環境變數偵測到 API Key", style("✓").green());
        }
        api_key
    };

    // ── 3. Agent config ──────────────────────────────────────
    let (agent_name, agent_display, agent_trigger, agent_soul) = if !skip_prompts && !quick_mode {
        println!();
        println!("  {} {}", style("▸").cyan(), style("AI 助理設定").bold());

        let display: String = Input::new()
            .with_prompt("助理名稱")
            .default("DuDu".to_string())
            .interact_text()
            .unwrap_or_else(|_| "DuDu".to_string());

        let name = display.to_lowercase().replace(' ', "-");

        let trigger: String = Input::new()
            .with_prompt("觸發詞")
            .default(format!("@{display}"))
            .interact_text()
            .unwrap_or_else(|_| format!("@{display}"));

        let soul_options = &[
            "使用預設人格（溫暖友善的助理）",
            "自訂人格描述",
        ];
        let soul_sel = Select::new()
            .with_prompt("人格設定")
            .items(soul_options)
            .default(0)
            .interact()
            .unwrap_or(0);

        let soul = if soul_sel == 1 {
            let custom: String = Input::new()
                .with_prompt("人格描述")
                .interact_text()
                .unwrap_or_default();
            custom
        } else {
            String::new()
        };

        (name, display, trigger, soul)
    } else {
        ("dudu".to_string(), "DuDu".to_string(), "@DuDu".to_string(), String::new())
    };

    // ── 4. Channels (advanced mode) ──────────────────────────
    let mut line_token = String::new();
    let mut line_secret = String::new();
    let mut telegram_token = String::new();
    let mut discord_token = String::new();

    if !skip_prompts && !quick_mode {
        println!();
        println!("  {} {}", style("▸").cyan(), style("通訊通道設定").bold());

        let channel_options = &["LINE", "Telegram", "Discord"];
        let channels: Vec<usize> = dialoguer::MultiSelect::new()
            .with_prompt("選擇要啟用的通道（空白鍵選取，Enter 確認）")
            .items(channel_options)
            .interact()
            .unwrap_or_default();

        for &ch in &channels {
            match ch {
                0 => {
                    line_token = Password::new()
                        .with_prompt("LINE Channel Access Token")
                        .interact()
                        .unwrap_or_default();
                    line_secret = Password::new()
                        .with_prompt("LINE Channel Secret")
                        .interact()
                        .unwrap_or_default();
                    if !line_token.is_empty() {
                        println!("  {} LINE 已設定", style("✓").green());
                    }
                }
                1 => {
                    telegram_token = Password::new()
                        .with_prompt("Telegram Bot Token")
                        .interact()
                        .unwrap_or_default();
                    if !telegram_token.is_empty() {
                        println!("  {} Telegram 已設定", style("✓").green());
                    }
                }
                2 => {
                    discord_token = Password::new()
                        .with_prompt("Discord Bot Token")
                        .interact()
                        .unwrap_or_default();
                    if !discord_token.is_empty() {
                        println!("  {} Discord 已設定", style("✓").green());
                    }
                }
                _ => {}
            }
        }
    }

    // ── 5. Gateway (advanced mode) ───────────────────────────
    let (gw_bind, gw_port) = if !skip_prompts && !quick_mode {
        println!();
        println!("  {} {}", style("▸").cyan(), style("Gateway 設定").bold());

        let bind_options = &["localhost (127.0.0.1) — 推薦", "LAN (0.0.0.0)", "自訂"];
        let bind_sel = Select::new()
            .with_prompt("Gateway 綁定地址")
            .items(bind_options)
            .default(0)
            .interact()
            .unwrap_or(0);

        let bind = match bind_sel {
            0 => "127.0.0.1".to_string(),
            1 => "0.0.0.0".to_string(),
            _ => {
                Input::new()
                    .with_prompt("綁定地址")
                    .default("127.0.0.1".to_string())
                    .interact_text()
                    .unwrap_or_else(|_| "127.0.0.1".to_string())
            }
        };

        let port: u16 = Input::new()
            .with_prompt("Gateway Port")
            .default(18789u16)
            .interact_text()
            .unwrap_or(18789);

        (bind, port)
    } else {
        ("127.0.0.1".to_string(), 18789u16)
    };

    // ── 6. Budget (advanced mode) ────────────────────────────
    let monthly_budget_usd: u32 = if !skip_prompts && !quick_mode {
        println!();
        Input::new()
            .with_prompt("每月預算上限 (USD)")
            .default(50u32)
            .interact_text()
            .unwrap_or(50)
    } else {
        50
    };

    // ── Confirm ──────────────────────────────────────────────
    if !skip_prompts {
        println!();
        println!("  {} {}", style("📋").bold(), style("設定摘要").bold());
        println!("  ├ 助理名稱：{}", style(&agent_display).cyan());
        println!("  ├ 觸發詞：{}", style(&agent_trigger).cyan());
        println!("  ├ API Key：{}", if api_key.is_empty() { style("未設定").red().to_string() } else { style("已設定").green().to_string() });
        println!("  ├ Gateway：{}:{}", style(&gw_bind).cyan(), style(gw_port).cyan());
        println!("  ├ 月預算：${}", style(monthly_budget_usd).cyan());
        if !line_token.is_empty() { println!("  ├ LINE：{}", style("已設定").green()); }
        if !telegram_token.is_empty() { println!("  ├ Telegram：{}", style("已設定").green()); }
        if !discord_token.is_empty() { println!("  ├ Discord：{}", style("已設定").green()); }
        println!("  └ 資料目錄：{}", style(home.display()).dim());
        println!();

        let proceed = Confirm::new()
            .with_prompt("確認並開始安裝？")
            .default(true)
            .interact()
            .unwrap_or(true);

        if !proceed {
            println!("  {} 已取消", style("✗").red());
            return Ok(());
        }
    }

    // ══════════════════════════════════════════════════════════
    // Write files
    // ══════════════════════════════════════════════════════════

    println!();
    println!("  {} {}", style("⚙").bold(), style("正在建立環境...").bold());

    // Create directory structure
    let agent_dir = home.join("agents").join(&agent_name);
    for dir in &[
        home.clone(),
        home.join("agents"),
        agent_dir.clone(),
        agent_dir.join("SKILLS"),
        home.join("logs"),
    ] {
        tokio::fs::create_dir_all(dir).await.map_err(|e| {
            DuDuClawError::Config(format!("Failed to create directory {}: {e}", dir.display()))
        })?;
    }

    // config.toml
    let config_path = home.join("config.toml");
    let config_content = format!(
        r#"# DuDuClaw configuration
# Generated by `duduclaw onboard`

[general]
default_agent = "{agent_name}"
log_level = "info"

[gateway]
bind = "{gw_bind}"
port = {gw_port}

[rotation]
strategy = "priority"
health_check_interval_seconds = 60
cooldown_after_rate_limit_seconds = 120

[channels]
line_channel_token = "{line_token}"
line_channel_secret = "{line_secret}"
telegram_bot_token = "{telegram_token}"
discord_bot_token = "{discord_token}"
"#
    );
    tokio::fs::write(&config_path, config_content).await.map_err(|e| {
        DuDuClawError::Config(format!("Failed to write {}: {e}", config_path.display()))
    })?;
    println!("  {} {}", style("✓").green(), config_path.display());

    // agent.toml
    let agent_toml_path = agent_dir.join("agent.toml");
    let budget_cents = monthly_budget_usd as u64 * 100;
    let agent_toml = format!(
        r#"[agent]
name = "{agent_name}"
display_name = "{agent_display}"
role = "main"
status = "active"
trigger = "{agent_trigger}"
reports_to = ""
icon = "🐾"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 3
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = true
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = {budget_cents}
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = true
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = ["*"]

[evolution]
micro_reflection = true
meso_reflection = true
macro_reflection = true
skill_auto_activate = true
skill_security_scan = true
"#
    );
    tokio::fs::write(&agent_toml_path, agent_toml).await.map_err(|e| {
        DuDuClawError::Config(format!("Failed to write {}: {e}", agent_toml_path.display()))
    })?;
    println!("  {} {}", style("✓").green(), agent_toml_path.display());

    // SOUL.md
    let soul_path = agent_dir.join("SOUL.md");
    let soul_content = if agent_soul.is_empty() {
        format!(
            r#"# {agent_display} — 你的 AI 助理

我是 {agent_display}，一個溫暖、可靠的 AI 助理，由 DuDuClaw 驅動。

## 核心價值

- 用心傾聽，真誠回應
- 撰寫乾淨、可維護的程式碼
- 清晰解釋我的思考過程
- 需要時主動詢問釐清

## 個性特質

- 專業但不冰冷
- 高效但不急躁
- 精準但有溫度
"#
        )
    } else {
        format!("# {agent_display}\n\n{agent_soul}\n")
    };
    tokio::fs::write(&soul_path, soul_content).await.map_err(|e| {
        DuDuClawError::Config(format!("Failed to write {}: {e}", soul_path.display()))
    })?;
    println!("  {} {}", style("✓").green(), soul_path.display());

    // ── Done ─────────────────────────────────────────────────
    println!();
    println!("  {} {}", style("✓").green().bold(), style("設定完成！").bold());
    println!();
    println!("  {}", style("下一步：").bold());
    println!("  $ {} {}", style("duduclaw run").cyan(), style("# 啟動服務").dim());
    println!("  $ {} {}", style("duduclaw agent").cyan(), style("# CLI 對話").dim());
    println!("  $ {} {}", style("duduclaw status").cyan(), style("# 檢查狀態").dim());

    if api_key.is_empty() {
        println!();
        println!("  {} 記得設定 API Key：", style("⚠").yellow());
        println!("  $ {}", style("export ANTHROPIC_API_KEY=sk-ant-...").cyan());
    }

    println!();
    Ok(())
}

/// `duduclaw run [--yes]` - Start the DuDuClaw server (gateway + dashboard).
async fn cmd_run_server(yes: bool) -> duduclaw_core::error::Result<()> {
    let home = duduclaw_home();

    // Auto-onboard if config doesn't exist
    if !home.join("config.toml").exists() {
        if yes {
            cmd_onboard(true).await?;
        } else {
            println!("No configuration found. Run `duduclaw onboard` first.");
            return Ok(());
        }
    }

    println!("🐾 DuDuClaw Server Starting...");
    println!("   Gateway: http://0.0.0.0:18789");
    println!("   Dashboard: http://localhost:18789");
    println!("   Press Ctrl+C to stop\n");

    let config = duduclaw_gateway::GatewayConfig {
        bind: std::env::var("DUDUCLAW_BIND").unwrap_or_else(|_| "127.0.0.1".to_string()),
        port: std::env::var("DUDUCLAW_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(18789),
        auth_token: None,
    };

    duduclaw_gateway::start_gateway(config).await
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
