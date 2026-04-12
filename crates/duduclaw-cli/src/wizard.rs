//! Interactive industry-specific agent setup wizard.
//!
//! Usage: `duduclaw wizard`

use std::path::{Path, PathBuf};

use console::style;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use duduclaw_core::error::{DuDuClawError, Result};

// ── Industry definitions ─────────────────────────────────────

const INDUSTRIES: &[&str] = &[
    "Restaurant",
    "Manufacturing",
    "Trading",
    "Retail",
    "Other",
];

/// Map industry selection index to template directory name (if available).
fn industry_template_dir(index: usize) -> Option<&'static str> {
    match index {
        0 => Some("restaurant"),
        1 => Some("manufacturing"),
        2 => Some("trading"),
        _ => None, // Retail and Other have no template yet
    }
}

const CHANNELS: &[&str] = &["LINE", "Telegram", "Discord", "Slack"];

const FEATURES: &[&str] = &[
    "Customer Service",
    "Sales",
    "Internal Assistant",
    "Inventory",
    "Scheduling",
];

// ── Wizard entry point ───────────────────────────────────────

/// Interactive industry-specific agent setup wizard.
///
/// Guides the user through selecting an industry, entering company info,
/// choosing a channel, naming the agent, selecting features, and optionally
/// importing a data file. On confirmation, scaffolds a new agent directory
/// from the matching template.
pub async fn cmd_wizard(home: &Path) -> Result<()> {
    println!(
        "\n  {} DuDuClaw Agent Setup Wizard\n",
        style("🐾").bold(),
    );

    // 1. Select industry
    let industry_idx = Select::new()
        .with_prompt("Select industry")
        .items(INDUSTRIES)
        .default(0)
        .interact()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;
    let industry_name = INDUSTRIES[industry_idx];

    // 2. Company name
    let company_name: String = Input::new()
        .with_prompt("Company name")
        .interact_text()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;

    // 3. Contact name
    let contact_name: String = Input::new()
        .with_prompt("Contact name")
        .interact_text()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;

    // 4. Primary channel
    let channel_idx = Select::new()
        .with_prompt("Primary channel")
        .items(CHANNELS)
        .default(0)
        .interact()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;
    let channel_name = CHANNELS[channel_idx];

    // 5. Agent name (default derived from company name)
    let default_agent_name = derive_agent_name(&company_name);
    let agent_name: String = Input::new()
        .with_prompt("Agent name")
        .default(default_agent_name)
        .interact_text()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;

    // Validate agent name using the canonical shared validator
    if !duduclaw_core::is_valid_agent_id(&agent_name) {
        return Err(DuDuClawError::Agent(
            "Invalid agent name: must be lowercase alphanumeric with hyphens, 1-64 chars, no leading/trailing hyphen".into(),
        ));
    }

    // 6. Select features
    let feature_indices = MultiSelect::new()
        .with_prompt("Select features (space to toggle, enter to confirm)")
        .items(FEATURES)
        .interact()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;
    let selected_features: Vec<&str> = feature_indices.iter().map(|&i| FEATURES[i]).collect();

    // 7. Optional import file
    let import_path: String = Input::new()
        .with_prompt("Import file path (leave empty to skip)")
        .default(String::new())
        .show_default(false)
        .interact_text()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;
    let import_path = if import_path.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(import_path.trim()))
    };

    // 8. Show summary and confirm
    println!("\n  {}", style("── Summary ──────────────────────").dim());
    println!("  Industry:    {}", style(industry_name).cyan());
    println!("  Company:     {}", style(&company_name).cyan());
    println!("  Contact:     {}", style(&contact_name).cyan());
    println!("  Channel:     {}", style(channel_name).cyan());
    println!("  Agent name:  {}", style(&agent_name).cyan());
    if selected_features.is_empty() {
        println!("  Features:    {}", style("(none)").dim());
    } else {
        println!(
            "  Features:    {}",
            style(selected_features.join(", ")).cyan()
        );
    }
    if let Some(ref path) = import_path {
        println!("  Import file: {}", style(path.display()).cyan());
    }
    println!();

    let confirmed = Confirm::new()
        .with_prompt("Create this agent?")
        .default(true)
        .interact()
        .map_err(|e| DuDuClawError::Config(format!("Prompt error: {e}")))?;

    if !confirmed {
        println!("  {} Wizard cancelled.", style("✗").yellow());
        return Ok(());
    }

    // ── Create agent directory ───────────────────────────────
    let agents_dir = home.join("agents");
    let agent_dir = agents_dir.join(&agent_name);

    if agent_dir.exists() {
        return Err(DuDuClawError::Agent(format!(
            "Agent directory already exists: {}",
            agent_dir.display()
        )));
    }

    tokio::fs::create_dir_all(&agent_dir).await.map_err(|e| {
        DuDuClawError::Agent(format!(
            "Failed to create agent directory {}: {e}",
            agent_dir.display()
        ))
    })?;

    // Copy template files if the industry has a template
    let template_dir = industry_template_dir(industry_idx).map(|dir| {
        // Resolve templates/ relative to the binary or project root
        find_templates_dir().join(dir)
    });

    if let Some(ref tpl_dir) = template_dir {
        if tpl_dir.exists() {
            copy_template_files(tpl_dir, &agent_dir).await?;
        }
    }

    // Ensure agent.toml exists even if no template was copied
    let agent_toml_path = agent_dir.join("agent.toml");
    if agent_toml_path.exists() {
        // Patch the copied template with wizard inputs
        patch_agent_toml(
            &agent_toml_path,
            &agent_name,
            &company_name,
            channel_name,
            &selected_features,
        )
        .await?;
    } else {
        // Generate a minimal agent.toml from scratch
        let toml_content = generate_agent_toml(
            &agent_name,
            &company_name,
            channel_name,
            &selected_features,
        );
        tokio::fs::write(&agent_toml_path, toml_content)
            .await
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to write agent.toml: {e}"))
            })?;
    }

    // Ensure SOUL.md exists
    let soul_path = agent_dir.join("SOUL.md");
    if !soul_path.exists() {
        let soul_content = generate_soul_md(&company_name, &contact_name, industry_name);
        tokio::fs::write(&soul_path, soul_content)
            .await
            .map_err(|e| DuDuClawError::Agent(format!("Failed to write SOUL.md: {e}")))?;
    }

    // Ensure CONTRACT.toml exists
    let contract_path = agent_dir.join("CONTRACT.toml");
    if !contract_path.exists() {
        let contract_content = generate_contract_toml();
        tokio::fs::write(&contract_path, contract_content)
            .await
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to write CONTRACT.toml: {e}"))
            })?;
    }

    // Ensure .mcp.json exists so the newly-created agent actually has the
    // duduclaw MCP tool suite (`create_agent`, `list_agents`, `spawn_agent`,
    // `send_to_agent`, …) available in its Claude Code sessions. Without
    // this file SOUL.md's "always use create_agent" rule is unenforceable
    // because the tool literally isn't registered.
    let mcp_path = agent_dir.join(".mcp.json");
    if !mcp_path.exists() {
        let mcp_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
            .unwrap_or_else(|| "duduclaw".to_string());
        let mcp_json = serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": mcp_bin,
                    "args": ["mcp-server"],
                    "env": {}
                }
            }
        });
        let mcp_content = serde_json::to_string_pretty(&mcp_json)
            .unwrap_or_else(|_| "{}".to_string());
        tokio::fs::write(&mcp_path, mcp_content)
            .await
            .map_err(|e| DuDuClawError::Agent(format!("Failed to write .mcp.json: {e}")))?;
    }

    // Install agent-file-guard PreToolUse hook for the new agent.
    {
        let bin = duduclaw_gateway::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = duduclaw_gateway::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await {
            tracing::warn!(
                agent = %agent_name,
                error = %e,
                "Failed to install agent-file-guard hook (wizard)"
            );
        }
    }

    println!(
        "\n  {} Agent '{}' created at {}",
        style("✓").green().bold(),
        style(&agent_name).cyan().bold(),
        style(agent_dir.display()).dim(),
    );

    // ── Optional import ──────────────────────────────────────
    if let Some(ref file) = import_path {
        if !file.exists() {
            println!(
                "  {} Import file not found: {} — skipping import",
                style("⚠").yellow(),
                file.display(),
            );
        } else {
            println!(
                "  {} Importing from {} ...",
                style("→").dim(),
                file.display(),
            );
            // Detect format from extension, default to "memory" type
            let ext = file
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let format = match ext.as_str() {
                "csv" => "csv",
                "json" => "json",
                "jsonl" => "jsonl",
                _ => {
                    println!(
                        "  {} Cannot auto-detect format from '.{}' — skipping import",
                        style("⚠").yellow(),
                        ext,
                    );
                    return Ok(());
                }
            };

            let memory_db = home.join("memory").join(format!("{agent_name}.db"));
            if let Some(parent) = memory_db.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DuDuClawError::Memory(format!(
                        "Failed to create memory directory: {e}"
                    ))
                })?;
            }

            let engine = duduclaw_memory::SqliteMemoryEngine::new(&memory_db)?;
            let entry_type = if ext == "csv" { "faq" } else { "memory" };

            let count = match format {
                "csv" => {
                    duduclaw_memory::import::import_csv(
                        &engine,
                        &agent_name,
                        file,
                        entry_type,
                    )
                    .await?
                }
                "json" => {
                    duduclaw_memory::import::import_json(&engine, &agent_name, file)
                        .await?
                }
                "jsonl" => {
                    duduclaw_memory::import::import_jsonl(&engine, &agent_name, file)
                        .await?
                }
                _ => unreachable!(),
            };

            println!(
                "  {} Imported {} entries",
                style("✓").green().bold(),
                style(count).cyan().bold(),
            );
        }
    }

    println!(
        "\n  Next steps:\n    1. Edit {} to customize the agent",
        style(agent_dir.join("SOUL.md").display()).dim(),
    );
    println!(
        "    2. Edit {} to add your API keys and channel tokens",
        style(agent_dir.join("agent.toml").display()).dim(),
    );
    println!(
        "    3. Run {} to encrypt sensitive credentials",
        style("duduclaw onboard").bold(),
    );
    println!(
        "    4. Run {} to start the server",
        style("duduclaw run").bold(),
    );

    Ok(())
}

// ── Helper functions ─────────────────────────────────────────

/// Derive a filesystem-safe agent name from a company name.
///
/// Converts to lowercase, replaces non-alphanumeric with hyphens,
/// collapses consecutive hyphens, and trims leading/trailing hyphens.
fn derive_agent_name(company: &str) -> String {
    let raw: String = company
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive hyphens and trim
    let mut result = String::with_capacity(raw.len());
    let mut prev_hyphen = true; // treat start as hyphen to trim leading
    for ch in raw.chars() {
        if ch == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(ch);
            prev_hyphen = false;
        }
    }

    // Trim trailing hyphen
    while result.ends_with('-') {
        result.pop();
    }

    if result.is_empty() {
        "my-agent".to_string()
    } else {
        result
    }
}

/// Locate the `templates/` directory.
///
/// Searches relative to the executable, then falls back to well-known
/// development paths.
fn find_templates_dir() -> PathBuf {
    // Check DUDUCLAW_TEMPLATES env var first
    if let Ok(custom) = std::env::var("DUDUCLAW_TEMPLATES") {
        let p = PathBuf::from(custom);
        if p.is_dir() {
            return p;
        }
    }

    // Next to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("templates");
            if candidate.is_dir() {
                return candidate;
            }
            // Development layout: exe is in target/debug or target/release
            if let Some(pp) = parent.parent().and_then(|p| p.parent()) {
                let candidate = pp.join("templates");
                if candidate.is_dir() {
                    return candidate;
                }
            }
        }
    }

    // Current working directory
    let cwd_templates = PathBuf::from("templates");
    if cwd_templates.is_dir() {
        return cwd_templates;
    }

    // Fallback — will not exist, but callers handle this gracefully
    PathBuf::from("templates")
}

/// Recursively copy all files from a template directory into the agent directory.
async fn copy_template_files(src: &Path, dst: &Path) -> Result<()> {
    let mut entries = tokio::fs::read_dir(src).await.map_err(|e| {
        DuDuClawError::Agent(format!(
            "Failed to read template directory {}: {e}",
            src.display()
        ))
    })?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        DuDuClawError::Agent(format!("Failed to read template entry: {e}"))
    })? {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Security: reject path traversal attempts
        if name_str.contains('/') || name_str.contains('\\') || name_str.starts_with("..") {
            tracing::warn!("Skipping unsafe template filename: {name_str}");
            continue;
        }

        let dst_path = dst.join(&file_name);

        let ft = entry.file_type().await.map_err(|e| {
            DuDuClawError::Agent(format!("Failed to get file type: {e}"))
        })?;

        if ft.is_dir() {
            tokio::fs::create_dir_all(&dst_path).await.map_err(|e| {
                DuDuClawError::Agent(format!(
                    "Failed to create directory {}: {e}",
                    dst_path.display()
                ))
            })?;
            Box::pin(copy_template_files(&src_path, &dst_path)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path).await.map_err(|e| {
                DuDuClawError::Agent(format!(
                    "Failed to copy {} → {}: {e}",
                    src_path.display(),
                    dst_path.display()
                ))
            })?;
        }
    }

    Ok(())
}

/// Patch an existing agent.toml with wizard-provided values.
async fn patch_agent_toml(
    path: &Path,
    agent_name: &str,
    company_name: &str,
    channel: &str,
    features: &[&str],
) -> Result<()> {
    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
        DuDuClawError::Agent(format!("Failed to read agent.toml: {e}"))
    })?;

    let mut doc: toml::Value = content.parse().map_err(|e: toml::de::Error| {
        DuDuClawError::Agent(format!("Failed to parse agent.toml: {e}"))
    })?;

    // Patch [agent] section
    if let Some(agent_table) = doc.get_mut("agent").and_then(|v| v.as_table_mut()) {
        agent_table.insert("name".into(), toml::Value::String(agent_name.into()));
        agent_table.insert(
            "display_name".into(),
            toml::Value::String(format!("{company_name} Assistant")),
        );
    }

    // Patch [permissions].allowed_channels
    let channel_lower = channel.to_lowercase();
    if let Some(perms) = doc.get_mut("permissions").and_then(|v| v.as_table_mut()) {
        perms.insert(
            "allowed_channels".into(),
            toml::Value::Array(vec![toml::Value::String(channel_lower)]),
        );
    }

    // Add features as a custom field for reference
    if !features.is_empty() {
        if let Some(agent_table) = doc.get_mut("agent").and_then(|v| v.as_table_mut()) {
            let feat_array: Vec<toml::Value> = features
                .iter()
                .map(|f| toml::Value::String(f.to_string()))
                .collect();
            agent_table.insert("features".into(), toml::Value::Array(feat_array));
        }
    }

    let serialized = toml::to_string_pretty(&doc).map_err(|e| {
        DuDuClawError::Agent(format!("Failed to serialize agent.toml: {e}"))
    })?;

    tokio::fs::write(path, serialized).await.map_err(|e| {
        DuDuClawError::Agent(format!("Failed to write agent.toml: {e}"))
    })?;

    Ok(())
}

/// Generate a minimal agent.toml for industries without a template.
fn generate_agent_toml(
    agent_name: &str,
    company_name: &str,
    channel: &str,
    features: &[&str],
) -> String {
    let channel_lower = channel.to_lowercase();
    let features_toml: String = if features.is_empty() {
        "[]".to_string()
    } else {
        let items: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
        format!("[{}]", items.join(", "))
    };

    format!(
        r#"# DuDuClaw Agent — generated by wizard

[agent]
name = "{agent_name}"
display_name = "{company_name} Assistant"
role = "main"
status = "active"
trigger = "{agent_name}"
reports_to = "none"
icon = "bot"
features = {features_toml}

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
api_mode = "cli"

[model.local]
model = ""
backend = "llama_cpp"
context_length = 4096
gpu_layers = -1
prefer_local = true
use_router = true

[container]
timeout_ms = 30000
max_concurrent = 2
readonly_project = true
sandbox_enabled = false
network_access = false

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 2000
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = ["{channel_lower}"]

[evolution]
skill_auto_activate = true
skill_security_scan = true
gvu_enabled = true
cognitive_memory = true
max_silence_hours = 12.0
max_gvu_generations = 3
observation_period_hours = 24.0
skill_token_budget = 2500
max_active_skills = 5

[evolution.external_factors]
user_feedback = true
security_events = false
channel_metrics = true
business_context = false
peer_signals = false

[capabilities]
computer_use = false
browser_via_bash = false
allowed_tools = []
denied_tools = []
"#
    )
}

/// Generate a basic SOUL.md for the agent.
fn generate_soul_md(company_name: &str, contact_name: &str, industry: &str) -> String {
    format!(
        r#"# {company_name} AI Assistant

## Identity

You are a helpful AI assistant for {company_name} ({industry} industry).
Your primary contact is {contact_name}.

## Personality

- **Professional and friendly**: Represent the company with warmth and competence
- **Efficient**: Provide clear, concise answers
- **Proactive**: Anticipate needs and suggest next steps when appropriate

## Language

- Primary: 繁體中文 (Traditional Chinese, zh-TW)
- Secondary: English (switch when the customer uses English)
- Tone: Polite but approachable

## Core Responsibilities

1. Answer customer inquiries about the business
2. Assist with common requests and workflows
3. Escalate complex issues to human staff when needed

## Escalation Rules

Hand off to a human when:
- The issue cannot be resolved after 2 exchanges
- Customer explicitly asks to speak with a person
- Requests involve sensitive financial or legal matters
"#
    )
}

/// Generate a basic CONTRACT.toml for the agent.
fn generate_contract_toml() -> String {
    r#"# Behavioral Contract — generated by wizard

[boundaries]
must_not = [
    "share customer personal data with other customers",
    "make promises the business cannot fulfill",
    "provide legal, medical, or financial advice",
    "reveal internal business operations or costs",
]

must_always = [
    "respond in the customer's language (zh-TW or English)",
    "escalate unresolved issues after 2 exchanges",
    "greet customers warmly",
    "suggest contacting the business directly for urgent matters",
]

max_tool_calls_per_turn = 5
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_agent_name_simple() {
        assert_eq!(derive_agent_name("My Company"), "my-company");
    }

    #[test]
    fn test_derive_agent_name_special_chars() {
        assert_eq!(derive_agent_name("Café & Bistro!"), "caf-bistro");
        // é and & are non-ASCII/special → hyphens, consecutive collapsed
    }

    #[test]
    fn test_derive_agent_name_chinese() {
        // CJK chars are non-ASCII, mapped to hyphens then collapsed
        let result = derive_agent_name("好吃餐廳 Good Food");
        assert!(!result.is_empty());
        assert!(result.contains("good"));
    }

    #[test]
    fn test_derive_agent_name_empty() {
        assert_eq!(derive_agent_name(""), "my-agent");
    }

    #[test]
    fn test_derive_agent_name_only_special() {
        assert_eq!(derive_agent_name("!!!"), "my-agent");
    }

    #[test]
    fn test_derive_agent_name_no_trailing_hyphen() {
        let name = derive_agent_name("test-");
        assert!(!name.ends_with('-'));
    }
}
