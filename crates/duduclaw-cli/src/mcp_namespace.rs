// mcp_namespace.rs — Namespace isolation for MCP server (W19-P0)
//
// Resolves which namespaces a Principal may read and write, enforcing
// strict isolation between external clients and internal services.

use crate::mcp_auth::Principal;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NamespaceContext {
    pub write_namespace: String,
    pub read_namespaces: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum NamespaceError {
    Forbidden { requested: String },
    InvalidClientId,
}

impl std::fmt::Display for NamespaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamespaceError::Forbidden { requested } => {
                write!(f, "Access to namespace '{requested}' is forbidden")
            }
            NamespaceError::InvalidClientId => {
                write!(f, "client_id contains invalid characters")
            }
        }
    }
}

// ── Validation ───────────────────────────────────────────────────────────────

fn validate_client_id(client_id: &str) -> Result<(), NamespaceError> {
    if client_id.is_empty() {
        return Err(NamespaceError::InvalidClientId);
    }
    let re = regex::Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(client_id) {
        return Err(NamespaceError::InvalidClientId);
    }
    Ok(())
}

// ── Public API ───────────────────────────────────────────────────────────────

/// The shared pool every gateway-spawned employee used before v1.68.0: the
/// internal key's own namespace, `internal/gateway-internal`. Still the
/// namespace of an internal-key caller without a verified employee identity.
pub fn shared_internal_pool() -> String {
    format!(
        "internal/{}",
        duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
    )
}

/// What is known about the caller beyond its key (see [`resolve_for_caller`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct CallerIdentity<'a> {
    /// The employee id proven by `DUDUCLAW_AGENT_TOKEN` (verdict
    /// [`duduclaw_core::IdentityVerdict::Verified`] only — see
    /// [`verified_employee`]). `None` when absent, unverified or rejected.
    pub verified_agent: Option<&'a str>,
    /// The key's `client_id` names an existing employee
    /// (`agents/<client_id>/agent.toml` exists) — a per-agent key.
    pub client_is_agent: bool,
}

/// The employee a `DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN` pair proves,
/// or `None`. Only a [`duduclaw_core::IdentityVerdict::Verified`] claim
/// counts: an absent claim, a soft-mode `Unverified` one (token missing or
/// wrong) and a `Rejected` one all return `None`, so nothing reaches an
/// employee's memory by asserting an id it cannot prove. `Disabled` (no
/// `identity.key`) also returns `None` — there is nothing to prove against.
pub fn verified_employee(home_dir: &std::path::Path, claimed: &str, token: &str) -> Option<String> {
    let claimed = claimed.trim();
    let token = token.trim();
    if claimed.is_empty()
        || token.is_empty()
        || !duduclaw_core::is_valid_agent_id(claimed)
        || claimed == duduclaw_core::UNTRUSTED_AGENT_ID
    {
        return None;
    }
    let require = duduclaw_core::require_identity_token_from_home(home_dir);
    match duduclaw_core::verify_identity_claim(home_dir, claimed, token, require) {
        duduclaw_core::IdentityVerdict::Verified => Some(claimed.to_string()),
        _ => None,
    }
}

/// [`verified_employee`] over this process's environment.
pub fn verified_employee_from_env(home_dir: &std::path::Path) -> Option<String> {
    let claimed = std::env::var(duduclaw_core::ENV_AGENT_ID).unwrap_or_default();
    let token = std::env::var(duduclaw_core::ENV_AGENT_TOKEN).unwrap_or_default();
    verified_employee(home_dir, &claimed, &token)
}

/// Whether `client_id` is an employee: a valid agent id with an
/// `agents/<id>/agent.toml` on disk (exact path, never a prefix test).
pub fn client_is_agent(home_dir: &std::path::Path, client_id: &str) -> bool {
    duduclaw_core::is_valid_agent_id(client_id)
        && home_dir
            .join("agents")
            .join(client_id)
            .join("agent.toml")
            .is_file()
}

/// Resolve the namespace context for a given Principal with no identity
/// beyond its key (see [`resolve_for_caller`]).
pub fn resolve(principal: &Principal) -> Result<NamespaceContext, NamespaceError> {
    resolve_for_caller(principal, CallerIdentity::default())
}

/// Resolve which memory namespace a caller writes and reads.
///
/// | caller | write namespace | read set |
/// |---|---|---|
/// | external key | `external/{client_id}` | own + `shared/public` |
/// | internal key (`gateway-internal`) + verified employee `a` | `a` | `a` + `shared/public` |
/// | internal key without a verified employee | `internal/gateway-internal` | own + `shared/public` |
/// | per-agent key (client id is an employee `a`) | `a` | `a` + `shared/public` |
/// | any other internal key | `internal/{client_id}` | own + `shared/public` |
///
/// The bare employee id is the namespace the gateway itself distils into,
/// injects from and lets the dashboard manage, so a memory tool and the
/// gateway see the same rows. Fails closed: an internal-key caller that
/// cannot prove an employee identity stays in the old shared pool, which
/// holds no gateway-written rows. Comparisons are exact equality.
pub fn resolve_for_caller(
    principal: &Principal,
    identity: CallerIdentity<'_>,
) -> Result<NamespaceContext, NamespaceError> {
    validate_client_id(&principal.client_id)?;

    let own_ns = if principal.is_external {
        format!("external/{}", principal.client_id)
    } else if principal.client_id == duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID {
        match identity
            .verified_agent
            .filter(|a| duduclaw_core::is_valid_agent_id(a) && *a != duduclaw_core::UNTRUSTED_AGENT_ID)
        {
            Some(agent) => agent.to_string(),
            None => shared_internal_pool(),
        }
    } else if identity.client_is_agent && duduclaw_core::is_valid_agent_id(&principal.client_id) {
        principal.client_id.clone()
    } else {
        format!("internal/{}", principal.client_id)
    };

    Ok(NamespaceContext {
        write_namespace: own_ns.clone(),
        read_namespaces: vec![own_ns, "shared/public".to_string()],
    })
}

/// Assert that a target namespace is accessible for reading in the given context.
///
/// The target is accessible if it starts with any of the allowed read namespaces.
pub fn assert_can_access(
    ctx: &NamespaceContext,
    target_namespace: &str,
) -> Result<(), NamespaceError> {
    for allowed in &ctx.read_namespaces {
        if target_namespace == allowed || target_namespace.starts_with(&format!("{allowed}/")) {
            return Ok(());
        }
    }
    Err(NamespaceError::Forbidden {
        requested: target_namespace.to_string(),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_auth::Principal;
    use chrono::Utc;
    use std::collections::HashSet;

    fn make_principal(client_id: &str, is_external: bool) -> Principal {
        Principal {
            client_id: client_id.to_string(),
            scopes: HashSet::new(),
            is_external,
            created_at: Utc::now(),
        }
    }

    // ── Test 1: external principal write namespace ────────────────────────────
    #[test]
    fn test_external_principal_write_namespace() {
        let p = make_principal("claude-desktop", true);
        let ctx = resolve(&p).unwrap();
        assert_eq!(ctx.write_namespace, "external/claude-desktop");
    }

    // ── Test 2: external principal read contains shared/public ───────────────
    #[test]
    fn test_external_principal_read_contains_shared_public() {
        let p = make_principal("claude-desktop", true);
        let ctx = resolve(&p).unwrap();
        assert!(
            ctx.read_namespaces.contains(&"shared/public".to_string()),
            "should contain shared/public"
        );
    }

    // ── Test 3: internal principal write namespace ────────────────────────────
    #[test]
    fn test_internal_principal_write_namespace() {
        let p = make_principal("duduclaw-tl", false);
        let ctx = resolve(&p).unwrap();
        assert_eq!(ctx.write_namespace, "internal/duduclaw-tl");
    }

    // ── Test 4: external cannot read internal/* ───────────────────────────────
    #[test]
    fn test_external_cannot_read_internal_namespace() {
        let p = make_principal("claude-desktop", true);
        let ctx = resolve(&p).unwrap();
        let result = assert_can_access(&ctx, "internal/anything");
        assert!(matches!(result, Err(NamespaceError::Forbidden { .. })));
    }

    // ── Test 5: external cannot read other external client's namespace ────────
    #[test]
    fn test_external_cannot_read_other_external_client() {
        let p = make_principal("claude-desktop", true);
        let ctx = resolve(&p).unwrap();
        let result = assert_can_access(&ctx, "external/other-client");
        assert!(matches!(result, Err(NamespaceError::Forbidden { .. })));
    }

    // ── Test 6: external can read shared/public ───────────────────────────────
    #[test]
    fn test_external_can_read_shared_public() {
        let p = make_principal("claude-desktop", true);
        let ctx = resolve(&p).unwrap();
        assert!(assert_can_access(&ctx, "shared/public").is_ok());
    }

    // ── Test 7: client_id "../etc" → InvalidClientId ──────────────────────────
    #[test]
    fn test_client_id_path_traversal_rejected() {
        let p = make_principal("../etc", true);
        let result = resolve(&p);
        assert_eq!(result.unwrap_err(), NamespaceError::InvalidClientId);
    }

    // ── Test 8: client_id "a/b" → InvalidClientId ────────────────────────────
    #[test]
    fn test_client_id_slash_rejected() {
        let p = make_principal("a/b", false);
        let result = resolve(&p);
        assert_eq!(result.unwrap_err(), NamespaceError::InvalidClientId);
    }

    // ── Test 9: empty client_id → InvalidClientId ────────────────────────────
    #[test]
    fn test_client_id_empty_rejected() {
        let p = make_principal("", false);
        let result = resolve(&p);
        assert_eq!(result.unwrap_err(), NamespaceError::InvalidClientId);
    }

    // ── Test 10: valid client_id "valid-client_123" → Ok ─────────────────────
    #[test]
    fn test_valid_client_id_accepted() {
        let p = make_principal("valid-client_123", false);
        let result = resolve(&p);
        assert!(result.is_ok(), "valid client_id should succeed");
    }

    fn agent_home(agents: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for a in agents {
            let d = tmp.path().join("agents").join(a);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        }
        tmp
    }

    #[test]
    fn internal_key_with_verified_employee_maps_to_bare_id() {
        let p = make_principal(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID, false);
        let ns = resolve_for_caller(
            &p,
            CallerIdentity { verified_agent: Some("agnes"), client_is_agent: false },
        )
        .unwrap();
        assert_eq!(ns.write_namespace, "agnes");
        assert_eq!(ns.read_namespaces, vec!["agnes".to_string(), "shared/public".to_string()]);
    }

    #[test]
    fn internal_key_without_identity_keeps_the_shared_pool() {
        let p = make_principal(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID, false);
        let ns = resolve(&p).unwrap();
        assert_eq!(ns.write_namespace, "internal/gateway-internal");
        assert_eq!(shared_internal_pool(), "internal/gateway-internal");
        // The untrusted sentinel and an invalid id are never a namespace.
        for bad in [duduclaw_core::UNTRUSTED_AGENT_ID, "../etc", ""] {
            let ns = resolve_for_caller(
                &p,
                CallerIdentity { verified_agent: Some(bad), client_is_agent: false },
            )
            .unwrap();
            assert_eq!(ns.write_namespace, "internal/gateway-internal", "{bad:?}");
        }
    }

    #[test]
    fn per_agent_key_maps_to_bare_id_only_for_an_employee() {
        let home = agent_home(&["agnes"]);
        let p = make_principal("agnes", false);
        let ident = CallerIdentity {
            verified_agent: None,
            client_is_agent: client_is_agent(home.path(), "agnes"),
        };
        assert_eq!(resolve_for_caller(&p, ident).unwrap().write_namespace, "agnes");
        // A client id with no employee directory keeps its internal namespace.
        let other = make_principal("duduclaw-tl", false);
        let ident = CallerIdentity {
            verified_agent: None,
            client_is_agent: client_is_agent(home.path(), "duduclaw-tl"),
        };
        assert_eq!(resolve_for_caller(&other, ident).unwrap().write_namespace, "internal/duduclaw-tl");
        // A verified identity never redirects a non-internal key.
        let ident = CallerIdentity { verified_agent: Some("agnes"), client_is_agent: false };
        assert_eq!(resolve_for_caller(&other, ident).unwrap().write_namespace, "internal/duduclaw-tl");
    }

    #[test]
    fn external_key_is_unchanged_by_identity() {
        let p = make_principal("agnes", true);
        let ns = resolve_for_caller(
            &p,
            CallerIdentity { verified_agent: Some("agnes"), client_is_agent: true },
        )
        .unwrap();
        assert_eq!(ns.write_namespace, "external/agnes");
    }

    #[test]
    fn verified_employee_requires_a_valid_token() {
        let home = tempfile::tempdir().unwrap();
        // No identity.key: nothing can be proven.
        assert_eq!(verified_employee(home.path(), "agnes", "anything"), None);
        let key = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let token = duduclaw_core::mint_identity_token(&key, "agnes");
        assert_eq!(verified_employee(home.path(), "agnes", &token).as_deref(), Some("agnes"));
        // Wrong token, token for another id, missing token, missing id.
        assert_eq!(verified_employee(home.path(), "agnes", "deadbeef"), None);
        assert_eq!(verified_employee(home.path(), "bob", &token), None);
        assert_eq!(verified_employee(home.path(), "agnes", ""), None);
        assert_eq!(verified_employee(home.path(), "", &token), None);
    }
}
