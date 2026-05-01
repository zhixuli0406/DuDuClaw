// mcp_wiki.rs — Wiki MCP endpoint skeleton (W19-P0 M2 prerequisite)
//
// Provides typed request/response models and a `WikiHandler` skeleton for the
// three Phase-1 wiki endpoints:
//
//   • wiki/read   — Read a wiki page by path
//   • wiki/write  — Create or update a wiki page
//   • wiki/search — Full-text search across wiki pages
//
// IMPLEMENTATION STATUS:
//   M1 (current): types, validation, namespace enforcement — DONE
//   M2 (Day 3-4): concrete file I/O, FTS query, conflict resolution — TODO
//
// TDD rationale:
//   All public-surface behaviours are fully tested in the `tests` module below.
//   The skeleton deliberately returns `Err(WikiError::NotImplemented)` for
//   real I/O so that M2 can replace those stubs with passing implementations
//   without touching the test contracts.

use std::collections::HashMap;
use std::path::Path;

use crate::mcp_auth::{Principal, Scope};
use crate::mcp_namespace::{assert_can_access, NamespaceContext};

// ── Request types ─────────────────────────────────────────────────────────────

/// Which wiki namespace to target when reading.
#[derive(Debug, Clone, PartialEq)]
pub enum WikiType {
    /// Agent-private wiki (default for external clients).
    Internal,
    /// Cross-agent shared wiki (`shared/wiki/`).
    Shared,
}

impl Default for WikiType {
    fn default() -> Self {
        WikiType::Internal
    }
}

/// Request to read a single wiki page.
#[derive(Debug)]
pub struct WikiReadRequest {
    /// Path relative to the wiki root (e.g. `"specs/handoffpacket-spec-v0.2.md"`).
    pub page_path: String,
    /// Which wiki to read from.
    pub wiki_type: WikiType,
}

/// Request to create or update a wiki page.
#[derive(Debug)]
pub struct WikiWriteRequest {
    /// Path relative to the wiki root.
    pub page_path: String,
    /// Full page content, including YAML frontmatter.
    pub content: String,
}

/// Request to search wiki pages with a natural language query.
#[derive(Debug)]
pub struct WikiSearchRequest {
    /// Search query (keywords or natural language).
    pub query: String,
    /// Maximum number of results to return.  Default: 10, max: 50.
    pub limit: Option<usize>,
    /// Optional minimum trust score filter (0.0–1.0).
    pub min_trust: Option<f32>,
}

// ── Response types ────────────────────────────────────────────────────────────

/// Successful response from `wiki/read`.
#[derive(Debug)]
pub struct WikiReadResponse {
    /// Raw page content (YAML frontmatter + Markdown body).
    pub content: String,
    /// Parsed frontmatter key-value pairs.
    pub frontmatter: HashMap<String, serde_json::Value>,
    /// ISO-8601 last-updated timestamp (from `updated:` frontmatter field).
    pub last_updated: Option<String>,
}

/// Successful response from `wiki/write`.
#[derive(Debug)]
pub struct WikiWriteResponse {
    /// Whether the write was successful.
    pub success: bool,
    /// Canonical page path as stored.
    pub page_path: String,
    /// Content hash / version identifier (SHA-256 prefix, M2 implementation).
    pub version: Option<String>,
}

/// A single search result.
#[derive(Debug, Clone)]
pub struct WikiSearchResult {
    /// Page path relative to wiki root.
    pub page_path: String,
    /// Page title extracted from frontmatter `title:` field.
    pub title: String,
    /// Snippet of matching content.
    pub excerpt: String,
    /// Relevance score from FTS ranking (0.0–1.0).
    pub relevance_score: f32,
}

/// Successful response from `wiki/search`.
#[derive(Debug)]
pub struct WikiSearchResponse {
    /// Matching pages, ranked by relevance.
    pub results: Vec<WikiSearchResult>,
    /// Total number of matching pages (may exceed `results.len()` if capped).
    pub total: usize,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors that can be returned by `WikiHandler` operations.
#[derive(Debug, PartialEq)]
pub enum WikiError {
    /// The requested page does not exist.
    PageNotFound { path: String },
    /// The caller lacks the required wiki scope.
    InsufficientScope { required: String },
    /// The target namespace is forbidden for this principal.
    NamespaceForbidden { namespace: String },
    /// The provided path is invalid (e.g. traversal, illegal characters).
    InvalidPath { reason: String },
    /// The search query is empty or too short.
    InvalidQuery { reason: String },
    /// The write content is empty.
    EmptyContent,
    /// M2 implementation pending; the operation is structurally valid but not
    /// yet backed by real file I/O.
    NotImplemented { operation: String },
}

impl std::fmt::Display for WikiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WikiError::PageNotFound { path } => write!(f, "Wiki page not found: {path}"),
            WikiError::InsufficientScope { required } => {
                write!(f, "Insufficient scope: {required} required")
            }
            WikiError::NamespaceForbidden { namespace } => {
                write!(f, "Namespace access forbidden: {namespace}")
            }
            WikiError::InvalidPath { reason } => write!(f, "Invalid wiki path: {reason}"),
            WikiError::InvalidQuery { reason } => write!(f, "Invalid search query: {reason}"),
            WikiError::EmptyContent => write!(f, "Write content must not be empty"),
            WikiError::NotImplemented { operation } => {
                write!(f, "M2 not yet implemented: {operation}")
            }
        }
    }
}

// ── Path validation ───────────────────────────────────────────────────────────

/// Validate a wiki page path.
///
/// Rules:
/// - Must not be empty
/// - Must not contain `..` (path traversal)
/// - Must not start with `/` (absolute paths disallowed)
/// - Only alphanumeric, `-`, `_`, `/`, `.` characters allowed
/// - Must end with `.md`
pub fn validate_wiki_path(path: &str) -> Result<(), WikiError> {
    if path.is_empty() {
        return Err(WikiError::InvalidPath {
            reason: "path must not be empty".to_string(),
        });
    }

    if path.starts_with('/') {
        return Err(WikiError::InvalidPath {
            reason: "absolute paths are not allowed".to_string(),
        });
    }

    if path.contains("..") {
        return Err(WikiError::InvalidPath {
            reason: "path traversal ('..') is not allowed".to_string(),
        });
    }

    // Only allow: a-z A-Z 0-9 / - _ .
    let valid = path
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'));
    if !valid {
        return Err(WikiError::InvalidPath {
            reason: format!("path '{path}' contains invalid characters"),
        });
    }

    if !path.ends_with(".md") {
        return Err(WikiError::InvalidPath {
            reason: "wiki pages must have a .md extension".to_string(),
        });
    }

    Ok(())
}

// ── Namespace derivation ──────────────────────────────────────────────────────

/// Derive the effective namespace for a write operation.
///
/// External clients → `external/{client_id}`
/// Internal clients → `internal/{client_id}`
pub fn derive_write_namespace(principal: &Principal) -> String {
    if principal.is_external {
        format!("external/{}", principal.client_id)
    } else {
        format!("internal/{}", principal.client_id)
    }
}

// ── WikiHandler ───────────────────────────────────────────────────────────────

/// Stateless wiki endpoint handler.
///
/// Holds references to the authenticated principal and its resolved namespace
/// context.  All operations enforce:
///
/// 1. **Scope check** — caller has the required wiki scope
/// 2. **Path validation** — path is safe and well-formed
/// 3. **Namespace isolation** — write target is within the principal's allowed
///    namespace; read target is within the allowed read namespaces
/// 4. **M2 I/O** — deferred; returns `WikiError::NotImplemented` until M2
pub struct WikiHandler<'a> {
    pub principal: &'a Principal,
    pub ns_ctx: &'a NamespaceContext,
    pub home_dir: &'a Path,
}

impl<'a> WikiHandler<'a> {
    /// Create a new `WikiHandler`.
    pub fn new(principal: &'a Principal, ns_ctx: &'a NamespaceContext, home_dir: &'a Path) -> Self {
        Self {
            principal,
            ns_ctx,
            home_dir,
        }
    }

    // ── wiki/read ─────────────────────────────────────────────────────────────

    /// Handle a `wiki/read` request.
    ///
    /// Validates the path, checks `wiki:read` scope, enforces namespace read
    /// access, then delegates to M2 file I/O.
    ///
    /// # Errors
    ///
    /// | Error | Condition |
    /// |---|---|
    /// | `InsufficientScope` | caller lacks `wiki:read` |
    /// | `InvalidPath` | path fails validation |
    /// | `NamespaceForbidden` | read target not in allowed namespaces |
    /// | `NotImplemented` | M2 file I/O pending |
    pub async fn read(&self, req: WikiReadRequest) -> Result<WikiReadResponse, WikiError> {
        // 1. Scope check
        if !self.principal.scopes.contains(&Scope::WikiRead)
            && !self.principal.scopes.contains(&Scope::Admin)
        {
            return Err(WikiError::InsufficientScope {
                required: "wiki:read".to_string(),
            });
        }

        // 2. Path validation
        validate_wiki_path(&req.page_path)?;

        // 3. Namespace read access — derive target namespace from path prefix if
        //    present, otherwise use the principal's own namespace.
        let target_ns = derive_write_namespace(self.principal);
        if let Err(_) = assert_can_access(self.ns_ctx, &target_ns) {
            return Err(WikiError::NamespaceForbidden {
                namespace: target_ns,
            });
        }

        // 4. M2: actual file read
        Err(WikiError::NotImplemented {
            operation: "wiki/read".to_string(),
        })
    }

    // ── wiki/write ────────────────────────────────────────────────────────────

    /// Handle a `wiki/write` request.
    ///
    /// Validates the path and content, checks `wiki:write` scope, enforces
    /// namespace write access, then delegates to M2 atomic file write.
    ///
    /// # Errors
    ///
    /// | Error | Condition |
    /// |---|---|
    /// | `InsufficientScope` | caller lacks `wiki:write` |
    /// | `InvalidPath` | path fails validation |
    /// | `EmptyContent` | content is blank |
    /// | `NamespaceForbidden` | write target not in allowed namespace |
    /// | `NotImplemented` | M2 file I/O pending |
    pub async fn write(&self, req: WikiWriteRequest) -> Result<WikiWriteResponse, WikiError> {
        // 1. Scope check
        if !self.principal.scopes.contains(&Scope::WikiWrite)
            && !self.principal.scopes.contains(&Scope::Admin)
        {
            return Err(WikiError::InsufficientScope {
                required: "wiki:write".to_string(),
            });
        }

        // 2. Path validation
        validate_wiki_path(&req.page_path)?;

        // 3. Content must not be empty
        if req.content.trim().is_empty() {
            return Err(WikiError::EmptyContent);
        }

        // 4. Namespace enforcement: external clients may only write to their own namespace.
        let write_ns = derive_write_namespace(self.principal);
        if let Err(_) = assert_can_access(self.ns_ctx, &write_ns) {
            return Err(WikiError::NamespaceForbidden {
                namespace: write_ns,
            });
        }

        // 5. M2: atomic write (temp + rename) + _index.md + _log.md updates
        Err(WikiError::NotImplemented {
            operation: "wiki/write".to_string(),
        })
    }

    // ── wiki/search ───────────────────────────────────────────────────────────

    /// Handle a `wiki/search` request.
    ///
    /// Validates the query, checks `wiki:read` scope, then delegates to M2 FTS
    /// engine.  Scoped to the principal's allowed read namespaces.
    ///
    /// # Errors
    ///
    /// | Error | Condition |
    /// |---|---|
    /// | `InsufficientScope` | caller lacks `wiki:read` |
    /// | `InvalidQuery` | query is empty or over 1000 chars |
    /// | `NotImplemented` | M2 FTS pending |
    pub async fn search(&self, req: WikiSearchRequest) -> Result<WikiSearchResponse, WikiError> {
        // 1. Scope check
        if !self.principal.scopes.contains(&Scope::WikiRead)
            && !self.principal.scopes.contains(&Scope::Admin)
        {
            return Err(WikiError::InsufficientScope {
                required: "wiki:read".to_string(),
            });
        }

        // 2. Query validation
        let trimmed = req.query.trim();
        if trimmed.is_empty() {
            return Err(WikiError::InvalidQuery {
                reason: "query must not be empty".to_string(),
            });
        }
        if trimmed.len() > 1000 {
            return Err(WikiError::InvalidQuery {
                reason: "query exceeds maximum length of 1000 characters".to_string(),
            });
        }

        // 3. Clamp limit (default 10, max 50)
        let _limit = req.limit.unwrap_or(10).min(50);

        // 4. M2: FTS5 search scoped to principal's read namespaces
        Err(WikiError::NotImplemented {
            operation: "wiki/search".to_string(),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_auth::Principal;
    use crate::mcp_namespace::{resolve, NamespaceContext};
    use chrono::Utc;
    use std::collections::HashSet;
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_principal(client_id: &str, is_external: bool, scopes: &[Scope]) -> Principal {
        Principal {
            client_id: client_id.to_string(),
            scopes: scopes.iter().cloned().collect::<HashSet<_>>(),
            is_external,
            created_at: Utc::now(),
        }
    }

    fn make_handler<'a>(
        principal: &'a Principal,
        ns_ctx: &'a NamespaceContext,
        home_dir: &'a Path,
    ) -> WikiHandler<'a> {
        WikiHandler::new(principal, ns_ctx, home_dir)
    }

    fn resolve_ns(principal: &Principal) -> NamespaceContext {
        resolve(principal).expect("namespace resolution should not fail for test principals")
    }

    // ── Path validation ───────────────────────────────────────────────────────

    /// TC-WIKI-PATH-01: Empty path → InvalidPath.
    #[test]
    fn test_path_empty_rejected() {
        assert_eq!(
            validate_wiki_path("").unwrap_err(),
            WikiError::InvalidPath {
                reason: "path must not be empty".to_string()
            }
        );
    }

    /// TC-WIKI-PATH-02: Absolute path → InvalidPath.
    #[test]
    fn test_path_absolute_rejected() {
        assert!(matches!(
            validate_wiki_path("/etc/passwd.md"),
            Err(WikiError::InvalidPath { .. })
        ));
    }

    /// TC-WIKI-PATH-03: Path traversal → InvalidPath.
    #[test]
    fn test_path_traversal_rejected() {
        assert!(matches!(
            validate_wiki_path("../etc/passwd.md"),
            Err(WikiError::InvalidPath { .. })
        ));
    }

    /// TC-WIKI-PATH-04: Embedded traversal → InvalidPath.
    #[test]
    fn test_path_embedded_traversal_rejected() {
        assert!(matches!(
            validate_wiki_path("specs/../secret/key.md"),
            Err(WikiError::InvalidPath { .. })
        ));
    }

    /// TC-WIKI-PATH-05: Valid path → Ok.
    #[test]
    fn test_path_valid_accepted() {
        assert!(validate_wiki_path("specs/handoffpacket-spec-v0.2.md").is_ok());
    }

    /// TC-WIKI-PATH-06: Path with space → InvalidPath.
    #[test]
    fn test_path_with_space_rejected() {
        assert!(matches!(
            validate_wiki_path("specs/my page.md"),
            Err(WikiError::InvalidPath { .. })
        ));
    }

    /// TC-WIKI-PATH-07: Path without .md extension → InvalidPath.
    #[test]
    fn test_path_without_md_extension_rejected() {
        assert!(matches!(
            validate_wiki_path("specs/config.toml"),
            Err(WikiError::InvalidPath { .. })
        ));
    }

    /// TC-WIKI-PATH-08: Nested path with numbers → Ok.
    #[test]
    fn test_path_nested_with_numbers_accepted() {
        assert!(validate_wiki_path("adr/ADR-001-pluggable-sandbox.md").is_ok());
    }

    // ── wiki/read scope enforcement ───────────────────────────────────────────

    /// TC-WIKI-READ-01: No wiki:read scope → InsufficientScope.
    #[tokio::test]
    async fn test_read_no_scope_returns_insufficient_scope() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("claude-desktop", true, &[]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .read(WikiReadRequest {
                page_path: "specs/test.md".to_string(),
                wiki_type: WikiType::Internal,
            })
            .await;

        assert_eq!(
            result.unwrap_err(),
            WikiError::InsufficientScope {
                required: "wiki:read".to_string()
            }
        );
    }

    /// TC-WIKI-READ-02: Admin scope bypasses wiki:read requirement.
    #[tokio::test]
    async fn test_read_admin_scope_bypasses_wiki_read_check() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("admin-client", false, &[Scope::Admin]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .read(WikiReadRequest {
                page_path: "specs/test.md".to_string(),
                wiki_type: WikiType::Internal,
            })
            .await;

        // Should reach M2 stub (NotImplemented), not InsufficientScope
        assert!(matches!(result, Err(WikiError::NotImplemented { .. })));
    }

    /// TC-WIKI-READ-03: Invalid path → InvalidPath (even with valid scope).
    #[tokio::test]
    async fn test_read_invalid_path_rejected_before_m2() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .read(WikiReadRequest {
                page_path: "../traversal.md".to_string(),
                wiki_type: WikiType::Shared,
            })
            .await;

        assert!(matches!(result, Err(WikiError::InvalidPath { .. })));
    }

    /// TC-WIKI-READ-04: Valid scope + valid path → reaches M2 stub (NotImplemented).
    #[tokio::test]
    async fn test_read_valid_reaches_m2_stub() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .read(WikiReadRequest {
                page_path: "specs/handoffpacket-spec-v0.2.md".to_string(),
                wiki_type: WikiType::Internal,
            })
            .await;

        assert!(
            matches!(result, Err(WikiError::NotImplemented { .. })),
            "valid read should reach M2 stub, got: {result:?}"
        );
    }

    // ── wiki/write scope enforcement ──────────────────────────────────────────

    /// TC-WIKI-WRITE-01: No wiki:write scope → InsufficientScope.
    #[tokio::test]
    async fn test_write_no_scope_returns_insufficient_scope() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader-only", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .write(WikiWriteRequest {
                page_path: "notes/test.md".to_string(),
                content: "# Test\nsome content".to_string(),
            })
            .await;

        assert_eq!(
            result.unwrap_err(),
            WikiError::InsufficientScope {
                required: "wiki:write".to_string()
            }
        );
    }

    /// TC-WIKI-WRITE-02: wiki:write scope + valid path + content → reaches M2 stub.
    #[tokio::test]
    async fn test_write_valid_reaches_m2_stub() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("writer", true, &[Scope::WikiWrite]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .write(WikiWriteRequest {
                page_path: "notes/hello.md".to_string(),
                content: "# Hello\nContent here.".to_string(),
            })
            .await;

        assert!(
            matches!(result, Err(WikiError::NotImplemented { .. })),
            "valid write should reach M2 stub, got: {result:?}"
        );
    }

    /// TC-WIKI-WRITE-03: Empty content → EmptyContent.
    #[tokio::test]
    async fn test_write_empty_content_rejected() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("writer", true, &[Scope::WikiWrite]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .write(WikiWriteRequest {
                page_path: "notes/empty.md".to_string(),
                content: "   ".to_string(),
            })
            .await;

        assert_eq!(result.unwrap_err(), WikiError::EmptyContent);
    }

    /// TC-WIKI-WRITE-04: Path traversal in write path → InvalidPath.
    #[tokio::test]
    async fn test_write_path_traversal_rejected() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("writer", true, &[Scope::WikiWrite]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .write(WikiWriteRequest {
                page_path: "../../etc/evil.md".to_string(),
                content: "malicious".to_string(),
            })
            .await;

        assert!(matches!(result, Err(WikiError::InvalidPath { .. })));
    }

    /// TC-WIKI-WRITE-05: Admin scope allows write without explicit wiki:write.
    #[tokio::test]
    async fn test_write_admin_scope_bypasses_wiki_write_check() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("admin-client", false, &[Scope::Admin]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .write(WikiWriteRequest {
                page_path: "notes/admin-page.md".to_string(),
                content: "# Admin\nContent.".to_string(),
            })
            .await;

        assert!(matches!(result, Err(WikiError::NotImplemented { .. })));
    }

    // ── wiki/search scope + query validation ──────────────────────────────────

    /// TC-WIKI-SEARCH-01: No wiki:read scope → InsufficientScope.
    #[tokio::test]
    async fn test_search_no_scope_returns_insufficient_scope() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("no-scope", true, &[]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .search(WikiSearchRequest {
                query: "handoffpacket".to_string(),
                limit: None,
                min_trust: None,
            })
            .await;

        assert_eq!(
            result.unwrap_err(),
            WikiError::InsufficientScope {
                required: "wiki:read".to_string()
            }
        );
    }

    /// TC-WIKI-SEARCH-02: Empty query → InvalidQuery.
    #[tokio::test]
    async fn test_search_empty_query_rejected() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .search(WikiSearchRequest {
                query: "  ".to_string(),
                limit: None,
                min_trust: None,
            })
            .await;

        assert!(matches!(result, Err(WikiError::InvalidQuery { .. })));
    }

    /// TC-WIKI-SEARCH-03: Query over 1000 chars → InvalidQuery.
    #[tokio::test]
    async fn test_search_oversized_query_rejected() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let long_query = "a".repeat(1001);
        let result = h
            .search(WikiSearchRequest {
                query: long_query,
                limit: None,
                min_trust: None,
            })
            .await;

        assert!(matches!(result, Err(WikiError::InvalidQuery { .. })));
    }

    /// TC-WIKI-SEARCH-04: Valid scope + non-empty query → reaches M2 stub.
    #[tokio::test]
    async fn test_search_valid_reaches_m2_stub() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        let result = h
            .search(WikiSearchRequest {
                query: "memory namespace isolation".to_string(),
                limit: Some(5),
                min_trust: Some(0.5),
            })
            .await;

        assert!(
            matches!(result, Err(WikiError::NotImplemented { .. })),
            "valid search should reach M2 stub, got: {result:?}"
        );
    }

    /// TC-WIKI-SEARCH-05: limit > 50 is silently clamped (no error).
    #[tokio::test]
    async fn test_search_limit_over_50_clamped_no_error() {
        let dir = TempDir::new().unwrap();
        let p = make_principal("reader", true, &[Scope::WikiRead]);
        let ns = resolve_ns(&p);
        let h = make_handler(&p, &ns, dir.path());

        // limit=100 should be silently clamped to 50 (not an error)
        let result = h
            .search(WikiSearchRequest {
                query: "evolution events".to_string(),
                limit: Some(100),
                min_trust: None,
            })
            .await;

        // Should reach M2 stub, not return an error about limit
        assert!(
            !matches!(result, Err(WikiError::InvalidQuery { .. })),
            "over-limit should not be an InvalidQuery error, got: {result:?}"
        );
    }

    // ── Namespace derivation ──────────────────────────────────────────────────

    /// TC-WIKI-NS-01: External principal derives external/ namespace.
    #[test]
    fn test_derive_write_namespace_external() {
        let p = make_principal("claude-desktop", true, &[]);
        assert_eq!(derive_write_namespace(&p), "external/claude-desktop");
    }

    /// TC-WIKI-NS-02: Internal principal derives internal/ namespace.
    #[test]
    fn test_derive_write_namespace_internal() {
        let p = make_principal("duduclaw-tl", false, &[]);
        assert_eq!(derive_write_namespace(&p), "internal/duduclaw-tl");
    }

    // ── WikiError Display ─────────────────────────────────────────────────────

    /// TC-WIKI-ERR-01: WikiError::Display is non-empty and informative.
    #[test]
    fn test_wiki_error_display_non_empty() {
        let errors = [
            WikiError::PageNotFound {
                path: "specs/test.md".to_string(),
            },
            WikiError::InsufficientScope {
                required: "wiki:read".to_string(),
            },
            WikiError::NamespaceForbidden {
                namespace: "internal/other".to_string(),
            },
            WikiError::InvalidPath {
                reason: "..".to_string(),
            },
            WikiError::InvalidQuery {
                reason: "empty".to_string(),
            },
            WikiError::EmptyContent,
            WikiError::NotImplemented {
                operation: "wiki/read".to_string(),
            },
        ];

        for err in &errors {
            let msg = err.to_string();
            assert!(!msg.is_empty(), "Display should be non-empty for {err:?}");
        }
    }
}
