//! Human access to employees bound to the machine-operation preset.
use std::path::Path;

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_core::preset::{self, PresetResolution};

/// Remove stale direct and team authority without changing stored bindings.
/// Keep this synchronous frame out of the stack-sensitive RPC future.
#[inline(never)]
pub(crate) fn without_restricted_access(home: &Path, mut ctx: duduclaw_auth::UserContext) -> duduclaw_auth::UserContext {
    if ctx.is_admin() { return ctx; }
    let lookup = RestrictionLookup::new(home);
    ctx.agent_access.retain(|id, _| !lookup.requires_admin(id));
    ctx.team_access.retain(|id, _| !lookup.requires_admin(id));
    ctx
}

pub(crate) fn live_user_context(home: &Path, db: &duduclaw_auth::UserDb, id: &str) -> Result<Option<duduclaw_auth::UserContext>, String> {
    db.live_user_context(id).map(|ctx| ctx.map(|ctx| without_restricted_access(home, ctx)))
}

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

/// Read-only identity check for readers without a registry. Inspect only the
/// authoritative binding: an unresolved system-operator binding is restricted
/// too. Unlike load_bindings, unreadable or malformed data must not be Unbound.
pub(crate) fn requires_admin_for_id(home: &Path, agent_id: &str) -> bool {
    RestrictionLookup::new(home).requires_admin(agent_id)
}

/// Preserve unknown restriction state for callers that must refuse without mutation.
pub(crate) fn restriction_known(home: &Path, agent_id: &str) -> Option<bool> {
    RestrictionLookup::new(home).classify(agent_id)
}

/// Request-local binding snapshot, loaded lazily after the per-agent directory check.
struct RestrictionLookup<'a> {
    home: &'a Path,
    bindings: std::cell::OnceCell<Result<Option<toml::Table>, ()>>,
}

impl<'a> RestrictionLookup<'a> {
    fn new(home: &'a Path) -> Self {
        Self { home, bindings: std::cell::OnceCell::new() }
    }

    fn read_bindings(&self) -> Result<Option<toml::Table>, ()> {
        let path = preset::bindings_path(self.home);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
                    Ok(None)
                } else { Err(()) };
            }
            Err(_) => return Err(()),
        };
        let table = content.parse::<toml::Table>().map_err(|_| ())?;
        if table.get("schema").is_some_and(|v| {
            v.as_integer().is_none_or(|schema| schema > preset::PRESET_BINDINGS_SCHEMA)
        }) { return Err(()); }
        Ok(Some(table))
    }

    fn requires_admin(&self, agent_id: &str) -> bool {
        self.classify(agent_id).unwrap_or(true)
    }

    fn classify(&self, agent_id: &str) -> Option<bool> {
        if !duduclaw_core::is_valid_agent_id(agent_id) { return None; }
        let dir = self.home.join("agents").join(agent_id);
        match std::fs::read_dir(&dir) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if matches!(std::fs::symlink_metadata(&dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
                    Some(false)
                } else { None };
            }
            Err(_) => return None,
        }
        let table = match self.bindings.get_or_init(|| self.read_bindings()) {
            Ok(Some(table)) => table,
            Ok(None) => return Some(false),
            Err(()) => return None,
        };
        let agents = match table.get("agents") {
            None => return Some(false),
            Some(value) => match value.as_table() {
                Some(agents) => agents,
                None => return None,
            },
        };
        let Some(binding) = agents.get(agent_id) else { return Some(false); };
        let Some(preset_id) = binding.as_table()
            .and_then(|b| b.get("preset_id")).and_then(toml::Value::as_str)
            .filter(|id| !id.is_empty()) else { return None; };
        Some(preset_id == "system-operator"
            && crate::on_behalf_of::access::system_operation_requires_admin(self.home))
    }
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
