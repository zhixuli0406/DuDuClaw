//! MCP server configuration template generator.
//!
//! Generates `.mcp.json` files for agent directories to connect
//! external MCP servers (e.g., Playwright for browser automation).

use std::path::Path;
use serde::{Serialize, Deserialize};
use tracing::info;

/// MCP server configuration for an agent directory's `.mcp.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::HashMap<String, McpServerDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDef {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Generate a Playwright MCP server configuration.
pub fn playwright_mcp_config(headless: bool) -> McpConfig {
    let mut args = vec!["@anthropic-ai/mcp-server-playwright".to_string()];
    if headless {
        args.push("--headless".to_string());
    }

    let mut servers = std::collections::HashMap::new();
    servers.insert("playwright".to_string(), McpServerDef {
        command: "npx".to_string(),
        args,
        env: std::collections::HashMap::new(),
    });

    McpConfig { mcp_servers: servers }
}

/// Write `.mcp.json` to an agent directory.
/// Returns Ok(true) if written, Ok(false) if file already exists.
pub fn write_mcp_config(agent_dir: &Path, config: &McpConfig) -> Result<bool, String> {
    use std::io::Write;

    let path = agent_dir.join(".mcp.json");
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;

    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            f.write_all(json.as_bytes()).map_err(|e| format!("Failed to write MCP config: {e}"))?;
            duduclaw_core::platform::set_owner_only(&path).ok();
            info!(path = %path.display(), "MCP config written");
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            info!(path = %path.display(), "MCP config already exists, skipping");
            Ok(false)
        }
        Err(e) => Err(format!("Failed to create MCP config: {e}")),
    }
}

/// Merge Playwright server into an existing `.mcp.json`, preserving other servers.
pub fn ensure_playwright_in_config(agent_dir: &Path, headless: bool) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");

    let mut config = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read MCP config: {e}"))?;
        serde_json::from_str::<McpConfig>(&content)
            .map_err(|e| format!("Failed to parse MCP config: {e}"))?
    } else {
        McpConfig { mcp_servers: std::collections::HashMap::new() }
    };

    if config.mcp_servers.contains_key("playwright") {
        return Ok(()); // Already configured
    }

    let playwright = playwright_mcp_config(headless);
    config.mcp_servers.extend(playwright.mcp_servers);

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;

    info!(path = %path.display(), "Playwright MCP server added to config");
    Ok(())
}

/// Generate a Browserbase MCP server configuration.
///
/// The `api_key` and `project_id` parameters are ignored; the generated config
/// always uses environment variable references (`${BROWSERBASE_API_KEY}` and
/// `${BROWSERBASE_PROJECT_ID}`) so that actual secrets are never written to
/// `.mcp.json` on disk. Callers must ensure the corresponding environment
/// variables are set at runtime.
pub fn browserbase_mcp_config(_api_key: &str, _project_id: &str) -> McpConfig {
    let mut env = std::collections::HashMap::new();
    env.insert("BROWSERBASE_API_KEY".to_string(), "${BROWSERBASE_API_KEY}".to_string());
    env.insert("BROWSERBASE_PROJECT_ID".to_string(), "${BROWSERBASE_PROJECT_ID}".to_string());

    let mut servers = std::collections::HashMap::new();
    servers.insert("browserbase".to_string(), McpServerDef {
        command: "npx".to_string(),
        args: vec!["@browserbasehq/mcp-server-browserbase".to_string()],
        env,
    });

    McpConfig { mcp_servers: servers }
}

/// Merge Browserbase server into an existing `.mcp.json`, preserving other servers.
pub fn ensure_browserbase_in_config(
    agent_dir: &Path,
    api_key: &str,
    project_id: &str,
) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");

    let mut config = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read MCP config: {e}"))?;
        serde_json::from_str::<McpConfig>(&content)
            .map_err(|e| format!("Failed to parse MCP config: {e}"))?
    } else {
        McpConfig { mcp_servers: std::collections::HashMap::new() }
    };

    if config.mcp_servers.contains_key("browserbase") {
        return Ok(());
    }

    let bb = browserbase_mcp_config(api_key, project_id);
    config.mcp_servers.extend(bb.mcp_servers);

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;
    duduclaw_core::platform::set_owner_only(&path).ok();

    info!(path = %path.display(), "Browserbase MCP server added to config");
    Ok(())
}

/// Ensure the `duduclaw` MCP server is registered in Claude Code's **global**
/// settings (`~/.claude/settings.json`), not per-agent `.mcp.json`.
///
/// The DuDuClaw MCP server provides platform-level tools (send_to_agent,
/// list_cron_tasks, create_agent, etc.) that ALL agents need. Placing it
/// globally avoids per-agent `.mcp.json` maintenance and the production bugs
/// caused by missing or stale configs.
///
/// Agent-specific MCP servers (Playwright, Browserbase, etc.) stay in
/// per-agent `.mcp.json` — Claude CLI merges both layers.
///
/// Returns `Ok(true)` if settings.json was updated, `Ok(false)` if no change needed.
pub fn ensure_global_mcp_server() -> Result<bool, String> {
    let abs_bin = duduclaw_core::resolve_duduclaw_bin();
    let abs_str = abs_bin.to_string_lossy().into_owned();
    if !std::path::Path::new(&abs_str).is_absolute() {
        return Ok(false);
    }

    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let settings_path = home.join(".claude").join("settings.json");

    // Read existing settings (or create empty)
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("Failed to read {}: {e}", settings_path.display()))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {e}", settings_path.display()))?
    } else {
        serde_json::json!({})
    };

    // Check current state
    let current_cmd = settings
        .get("mcpServers")
        .and_then(|s| s.get("duduclaw"))
        .and_then(|d| d.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    if current_cmd == abs_str {
        return Ok(false); // Already correct
    }

    // Upsert mcpServers.duduclaw
    let mcp_servers = settings
        .as_object_mut()
        .ok_or("settings.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert(serde_json::json!({}));

    mcp_servers
        .as_object_mut()
        .ok_or("mcpServers is not a JSON object")?
        .insert("duduclaw".to_string(), serde_json::json!({
            "command": abs_str,
            "args": ["mcp-server"]
        }));

    // Write back atomically
    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    let tmp = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &settings_path)
        .map_err(|e| format!("Failed to rename {}: {e}", tmp.display()))?;

    info!(
        path = %settings_path.display(),
        command = %abs_str,
        "Registered duduclaw MCP server in global Claude settings"
    );
    Ok(true)
}

/// Remove the `duduclaw` entry from a per-agent `.mcp.json` (migrated to global).
///
/// Preserves other server entries (playwright, browserbase, etc.).
/// Deletes the file entirely if no servers remain.
///
/// Returns `Ok(true)` if changed, `Ok(false)` if nothing to do.
pub fn remove_duduclaw_from_agent_mcp(agent_dir: &Path) -> Result<bool, String> {
    let path = agent_dir.join(".mcp.json");
    if !path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let mut config: McpConfig = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;

    if config.mcp_servers.remove("duduclaw").is_none() {
        return Ok(false); // No duduclaw entry
    }

    if config.mcp_servers.is_empty() {
        // No servers left — remove the file
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
        info!(path = %path.display(), "Removed empty .mcp.json (duduclaw migrated to global)");
    } else {
        // Write back without duduclaw entry
        let json = serde_json::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize: {e}"))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        info!(path = %path.display(), "Removed duduclaw from per-agent .mcp.json (migrated to global)");
    }
    Ok(true)
}

/// Legacy per-agent `.mcp.json` fixup — kept for backwards compatibility.
///
/// Prefer `ensure_global_mcp_server()` for new installations.
/// This function is called after global migration to clean up stale entries.
pub fn ensure_duduclaw_absolute_path(agent_dir: &Path) -> Result<bool, String> {
    let path = agent_dir.join(".mcp.json");

    let abs_bin = duduclaw_core::resolve_duduclaw_bin();
    let abs_str = abs_bin.to_string_lossy().into_owned();

    // Still relative after resolution (fallback "duduclaw") — skip.
    if !std::path::Path::new(&abs_str).is_absolute() {
        return Ok(false);
    }

    // Case 1: No .mcp.json exists → create with duduclaw server entry
    if !path.exists() {
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), McpServerDef {
            command: abs_str.clone(),
            args: vec!["mcp-server".to_string()],
            env: std::collections::HashMap::new(),
        });
        let config = McpConfig { mcp_servers: servers };
        let json = serde_json::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
        std::fs::write(&path, &json)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        duduclaw_core::platform::set_owner_only(&path).ok();
        info!(path = %path.display(), command = %abs_str, "Created .mcp.json with duduclaw server");
        return Ok(true);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let mut config: McpConfig = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;

    // Case 2 & 3: Check if duduclaw server needs updating.
    let needs_update = match config.mcp_servers.get("duduclaw") {
        None => true, // No duduclaw entry at all
        Some(entry) => {
            let cmd_path = std::path::Path::new(&entry.command);
            !cmd_path.is_absolute()             // Case 2: relative path
                || !cmd_path.exists()            // Case 3: binary doesn't exist
                || entry.command != abs_str      // Command changed (e.g., duduclaw-pro → duduclaw)
        }
    };

    if !needs_update {
        return Ok(false);
    }

    config
        .mcp_servers
        .entry("duduclaw".to_string())
        .and_modify(|e| e.command = abs_str.clone())
        .or_insert(McpServerDef {
            command: abs_str.clone(),
            args: vec!["mcp-server".to_string()],
            env: std::collections::HashMap::new(),
        });

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

    info!(
        path = %path.display(),
        command = %abs_str,
        "Updated duduclaw MCP server to absolute path"
    );
    Ok(true)
}

/// Scan all agent directories and fix relative `duduclaw` MCP server paths.
///
/// Called on gateway startup to ensure subprocess-spawned Claude CLI can
/// discover the MCP server without PATH inheritance.
pub fn ensure_mcp_absolute_paths_all(agents_dir: &Path) -> usize {
    let mut fixed = 0usize;
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %agents_dir.display(),
                error = %e,
                "Cannot read agents directory for MCP path fixup"
            );
            return 0;
        }
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Skip trash / defaults directories
        if let Some(name) = dir.file_name().and_then(|n| n.to_str())
            && (name.starts_with('_') || name.starts_with('.'))
        {
            continue;
        }
        match ensure_duduclaw_absolute_path(&dir) {
            Ok(true) => fixed += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    agent_dir = %dir.display(),
                    error = %e,
                    "Failed to fix MCP path"
                );
            }
        }
    }

    if fixed > 0 {
        info!(count = fixed, "Fixed relative MCP paths on startup");
    }
    fixed
}

/// An entry in the MCP marketplace catalog.
#[derive(Debug, Clone, Serialize)]
pub struct McpCatalogItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub requires_oauth: bool,
    pub default_def: McpServerDef,
    pub required_env: Vec<String>,
}

/// Return the built-in MCP marketplace catalog.
pub fn marketplace_catalog() -> Vec<McpCatalogItem> {
    vec![
        McpCatalogItem {
            id: "playwright".into(),
            name: "Playwright".into(),
            description: "Browser automation".into(),
            category: "browser".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-playwright".into(), "--headless".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "browserbase".into(),
            name: "Browserbase".into(),
            description: "Cloud browser".into(),
            category: "browser".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-browserbase".into()],
                env: [
                    ("BROWSERBASE_API_KEY".into(), "${BROWSERBASE_API_KEY}".into()),
                    ("BROWSERBASE_PROJECT_ID".into(), "${BROWSERBASE_PROJECT_ID}".into()),
                ].into_iter().collect(),
            },
            required_env: vec!["BROWSERBASE_API_KEY".into(), "BROWSERBASE_PROJECT_ID".into()],
        },
        McpCatalogItem {
            id: "filesystem".into(),
            name: "Filesystem".into(),
            description: "File access".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-filesystem".into(), ".".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "github".into(),
            name: "GitHub".into(),
            description: "GitHub API".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-github".into()],
                env: [("GITHUB_TOKEN".into(), "${GITHUB_TOKEN}".into())].into_iter().collect(),
            },
            required_env: vec!["GITHUB_TOKEN".into()],
        },
        McpCatalogItem {
            id: "slack".into(),
            name: "Slack".into(),
            description: "Slack".into(),
            category: "communication".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-slack".into()],
                env: [("SLACK_BOT_TOKEN".into(), "${SLACK_BOT_TOKEN}".into())].into_iter().collect(),
            },
            required_env: vec!["SLACK_BOT_TOKEN".into()],
        },
        McpCatalogItem {
            id: "postgres".into(),
            name: "PostgreSQL".into(),
            description: "PostgreSQL".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-postgres".into()],
                env: [("DATABASE_URL".into(), "${DATABASE_URL}".into())].into_iter().collect(),
            },
            required_env: vec!["DATABASE_URL".into()],
        },
        McpCatalogItem {
            id: "sqlite".into(),
            name: "SQLite".into(),
            description: "SQLite".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-sqlite".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "memory".into(),
            name: "Memory".into(),
            description: "Persistent memory".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-memory".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "fetch".into(),
            name: "Fetch".into(),
            description: "HTTP fetch".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-fetch".into()],
                env: Default::default(),
            },
            required_env: vec![],
        },
        McpCatalogItem {
            id: "brave-search".into(),
            name: "Brave Search".into(),
            description: "Brave Search".into(),
            category: "data".into(),
            requires_oauth: false,
            default_def: McpServerDef {
                command: "npx".into(),
                args: vec!["@anthropic-ai/mcp-server-brave-search".into()],
                env: [("BRAVE_API_KEY".into(), "${BRAVE_API_KEY}".into())].into_iter().collect(),
            },
            required_env: vec!["BRAVE_API_KEY".into()],
        },
    ]
}

/// Read and parse `.mcp.json` from an agent directory.
/// Returns an empty config if the file does not exist.
pub fn read_mcp_config(agent_dir: &Path) -> Result<McpConfig, String> {
    let path = agent_dir.join(".mcp.json");
    if !path.exists() {
        return Ok(McpConfig { mcp_servers: std::collections::HashMap::new() });
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read MCP config: {e}"))?;
    serde_json::from_str::<McpConfig>(&content)
        .map_err(|e| format!("Failed to parse MCP config: {e}"))
}

/// Add a server entry to an agent's `.mcp.json`, creating the file if needed.
/// Writes atomically via temp file + rename.
pub fn add_server_to_config(agent_dir: &Path, name: &str, def: &McpServerDef) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");
    let mut config = read_mcp_config(agent_dir)?;
    config.mcp_servers.insert(name.to_string(), def.clone());

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;

    let tmp_path = agent_dir.join(".mcp.json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Failed to write temp MCP config: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename temp MCP config: {e}"))?;
    duduclaw_core::platform::set_owner_only(&path).ok();

    info!(path = %path.display(), server = name, "MCP server added to config");
    Ok(())
}

/// Remove a server entry from an agent's `.mcp.json`.
/// Returns an error if the server does not exist.
pub fn remove_server_from_config(agent_dir: &Path, server_name: &str) -> Result<(), String> {
    let path = agent_dir.join(".mcp.json");
    let mut config = read_mcp_config(agent_dir)?;

    if config.mcp_servers.remove(server_name).is_none() {
        return Err(format!("MCP server '{server_name}' not found in config"));
    }

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;

    let tmp_path = agent_dir.join(".mcp.json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Failed to write temp MCP config: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename temp MCP config: {e}"))?;
    duduclaw_core::platform::set_owner_only(&path).ok();

    info!(path = %path.display(), server = server_name, "MCP server removed from config");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn playwright_config_headless() {
        let config = playwright_mcp_config(true);
        assert!(config.mcp_servers.contains_key("playwright"));
        let server = &config.mcp_servers["playwright"];
        assert_eq!(server.command, "npx");
        assert!(server.args.contains(&"--headless".to_string()));
    }

    #[test]
    fn write_and_read_config() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let config = playwright_mcp_config(true);
        assert!(write_mcp_config(dir.path(), &config).expect("first write should succeed"));
        // Second write should return false (already exists)
        assert!(!write_mcp_config(dir.path(), &config).expect("second write should return false"));
    }

    #[test]
    fn browserbase_config_has_env() {
        let config = browserbase_mcp_config("key123", "proj456");
        let server = &config.mcp_servers["browserbase"];
        // Values must be env var references, never the literal secret.
        assert_eq!(server.env["BROWSERBASE_API_KEY"], "${BROWSERBASE_API_KEY}");
        assert_eq!(server.env["BROWSERBASE_PROJECT_ID"], "${BROWSERBASE_PROJECT_ID}");
        assert!(server.args.contains(&"@browserbasehq/mcp-server-browserbase".to_string()));
    }

    #[test]
    fn ensure_playwright_merges() {
        let dir = TempDir::new().unwrap();
        // Write initial config with another server
        let mut initial = McpConfig { mcp_servers: std::collections::HashMap::new() };
        initial.mcp_servers.insert("memory".to_string(), McpServerDef {
            command: "npx".to_string(),
            args: vec!["@anthropic-ai/mcp-server-memory".to_string()],
            env: std::collections::HashMap::new(),
        });
        write_mcp_config(dir.path(), &initial).expect("initial write should succeed");
        // Need to remove the file first since write_mcp_config skips existing
        std::fs::remove_file(dir.path().join(".mcp.json")).expect("remove should succeed");
        write_mcp_config(dir.path(), &initial).expect("second write should succeed");

        ensure_playwright_in_config(dir.path(), true).expect("ensure playwright should succeed");

        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).expect("read config should succeed");
        let config: McpConfig = serde_json::from_str(&content).expect("config should be valid JSON");
        assert!(config.mcp_servers.contains_key("playwright"));
        assert!(config.mcp_servers.contains_key("memory"));
    }
}
