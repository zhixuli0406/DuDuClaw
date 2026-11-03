//! Main-agent admission is an edition-independent authority rule, not the
//! optional human-principal delegation policy. Bindings never override it.
use std::path::Path;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_auth::{models::UserStatus, UserRole};
use duduclaw_core::{types::AgentRole, HumanPrincipal};

const REFUSAL: &str = "主代理僅管理員可以使用。";

/// Only a live account can establish Admin authority. Background and unmapped
/// channel senders retain their existing policy; they are not dashboard users.
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
    let default = crate::webchat::access::gateway_default_id(home).await
        .map_err(|_| REFUSAL.to_owned())?;
    if agent.config.agent.role == AgentRole::Main || agent.config.agent.name == default {
        tracing::warn!(agent = %agent.config.agent.name, principal, "non-admin main-agent admission refused");
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
    // Registry loading has a large future even when this path is not taken.
    // Keep it off the queue consumer's bounded poll frame.
    Box::pin(async move {
        let mut registry = AgentRegistry::new(home.join("agents"));
        registry.scan().await.map_err(|_| REFUSAL.to_owned())?;
        let agent = if target == "default" { registry.main_agent() } else { registry.get(target) };
        if let Some(agent) = agent {
            return check_loaded_main_target(home, principal, agent).await;
        }
        // Ephemeral children are absent from the registry, but have a host-created
        // config. Do not infer that a registry miss means an ordinary employee.
        if crate::ephemeral::is_ephemeral_id(target) {
            let agent = AgentRegistry::load_agent(&home.join("agents/.ephemeral").join(target))
                .await.map_err(|_| REFUSAL.to_owned())?;
            return check_loaded_main_target(home, principal, &agent).await;
        }
        Err(REFUSAL.into())
    }).await
}
