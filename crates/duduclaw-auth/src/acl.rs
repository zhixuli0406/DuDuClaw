use std::collections::HashMap;
use std::sync::OnceLock;

use crate::models::{AccessLevel, User, UserRole};
use crate::org::{EffectiveAccess, TeamGrant};

/// v1.81 P1g (owner decision 2026-11-09): the protected-mode default for
/// Personal employees is inverted. The gateway installs one restriction at
/// boot ([`install_personal_restriction`]); every ACL predicate of
/// [`UserContext`] asks it first, so an Admin (or anyone else) is refused a
/// Personal employee they do not own — the role shortcut no longer reaches
/// it. Handlers gated only by the ACL therefore fail closed by default.
/// Management actions that stay open for Admins use
/// [`UserContext::has_management_access`] explicitly (each audited by its
/// caller); private data goes through the gateway's audited step-up.
pub trait PersonalRestriction: Send + Sync {
    /// `true` when `user_id` must not reach `agent` through the ACL: a
    /// Personal employee whose recorded owner is someone else, or ownership
    /// that cannot be read (fail closed). Read fresh on every call; `false`
    /// outside protected mode.
    fn denies(&self, user_id: &str, agent: &str) -> bool;

    /// `true` when `user_id` must not hand `agent` work ([`UserContext::
    /// can_assign_to`]). Delegation is owner-only: an implementation must not
    /// relax it for an audited Admin read (break-glass). Default: [`Self::denies`].
    fn denies_delegation(&self, user_id: &str, agent: &str) -> bool {
        self.denies(user_id, agent)
    }
}

static PERSONAL_RESTRICTION: OnceLock<Box<dyn PersonalRestriction>> = OnceLock::new();

/// Install the process-wide Personal restriction (gateway boot). One-shot:
/// `false` if one was already installed (the first stays).
pub fn install_personal_restriction(restriction: Box<dyn PersonalRestriction>) -> bool {
    PERSONAL_RESTRICTION.set(restriction).is_ok()
}

/// The installed restriction, if any.
pub fn personal_restriction() -> Option<&'static dyn PersonalRestriction> {
    PERSONAL_RESTRICTION.get().map(|restriction| restriction.as_ref())
}

/// Per-connection user context extracted from JWT claims.
/// Carried through the entire WebSocket session for ACL checks.
#[derive(Debug, Clone)]
pub struct UserContext {
    pub credential_epoch: u64,
    pub user_id: String,
    pub email: String,
    pub role: UserRole,
    /// Agent name → access level mapping.
    pub agent_access: HashMap<String, AccessLevel>,
    /// Set for a user whose account still carries the DB-level
    /// `must_change_password` flag (the bootstrap `admin@local`, or any
    /// account an operator has reset). The WS handshake authenticates such a
    /// user normally — see `server.rs::authenticate_jwt` — rather than
    /// refusing the handshake outright, which previously left the frontend
    /// stuck on an unexplained connecting spinner
    /// (`wiki/reports/resolved-todos/TODO-bootstrap-admin-ws-deadlock.md`). The single RPC
    /// dispatch chokepoint (`MethodHandler::dispatch`) reads this flag and
    /// restricts the caller to a small self-service allowlist
    /// (`users.change_password` chief among them) until it clears.
    pub must_change_password: bool,
    /// Agents reachable through subordinates' bindings (`org.rs`), each at
    /// most Operator. Read only by [`Self::can_call_agent`] and
    /// [`Self::can_assign_to`]; [`Self::has_agent_access`] ignores it, so the
    /// Operator-level RPCs (memory, history, settings) do not widen.
    pub team_access: HashMap<String, TeamGrant>,
    /// `auth_meta.acl_generation` this context was built at; 0 for a context
    /// built from JWT claims or a fallback.
    pub acl_generation: u64,
}

impl UserContext {
    /// Create an admin-level context for **backward-compatible authentication only**.
    ///
    /// # Security
    /// This context bypasses all ACL checks. Use ONLY for:
    /// - Legacy pre-shared token auth
    /// - Local-only mode (no users in DB, no auth token configured)
    ///
    /// Do NOT call this for JWT-authenticated users — use `from_claims` instead.
    pub fn admin_fallback() -> Self {
        Self {
            credential_epoch: 0,
            user_id: "system".to_string(),
            email: "admin@local".to_string(),
            role: UserRole::Admin,
            agent_access: HashMap::new(),
            must_change_password: false,
            team_access: HashMap::new(),
            acl_generation: 0,
        }
    }

    /// Build from a fresh database read (`UserDb::live_user_context`). The
    /// JWT only proves who the caller is; role, bindings, team grants and the
    /// forced-password-change flag all come from here.
    pub fn from_live(user: &User, access: &EffectiveAccess) -> Self {
        Self {
            credential_epoch: user.credential_epoch,
            user_id: user.id.clone(),
            email: user.email.clone(),
            role: access.role,
            agent_access: access.direct.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            must_change_password: user.must_change_password,
            team_access: access.team.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            acl_generation: access.generation,
        }
    }

    /// Build from JWT claims.
    ///
    /// `must_change_password` is deliberately NOT part of the JWT claims
    /// (a token issued at login can outlive a password change, and claims
    /// are never re-verified against the DB mid-session) — the caller passes
    /// in a value it just read fresh from the user table, mirroring how
    /// `role` and `agent_access` are already point-in-time snapshots taken
    /// at handshake time, not re-checked per RPC.
    pub fn from_claims(claims: &crate::jwt::Claims, must_change_password: bool) -> Result<Self, String> {
        let role: UserRole = claims.role.parse()
            .map_err(|e: String| format!("invalid role in JWT: {e}"))?;
        let mut agent_access = HashMap::new();
        for (agent, level_str) in &claims.access_levels {
            let level: AccessLevel = level_str.parse().unwrap_or_else(|_| {
                tracing::warn!(agent = %agent, value = %level_str, "invalid access_level in JWT, defaulting to Viewer");
                AccessLevel::Viewer
            });
            agent_access.insert(agent.clone(), level);
        }
        Ok(Self {
            credential_epoch: claims.credential_epoch,
            user_id: claims.sub.clone(),
            email: claims.email.clone(),
            role,
            agent_access,
            must_change_password,
            team_access: HashMap::new(),
            acl_generation: 0,
        })
    }

    /// Returns `true` if this user has at least the given role.
    pub fn has_role(&self, min: UserRole) -> bool {
        self.role.at_least(min)
    }

    /// Returns `true` if the user is an admin.
    pub fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }

    /// Returns `true` if this caller must change their password before doing
    /// anything else. `MethodHandler::dispatch` is the enforcement point; this
    /// is just the named predicate so call sites read as intent, not a raw
    /// field poke.
    pub fn requires_password_change(&self) -> bool {
        self.must_change_password
    }

    /// P1g: `agent_name` is a Personal employee this caller does not own (or
    /// its ownership is unreadable) under `restriction`.
    pub fn personal_denied_under(&self, agent_name: &str, restriction: Option<&dyn PersonalRestriction>) -> bool {
        restriction.is_some_and(|restriction| restriction.denies(&self.user_id, agent_name))
    }

    /// P1g: [`Self::personal_denied_under`] with the installed restriction.
    /// Listings must drop such an employee (hidden, not redacted) unless they
    /// are an explicit metadata surface.
    pub fn hides_agent(&self, agent_name: &str) -> bool {
        self.personal_denied_under(agent_name, personal_restriction())
    }

    /// P1g: `agent_name` is a Personal employee this caller does not own,
    /// ignoring any audited break-glass read of this request — for actions
    /// that are owner-only (delegation, taking over or stopping its live
    /// session, running work on it).
    pub fn owner_only_denied(&self, agent_name: &str) -> bool {
        personal_restriction().is_some_and(|r| r.denies_delegation(&self.user_id, agent_name))
    }

    /// Check if the user can access a specific agent (any level).
    /// Admins can access all agents — except, in protected mode, a Personal
    /// employee they do not own (P1g).
    pub fn can_access_agent(&self, agent_name: &str) -> bool {
        self.can_access_agent_under(agent_name, personal_restriction())
    }

    /// [`Self::can_access_agent`] under an explicit restriction (tests).
    pub fn can_access_agent_under(&self, agent_name: &str, restriction: Option<&dyn PersonalRestriction>) -> bool {
        if self.personal_denied_under(agent_name, restriction) {
            return false;
        }
        self.is_admin() || self.agent_access.contains_key(agent_name)
    }

    /// Check if the user has at least the given access level for an agent.
    /// Admins pass — except, in protected mode, for a Personal employee they
    /// do not own (P1g: refused, whatever the role).
    pub fn has_agent_access(&self, agent_name: &str, min_level: AccessLevel) -> bool {
        self.has_agent_access_under(agent_name, min_level, personal_restriction())
    }

    /// [`Self::has_agent_access`] under an explicit restriction (tests).
    pub fn has_agent_access_under(
        &self,
        agent_name: &str,
        min_level: AccessLevel,
        restriction: Option<&dyn PersonalRestriction>,
    ) -> bool {
        if self.personal_denied_under(agent_name, restriction) {
            return false;
        }
        self.has_management_access(agent_name, min_level)
    }

    /// P1g: the pre-P1g rule (Admins always pass, everyone else needs the
    /// binding level) WITHOUT the Personal restriction. Only for the explicit
    /// Admin management allowlist — name / status metadata, pause / resume,
    /// the lifecycle RPCs and list metadata — whose callers audit an Admin
    /// acting on another person's Personal employee. Never for private data,
    /// configuration or delegation.
    pub fn has_management_access(&self, agent_name: &str, min_level: AccessLevel) -> bool {
        if self.is_admin() {
            return true;
        }
        self.agent_access
            .get(agent_name)
            .map(|level| level.at_least(min_level))
            .unwrap_or(false)
    }

    /// May talk to `agent_name`: Admin, any direct binding, or a team grant.
    /// Wider than [`Self::has_agent_access`] on purpose; use it only for
    /// "who can I call", never for memory, history or settings. P1g: never a
    /// Personal employee the caller does not own.
    pub fn can_call_agent(&self, agent_name: &str) -> bool {
        self.can_call_agent_under(agent_name, personal_restriction())
    }

    /// [`Self::can_call_agent`] under an explicit restriction (tests).
    pub fn can_call_agent_under(&self, agent_name: &str, restriction: Option<&dyn PersonalRestriction>) -> bool {
        if self.personal_denied_under(agent_name, restriction) {
            return false;
        }
        self.is_admin()
            || self.agent_access.contains_key(agent_name)
            || self.team_access.contains_key(agent_name)
    }

    /// May hand `agent_name` a task: Admin, a direct binding of Operator or
    /// above, or a team grant that derived Operator. P1g: never a Personal
    /// employee the caller does not own (delegation is owner-only).
    pub fn can_assign_to(&self, agent_name: &str) -> bool {
        self.can_assign_to_under(agent_name, personal_restriction())
    }

    /// [`Self::can_assign_to`] under an explicit restriction (tests).
    pub fn can_assign_to_under(&self, agent_name: &str, restriction: Option<&dyn PersonalRestriction>) -> bool {
        if restriction.is_some_and(|r| r.denies_delegation(&self.user_id, agent_name)) {
            return false;
        }
        if self.is_admin() || self.has_agent_access_under(agent_name, AccessLevel::Operator, restriction) {
            return true;
        }
        self.team_access
            .get(agent_name)
            .is_some_and(|g| g.level.at_least(AccessLevel::Operator))
    }

    /// List of agent names this user can see.
    /// For admin, returns None (meaning "all") — P1g: callers that treat
    /// `None` as "all" must still drop [`Self::hides_agent`] employees.
    pub fn visible_agents(&self) -> Option<Vec<String>> {
        if self.is_admin() {
            None // All agents visible (minus `hides_agent`, P1g)
        } else {
            Some(
                self.agent_access
                    .keys()
                    .filter(|agent| !self.hides_agent(agent))
                    .cloned()
                    .collect(),
            )
        }
    }
}

/// Require a minimum role, returning a generic error string on failure.
/// Does NOT leak the user's actual role or the required role to prevent enumeration.
pub fn require_role(ctx: &UserContext, min: UserRole) -> Result<(), String> {
    if ctx.has_role(min) {
        Ok(())
    } else {
        Err("permission denied".to_string())
    }
}

/// Require that the user can access a specific agent.
/// Uses generic error message to prevent agent name enumeration.
pub fn require_agent_access(
    ctx: &UserContext,
    agent_name: &str,
    min_level: AccessLevel,
) -> Result<(), String> {
    if ctx.has_agent_access(agent_name, min_level) {
        Ok(())
    } else {
        Err("permission denied".to_string())
    }
}

/// Extract an agent_id from RPC params and verify access.
/// Returns the agent_id on success, or an error string.
pub fn extract_and_check_agent(
    ctx: &UserContext,
    params: &serde_json::Value,
    min_level: AccessLevel,
) -> Result<String, String> {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing agent_id parameter".to_string())?
        .to_string();

    require_agent_access(ctx, &agent_id, min_level)?;
    Ok(agent_id)
}

/// P1g: [`extract_and_check_agent`] for the explicit Admin management
/// allowlist only ([`UserContext::has_management_access`]: name / status
/// metadata, pause / resume). The caller audits an Admin acting on another
/// person's Personal employee.
pub fn extract_and_check_management(
    ctx: &UserContext,
    params: &serde_json::Value,
    min_level: AccessLevel,
) -> Result<String, String> {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing agent_id parameter".to_string())?
        .to_string();
    if ctx.has_management_access(&agent_id, min_level) {
        Ok(agent_id)
    } else {
        Err("permission denied".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn employee_ctx() -> UserContext {
        let mut access = HashMap::new();
        access.insert("my-agent".to_string(), AccessLevel::Owner);
        UserContext {
            credential_epoch: 0,
            user_id: "u1".to_string(),
            email: "emp@test.com".to_string(),
            role: UserRole::Employee,
            agent_access: access,
            must_change_password: false,
            team_access: HashMap::new(),
            acl_generation: 0,
        }
    }

    fn admin_ctx() -> UserContext {
        UserContext::admin_fallback()
    }

    fn claims(role: &str) -> crate::jwt::Claims {
        crate::jwt::Claims {
            credential_epoch: 0,
            sub: "u1".to_string(),
            email: "u1@test.local".to_string(),
            role: role.to_string(),
            bound_agents: Vec::new(),
            access_levels: HashMap::new(),
            exp: 0,
            iat: 0,
            token_type: "access".to_string(),
            jti: None,
        }
    }

    #[test]
    fn admin_fallback_never_requires_password_change() {
        assert!(!admin_ctx().requires_password_change());
    }

    #[test]
    fn from_claims_carries_the_caller_supplied_must_change_password_flag() {
        // The flag is a fresh DB read the caller passes in, NOT decoded from
        // the JWT itself (see the doc comment on `from_claims`) — this locks
        // that contract down: both `true` and `false` pass through verbatim.
        let flagged = UserContext::from_claims(&claims("admin"), true).unwrap();
        assert!(flagged.requires_password_change());

        let clear = UserContext::from_claims(&claims("admin"), false).unwrap();
        assert!(!clear.requires_password_change());
    }

    #[test]
    fn admin_can_access_any_agent() {
        let ctx = admin_ctx();
        assert!(ctx.can_access_agent("any-agent"));
        assert!(ctx.has_agent_access("any-agent", AccessLevel::Owner));
    }

    #[test]
    fn employee_can_only_access_bound_agent() {
        let ctx = employee_ctx();
        assert!(ctx.can_access_agent("my-agent"));
        assert!(!ctx.can_access_agent("other-agent"));
    }

    #[test]
    fn access_level_check() {
        let ctx = employee_ctx();
        assert!(ctx.has_agent_access("my-agent", AccessLevel::Viewer));
        assert!(ctx.has_agent_access("my-agent", AccessLevel::Operator));
        assert!(ctx.has_agent_access("my-agent", AccessLevel::Owner));
        assert!(!ctx.has_agent_access("other-agent", AccessLevel::Viewer));
    }

    #[test]
    fn role_check() {
        let ctx = employee_ctx();
        assert!(ctx.has_role(UserRole::Employee));
        assert!(!ctx.has_role(UserRole::Manager));
        assert!(!ctx.has_role(UserRole::Admin));
    }

    #[test]
    fn team_grants_open_calling_and_assigning_but_not_agent_access() {
        let mut ctx = employee_ctx();
        ctx.role = UserRole::Manager;
        ctx.team_access.insert(
            "sales".to_string(),
            TeamGrant { level: AccessLevel::Operator, via_user: "e1".into(), depth: 2 },
        );
        ctx.team_access.insert(
            "hr".to_string(),
            TeamGrant { level: AccessLevel::Viewer, via_user: "e2".into(), depth: 1 },
        );
        assert!(ctx.can_call_agent("sales"));
        assert!(ctx.can_assign_to("sales"));
        assert!(ctx.can_call_agent("hr"));
        assert!(!ctx.can_assign_to("hr"), "a Viewer-derived grant can only talk");
        assert!(!ctx.can_call_agent("finance"));
        // has_agent_access is unchanged: team grants never reach the
        // memory / history / settings RPCs.
        assert!(!ctx.has_agent_access("sales", AccessLevel::Viewer));
        assert!(!ctx.can_access_agent("sales"));
        assert_eq!(ctx.visible_agents().unwrap(), vec!["my-agent"]);
        // A direct Viewer binding does not allow assigning.
        ctx.agent_access.insert("viewer-only".into(), AccessLevel::Viewer);
        assert!(ctx.can_call_agent("viewer-only"));
        assert!(!ctx.can_assign_to("viewer-only"));
        assert!(ctx.can_assign_to("my-agent"));
    }

    /// `alice-personal` belongs to `alice`; `broken` has unreadable ownership.
    struct Fixture;
    impl PersonalRestriction for Fixture {
        fn denies(&self, user_id: &str, agent: &str) -> bool {
            match agent {
                "alice-personal" => user_id != "alice",
                "broken" => true,
                _ => false,
            }
        }
    }

    #[test]
    fn p1g_admin_is_refused_a_personal_employee_it_does_not_own() {
        let r: Option<&dyn PersonalRestriction> = Some(&Fixture);
        let admin = admin_ctx();
        for level in [AccessLevel::Viewer, AccessLevel::Operator, AccessLevel::Owner] {
            assert!(!admin.has_agent_access_under("alice-personal", level, r), "{level:?}");
            assert!(admin.has_agent_access_under("shared", level, r));
        }
        assert!(!admin.can_access_agent_under("alice-personal", r));
        assert!(!admin.can_call_agent_under("alice-personal", r));
        assert!(!admin.can_assign_to_under("alice-personal", r));
        assert!(!admin.has_agent_access_under("broken", AccessLevel::Viewer, r), "unreadable ⇒ refused");
        // The explicit management rule keeps the pre-P1g Admin shortcut.
        assert!(admin.has_management_access("alice-personal", AccessLevel::Owner));
        // The owner keeps its binding; another bound user is refused.
        let mut alice = employee_ctx();
        alice.user_id = "alice".into();
        alice.agent_access.insert("alice-personal".into(), AccessLevel::Owner);
        assert!(alice.has_agent_access_under("alice-personal", AccessLevel::Owner, r));
        assert!(alice.can_assign_to_under("alice-personal", r));
        let mut bob = employee_ctx();
        bob.agent_access.insert("alice-personal".into(), AccessLevel::Owner);
        assert!(!bob.has_agent_access_under("alice-personal", AccessLevel::Viewer, r));
        assert!(!bob.can_call_agent_under("alice-personal", r));
        // No restriction (unprotected install): unchanged.
        assert!(admin.has_agent_access_under("alice-personal", AccessLevel::Owner, None));
        assert!(bob.has_agent_access_under("alice-personal", AccessLevel::Owner, None));
    }

    #[test]
    fn visible_agents_filtered() {
        let ctx = employee_ctx();
        let visible = ctx.visible_agents().unwrap();
        assert_eq!(visible, vec!["my-agent"]);

        let admin = admin_ctx();
        assert!(admin.visible_agents().is_none()); // All visible
    }
}
