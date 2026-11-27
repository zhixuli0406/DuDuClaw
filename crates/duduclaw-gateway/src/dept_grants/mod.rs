//! Request-local department authority. No existing binding or routing gate is widened here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use duduclaw_auth::acl::ContextOrigin;
use duduclaw_auth::{UserContext, UserRole, UserStatus};
use duduclaw_core::types::AgentRole;

use crate::handlers::MethodHandler;

pub(crate) use duduclaw_auth::dept_grants::GrantGroup;

pub(crate) mod methods;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantAction {
    Read, Create, Edit, Toggle, Remove, Restore, RunNow, RequestInstall, Decide,
}

impl GrantAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Toggle => "toggle",
            Self::Remove => "remove",
            Self::Restore => "restore",
            Self::RunNow => "run_now",
            Self::RequestInstall => "request_install",
            Self::Decide => "decide",
        }
    }
}

/// Build again for each RPC; never cache this view on a connection.
#[derive(Default)]
pub(crate) struct DeptGrantView {
    grants: Vec<(String, Vec<GrantGroup>)>,
    home: PathBuf,
    ctx: Option<UserContext>,
    default_id: String,
    main_agents: BTreeSet<String>,
    loaded_agents: BTreeMap<String, String>,
}

impl DeptGrantView {
    pub(crate) async fn load(h: &MethodHandler, ctx: &UserContext) -> Self {
        if ctx.origin != ContextOrigin::DashboardSession
            || ctx.member_key_id.is_some() || ctx.user_id == "system"
        {
            return Self::denied("ineligible authentication provenance");
        }
        if h.resolve_edition_profile().await.is_personal() {
            return Self::denied("personal edition");
        }
        let Some(db) = h.user_db().await else {
            return Self::denied("user store unavailable");
        };
        let user_id = ctx.user_id.clone();
        let epoch = ctx.credential_epoch;
        let result = tokio::task::spawn_blocking(move || {
            let user = db.get_user(&user_id).map_err(|_| "account read failed")?
                .ok_or("account missing")?;
            if user.status != UserStatus::Active || user.role != UserRole::Manager
                || user.must_change_password
                || !duduclaw_auth::mfa::credential_epoch_matches(epoch, user.credential_epoch)
            {
                return Err("account no longer eligible");
            }
            db.dept_grants_for_user(&user_id).map_err(|_| "grant read failed")
        }).await;
        let grants = match result {
            Ok(Ok(grants)) => grants,
            Ok(Err(reason)) => return Self::denied(reason),
            Err(_) => return Self::denied("account read task failed"),
        };
        if grants.is_empty() { return Self::empty(); }

        let home = h.home_dir().to_path_buf();
        let default_id = match crate::webchat::access::gateway_default_id(&home).await {
            Ok(id) => id,
            Err(_) => return Self::denied("gateway default unavailable"),
        };
        let mut main_agents = BTreeSet::new();
        let mut loaded_agents = BTreeMap::new();
        {
            let registry = h.registry().read().await;
            for agent in registry.list() {
                let dir_id = agent.dir.file_name().and_then(|name| name.to_str());
                if agent.config.agent.role == AgentRole::Main
                    || agent.config.agent.name == default_id
                    || dir_id == Some(default_id.as_str())
                {
                    let Some(dir_id) = dir_id else {
                        return Self::denied("main agent directory unavailable");
                    };
                    main_agents.insert(agent.config.agent.name.clone());
                    main_agents.insert(dir_id.to_owned());
                }
                if let Some(dir_id) = dir_id {
                    loaded_agents.insert(dir_id.to_owned(), agent.config.agent.name.clone());
                }
            }
        }
        Self { grants, home, ctx: Some(ctx.clone()), default_id, main_agents, loaded_agents }
    }

    // Only fixed, non-sensitive reasons reach logs; never include database errors.
    fn denied(reason: &'static str) -> Self {
        tracing::warn!(reason, "department grant view denied");
        Self::empty()
    }

    pub(crate) fn empty() -> Self { Self::default() }

    pub(crate) fn is_empty(&self) -> bool {
        self.ctx.is_none() || self.grants.iter().all(|(_, groups)| groups.is_empty())
    }

    pub(crate) fn departments_for(&self, g: GrantGroup) -> Vec<String> {
        self.grants.iter().filter(|(_, groups)| groups.contains(&g))
            .map(|(department, _)| department.clone()).collect()
    }
}

pub(crate) fn department_grant_allows(
    v: &DeptGrantView, agent: &str, g: GrantGroup, a: GrantAction,
) -> bool {
    if v.is_empty() || !duduclaw_core::is_valid_agent_id(agent)
        || agent == v.default_id || v.main_agents.contains(agent)
    {
        return false;
    }
    let Some(ctx) = v.ctx.as_ref() else { return false; };
    if ctx.owner_only_denied(agent) { return false; }
    // Only successfully loaded directory ids qualify; config names are not aliases.
    if !v.loaded_agents.contains_key(agent) { return false; }
    // The registry may be stale. Deserialize the fresh role with the same
    // AgentRole serde implementation used by the registry's AgentConfig.
    let path = v.home.join("agents").join(agent).join("agent.toml");
    let Ok(raw) = std::fs::read_to_string(path) else { return false; };
    let Ok(config) = toml::from_str::<toml::Value>(&raw) else { return false; };
    let role: Option<AgentRole> = config.get("agent").and_then(|info| info.get("role"))
        .and_then(|role| role.clone().try_into().ok());
    match role {
        Some(AgentRole::Main) | None => return false,
        Some(_) => {},
    }
    // A present user principal never parses as Background. load admits only
    // live Managers in non-Personal editions, not the machine Admin shortcut.
    let principal = format!("user:{}", ctx.user_id);
    if crate::machine_employee::check_target(&v.home, agent, Some(&principal)).is_err() {
        return false;
    }
    let Some(department) = duduclaw_agent::skill_loader::authoritative_department(&v.home, agent) else {
        return false;
    };
    v.grants.iter().any(|(name, groups)| name == &department && groups.contains(&g))
        && action_allowed(g, a)
}

/// Exhaustive on both axes: every new group or action requires an explicit decision.
pub(crate) fn action_allowed(g: GrantGroup, a: GrantAction) -> bool {
    use GrantAction::*;
    match g {
        GrantGroup::Schedules => match a {
            Read | Create | Edit | Toggle | Remove | RunNow => true,
            Restore | RequestInstall | Decide => false,
        },
        GrantGroup::Automation => match a {
            Read | Create | Edit | Toggle | Remove => true,
            Restore | RunNow | RequestInstall | Decide => false,
        },
        GrantGroup::Skills => match a {
            Read | Toggle | Remove | Restore | RequestInstall => true,
            Create | Edit | RunNow | Decide => false,
        },
        GrantGroup::Cost => match a {
            Read => true,
            Create | Edit | Toggle | Remove | Restore | RunNow | RequestInstall | Decide => false,
        },
        GrantGroup::EgressReview => match a {
            Read | Decide => true,
            Create | Edit | Toggle | Remove | Restore | RunNow | RequestInstall => false,
        },
    }
}

/// Call only after a successful grant-backed mutation. Reads are never audited.
pub(crate) async fn record_grant_use(
    h: &MethodHandler, ctx: &UserContext, agent: &str, g: GrantGroup, a: GrantAction, method: &str,
) {
    if a == GrantAction::Read { return; }
    let Some(db) = h.user_db().await else {
        tracing::warn!("department grant audit failed: user store unavailable");
        return;
    };
    let actor = ctx.user_id.clone();
    let agent = agent.to_owned();
    let method = method.to_owned();
    match tokio::task::spawn_blocking(move || {
        db.record_dept_grant_use(&actor, &agent, g, a.as_str(), &method)
    }).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::warn!("department grant audit write failed"),
        Err(_) => tracing::warn!("department grant audit task failed"),
    }
}

#[cfg(test)]
pub(crate) mod tests;
