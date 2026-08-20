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
    "admin",
];

#[cfg(test)]
mod tests {
    use super::*;

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
    fn scope_list_has_22_entries() {
        assert_eq!(MCP_SCOPE_STRINGS.len(), 22);
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
