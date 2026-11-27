//! RM-142 P16: main employees cannot lend administrative powers to a person.
//! The caller supplies only the host-validated principal, never tool arguments.

use std::path::Path;

pub(crate) const REFUSAL: &str = "這個動作需要管理員本人操作。";

// `agent_create` in the design is named `create_agent` in the live registry.
// Keep the design spelling too, so an old/unknown call is refused explicitly.
const NAMED_TOOLS: &[&str] = &[
    "agent_create", "create_agent", "agent_remove", "agent_update",
    "agent_update_soul", "shared_wiki_delete", "channel_config", "capability_request",
];

/// Read once per call/list, without consulting process env or tool arguments.
/// Errors establishing either main identity or human authority only narrow.
pub(crate) fn restricted(home: &Path, agent: &str, principal: Option<&str>) -> bool {
    if duduclaw_gateway::ephemeral::is_ephemeral_id(agent) {
        // Host-created ephemeral workers are not main. Their existing scaffold,
        // capability and identity gates still apply, including on invalid IDs.
        return false;
    }
    let main = is_main(home, agent).unwrap_or(true);
    main && duduclaw_gateway::on_behalf_of::access::requires_main_restriction(home, principal)
        .unwrap_or(true)
}

fn is_main(home: &Path, agent: &str) -> Result<bool, String> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err("invalid agent id".into());
    }
    let dir = home.join("agents").join(agent);
    // Match load_agent_permissions' preset-aware checked loader. In particular,
    // a broken binding/resolved artifact must not fall back to a local role.
    duduclaw_core::agent_toml::load_permissions_checked(&dir)?;
    let raw = duduclaw_core::agent_toml::read_effective_text(&dir)
        .map_err(|e| e.to_string())?;
    let config: toml::Value = raw.parse().map_err(|e: toml::de::Error| e.to_string())?;
    // Require the loader's identity fields and use its exact role serde rules.
    #[derive(serde::Deserialize)]
    struct AgentIdentity {
        name: String,
        role: duduclaw_core::types::AgentRole,
    }
    let identity: AgentIdentity = config.get("agent")
        .ok_or_else(|| "missing agent table".to_string())?
        .clone().try_into().map_err(|e: toml::de::Error| e.to_string())?;
    let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|e| e.to_string())?;
    let config: toml::Value = raw.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let default = string_field(&config, "general", "default_agent")?;
    Ok(identity.role == duduclaw_core::types::AgentRole::Main
        || default == Some(identity.name.as_str()) || default == Some(agent))
}

fn string_field<'a>(config: &'a toml::Value, section: &str, key: &str) -> Result<Option<&'a str>, String> {
    let Some(value) = config.get(section) else { return Ok(None); };
    let table = value.as_table().ok_or_else(|| format!("invalid {section}"))?;
    table.get(key).map(|value| value.as_str().ok_or_else(|| format!("invalid {section}.{key}")))
        .transpose()
}

/// One name-only predicate for discovery and dispatch. Creation permission is
/// OFF even if explicitly enabled: reuse the permission table for all creators.
pub(crate) fn refusal(restricted: bool, tool: &str) -> Option<&'static str> {
    (restricted && (duduclaw_core::effect_of(tool) == duduclaw_core::ToolEffect::Admin
        || NAMED_TOOLS.contains(&tool)
        || crate::mcp_dispatch::PERMISSION_GATED_TOOLS.iter()
            .any(|(name, permission)| *name == tool && *permission == "can_create_agents")))
        .then_some(REFUSAL)
}
