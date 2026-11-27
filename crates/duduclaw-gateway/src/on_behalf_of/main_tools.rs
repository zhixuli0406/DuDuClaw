//! Shell-free proof for the explicit main-agent person list. No ambient
//! principal or task-local explore lane can establish this durable permission.
use std::path::Path;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_core::types::{AgentRole, AgentStatus, CapabilitiesConfig, RuntimeType, SHELL_TOOLS};

use super::MAIN_TURN_REFUSAL;

/// True only when employee work is isolated, or the actual Claude spawn tool
/// surface is shell-free. Errors are intentionally not converted into defaults.
pub async fn listed_access_effective(home: &Path, agent: &str) -> Result<bool, String> {
    listed_access_effective_with_isolation(
        home, agent, duduclaw_core::execution_gate::isolation_required(),
    ).await
}

// Injectable only inside this module's parent tests: never latch process-global
// isolation in a parallel unit test and never accept this input over the wire.
pub(super) async fn listed_access_effective_with_isolation(
    home: &Path, target: &str, isolated: bool,
) -> Result<bool, String> {
    if isolated { return Ok(true); }
    let agents = load_agents(home).await?;
    let mut matches = agents.iter().filter(|agent| {
        if target == "default" { agent.config.agent.role == AgentRole::Main } else {
            agent.config.agent.name == target
                || agent.dir.file_name().and_then(|name| name.to_str()) == Some(target)
        }
    });
    let agent = matches.next().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
    // A name/directory collision must not prove safety for a different target.
    if matches.next().is_some() { return Err(MAIN_TURN_REFUSAL.into()); }
    loaded_shell_free(agent)
}

/// Synchronous half of the proof, also used against the executing registry
/// snapshot so a stale precomputed admission never blesses loaded shell tools.
pub(crate) fn loaded_access_effective(agent: &LoadedAgent) -> Result<bool, String> {
    if duduclaw_core::execution_gate::isolation_required() { return Ok(true); }
    loaded_shell_free(agent)
}

fn loaded_shell_free(agent: &LoadedAgent) -> Result<bool, String> {
    let settings = crate::runtime_config::load_runtime_settings_strict(&agent.dir)
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    if settings.provider != RuntimeType::Claude
        || settings.fallback.is_some_and(|provider| provider != RuntimeType::Claude)
        || agent.config.model.api_mode != "cli"
    {
        // Codex/Grok coarse sandboxes retain a shell; generic/removed CLI and
        // API tool loops have no proven shell-free surface here. A Claude
        // fallback or hybrid/direct-API route must not bypass this decision.
        return Ok(false);
    }
    Ok(!claude_has_shell(
        &agent.config.capabilities,
        duduclaw_core::agent_toml::resolve_minimal_context(Some(&agent.dir)),
    ))
}

fn shell_name(entry: &str) -> bool {
    let base = entry.split('(').next().unwrap_or(entry).trim();
    SHELL_TOOLS.iter().any(|name| base.eq_ignore_ascii_case(name))
}

pub(super) fn claude_has_shell(caps: &CapabilitiesConfig, minimal: bool) -> bool {
    if minimal {
        // These are the exact helpers/defaults used for --tools by
        // claude_runner::prepare_claude_cmd and channel_reply::cli_env.
        // Check both because this permission covers Converse AND Assign.
        return [&duduclaw_core::types::DISPATCH_DEFAULT_BUILTIN_TOOLS[..],
            &duduclaw_core::types::CURATED_BUILTIN_TOOLS[..]]
            .into_iter().flat_map(|defaults| caps.minimal_builtin_tools(defaults))
            .any(|tool| shell_name(&tool) || tool.contains('*'));
    }
    // Without --tools the schema surface is unbounded. Reuse the same
    // --allowedTools/--disallowedTools helpers; only a finite explicit
    // allowlist can prove it shell-free. Qualified denies do not remove an
    // entire shell when the --tools schema filter is absent.
    let allowed = caps.allowed_tools();
    let denied = caps.disallowed_tools();
    if allowed.is_empty() { return true; }
    allowed.iter().any(|tool| {
        let base = tool.split('(').next().unwrap_or(tool).trim();
        let bare_denied = denied.iter().any(|deny| deny.trim().eq_ignore_ascii_case(base));
        !bare_denied && (shell_name(tool) || base.contains('*'))
    })
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

async fn load_agents(home: &Path) -> Result<Vec<LoadedAgent>, String> {
    let mut entries = tokio::fs::read_dir(home.join("agents")).await
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    let mut agents = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|_| MAIN_TURN_REFUSAL.to_owned())? {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| MAIN_TURN_REFUSAL.to_owned())?;
        if duduclaw_core::is_internal_agent_dir(name) { continue; }
        let dir = entry.path();
        let metadata = tokio::fs::metadata(&dir).await.map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
        if !metadata.is_dir() { continue; }
        // Like the registry, skip non-agent directories, but unlike scan(),
        // never silently drop a corrupt employee (it could be another Main).
        if !tokio::fs::try_exists(dir.join("agent.toml")).await
            .map_err(|_| MAIN_TURN_REFUSAL.to_owned())? { continue; }
        // Validate before the registry refreshes/removes a resolved artifact.
        validate_tool_config(&dir.join("agent.toml"), true)?;
        let resolved = duduclaw_core::preset::agent_resolved_path(home, name);
        validate_tool_config(&resolved, false)?;
        crate::runtime_config::read_agent_sections_for_gate(&dir)
            .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
        let agent = Box::pin(AgentRegistry::load_agent(&dir)).await
            .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
        if matches!(agent.preset_resolution, duduclaw_core::preset::PresetResolution::Unresolved { .. }) {
            return Err(MAIN_TURN_REFUSAL.into());
        }
        // A newly materialized preset must pass the same strict validation;
        // a failed write must not silently switch shadow readers to raw config.
        validate_tool_config(&resolved, agent.preset_resolution.is_applied())?;
        if agent.config.agent.status != AgentStatus::Deleted { agents.push(agent); }
    }
    Ok(agents)
}

fn validate_tool_config(path: &Path, required: bool) -> Result<(), String> {
    let Some(doc) = crate::runtime_config::read_config_for_gate(path)
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())? else {
        return if required { Err(MAIN_TURN_REFUSAL.into()) } else { Ok(()) };
    };
    // The general-purpose readers intentionally drop wrong-typed elements.
    // Admission must not mistake that recovery for a proven shell-free list.
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
