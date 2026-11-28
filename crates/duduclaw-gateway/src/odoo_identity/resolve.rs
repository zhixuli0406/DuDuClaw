//! Ordered identity selection from live gateway-owned facts. Blocking only.
use std::path::Path;
use duduclaw_auth::{AccessLevel, UserDb, UserStatus};
use duduclaw_auth::odoo_credentials::UnsealedOdooCredential;
use duduclaw_knowledge::requester::{RequesterIdentity, is_public_visitor_channel};
use duduclaw_odoo::AgentOdooConfig;
use duduclaw_odoo::access_policy::{OdooAccessConfig, IdentityKind, EffectiveMode,
    PermissionOverride, effective_mode};
use serde_json::{Value, json};

use super::Refusal;
use super::server::{Server, company_server_from_document, member_credentials_enabled_from_document};

pub(super) struct Settings {
    pub access: OdooAccessConfig,
    pub server: Option<Server>,
    pub member_credentials_enabled: bool,
}

impl Settings {
    pub fn load(home: &Path) -> Result<Self, Refusal> {
        let raw = std::fs::read_to_string(home.join("config.toml"))
            .map_err(|_| Refusal::access_invalid())?;
        let access = OdooAccessConfig::parse_config_toml(&raw)
            .map_err(|_| Refusal::access_invalid())?;
        let doc = toml::from_str(&raw).map_err(|_| Refusal::access_invalid())?;
        let server = company_server_from_document(&doc).map_err(|_| Refusal::unavailable())?;
        Ok(Self { access, server, member_credentials_enabled: member_credentials_enabled_from_document(&doc) })
    }
}

pub(super) struct Decision {
    pub identity: IdentityKind,
    pub permission: EffectiveMode,
    pub personal: Option<(String, UnsealedOdooCredential, String, String, String)>,
}

impl Decision {
    pub fn identity_name(&self) -> &'static str {
        match self.identity {
            IdentityKind::Personal => "personal",
            IdentityKind::Agent => "agent",
            IdentityKind::Company => "company",
            IdentityKind::NotConnected => "none",
        }
    }
    pub fn user_id(&self) -> Option<&str> {
        self.personal.as_ref().map(|(user, ..)| user.as_str())
    }
    pub fn response(self, have_rev: Option<i64>) -> Value {
        let mut value = json!({"ok":true,"identity":self.identity_name(),"permission":match self.permission {
            EffectiveMode::Follow => "follow", EffectiveMode::Engine => "engine", EffectiveMode::ReadOnly => "read_only",
        }});
        if let Some((user, credential, url, db, server_fp)) = self.personal {
            value["url"] = json!(url);
            value["db"] = json!(db);
            value["server_fp"] = json!(server_fp);
            value["user_id"] = json!(user);
            value["odoo_uid"] = json!(credential.odoo_uid);
            value["login"] = json!(credential.login);
            value["rev"] = json!(credential.rev);
            if have_rev != Some(credential.rev) { value["secret"] = json!(credential.secret.as_str()); }
        }
        value
    }
}

fn personal(home: &Path, db: &UserDb, user: &str, settings: &Settings,
    permission: EffectiveMode) -> Result<Option<Decision>, Refusal> {
    if !settings.member_credentials_enabled { return Ok(None); }
    let Some(server) = &settings.server else { return Ok(None); };
    // Missing credentials do not require a machine key. Once a row exists,
    // only unseal decides usability (including fingerprint invalidation).
    if db.odoo_credential_status(user).map_err(|_| Refusal::unavailable())?.is_none() {
        return Ok(None);
    }
    let sealer = crate::odoo_credentials::Sealer::load(home).map_err(|_| Refusal::unavailable())?;
    let Some(credential) = db.odoo_credential_unseal(user, &server.fp, &sealer)
        .map_err(|_| Refusal::unavailable())? else { return Ok(None); };
    db.odoo_credential_touch(user).map_err(|_| Refusal::unavailable())?;
    Ok(Some(Decision { identity: IdentityKind::Personal, permission,
        personal: Some((user.to_owned(), credential, server.url.clone(), server.db.clone(), server.fp.clone())) }))
}

/// Reverse avatar lookup with the live active-member and owner/operator gates.
/// A primary-agent preference alone is never an avatar assignment.
fn avatar_owner(db: &UserDb, agent: &str) -> Result<Option<String>, Refusal> {
    let bindings = db.get_agent_users(agent).map_err(|_| Refusal::unavailable())?;
    let mut owner = None;
    for binding in bindings {
        if !matches!(binding.access_level, AccessLevel::Owner | AccessLevel::Operator) { continue; }
        let user = db.get_user(&binding.user_id).map_err(|_| Refusal::unavailable())?;
        if let Some(user) = user.filter(|u| u.status == UserStatus::Active
            && u.avatar_agent_id.as_deref() == Some(agent)) {
            if owner.is_some() { return Err(Refusal::unavailable()); }
            owner = Some(user.id);
        }
    }
    Ok(owner)
}

/// System/unknown turns never stand for a human here, even if a system
/// record carries on_behalf_of for another subsystem's delegation policy.
fn requester_person(requester: &RequesterIdentity) -> Option<&str> {
    match requester {
        RequesterIdentity::DashboardUser { user_id } => Some(user_id),
        RequesterIdentity::ChannelUser { channel, user_id: Some(user_id), .. }
            if !is_public_visitor_channel(channel) => Some(user_id),
        _ => None,
    }
}

pub(super) fn employee(home: &Path, db: &UserDb, agent: &str,
    requester: &RequesterIdentity, settings: &Settings) -> Result<Decision, Refusal> {
    if !duduclaw_core::is_valid_agent_id(agent) { return Err(Refusal::unauthorized()); }
    let dir = if crate::ephemeral::is_ephemeral_id(agent) {
        crate::ephemeral::resolve_agent_dir(home, agent).ok_or_else(Refusal::unavailable)?
    } else { home.join("agents").join(agent) };
    let raw = std::fs::read_to_string(dir.join("agent.toml")).map_err(|_| Refusal::unavailable())?;
    let config = AgentOdooConfig::parse_agent_toml(&raw).map_err(|_| Refusal::unavailable())?.unwrap_or_default();
    let permission = |kind| effective_mode(kind, &settings.access, config.permission_override_mode());
    let avatar = avatar_owner(db, agent)?;
    // An avatar without its owner's credential falls through to rows 3–5,
    // rather than borrowing a different person's requester credential.
    let requester = (avatar.is_none() && config.act_as_requester)
        .then(|| requester_person(requester)).flatten();
    // Row 1, then row 2. Neither a missing row nor a failed unseal grants
    // company access: row 4 always requires an explicit administrator list.
    for user in [avatar.as_deref(), requester].into_iter().flatten() {
        if let Some(decision) = personal(home, db, user, settings, permission(IdentityKind::Personal))? {
            return Ok(decision);
        }
    }
    let populated = |value: &Option<String>| value.as_ref().is_some_and(|v| !v.trim().is_empty());
    if populated(&config.username) && (populated(&config.api_key_enc) || populated(&config.password_enc)) {
        return Ok(Decision { identity: IdentityKind::Agent, permission: permission(IdentityKind::Agent), personal: None });
    }
    // Host org authority cross-checks every ephemeral sidecar, rejects cycles,
    // and ends at the regular parent. Never infer a parent from the id string.
    let identities = duduclaw_core::autonomy_gate::effective_identities(
        home, agent, duduclaw_core::autonomy_gate::AutonomyPath::Dispatcher,
    ).map_err(|_| Refusal::unavailable())?;
    let parent = identities.last().ok_or_else(Refusal::unavailable)?;
    if settings.access.company_key_agents.iter().any(|id| id == parent) {
        return Ok(Decision { identity: IdentityKind::Company, permission: permission(IdentityKind::Company), personal: None });
    }
    let person = avatar.is_some() || requester.is_some();
    if person && !settings.member_credentials_enabled {
        return Err(Refusal::member_credentials_off());
    }
    Err(Refusal::not_connected(person))
}

pub(super) fn person_key(home: &Path, db: &UserDb, user: &str,
    settings: &Settings, write: bool) -> Result<Decision, Refusal> {
    if !settings.member_credentials_enabled { return Err(Refusal::member_credentials_off()); }
    let mode = effective_mode(IdentityKind::Personal, &settings.access, PermissionOverride::Inherit);
    if write && mode != EffectiveMode::Follow {
        return Err(Refusal::new(axum::http::StatusCode::FORBIDDEN, "odoo_follow_required",
            "公司尚未啟用依 Odoo 帳號權限模式，本人金鑰目前只能查詢，不能修改。"));
    }
    let permission = if mode == EffectiveMode::Follow { mode } else { EffectiveMode::ReadOnly };
    personal(home, db, user, settings, permission)?.ok_or_else(|| Refusal::not_connected(true))
}
