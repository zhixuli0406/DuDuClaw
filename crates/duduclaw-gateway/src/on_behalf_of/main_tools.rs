//! Isolation gate for the explicit main-agent person list. Tool settings
//! never establish this permission outside protected execution.
use std::path::Path;

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_core::types::{AgentConfig, AgentRole, AgentStatus, RuntimeType};

use super::MAIN_TURN_REFUSAL;

/// The person list takes effect only when employee execution is isolated.
pub async fn listed_access_effective(home: &Path, agent: &str) -> Result<bool, String> {
    listed_access_effective_with_isolation(
        home, agent, duduclaw_core::execution_gate::isolation_required(),
    ).await
}

// Host-only injection boundary: tests never latch process-global isolation,
// and request parameters can never supply this fact.
pub(super) async fn listed_access_effective_with_isolation(
    _home: &Path, _target: &str, isolated: bool,
) -> Result<bool, String> {
    Ok(isolated)
}

/// Recheck isolation against the executing registry snapshot.
pub(crate) fn loaded_access_effective(_agent: &LoadedAgent) -> Result<bool, String> {
    Ok(duduclaw_core::execution_gate::isolation_required())
}

/// Enumerate the same main/default identities as main_target's legacy gate.
/// Fail closed on a missing default, an empty roster or an unreadable config.
pub(crate) async fn main_default_targets(home: &Path) -> Result<Vec<String>, String> {
    let default = crate::webchat::access::gateway_default_id(home).await
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    let agents = load_agents(home).await?;
    let mut found_default = default.is_empty();
    let mut targets = Vec::new();
    for agent in agents {
        let id = agent.dir.file_name().and_then(|name| name.to_str())
            .ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
        let is_default = agent.config.agent.name == default || id == default;
        found_default |= is_default;
        if agent.config.agent.role == AgentRole::Main || is_default {
            targets.push(id.to_owned());
        }
    }
    if !found_default || targets.is_empty() { return Err(MAIN_TURN_REFUSAL.into()); }
    targets.sort();
    Ok(targets)
}

/// Visibility follows the same isolation requirement as admission.
pub(crate) async fn snapshot_access_effective(
    home: &Path, agents: &[LoadedAgent],
) -> Result<bool, String> {
    snapshot_access_effective_with_isolation(home, agents,
        duduclaw_core::execution_gate::isolation_required()).await
}

pub(super) async fn snapshot_access_effective_with_isolation(
    _home: &Path, _agents: &[LoadedAgent], isolated: bool,
) -> Result<bool, String> {
    Ok(isolated)
}

struct ReadAgent {
    dir: std::path::PathBuf,
    config: AgentConfig,
}

/// Pure reads only: display/admission must never materialize or delete a
/// resolved artifact, reconcile incarnations, fence, or take an authority lock.
async fn load_agents(home: &Path) -> Result<Vec<ReadAgent>, String> {
    let mut entries = tokio::fs::read_dir(home.join("agents")).await
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    let mut agents = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|_| MAIN_TURN_REFUSAL.to_owned())? {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
        if duduclaw_core::is_internal_agent_dir(name) { continue; }
        let dir = entry.path();
        if !entry.metadata().await.map_err(|_| MAIN_TURN_REFUSAL.to_owned())?.is_dir() { continue; }
        if !tokio::fs::try_exists(dir.join("agent.toml")).await
            .map_err(|_| MAIN_TURN_REFUSAL.to_owned())? { continue; }
        let config = read_config(&dir)?;
        if config.agent.status != AgentStatus::Deleted { agents.push(ReadAgent { dir, config }); }
    }
    Ok(agents)
}

pub(super) fn read_config(dir: &Path) -> Result<AgentConfig, String> {
    use duduclaw_core::preset;
    let deny = |_| MAIN_TURN_REFUSAL.to_owned();
    let source = dir.join("agent.toml");
    validate_tool_config(&source, true)?;
    let raw = crate::runtime_config::read_config_for_gate(&source).map_err(deny)?
        .ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
    let home = preset::agent_home_dir(dir).ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
    let id = dir.file_name().and_then(|v| v.to_str()).ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
    let bindings = preset::load_bindings_checked(&home).map_err(deny)?;
    let (effective, resolution) = preset::resolve_for_agent_with_bindings(
        &home, id, raw.as_table().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?, &bindings);
    if matches!(resolution, preset::PresetResolution::Unresolved { .. }) { return Err(MAIN_TURN_REFUSAL.into()); }
    let resolved = preset::agent_resolved_path(&home, id);
    validate_tool_config(&resolved, resolution.is_applied())?;
    let written = crate::runtime_config::read_config_for_gate(&resolved).map_err(deny)?;
    // Shadow readers must see exactly the policy the registry would load.
    // A stale artifact is refused, never repaired as a side effect of a read.
    if resolution.is_applied() {
        if written.as_ref().and_then(toml::Value::as_table) != Some(&effective) { return Err(MAIN_TURN_REFUSAL.into()); }
    } else if written.is_some() { return Err(MAIN_TURN_REFUSAL.into()); }
    crate::runtime_config::read_agent_sections_for_gate(dir).map_err(deny)?;
    toml::Value::Table(effective).try_into().map_err(|_| MAIN_TURN_REFUSAL.to_owned())
}

fn validate_tool_config(path: &Path, required: bool) -> Result<(), String> {
    let Some(doc) = crate::runtime_config::read_config_for_gate(path)
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())? else {
        return if required { Err(MAIN_TURN_REFUSAL.into()) } else { Ok(()) };
    };
    // The general-purpose readers intentionally drop wrong-typed elements.
    // Target resolution must not silently recover malformed configuration.
    if let Some(caps) = doc.get("capabilities") {
        let caps = caps.as_table().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
        for key in ["allowed_tools", "denied_tools"] {
            if let Some(value) = caps.get(key) {
                let list = value.as_array().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
                if list.iter().any(|item| item.as_str().is_none_or(|name| name.trim().is_empty())) {
                    return Err(MAIN_TURN_REFUSAL.into());
                }
            }
        }
    }
    if let Some(runtime) = doc.get("runtime") {
        if runtime.get("minimal_context").is_some_and(|value| !value.is_bool()) {
            return Err(MAIN_TURN_REFUSAL.into());
        }
        for field in ["provider", "fallback"] {
            if let Some(value) = runtime.get(field).and_then(toml::Value::as_str) {
                if RuntimeType::parse(value).is_none() { return Err(MAIN_TURN_REFUSAL.into()); }
            }
        }
    }
    Ok(())
}
