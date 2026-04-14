//! Migration tool: converts agent.toml to Claude Code compatible structure.
//!
//! For each agent in `~/.duduclaw/agents/*/`:
//! 1. Read `agent.toml`
//! 2. Create `.claude/` directory
//! 3. Generate `.claude/settings.local.json`
//! 4. Generate `CLAUDE.md`
//! 5. Generate `.mcp.json`
//! 6. Keep SOUL.md untouched

use std::path::Path;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::error::{DuDuClawError, Result};
use tracing::info;

/// Migrate all agents under `home_dir/agents/` to Claude Code format.
pub async fn migrate(home_dir: &Path) -> Result<()> {
    let agents_dir = home_dir.join("agents");
    if !agents_dir.exists() {
        return Err(DuDuClawError::Config(format!(
            "Agents directory not found: {}",
            agents_dir.display()
        )));
    }

    let mut registry = AgentRegistry::new(agents_dir);
    registry.scan().await?;

    let agents = registry.list();
    if agents.is_empty() {
        println!("No agents found to migrate.");
        return Ok(());
    }

    let mut migrated = 0u32;
    let mut skipped = 0u32;

    for agent in &agents {
        let agent_dir = &agent.dir;
        let agent_name = &agent.config.agent.name;
        let display_name = &agent.config.agent.display_name;

        // Skip already-migrated agents (CLI-H6)
        let claude_dir = agent_dir.join(".claude");
        if claude_dir.join("settings.local.json").exists()
            && agent_dir.join("CLAUDE.md").exists()
            && agent_dir.join(".mcp.json").exists()
        {
            info!(agent = %agent_name, "already migrated, skipping");
            skipped += 1;
            continue;
        }

        info!(agent = %agent_name, "migrating agent");

        // Create .claude/ directory
        let claude_dir = agent_dir.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.map_err(|e| {
            DuDuClawError::Config(format!(
                "Failed to create .claude/ for agent '{}': {e}",
                agent_name
            ))
        })?;

        // Generate .claude/settings.local.json
        // Read allow_bash from agent permissions instead of hardcoding (CLI-L5)
        let allow_bash = agent.config.permissions.can_modify_own_skills;
        let settings = serde_json::json!({
            "model": agent.config.model.preferred,
            "permissions": {
                "allow_bash": allow_bash,
                "allow_mcp": true
            }
        });
        let settings_path = claude_dir.join("settings.local.json");
        let settings_content = serde_json::to_string_pretty(&settings)
            .map_err(|e| DuDuClawError::Config(format!("Failed to serialize settings: {e}")))?;
        tokio::fs::write(&settings_path, &settings_content)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "Failed to write {}: {e}",
                    settings_path.display()
                ))
            })?;

        // Generate CLAUDE.md
        let role = format!("{:?}", agent.config.agent.role).to_lowercase();
        let trigger = &agent.config.agent.trigger;
        let claude_md_content = format!(
            "# Agent Configuration\n\
             \n\
             - Name: {display_name}\n\
             - Role: {role}\n\
             - Trigger: {trigger}\n\
             \n\
             ## Guidelines\n\
             - Use tools defined in .mcp.json when needed\n\
             - Respect budget limits\n"
        );
        let claude_md_path = agent_dir.join("CLAUDE.md");
        tokio::fs::write(&claude_md_path, &claude_md_content)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "Failed to write {}: {e}",
                    claude_md_path.display()
                ))
            })?;

        // Generate .mcp.json — use absolute path so Claude CLI subprocesses
        // can find the MCP server without PATH inheritance.
        let mcp_bin = duduclaw_core::resolve_duduclaw_bin()
            .to_string_lossy()
            .into_owned();
        let mcp_json = serde_json::json!({
            "mcpServers": {
                "duduclaw": {
                    "command": mcp_bin,
                    "args": ["mcp-server"],
                    "env": {}
                }
            }
        });
        let mcp_path = agent_dir.join(".mcp.json");
        let mcp_content = serde_json::to_string_pretty(&mcp_json)
            .map_err(|e| DuDuClawError::Config(format!("Failed to serialize .mcp.json: {e}")))?;
        tokio::fs::write(&mcp_path, &mcp_content)
            .await
            .map_err(|e| {
                DuDuClawError::Config(format!("Failed to write {}: {e}", mcp_path.display()))
            })?;

        println!(
            "  [OK] {} ({}) -> .claude/settings.local.json, CLAUDE.md, .mcp.json",
            display_name, agent_name
        );
        migrated += 1;
    }

    println!();
    println!(
        "Migration complete: {} migrated, {} skipped.",
        migrated, skipped
    );
    Ok(())
}
