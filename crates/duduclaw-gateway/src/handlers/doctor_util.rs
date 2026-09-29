//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Check if an API key is available (env var, `config.toml [api]`, or a
/// non-empty `accounts` array). Pure function of `home_dir` — extracted from
/// `MethodHandler::has_api_key` (now a thin delegate to this) so the O-0
/// agent-facing `os_doctor_repair` MCP tool (`duduclaw-cli::mcp`) can reuse
/// the EXACT same three-source check instead of re-deriving it, avoiding the
/// two-implementation drift the O-0 design explicitly calls out.
pub async fn has_api_key_configured(home_dir: &Path) -> bool {
    // 1. Check environment variable
    if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()) {
        return true;
    }
    // 2. Check config.toml [api] section
    let table = match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
        Err(_) => toml::Table::new(),
    };
    if let Some(api) = table.get("api").and_then(|v| v.as_table())
        && api
            .get("anthropic_api_key")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    // 3. Check accounts in config.toml
    if let Some(accounts) = table.get("accounts")
        && let Some(arr) = accounts.as_array()
    {
        return !arr.is_empty();
    }
    false
}

/// Repair-hint text for one failing/warning `system.doctor` check name.
/// Extracted from `MethodHandler::handle_system_doctor_repair` (now a thin
/// delegate) so the O-0 `os_doctor_repair` MCP tool (`duduclaw-cli::mcp`)
/// shares the exact same hint copy instead of re-deriving it.
pub fn doctor_repair_hint(check_name: &str) -> &'static str {
    match check_name {
        "agents" => "Run 'duduclaw agent create <name>' to create your first agent.",
        "api_key" => "Set ANTHROPIC_API_KEY environment variable with a valid key.",
        "config_file" => "Run 'duduclaw init' to create a default config.toml.",
        _ => "Check the documentation for repair instructions.",
    }
}
