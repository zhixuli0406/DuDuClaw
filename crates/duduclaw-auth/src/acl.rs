use std::collections::HashMap;

use crate::models::{AccessLevel, User, UserRole};
use crate::org::{EffectiveAccess, TeamGrant};

/// Per-connection user context extracted from JWT claims.
/// Carried through the entire WebSocket session for ACL checks.
#[derive(Debug, Clone)]
pub struct UserContext {
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

    /// Check if the user can access a specific agent (any level).
    /// Admins can access all agents.
    pub fn can_access_agent(&self, agent_name: &str) -> bool {
        if self.is_admin() {
            return true;
        }
        self.agent_access.contains_key(agent_name)
    }

    /// Check if the user has at least the given access level for an agent.
    /// Admins always pass.
    pub fn has_agent_access(&self, agent_name: &str, min_level: AccessLevel) -> bool {
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
    /// "who can I call", never for memory, history or settings.
    pub fn can_call_agent(&self, agent_name: &str) -> bool {
        self.is_admin()
            || self.agent_access.contains_key(agent_name)
            || self.team_access.contains_key(agent_name)
    }

    /// May hand `agent_name` a task: Admin, a direct binding of Operator or
    /// above, or a team grant that derived Operator.
    pub fn can_assign_to(&self, agent_name: &str) -> bool {
        if self.is_admin() || self.has_agent_access(agent_name, AccessLevel::Operator) {
            return true;
        }
        self.team_access
            .get(agent_name)
            .is_some_and(|g| g.level.at_least(AccessLevel::Operator))
    }

    /// List of agent names this user can see.
    /// For admin, returns None (meaning "all").
    pub fn visible_agents(&self) -> Option<Vec<String>> {
        if self.is_admin() {
            None // All agents visible
        } else {
            Some(self.agent_access.keys().cloned().collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn employee_ctx() -> UserContext {
        let mut access = HashMap::new();
        access.insert("my-agent".to_string(), AccessLevel::Owner);
        UserContext {
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

    #[test]
    fn visible_agents_filtered() {
        let ctx = employee_ctx();
        let visible = ctx.visible_agents().unwrap();
        assert_eq!(visible, vec!["my-agent"]);

        let admin = admin_ctx();
        assert!(admin.visible_agents().is_none()); // All visible
    }
}
