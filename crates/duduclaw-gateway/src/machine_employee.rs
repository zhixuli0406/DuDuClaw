//! Human access to employees bound to the machine-operation preset.
use std::path::Path;

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_core::preset::{self, PresetResolution};

pub(crate) const REFUSAL: &str = "這位 AI 員工可操作電腦，僅限管理員使用或綁定。";

/// Use the server's resolved binding, including a binding that failed to
/// resolve. Names, capabilities and client-supplied preset mirrors are not authority.
fn restricted(resolution: &PresetResolution) -> bool {
    match resolution {
        PresetResolution::Applied { preset_id, .. }
        | PresetResolution::Unresolved { preset_id, .. } => preset_id == "system-operator",
        PresetResolution::Unbound => false,
    }
}

pub(crate) fn requires_admin(agent: &LoadedAgent) -> bool {
    restricted(&agent.preset_resolution)
        && preset::agent_home_dir(&agent.dir)
            .is_none_or(|home| crate::on_behalf_of::access::system_operation_requires_admin(&home))
}

pub(crate) fn check_loaded(home: &Path, agent: &LoadedAgent, principal: Option<&str>) -> Result<(), String> {
    if !requires_admin(agent)
        || crate::on_behalf_of::access::system_operation_permitted(home, principal)
    {
        Ok(())
    } else {
        Err(REFUSAL.into())
    }
}

/// Shared creation-time check for paths without an in-memory registry.
/// Resolve exactly as AgentRegistry does, without trusting `[preset]` in agent.toml.
pub(crate) fn check_target(home: &Path, target: &str, principal: Option<&str>) -> Result<(), String> {
    if crate::on_behalf_of::access::system_operation_permitted(home, principal) {
        return Ok(());
    }
    if !duduclaw_core::is_valid_agent_id(target) {
        return Err(REFUSAL.into());
    }
    // The resolved preset id comes exclusively from the server's binding
    // store; per-agent overrides cannot change it. An empty config is enough
    // for this identity-only resolution and preserves normal ephemeral work.
    let (_, resolution) = preset::resolve_for_agent(home, target, &toml::Table::new());
    if restricted(&resolution) {
        return Err(REFUSAL.into());
    }
    Ok(())
}

/// Chat-created goals cannot infer background authority from a missing
/// account mapping. Preserve only genuine internal sessions and token auth.
pub(crate) async fn check_chat_goal(ctx: &crate::channel_reply::ReplyContext, target: &str, session: &str) -> Result<(), String> {
    let principal = crate::chat_commands::goal_principal_for_session(session).map(|(who, _)| who);
    let internal = session.split_once(':').is_some_and(|(channel, _)| {
        crate::knowledge::requester_map::INTERNAL_SESSION_PREFIXES.contains(&channel)
    });
    let token_admin = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
        .try_with(|id| id == "system").unwrap_or(false);
    let wire = principal.as_deref().or(if internal || token_admin { None } else { Some("unknown:chat-goal") });
    let registry = ctx.registry.read().await;
    let agent = if target == "default" { registry.main_agent() } else { registry.get(target) };
    match agent {
        Some(agent) => check_loaded(&ctx.home_dir, agent, wire),
        None => check_target(&ctx.home_dir, target, wire),
    }
}

#[cfg(test)]
mod tests;
