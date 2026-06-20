//! Per-agent API key isolation.
//!
//! [C-3a] Each agent can only access API keys for its `allowed_channels`.
//! Keys are resolved at runtime via `ReplyContext`, never written to env vars
//! or agent directories.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

/// Resolved key set for an agent, filtered by its permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKeySet {
    pub agent_id: String,
    /// Channel name -> API key (only channels the agent is allowed to access).
    pub keys: HashMap<String, String>,
}

/// Resolve the API keys an agent is allowed to use, based on its
/// `allowed_channels` permission list and the global config.
pub fn resolve_agent_keys(
    agent_id: &str,
    allowed_channels: &[String],
    config: &toml::Table,
) -> AgentKeySet {
    let channels = config.get("channels").and_then(|c| c.as_table());
    let mut keys = HashMap::new();

    let allow_all = allowed_channels.iter().any(|c| c == "*");

    // Encrypted field mappings: (channel, enc_field, plaintext_fallback_field)
    let channel_key_mapping_enc = [
        ("telegram", "telegram_bot_token_enc", "telegram_bot_token"),
        ("line", "line_channel_token_enc", "line_channel_token"),
        ("discord", "discord_bot_token_enc", "discord_bot_token"),
    ];

    for (channel, enc_key, plain_key) in &channel_key_mapping_enc {
        if !allow_all && !allowed_channels.iter().any(|c| c == *channel) {
            continue;
        }

        // Try encrypted field first, fallback to plaintext for backwards compat
        let value = channels
            .and_then(|c| c.get(*enc_key))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                channels
                    .and_then(|c| c.get(*plain_key))
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            });

        if let Some(value) = value {
            keys.insert(channel.to_string(), value.to_string());
        }
    }

    // API key (Anthropic) — gated by `allowed_channels` just like channel keys.
    //
    // D9/D10: previously the global Anthropic key was returned unconditionally,
    // ignoring `allowed_channels`. We now require the agent to be allowed `"*"`
    // or the `"anthropic"` capability before exposing the LLM key. This is a
    // guarded check that is correct once a caller actually passes the agent's
    // channel/capability list — today there is NO production caller of
    // `resolve_agent_keys` (latent), so this changes no live behaviour, but it
    // closes the gap by construction the moment one is added.
    let anthropic_allowed =
        allow_all || allowed_channels.iter().any(|c| c == "anthropic");
    if anthropic_allowed {
        let api_key = config
            .get("api")
            .and_then(|a| a.get("anthropic_api_key_enc"))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                config
                    .get("api")
                    .and_then(|a| a.get("anthropic_api_key"))
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            });

        if let Some(api_key) = api_key {
            keys.insert("anthropic".to_string(), api_key.to_string());
        }
    }

    info!(
        agent = agent_id,
        channels = keys.len(),
        "Agent key set resolved"
    );

    AgentKeySet {
        agent_id: agent_id.to_string(),
        keys,
    }
}

/// Verify that an agent is allowed to access a specific channel.
pub fn check_channel_access(
    allowed_channels: &[String],
    requested_channel: &str,
) -> bool {
    allowed_channels.iter().any(|c| c == "*" || c == requested_channel)
}

/// Load config.toml and resolve keys for an agent.
pub async fn resolve_keys_from_config(
    home_dir: &Path,
    agent_id: &str,
    allowed_channels: &[String],
) -> AgentKeySet {
    let config_path = home_dir.join("config.toml");
    let config: toml::Table = match tokio::fs::read_to_string(&config_path).await {
        Ok(content) => content.parse().unwrap_or_default(),
        Err(_) => toml::Table::new(),
    };

    resolve_agent_keys(agent_id, allowed_channels, &config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_api() -> toml::Table {
        r#"
[api]
anthropic_api_key = "sk-test-123"

[channels]
telegram_bot_token = "tg-token"
"#
        .parse()
        .unwrap()
    }

    #[test]
    fn anthropic_key_granted_with_wildcard() {
        let cfg = config_with_api();
        let ks = resolve_agent_keys("a", &["*".to_string()], &cfg);
        assert_eq!(ks.keys.get("anthropic").map(String::as_str), Some("sk-test-123"));
    }

    #[test]
    fn anthropic_key_granted_when_explicitly_allowed() {
        let cfg = config_with_api();
        let ks = resolve_agent_keys("a", &["anthropic".to_string()], &cfg);
        assert!(ks.keys.contains_key("anthropic"));
    }

    #[test]
    fn anthropic_key_withheld_when_not_allowed() {
        // D9/D10: an agent restricted to telegram must not receive the LLM key.
        let cfg = config_with_api();
        let ks = resolve_agent_keys("a", &["telegram".to_string()], &cfg);
        assert!(!ks.keys.contains_key("anthropic"), "anthropic key must be gated");
        assert!(ks.keys.contains_key("telegram"));
    }
}
