//! Request-local department authority. No existing binding or routing gate is widened here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use duduclaw_auth::acl::ContextOrigin;
use duduclaw_auth::{UserContext, UserRole, UserStatus};
use duduclaw_core::types::AgentRole;

use crate::handlers::MethodHandler;

pub(crate) use duduclaw_auth::dept_grants::GrantGroup;

pub(crate) mod methods;
pub(crate) mod reach;
pub(crate) mod rules;

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

        let Some(mut view) = Self::load_targets(h).await else {
            return Self::denied("agent targets unavailable");
        };
        view.grants = grants;
        view.ctx = Some(ctx.clone());
        view
    }

    /// Broker/push readers have no MethodHandler. Require the actual session
    /// provenance and re-read identity, epoch, grants and disk targets each time.
    pub(crate) fn load_session_home(home: &std::path::Path, ctx: &UserContext) -> Self {
        if ctx.origin != ContextOrigin::DashboardSession || ctx.member_key_id.is_some()
            || ctx.user_id == "system" {
            return Self::empty();
        }
        Self::load_home_account(home, &ctx.user_id, Some(ctx)).unwrap_or_default()
    }

    /// Notification eligibility only. This never manufactures a dashboard session.
    pub(crate) fn notification_allows(home: &std::path::Path, user: &str, agent: &str) -> bool {
        Self::load_home_account(home, user, None).is_some_and(|view|
            department_grant_allows(&view, agent, GrantGroup::EgressReview, GrantAction::Read))
    }

    fn load_home_account(home: &std::path::Path, user: &str, session: Option<&UserContext>) -> Option<Self> {
        if !crate::team_edition::team_grants_enabled(home) { return None; }
        let db = crate::decision_notify::open_user_db_read_only(home).ok()??;
        let account = db.get_user(user).ok()??;
        if account.status != UserStatus::Active || account.role != UserRole::Manager
            || account.must_change_password { return None; }
        if session.is_some_and(|ctx|
            !duduclaw_auth::mfa::credential_epoch_matches(ctx.credential_epoch, account.credential_epoch)) {
            return None;
        }
        let grants = db.dept_grants_for_user(user).ok()?;
        let mut ctx = db.live_user_context(user).ok()??;
        if let Some(session) = session { ctx.origin = session.origin; }
        let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
        let default_id = crate::webchat::access::parse_gateway_default(&raw).ok()?;
        let mut view = Self { home: home.to_path_buf(), grants, ctx: Some(ctx), default_id,
            ..Self::default() };
        for entry in std::fs::read_dir(home.join("agents")).ok()? {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() { continue; }
            let id = entry.file_name().into_string().ok()?;
            if duduclaw_core::is_internal_agent_dir(&id)
                || !duduclaw_core::is_valid_agent_id(&id) { continue; }
            let raw = match std::fs::read_to_string(entry.path().join("agent.toml")) {
                Ok(raw) => raw,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            let config: toml::Value = toml::from_str(&raw).ok()?;
            let info = config.get("agent")?;
            let name = info.get("name")?.as_str()?;
            let role: AgentRole = info.get("role")?.clone().try_into().ok()?;
            if role == AgentRole::Main || name == view.default_id || id == view.default_id {
                view.main_agents.insert(id.clone());
                view.main_agents.insert(name.to_owned());
            }
            view.loaded_agents.insert(id, name.to_owned());
        }
        Some(view)
    }

    async fn load_targets(h: &MethodHandler) -> Option<Self> {
        let home = h.home_dir().to_path_buf();
        let default_id = crate::webchat::access::gateway_default_id(&home).await.ok()?;
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
                        return None;
                    };
                    main_agents.insert(agent.config.agent.name.clone());
                    main_agents.insert(dir_id.to_owned());
                }
                if let Some(dir_id) = dir_id {
                    loaded_agents.insert(dir_id.to_owned(), agent.config.agent.name.clone());
                }
            }
        }
        Some(Self { home, default_id, main_agents, loaded_agents, ..Self::default() })
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
    if v.is_empty() { return false; }
    let Some(ctx) = v.ctx.as_ref() else { return false; };
    if ctx.owner_only_denied(agent) { return false; }
    let Ok(Some(department)) = grant_target_department(v, agent) else { return false; };
    // A present user principal never parses as Background. load admits only
    // live Managers in non-Personal editions, not the machine Admin shortcut.
    let principal = format!("user:{}", ctx.user_id);
    if crate::machine_employee::check_target(&v.home, agent, Some(&principal)).is_err() {
        return false;
    }
    v.grants.iter().any(|(name, groups)| name == &department && groups.contains(&g))
        && action_allowed(g, a)
}

/// Shared target checks for authorization and display. Errors stay unknown for
/// display callers; authorization always denies them.
fn grant_target_department(v: &DeptGrantView, agent: &str) -> Result<Option<String>, ()> {
    if !duduclaw_core::is_valid_agent_id(agent) || agent == v.default_id
        || v.main_agents.contains(agent) || !v.loaded_agents.contains_key(agent)
    { return Ok(None); }
    // Personal employees never inherit department grants, including for their
    // owner. Explicit Owner bindings remain the authority for owner operations.
    let owners = crate::personal_visibility::personal_owners(&v.home).map_err(|_| ())?;
    if owners.contains_key(agent) { return Ok(None); }
    // Registry roles can be stale; only exact loaded directory ids qualify.
    let raw = std::fs::read_to_string(v.home.join("agents").join(agent).join("agent.toml"))
        .map_err(|_| ())?;
    let config: toml::Value = toml::from_str(&raw).map_err(|_| ())?;
    let role: AgentRole = config.get("agent").and_then(|info| info.get("role"))
        .ok_or(())?.clone().try_into().map_err(|_| ())?;
    if role == AgentRole::Main { return Ok(None); }
    duduclaw_agent::skill_loader::try_authoritative_department(&v.home, agent)
}

/// DISPLAY ONLY: never use this cache or its results to authorize any operation.
/// One fresh users.db snapshot per distinct creator per list request; target
/// metadata and the registry/default/main snapshot are shared by all rows.
/// No session/origin requirement: this describes an account, not a login.
pub(crate) struct DisplayCreatorAccessCache {
    db: Option<std::sync::Arc<duduclaw_auth::UserDb>>,
    targets: Option<DeptGrantView>,
    grants_enabled: bool,
    creators: BTreeMap<String, Result<Option<duduclaw_auth::dept_grants::DisplayCreatorAccess>, String>>,
    departments: BTreeMap<String, Result<Option<String>, ()>>,
    machine_restricted: BTreeMap<String, Option<bool>>,
}

impl DisplayCreatorAccessCache {
    pub(crate) async fn load(h: &MethodHandler) -> Self {
        Self {
            db: h.user_db().await,
            targets: DeptGrantView::load_targets(h).await,
            grants_enabled: !h.resolve_edition_profile().await.is_personal(),
            creators: BTreeMap::new(), departments: BTreeMap::new(),
            machine_restricted: BTreeMap::new(),
        }
    }

    /// DISPLAY ONLY. None means unknown (serialize as null), never permission.
    pub(crate) fn display_creator_access_lost(&mut self, user_id: Option<&str>, agent: &str) -> Option<bool> {
        let Some(user_id) = user_id.filter(|id| !id.is_empty()) else { return Some(false); };
        let db = self.db.as_ref()?;
        let record = self.creators.entry(user_id.to_owned())
            .or_insert_with(|| db.display_creator_access(user_id));
        let record = record.as_ref().ok()?;
        let Some(record) = record else { return Some(true); };
        if record.user.status != UserStatus::Active { return Some(true); }
        if record.user.role == UserRole::Admin { return Some(false); }
        if record.bindings.get(agent).is_some_and(|level| level.at_least(duduclaw_auth::AccessLevel::Operator)) {
            return Some(false);
        }
        if !self.grants_enabled || record.user.role != UserRole::Manager
            || !record.grants.iter().any(|(_, groups)| groups.contains(&GrantGroup::Schedules))
            || !action_allowed(GrantGroup::Schedules, GrantAction::Toggle)
        { return Some(true); }
        let targets = self.targets.as_ref()?;
        let department = self.departments.entry(agent.to_owned())
            .or_insert_with(|| grant_target_department(targets, agent)).as_ref().ok()?;
        let Some(department) = department else { return Some(true); };
        if !record.grants.iter().any(|(name, groups)| name == department && groups.contains(&GrantGroup::Schedules)) {
            return Some(true);
        }
        if duduclaw_auth::acl::personal_restriction()
            .is_some_and(|r| r.denies_delegation(user_id, agent))
        {
            // This API conflates ownership denial with unreadable ownership.
            // Do not present that ambiguous result as a confirmed revocation.
            return None;
        }
        // Preserve unknown preset reads; do not re-read the creator via check_target.
        let restricted = *self.machine_restricted.entry(agent.to_owned())
            .or_insert_with(|| crate::machine_employee::restriction_known(&targets.home, agent));
        restricted
    }

    pub(crate) fn display_creator_name(&self, user_id: &str) -> Option<&str> {
        let name = self.creators.get(user_id)?.as_ref().ok()?.as_ref()?.user.display_name.trim();
        (!name.is_empty()).then_some(name)
    }
}

/// DISPLAY ONLY: rule ownership is department-based, never an agent binding.
/// Batch all rows through one request-local cache, reading each creator once.
pub(crate) struct DisplayAutopilotCreatorAccessCache {
    db: Option<std::sync::Arc<duduclaw_auth::UserDb>>,
    grants_enabled: bool,
    creators: BTreeMap<String, Result<Option<duduclaw_auth::dept_grants::DisplayCreatorAccess>, String>>,
}

impl DisplayAutopilotCreatorAccessCache {
    pub(crate) async fn load(h: &MethodHandler) -> Self {
        Self {
            db: h.user_db().await,
            grants_enabled: !h.resolve_edition_profile().await.is_personal(),
            creators: BTreeMap::new(),
        }
    }

    /// None is unknown and must become JSON null. Never use this to authorize.
    pub(crate) fn display_creator_access_lost(
        &mut self, user_id: Option<&str>, owner_department: Option<&str>,
    ) -> Option<bool> {
        let Some(user_id) = user_id else { return Some(false); };
        let db = self.db.as_ref()?;
        let record = self.creators.entry(user_id.to_owned())
            .or_insert_with(|| db.display_creator_access(user_id));
        let Some(record) = record.as_ref().ok()? else { return Some(true); };
        if record.user.status != UserStatus::Active { return Some(true); }
        if record.user.role == UserRole::Admin { return Some(false); }
        let Some(department) = owner_department else { return Some(true); };
        Some(!self.grants_enabled || record.user.role != UserRole::Manager
            || !record.grants.iter().any(|(name, groups)|
                name == department && groups.contains(&GrantGroup::Automation)))
    }
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
