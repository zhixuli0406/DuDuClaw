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

/// Resolve the namespace context for a given Principal.
///
/// Rules:
/// - is_external=true  → write = "external/{client_id}"
///                        read  = ["external/{client_id}", "shared/public"]
/// - is_external=false → write = "internal/{client_id}"
///                        read  = ["internal/{client_id}", "shared/public"]
pub fn resolve(principal: &Principal) -> Result<NamespaceContext, NamespaceError> {
    validate_client_id(&principal.client_id)?;

    let prefix = if principal.is_external {
        "external"
    } else {
        "internal"
    };
    let own_ns = format!("{prefix}/{}", principal.client_id);

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
}
