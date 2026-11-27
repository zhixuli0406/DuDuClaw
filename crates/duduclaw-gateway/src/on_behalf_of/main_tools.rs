//! Shell-free proof for the explicit main-agent person list. No ambient
//! principal or task-local explore lane can establish this durable permission.
use std::path::Path;

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_core::types::{AgentConfig, AgentRole, AgentStatus, CapabilitiesConfig, RuntimeType, SHELL_TOOLS, COMMAND_CAPABLE_MCP_TOOLS};

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
    config_shell_free(&agent.dir, &agent.config)
}

/// Synchronous half of the proof, also used against the executing registry
/// snapshot so a stale precomputed admission never blesses loaded shell tools.
pub(crate) fn loaded_access_effective(agent: &LoadedAgent) -> Result<bool, String> {
    if duduclaw_core::execution_gate::isolation_required() { return Ok(true); }
    loaded_shell_free(agent)
}

fn loaded_shell_free(agent: &LoadedAgent) -> Result<bool, String> {
    // Check both the cached runtime config and the current MCP policy reader.
    Ok(config_shell_free(&agent.dir, &agent.config)?
        && config_shell_free(&agent.dir, &read_config(&agent.dir)?)?)
}

fn config_shell_free(dir: &Path, config: &AgentConfig) -> Result<bool, String> {
    let settings = crate::runtime_config::load_runtime_settings_strict(dir)
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    if settings.provider != RuntimeType::Claude
        || settings.fallback.is_some_and(|provider| provider != RuntimeType::Claude)
        || config.model.api_mode != "cli"
    { return Ok(false); }
    if claude_has_shell(&config.capabilities,
        duduclaw_core::agent_toml::resolve_minimal_context(Some(dir))) { return Ok(false); }
    // This is the read-only counterpart of the spawn gate: includes servers
    // retained by repair, without rewriting .mcp.json or acquiring a write lock.
    let mcp = duduclaw_agent::mcp_template::effective_mcp_config_for_probe(dir)
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    Ok(mcp_shell_free(&mcp, &config.capabilities))
}

fn mcp_shell_free(mcp: &serde_json::Value, caps: &CapabilitiesConfig) -> bool {
    use duduclaw_core::tool_catalog::{tool_list_verdict, ToolListVerdict};
    let Some(servers) = mcp.get("mcpServers").and_then(serde_json::Value::as_object) else { return false; };
    if servers.keys().any(|name| name != "duduclaw") { return false; }
    servers.values().all(serde_json::Value::is_object)
        && (!servers.contains_key("duduclaw") || COMMAND_CAPABLE_MCP_TOOLS.iter().all(|tool|
            tool_list_verdict(tool, &caps.denied_tools, &caps.allowed_tools) != ToolListVerdict::Allowed))
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

/// Display reads the already-loaded roster. It must also prove the current
/// file policy, so a stale registry cannot advertise a newly unsafe employee.
pub(crate) async fn snapshot_access_effective(
    home: &Path, agents: &[LoadedAgent],
) -> Result<bool, String> {
    let default = crate::webchat::access::gateway_default_id(home).await
        .map_err(|_| MAIN_TURN_REFUSAL.to_owned())?;
    let mut found_default = default.is_empty();
    let mut found_main = false;
    for agent in agents {
        let is_default = agent.config.agent.name == default
            || agent.dir.file_name().and_then(|v| v.to_str()) == Some(default.as_str());
        found_default |= is_default;
        if agent.config.agent.role == AgentRole::Main || is_default {
            found_main = true;
            if !loaded_access_effective(agent)? { return Ok(false); }
        }
    }
    Ok(found_default && found_main)
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

/// Inspect the final command, after MCP freezing and all CLI argument edits.
/// Without a finite --tools schema and a strict, readable MCP file there is
/// no proof. --allowedTools alone does not remove built-in schemas on the
/// channel skip-permissions path; it also never limits foreign MCP servers.
pub(super) fn final_command_shell_free(cmd: &tokio::process::Command, config: &AgentConfig) -> bool {
    let Some(args) = cmd.as_std().get_args().map(|v| v.to_str()).collect::<Option<Vec<_>>>() else { return false; };
    // Builders emit separate flag/value pairs. Do not accept an uninspected
    // alternate spelling that could override the earlier checked arguments.
    const SURFACE_FLAGS: &[&str] = &["--tools", "--allowedTools", "--disallowedTools",
        "--mcp-config", "--strict-mcp-config"];
    if args.iter().any(|arg| arg.split_once('=').is_some_and(|(flag, _)| SURFACE_FLAGS.contains(&flag))
        || matches!(*arg, "--allowed-tools" | "--disallowed-tools")) { return false; }
    let one = |flag: &str| -> Option<&str> {
        let mut found = args.windows(2).filter(|pair| pair[0] == flag);
        let value = found.next()?.get(1).copied()?;
        if found.next().is_some() { None } else { Some(value) }
    };
    let Some(tools) = one("--tools") else { return false; };
    for flag in ["--allowedTools", "--disallowedTools"] {
        let count = args.iter().filter(|arg| **arg == flag).count();
        if count > 1 || (count == 1 && one(flag).is_none_or(|value|
            value.split(',').any(|selector| selector.trim().is_empty()))) { return false; }
    }
    let denied: Vec<&str> = one("--disallowedTools").map(|v| v.split(',').collect()).unwrap_or_default();
    // Unknown built-ins are not a proof (Task/Agent, NotebookEdit and computer
    // can execute too). Qualified denies remove only some invocations.
    const DATA_ONLY: &[&str] = &["Read", "Glob", "Grep", "Write", "Edit", "MultiEdit",
        "WebFetch", "WebSearch", "TodoRead", "TodoWrite", "AskUserQuestion"];
    if tools.split(',').filter(|v| !v.is_empty()).any(|tool|
        !denied.contains(&tool) && !DATA_ONLY.contains(&tool)) { return false; }
    // --allowedTools controls auto-approval, not the complete tool surface.
    // In particular it does not prove absence on skip-permissions spawns.
    if args.iter().filter(|arg| **arg == "--strict-mcp-config").count() != 1 { return false; }
    let Some(path) = one("--mcp-config") else { return false; };
    let Ok(bytes) = std::fs::read(path) else { return false; };
    let Ok(mcp) = serde_json::from_slice(&bytes) else { return false; };
    // The MCP server independently enforces these live capability selectors;
    // CLI allow/deny flags can only narrow this proven command-free surface.
    if !mcp_shell_free(&mcp, &config.capabilities) { return false; };

    true
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
