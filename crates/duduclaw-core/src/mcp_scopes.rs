//! Canonical MCP scope wire-string list — the single source of truth shared
//! by `duduclaw-cli::mcp_auth::Scope` (the enum + `parse_scopes`) and
//! `duduclaw-gateway::handlers` (the dashboard `mcp_keys.create` validator).
//!
//! Before this module existed, the gateway kept its own hand-copied 10-scope
//! list next to a comment reading "mirrors `duduclaw-cli::mcp_auth::parse_scopes`
//! ... keep in sync" — nobody did, so `mcp_keys.create` rejected 12 of the 22
//! real scopes with "Unknown scope" and dashboard operators had to hand-edit
//! `config.toml` to grant them (2026-08 audit finding).
//!
//! `duduclaw-gateway` cannot depend on `duduclaw-cli` (the workspace
//! dependency runs the other way: `duduclaw-cli` depends on
//! `duduclaw-gateway`), so the canonical *string* list — not the `Scope` enum
//! itself, which stays in `duduclaw-cli` — lives here, in the one crate both
//! already depend on.
//!
//! Order and membership MUST stay byte-identical to `Scope`'s `Display` impl
//! in `crates/duduclaw-cli/src/mcp_auth.rs`. That file's own test suite
//! (`scope_enum_matches_canonical_list`) locks the alignment bidirectionally:
//! every enum variant's string must appear here, and every string here must
//! parse back to its variant.

/// All valid MCP scope wire strings, in the same order as the `Scope` enum
/// variants they mirror.
pub const MCP_SCOPE_STRINGS: &[&str] = &[
    "memory:read",
    "memory:write",
    "wiki:read",
    "wiki:write",
    "messaging:send",
    "messaging:read",
    "identity:read",
    "odoo:read",
    "odoo:write",
    "odoo:execute",
    "google:read",
    "google:write",
    "notion:read",
    "notion:write",
    "github:read",
    "github:write",
    "fork:execute",
    "os:native",
    "skill:execute",
    "recording",
    "mail:read",
    "mail:send",
    "db:read",
    "files:read",
    "team:handoff",
    "discovery:execute",
    "knowledge:read",
    "knowledge:write",
    "admin",
];

/// The scopes an operator may grant to an EXTERNAL MCP key — the backend's
/// single source for them (RM-157). `duduclaw-cli`'s
/// `mcp_auth::EXTERNALLY_GRANTABLE_SCOPES` is derived from this list (its
/// `externally_grantable_scopes_derive_from_core` test pins the two), the
/// dashboard's `mcp_keys.list` returns it as `effective_scopes`, and
/// `mcp_keys.create` refuses any scope outside it. Credential-adjacent
/// connector scopes, execution-class scopes, the person registry and `admin`
/// are deliberately absent: external surfaces never reach them regardless of
/// what a key claims.
pub const EXTERNALLY_GRANTABLE_SCOPE_STRINGS: [&str; 5] =
    ["memory:read", "memory:write", "wiki:read", "wiki:write", "messaging:send"];

/// The scopes the v1.76 dedicated-employee Google lane honours on an
/// EXTERNAL key whose client is an AI employee
/// (`duduclaw-cli::mcp_auth::dedicated_google_tool_allowed`): on such a key
/// they take effect although they are not externally grantable.
pub const EMPLOYEE_GOOGLE_KEY_SCOPES: [&str; 2] = ["google:read", "google:write"];

/// The scopes a dashboard-issued per-agent key (an internal key whose client
/// id is one AI employee — RFC-21's per-agent keys and the v1.76 dedicated
/// Google key) may carry: exactly the scopes the v1.81 key picker offered,
/// minus `admin`. Nothing is added; the internal-only scopes the picker never
/// offered (`messaging:read`, `team:handoff`, `discovery:execute`,
/// `knowledge:*`) stay out, as does `admin` (RM-134 H3: a per-agent key acts
/// as that employee; it never carries the all-tools scope).
pub const EMPLOYEE_KEY_SCOPE_STRINGS: [&str; 23] = [
    "memory:read",
    "memory:write",
    "wiki:read",
    "wiki:write",
    "messaging:send",
    "identity:read",
    "odoo:read",
    "odoo:write",
    "odoo:execute",
    "google:read",
    "google:write",
    "notion:read",
    "notion:write",
    "github:read",
    "github:write",
    "fork:execute",
    "os:native",
    "skill:execute",
    "recording",
    "mail:read",
    "mail:send",
    "db:read",
    "files:read",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grantable_lists_are_known_scopes() {
        for s in EXTERNALLY_GRANTABLE_SCOPE_STRINGS
            .iter()
            .chain(EMPLOYEE_GOOGLE_KEY_SCOPES.iter())
            .chain(EMPLOYEE_KEY_SCOPE_STRINGS.iter())
        {
            assert!(MCP_SCOPE_STRINGS.contains(s), "{s}");
        }
        assert!(!EXTERNALLY_GRANTABLE_SCOPE_STRINGS.contains(&"admin"));
        assert!(!EMPLOYEE_KEY_SCOPE_STRINGS.contains(&"admin"));
        for s in EXTERNALLY_GRANTABLE_SCOPE_STRINGS.iter().chain(EMPLOYEE_GOOGLE_KEY_SCOPES.iter()) {
            assert!(EMPLOYEE_KEY_SCOPE_STRINGS.contains(s), "{s}");
        }
    }

    #[test]
    fn scope_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for s in MCP_SCOPE_STRINGS {
            assert!(seen.insert(*s), "duplicate scope string: {s}");
        }
    }

    /// Pinned count. A deliberate scope addition/removal must update this
    /// alongside the bidirectional test in `duduclaw-cli/src/mcp_auth.rs` —
    /// two independent trip-wires on the same drift.
    #[test]
    fn scope_list_has_29_entries() {
        assert_eq!(MCP_SCOPE_STRINGS.len(), 29);
    }

    #[test]
    fn every_entry_is_lowercase_ascii_and_non_empty() {
        for s in MCP_SCOPE_STRINGS {
            assert!(!s.is_empty());
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase() || c == ':'),
                "unexpected char in scope string: {s}"
            );
        }
    }
}
