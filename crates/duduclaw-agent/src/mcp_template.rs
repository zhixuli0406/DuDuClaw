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
