//! Legacy main-agent admission is Admin-only in every edition. The separate
//! opt-in use-aware decision also admits Enterprise's explicit person list.
//! Neither decision is the optional delegation policy; bindings never override it.
use std::path::Path;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_auth::{models::UserStatus, UserRole};
use duduclaw_core::{types::AgentRole, HumanPrincipal};

const REFUSAL: &str = "主代理僅管理員可以使用。";
pub const MAIN_TURN_REFUSAL: &str = "主要 AI 員工目前還不能替你處理工作，請聯絡管理員。";

#[path = "main_tools.rs"]
mod main_tools;
pub use main_tools::listed_access_effective;
pub(crate) use main_tools::main_default_targets;
pub(crate) use main_tools::loaded_access_effective;

/// Declare the intended use explicitly so future policy can narrow either one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainUse {
    Converse,
    Assign,
}

/// RM-142: opt-in admission for conversation and assignment callers;
/// management and scheduling retain the separate Admin-only gate.
/// Missing accounts deny; identity-store read failures are errors (callers deny).
pub fn main_access_allowed(home: &Path, user_id: &str, use_: MainUse) -> Result<bool, String> {
    admitted_main_role(home, user_id, use_).map(|role| role.is_some())
}

fn admitted_main_role(home: &Path, user_id: &str, use_: MainUse) -> Result<Option<UserRole>, String> {
    let db = crate::decision_notify::open_user_db(home)
        .map_err(|_| REFUSAL.to_owned())?.ok_or_else(|| REFUSAL.to_owned())?;
    let Some(account) = db.get_user(user_id).map_err(|_| REFUSAL.to_owned())? else {
        return Ok(None);
    };
    if account.status != UserStatus::Active { return Ok(None); }
    if account.role == UserRole::Admin { return Ok(Some(account.role)); }
    // This is the gateway's home-bound, license-capped edition authority, not
    // an environment/config guess. Personal and unresolved editions ignore rows.
    if !crate::team_edition::team_grants_enabled(home) { return Ok(None); }
    match use_ {
        MainUse::Converse | MainUse::Assign =>
            db.is_main_agent_user(user_id).map(|listed| listed.then_some(account.role))
                .map_err(|_| REFUSAL.to_owned()),
    }
}

/// Opt-in admission for a concrete target. Only a live Admin bypasses the
/// shell safeguard; listing a person never bypasses edition/account checks.
pub async fn main_turn_allowed(
    home: &Path, user_id: &str, agent: &str, use_: MainUse,
) -> Result<bool, String> {
    match admitted_main_role(home, user_id, use_)? {
        Some(UserRole::Admin) => Ok(true),
        Some(_) => listed_access_effective(home, agent).await,
        None => Ok(false),
    }
}

/// `principal` must be server-stamped, never taken from request tool arguments.
pub async fn requires_main_restriction_for_target(
    home: &Path, principal: Option<&str>, agent: &str, use_: MainUse,
) -> Result<bool, String> {
    match super::super::PrincipalState::from_wire(principal) {
        super::super::PrincipalState::Background
        | super::super::PrincipalState::Human(HumanPrincipal::Unmapped { .. }) => Ok(false),
        super::super::PrincipalState::Human(HumanPrincipal::User(id)) =>
            Ok(!main_turn_allowed(home, &id, agent, use_).await?),
        super::super::PrincipalState::Unknown => Err(MAIN_TURN_REFUSAL.into()),
    }
}

/// Visibility covers every main/default target: showing a single entry must
/// not promise access to a main employee whose turns would be refused.
pub(crate) async fn main_entry_available(home: &Path, user_id: &str) -> Result<bool, String> {
    match admitted_main_role(home, user_id, MainUse::Converse)? {
        Some(UserRole::Admin) => return Ok(true),
        None => return Ok(false),
        Some(_) => {},
    }
    for agent in main_default_targets(home).await? {
        if !main_turn_allowed(home, user_id, &agent, MainUse::Converse).await? {
            return Ok(false);
        }
    }
    Ok(true)
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

/// Conversation/assignment sibling; legacy management callers stay unchanged.
/// Validate the actual loaded role, name and directory, even after routing.
/// Call without holding the live registry lock: the tool proof loads agents.
pub async fn check_loaded_main_target_for(
    home: &Path, principal: Option<&str>, agent: &LoadedAgent, use_: MainUse,
) -> Result<(), String> {
    // Preserve Admin/background/unmapped behavior and reject unknown identities.
    if !requires_main_restriction(home, principal)? { return Ok(()); }
    let default = crate::webchat::access::gateway_default_id(home).await
        .map_err(|_| REFUSAL.to_owned())?;
    if agent.config.agent.role != AgentRole::Main && agent.config.agent.name != default
        && agent.dir.file_name().and_then(|name| name.to_str()) != Some(default.as_str())
    {
        return Ok(());
    }
    let target = agent.dir.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| REFUSAL.to_owned())?;
    let restricted = Box::pin(requires_main_restriction_for_target(home, principal, target, use_)).await;
    if matches!(restricted, Ok(false)) {
        // A shell-free file must not bless a stale loaded config with a shell.
        if loaded_access_effective(agent).unwrap_or(false) {
            return Ok(());
        }
    }
    let listed = match super::super::PrincipalState::from_wire(principal) {
        super::super::PrincipalState::Human(HumanPrincipal::User(id)) =>
            main_access_allowed(home, &id, use_).unwrap_or(false),
        _ => false,
    };
    Err(if listed { MAIN_TURN_REFUSAL } else { REFUSAL }.into())
}

/// Execution accepts either explicitly admitted use, and validates the loaded artifact.
pub async fn check_loaded_main_target_for_execution(
    home: &Path, principal: Option<&str>, agent: &LoadedAgent,
) -> Result<(), String> {
    let assign = Box::pin(check_loaded_main_target_for(home, principal, agent, MainUse::Assign)).await;
    if assign.is_ok() { return Ok(()); }
    let converse = Box::pin(check_loaded_main_target_for(home, principal, agent, MainUse::Converse)).await;
    if converse.is_ok() { return Ok(()); }
    if assign.as_ref().err().is_some_and(|reason| reason == MAIN_TURN_REFUSAL) {
        assign
    } else {
        converse
    }
}

/// Load the concrete target for an opt-in assignment or execution check.
/// Legacy management and scheduling callers keep `check_main_target`.
pub async fn check_main_target_for(
    home: &Path, principal: Option<&str>, target: &str, use_: MainUse,
) -> Result<(), String> {
    if !requires_main_restriction(home, principal)? { return Ok(()); }
    let agent = Box::pin(load_main_target(home, target)).await?;
    Box::pin(check_loaded_main_target_for(home, principal, &agent, use_)).await
}

pub async fn check_main_target_for_execution(
    home: &Path, principal: Option<&str>, target: &str,
) -> Result<(), String> {
    if !requires_main_restriction(home, principal)? { return Ok(()); }
    let agent = Box::pin(load_main_target(home, target)).await?;
    Box::pin(check_loaded_main_target_for_execution(home, principal, &agent)).await
}

async fn load_main_target(home: &Path, target: &str) -> Result<LoadedAgent, String> {
    let mut registry = AgentRegistry::new(home.join("agents"));
    registry.scan().await.map_err(|_| REFUSAL.to_owned())?;
    let agent = if target == "default" { registry.main_agent() } else {
        registry.get(target).or_else(|| registry.list().into_iter().find(|agent|
            agent.dir.file_name().and_then(|name| name.to_str()) == Some(target)))
    };
    if let Some(agent) = agent { return Ok(agent.clone()); }
    if crate::ephemeral::is_ephemeral_id(target) {
        return AgentRegistry::load_agent(&home.join("agents/.ephemeral").join(target))
            .await.map_err(|_| REFUSAL.to_owned());
    }
    Err(REFUSAL.into())
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

/// Execution-only sibling: delegation_gate keeps the legacy queue boundary.
pub(crate) async fn check_loaded_queue_main_target_for_execution(
    home: &Path, principal: Option<&str>, agent: &LoadedAgent, background_system: bool,
) -> Result<(), String> {
    if principal.is_none() && !background_system {
        check_loaded_restricted_target(home, agent).await
    } else {
        Box::pin(check_loaded_main_target_for_execution(home, principal, agent)).await
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
