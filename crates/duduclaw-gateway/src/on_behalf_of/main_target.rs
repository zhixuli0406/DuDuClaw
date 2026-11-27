//! Legacy main-agent admission is Admin-only in every edition. The separate
//! opt-in use-aware decision also admits Enterprise's explicit person list.
//! Neither decision is the optional delegation policy; bindings never override it.
use std::path::Path;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_auth::{models::UserStatus, UserRole};
use duduclaw_core::{types::AgentRole, HumanPrincipal};

const REFUSAL: &str = "主代理僅管理員可以使用。";

/// Declare the intended use explicitly so future policy can narrow either one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainUse {
    Converse,
    Assign,
}

/// RM-142 P13: the list is editable and exposed through users.me, but no
/// conversation or assignment admission path consults it yet. Later packages
/// switch those callers; management and scheduling retain the Admin-only gate.
/// Missing accounts deny; identity-store read failures are errors (callers deny).
pub fn main_access_allowed(home: &Path, user_id: &str, use_: MainUse) -> Result<bool, String> {
    let db = crate::decision_notify::open_user_db(home)
        .map_err(|_| REFUSAL.to_owned())?.ok_or_else(|| REFUSAL.to_owned())?;
    let Some(account) = db.get_user(user_id).map_err(|_| REFUSAL.to_owned())? else {
        return Ok(false);
    };
    if account.status != UserStatus::Active { return Ok(false); }
    if account.role == UserRole::Admin { return Ok(true); }
    // This is the gateway's home-bound, license-capped edition authority, not
    // an environment/config guess. Personal and unresolved editions ignore rows.
    if !crate::team_edition::team_grants_enabled(home) { return Ok(false); }
    match use_ {
        MainUse::Converse | MainUse::Assign =>
            db.is_main_agent_user(user_id).map_err(|_| REFUSAL.to_owned()),
    }
}

/// Opt-in list-aware admission. Unlike requires_main_restriction, this allows
/// active listed users on Enterprise for the explicitly declared use only.
pub fn requires_main_restriction_for(
    home: &Path, principal: Option<&str>, use_: MainUse,
) -> Result<bool, String> {
    match super::super::PrincipalState::from_wire(principal) {
        super::super::PrincipalState::Background
        | super::super::PrincipalState::Human(HumanPrincipal::Unmapped { .. }) => Ok(false),
        super::super::PrincipalState::Human(HumanPrincipal::User(id)) =>
            Ok(!main_access_allowed(home, &id, use_)?),
        super::super::PrincipalState::Unknown => Err(REFUSAL.into()),
    }
}

/// Only a live account can establish Admin authority. Background and unmapped
/// channel senders retain their existing policy; they are not dashboard users.
/// Deliberately Admin-only even for listed users; opt-in callers use
/// requires_main_restriction_for instead. Existing callers are unchanged in P13.
pub fn requires_main_restriction(home: &Path, principal: Option<&str>) -> Result<bool, String> {
    let user = match super::super::PrincipalState::from_wire(principal) {
        super::super::PrincipalState::Background
        | super::super::PrincipalState::Human(HumanPrincipal::Unmapped { .. }) => return Ok(false),
        super::super::PrincipalState::Human(HumanPrincipal::User(id)) => id,
        super::super::PrincipalState::Unknown => return Err(REFUSAL.into()),
    };
    let db = crate::decision_notify::open_user_db(home)
        .map_err(|_| REFUSAL.to_owned())?.ok_or_else(|| REFUSAL.to_owned())?;
    let account = db.get_user(&user).map_err(|_| REFUSAL.to_owned())?
        .ok_or_else(|| REFUSAL.to_owned())?;
    if account.status != UserStatus::Active { return Err(REFUSAL.into()); }
    Ok(account.role != UserRole::Admin)
}

/// Check the actual config about to execute, after the last registry lookup.
pub async fn check_loaded_main_target(
    home: &Path, principal: Option<&str>, agent: &LoadedAgent,
) -> Result<(), String> {
    if !requires_main_restriction(home, principal)? { return Ok(()); }
    check_loaded_restricted_target(home, agent).await
}

async fn check_loaded_restricted_target(home: &Path, agent: &LoadedAgent) -> Result<(), String> {
    let default = crate::webchat::access::gateway_default_id(home).await
        .map_err(|_| REFUSAL.to_owned())?;
    if agent.config.agent.role == AgentRole::Main || agent.config.agent.name == default
        || agent.dir.file_name().and_then(|name| name.to_str()) == Some(default.as_str())
    {
        tracing::warn!(agent = %agent.config.agent.name, "non-admin main-agent admission refused");
        return Err(REFUSAL.into());
    }
    Ok(())
}

/// Creation paths without a live registry use the same loader and canonical
/// names as execution. Dispatch must check again against its final LoadedAgent.
pub async fn check_main_target(
    home: &Path, principal: Option<&str>, target: &str,
) -> Result<(), String> {
    if !requires_main_restriction(home, principal)? { return Ok(()); }
    check_restricted_target(home, target).await
}

/// Queue Background is authorized only for established system senders.
/// Absence of a principal alone does not establish authority for Main.
pub(crate) async fn check_loaded_queue_main_target(
    home: &Path, principal: Option<&str>, agent: &LoadedAgent, background_system: bool,
) -> Result<(), String> {
    if principal.is_none() && !background_system {
        check_loaded_restricted_target(home, agent).await
    } else {
        check_loaded_main_target(home, principal, agent).await
    }
}

pub(crate) async fn check_queue_main_target(
    home: &Path, principal: Option<&str>, target: &str, background_system: bool,
) -> Result<(), String> {
    if principal.is_none() && !background_system {
        check_restricted_target(home, target).await
    } else {
        check_main_target(home, principal, target).await
    }
}

async fn check_restricted_target(home: &Path, target: &str) -> Result<(), String> {
    // Registry loading has a large future even when this path is not taken.
    // Keep it off the queue consumer's bounded poll frame.
    Box::pin(async move {
        let mut registry = AgentRegistry::new(home.join("agents"));
        registry.scan().await.map_err(|_| REFUSAL.to_owned())?;
        let agent = if target == "default" { registry.main_agent() } else {
            registry.get(target).or_else(|| registry.list().into_iter().find(|agent|
                agent.dir.file_name().and_then(|name| name.to_str()) == Some(target)))
        };
        if let Some(agent) = agent {
            return check_loaded_restricted_target(home, agent).await;
        }
        // Ephemeral children are absent from the registry, but have a host-created
        // config. Do not infer that a registry miss means an ordinary employee.
        if crate::ephemeral::is_ephemeral_id(target) {
            let agent = AgentRegistry::load_agent(&home.join("agents/.ephemeral").join(target))
                .await.map_err(|_| REFUSAL.to_owned())?;
            return check_loaded_restricted_target(home, &agent).await;
        }
        Err(REFUSAL.into())
    }).await
}

#[cfg(test)]
#[path = "main_target_tests.rs"]
mod tests;
