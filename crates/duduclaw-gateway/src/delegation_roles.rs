//! RM-183: role evidence is resolved only in the gateway, never from tool claims.
use std::path::Path;
pub(crate) mod initiators;
use duduclaw_core::delegation_policy::{OwnerRoleView, OwnerRoleLookupError, DelegationRequesterRole,
    DelegationAllowSource, DelegationBoundaries};
use duduclaw_knowledge::requester::RequesterIdentity;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// Wire snapshot, tied to one authenticated employee and one live host context.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleFacts {
    pub agent: String,
    pub owner_role: Option<String>,
    pub requester_role: Option<String>,
    pub requester_account_id: Option<String>,
    pub background: bool,
}
impl OwnerRoleView for RoleFacts {
    fn avatar_owner_role(&self, agent: &str) -> Result<Option<String>, OwnerRoleLookupError> {
        Ok((self.agent == agent).then(|| self.owner_role.clone()).flatten())
    }
    fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError> {
        if self.background { return Ok(None); }
        Ok(self.requester_role.clone().map(DelegationRequesterRole::Account))
    }
}

/// An owned identity captured by the gateway before spawning any CLI.
#[derive(Debug, Clone)]
pub(crate) struct Requester {
    who: RequesterIdentity,
    background: bool,
}
impl Requester {
    pub(crate) fn identity(&self) -> &RequesterIdentity { &self.who }

    /// Reuse the RM-183 live channel binding check without inferring an account role.
    pub(crate) fn verified_account_id(&self, home: &Path) -> Result<Option<String>, OwnerRoleLookupError> {
        GatewayOwnerRoles::new(home, self.clone()).requester_id()
            .map(|requester| requester.map(|(id, _)| id))
    }

    #[cfg(test)]
    pub(crate) fn attended(user_id: &str) -> Self {
        Self { who: RequesterIdentity::DashboardUser { user_id: user_id.into() },
            background: false }
    }

    pub(crate) fn current(_home: &Path, _agent: &str) -> Self {
        let dispatched = crate::memory_provenance::DISPATCH_RUN
            .try_with(|run| run.is_some()).unwrap_or(false);
        if dispatched || crate::runtime::round_task_env().is_some() {
            return Self::background();
        }
        let who = crate::knowledge::turn_requesters::current_requester();
        match who {
            RequesterIdentity::DashboardUser { .. } | RequesterIdentity::ChannelUser { .. } =>
                Self { who, background: false },
            _ => Self::background(),
        }
    }
    pub(crate) fn background() -> Self {
        Self { who: RequesterIdentity::System { kind: "delegation".into(), on_behalf_of: None },
            background: true }
    }
    pub(crate) fn isolated(context: &duduclaw_mcp_boundary::RequestContext, metadata: Option<&crate::isolation_egress::RunMetadata>) -> Self {
        use duduclaw_isolation::capability::Principal;
        let principal = match &context.record().principal {
            Principal::Human { user_id, .. } => duduclaw_core::HumanPrincipal::user(user_id.as_str()).map(|p| p.to_string()),
            Principal::Background { .. } => None,
        };
        // Only a protected run with interactive ingress can carry an attended requester.
        let mut requester = Self::background();
        if let (Some(id), Some(ingress)) = (principal.as_deref()
            .and_then(|p| duduclaw_core::HumanPrincipal::parse(p).ok())
            .and_then(|p| p.user_id().map(str::to_owned)), metadata.and_then(|m| m.ingress.as_ref())) {
            requester.background = false;
            requester.who = match ingress {
                crate::isolation_egress::IngressRequester::Dashboard => RequesterIdentity::DashboardUser { user_id: id },
                crate::isolation_egress::IngressRequester::Channel { channel, sender } => RequesterIdentity::ChannelUser {
                    user_id: Some(id), channel: channel.clone(), sender: sender.clone(), chat: String::new(), attested_private: false,
                },
            };
        }
        requester
    }
}

/// Reads users.db read-only for each evaluation; absence, schema and I/O errors
/// are lookup errors, never a new empty database or a privileged default.
pub(crate) struct GatewayOwnerRoles<'a> {
    home: &'a Path,
    requester: Requester,
}
impl<'a> GatewayOwnerRoles<'a> {
    pub(crate) fn new(home: &'a Path, requester: Requester) -> Self { Self { home, requester } }
    fn db(&self) -> Result<Connection, OwnerRoleLookupError> {
        Connection::open_with_flags(self.home.join("users.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| OwnerRoleLookupError)
    }
    fn requester_id(&self) -> Result<Option<(String, bool)>, OwnerRoleLookupError> {
        if self.requester.background { return Ok(None); }
        let id = match &self.requester.who {
            RequesterIdentity::DashboardUser { user_id } => Some((user_id.clone(), self.requester.background)),
            RequesterIdentity::ChannelUser { user_id: Some(user_id), channel, sender, .. } => {
                let db = crate::decision_notify::open_user_db_read_only(self.home)
                    .map_err(|_| OwnerRoleLookupError)?.ok_or(OwnerRoleLookupError)?;
                let mapped = crate::googlechat_identity::verified_user_id(self.home, &db, channel, sender)
                    .map_err(|_| OwnerRoleLookupError)?;
                (mapped.as_deref() == Some(user_id.as_str())).then(|| (user_id.clone(), self.requester.background))
            }
            _ => None,
        };
        Ok(id)
    }
    /// Keep the owner identity and role tied to the same unique active row.
    pub(crate) fn avatar_owner(&self, agent: &str) -> Result<Option<(String, String)>, OwnerRoleLookupError> {
        let db = self.db()?;
        // Reject ambiguous ownership even on an old database without the unique index.
        db.query_row("SELECT id, role FROM users WHERE avatar_agent_id=?1 AND status='active'
            AND (SELECT COUNT(*) FROM users WHERE avatar_agent_id=?1)=1", [agent], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(|_| OwnerRoleLookupError)
    }

    pub(crate) fn snapshot(&self, agent: &str) -> Result<RoleFacts, OwnerRoleLookupError> {
        let owner_role = self.avatar_owner_role(agent)?;
        let requester = self.requester_id()?;
        let role = self.requester_role()?;
        Ok(RoleFacts { agent: agent.into(), owner_role,
            requester_role: role.map(|r| match r {
                DelegationRequesterRole::Account(r) | DelegationRequesterRole::BackgroundStartedBy(r) => r }),
            requester_account_id: requester.as_ref().map(|(id, _)| id.clone()),
            background: requester.is_some_and(|(_, background)| background) })
    }
}
impl OwnerRoleView for GatewayOwnerRoles<'_> {
    fn avatar_owner_role(&self, agent: &str) -> Result<Option<String>, OwnerRoleLookupError> {
        let db = self.db()?;
        // Reject ambiguous ownership even on an old database without the unique index.
        db.query_row("SELECT role FROM users WHERE avatar_agent_id=?1 AND status='active'
            AND (SELECT COUNT(*) FROM users WHERE avatar_agent_id=?1)=1", [agent], |r| r.get(0))
            .optional().map_err(|_| OwnerRoleLookupError)
    }
    fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError> {
        let Some((id, _)) = self.requester_id()? else { return Ok(None); };
        let role: Option<String> = self.db()?.query_row(
            "SELECT role FROM users WHERE id=?1 AND status='active'", [id], |r| r.get(0))
            .optional().map_err(|_| OwnerRoleLookupError)?;
        Ok(role.map(DelegationRequesterRole::Account))
    }
}

pub fn can_send(home: &Path, sender: &str) -> bool {
    let dir = crate::ephemeral::resolve_agent_dir(home, sender)
        .unwrap_or_else(|| home.join("agents").join(sender));
    duduclaw_core::is_valid_agent_id(sender)
        && duduclaw_core::agent_toml::load_permissions_checked(&dir)
            .is_ok_and(|p| p.allows("can_send_cross_agent"))
}

/// Synchronous callers use the same main/default boundary, reading the actual
/// target config and configured default. Failed reads attest no boundary.
pub fn sync_boundaries(home: &Path, sender: &str, target: &str, principal: Option<&str>) -> DelegationBoundaries {
    let main_target_allowed = (|| -> Option<bool> {
        let restricted = crate::on_behalf_of::access::requires_main_restriction(home, principal).ok()?;
        if !restricted { return Some(true); }
        if target == "default" { return Some(false); }
        let dir = crate::ephemeral::resolve_agent_dir(home, target)
            .unwrap_or_else(|| home.join("agents").join(target));
        let cfg: duduclaw_core::types::AgentConfig = toml::from_str(&std::fs::read_to_string(dir.join("agent.toml")).ok()?).ok()?;
        let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
        let table: toml::Table = raw.parse().ok()?;
        let default = match table.get("general") {
            None => "",
            Some(general) => match general.as_table()?.get("default_agent") {
                None => "", Some(value) => value.as_str()?,
            },
        };
        Some(cfg.agent.role != duduclaw_core::types::AgentRole::Main
            && target != default && cfg.agent.name != default)
    })().unwrap_or(false);
    DelegationBoundaries {
        personal_target_allowed: crate::personal_visibility::delegation_refusal(
            home, crate::runtime_readiness::isolation_active(), sender, target).is_none(),
        main_target_allowed: main_target_allowed && crate::machine_employee::check_target(home, target, principal).is_ok(),
        // The MCP permission gate checks can_send_cross_agent before handlers.
        can_send_cross_agent: true,
    }
}

pub(crate) fn audit_allow(home: &Path, sender: &str, target: &str, source: DelegationAllowSource,
    requester: Option<&str>, path: &str) {
    if !matches!(source, DelegationAllowSource::UnrestrictedRole | DelegationAllowSource::OneWayPair) { return; }
    let event = duduclaw_security::audit::AuditEvent::new("delegation_allowed", sender,
        duduclaw_security::audit::Severity::Info, serde_json::json!({
            "sender": sender, "target": target, "source": source.as_str(), "path_kind": path,
            "requester_account_id": if source == DelegationAllowSource::UnrestrictedRole { requester } else { None },
        }));
    crate::security_autopilot::audit_and_emit(home, &event);
}

#[cfg(test)]
mod tests;

/// Ordinary task hand-overs opt into the main list; other boundaries are unchanged.
pub async fn assignment_boundaries(
    home: &Path, sender: &str, target: &str, principal: Option<&str>,
) -> Result<DelegationBoundaries, String> {
    Box::pin(crate::on_behalf_of::access::check_main_target_for(
        home, principal, target, crate::on_behalf_of::access::MainUse::Assign,
    )).await?;
    let mut boundaries = sync_boundaries(home, sender, target, principal);
    boundaries.main_target_allowed = crate::machine_employee::check_target(home, target, principal).is_ok();
    Ok(boundaries)
}
