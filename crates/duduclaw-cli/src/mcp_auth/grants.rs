//! What an EXTERNAL principal may reach (ecosystem WP3.2 C4).
//!
//! Split out of `mcp_auth.rs` on 2026-09-29 (audit O14). The grantable-scope
//! list and the `external_tool_allowed` predicate are moved verbatim. This
//! is the first gate a non-local client's call meets, not the only one: the
//! scope check and the process-agent rule (see [`external_tool_allowed`])
//! follow, and `tools/list` filters with all of them so callable ⇔
//! discoverable.

use super::{Principal, Scope, tool_requires_scope};

/// C4 (ecosystem WP3.2): scopes an OPERATOR may grant to external clients at
/// key issuance. Curated and conservative — credential-adjacent connector
/// scopes (Odoo/Google/Notion/Github), execution-class scopes (Fork/OsNative/
/// SkillExecute/Recording), the person registry (IdentityRead) and Admin are
/// deliberately absent: external surfaces never reach them regardless of what
/// a key claims.
pub const EXTERNALLY_GRANTABLE_SCOPES: &[Scope] = &[
    Scope::MemoryRead,
    Scope::MemoryWrite,
    Scope::WikiRead,
    Scope::WikiWrite,
    Scope::MessagingSend,
];

/// C4: may this EXTERNAL principal call `tool_name`?
///
/// Replaces the binary 7-tool whitelist with a scope-driven policy whose
/// zero-config default is byte-identical to the old behavior:
///   1. legacy whitelist tools → allowed (baseline unchanged), else
///   2. the tool must HAVE a scope-table entry (unscoped ⇒ Admin-class ⇒
///      never external), and
///   3. that scope must be externally grantable ([`EXTERNALLY_GRANTABLE_SCOPES`]), and
///   4. the key must carry that scope EXPLICITLY — Admin does not substitute
///      here (an external Admin key still only widens within the grantable set).
///
/// This is one of several gates an external call passes. Rule 1 lets a
/// legacy whitelist tool through without a scope, but the dispatch gate's
/// scope check then still needs that tool's scope (or `admin`), and tools
/// that act for the process's agent (`tools_list::PROCESS_AGENT_TOOLS`) are
/// refused to every external key. `tools/list` applies all three (this
/// predicate, `scope_listing_applies` / `scoped_caller_can_call` and
/// `process_agent_tool_refused`), so callable ⇔ discoverable holds for the
/// combination, not for this predicate alone.
pub fn external_tool_allowed(tool_name: &str, principal: &Principal) -> bool {
    if crate::mcp::EXTERNAL_TOOLS_WHITELIST.contains(&tool_name) {
        return true;
    }
    let Some(required) = tool_requires_scope(tool_name) else {
        return false;
    };
    EXTERNALLY_GRANTABLE_SCOPES.contains(&required) && principal.scopes.contains(&required)
}

#[cfg(test)]
mod external_scope_tests {
    use super::*;
    use crate::mcp_auth::parse_scopes;
    use std::collections::HashSet;

    fn ext(scopes: &[Scope]) -> Principal {
        Principal {
            client_id: "t".into(),
            scopes: scopes.iter().cloned().collect::<HashSet<_>>(),
            is_external: true,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn default_external_key_is_byte_identical_to_legacy_whitelist() {
        let p = ext(&[]);
        for t in crate::mcp::EXTERNAL_TOOLS_WHITELIST {
            assert!(external_tool_allowed(t, &p), "{t} must stay allowed");
        }
        // Same-family-but-off-whitelist tools stay hidden without a grant.
        assert!(!external_tool_allowed("memory_alias_add", &p));
        assert!(!external_tool_allowed("memory_fetch_batch", &p));
        // Unscoped (Admin-class) tools are never external.
        assert!(!external_tool_allowed("create_agent", &p));
    }

    #[test]
    fn explicit_grant_widens_within_family_only() {
        let p = ext(&[Scope::MemoryWrite]);
        assert!(external_tool_allowed("memory_alias_add", &p));
        assert!(external_tool_allowed("working_state_set", &p));
        // Different family still needs its own grant.
        assert!(
            !external_tool_allowed("memory_fetch_batch", &p),
            "read tier not granted"
        );
    }

    /// WP-D §13.7: all four SQL connector tools sit on the one read scope,
    /// and no external key can reach them whatever it claims.
    #[test]
    fn db_tools_require_db_read_and_are_never_external() {
        for tool in ["db_sources", "db_tables", "db_select", "db_query"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::DbRead),
                "{tool} must require db:read"
            );
            let p = ext(&[Scope::DbRead, Scope::Admin]);
            assert!(
                !external_tool_allowed(tool, &p),
                "{tool} must never be reachable by an external key"
            );
        }
        assert_eq!(Scope::DbRead.to_string(), "db:read");
        let scopes = parse_scopes("db:read").expect("db:read must parse");
        assert!(scopes.contains(&Scope::DbRead));
    }

    /// WP-F2 §14.2: the three local data-file tools sit on one read scope and
    /// are never reachable from an external key — a remote MCP client must not
    /// be able to read this host's filesystem, whatever scopes it claims.
    #[test]
    fn file_tools_require_files_read_and_are_never_external() {
        for tool in ["file_read", "csv_read", "xlsx_read"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::FilesRead),
                "{tool} must require files:read"
            );
            let p = ext(&[Scope::FilesRead, Scope::Admin]);
            assert!(
                !external_tool_allowed(tool, &p),
                "{tool} must never be reachable by an external key"
            );
        }
        assert_eq!(Scope::FilesRead.to_string(), "files:read");
        let scopes = parse_scopes("files:read").expect("files:read must parse");
        assert!(scopes.contains(&Scope::FilesRead));
        assert!(!EXTERNALLY_GRANTABLE_SCOPES.contains(&Scope::FilesRead));
    }

    /// W3-3b regression (debt #12, `review_team.md:329-336`): `team_handoff`
    /// used to sit on `Scope::MemoryWrite`, which IS externally grantable — an
    /// external MCP key holding `memory:write` could file a `TaskPacket` into
    /// the shared `<home>/team_packets/` tree. It now has its own scope that
    /// no operator can hand to an external client.
    #[test]
    fn team_handoff_is_unreachable_from_every_external_principal() {
        assert_eq!(
            tool_requires_scope("team_handoff"),
            Some(Scope::TeamHandoff),
            "team_handoff must sit on its own internal-only scope"
        );
        assert!(
            !EXTERNALLY_GRANTABLE_SCOPES.contains(&Scope::TeamHandoff),
            "team:handoff must never be operator-grantable to an external key"
        );
        // The exact principal the review called out: a `memory:write` holder.
        let memory_write = ext(&[Scope::MemoryWrite]);
        assert!(
            !external_tool_allowed("team_handoff", &memory_write),
            "a memory:write external key must not reach team_handoff"
        );
        // Nor does claiming the new scope, or Admin, help.
        for claimed in [
            vec![Scope::TeamHandoff],
            vec![Scope::Admin],
            vec![Scope::TeamHandoff, Scope::MemoryWrite, Scope::Admin],
        ] {
            let p = ext(&claimed);
            assert!(
                !external_tool_allowed("team_handoff", &p),
                "external principal with {claimed:?} must not reach team_handoff"
            );
        }
        // …while `working_state_handoff` (per-agent state only) keeps the
        // `memory:write` grant it always had — this change narrows exactly one
        // tool, not the family.
        assert_eq!(
            tool_requires_scope("working_state_handoff"),
            Some(Scope::MemoryWrite)
        );
        assert!(external_tool_allowed("working_state_handoff", &memory_write));
        // Wire string round-trips both ways.
        assert_eq!(Scope::TeamHandoff.to_string(), "team:handoff");
        assert!(
            parse_scopes("team:handoff")
                .expect("team:handoff must parse")
                .contains(&Scope::TeamHandoff)
        );
    }

    #[test]
    fn non_grantable_scopes_are_refused_even_when_the_key_claims_them() {
        // A key that somehow carries a connector scope gains nothing:
        // Odoo/Google/etc. are outside EXTERNALLY_GRANTABLE_SCOPES.
        let p = ext(&[Scope::OdooRead, Scope::GoogleRead, Scope::IdentityRead]);
        assert!(!external_tool_allowed("odoo_search", &p));
        assert!(!external_tool_allowed("identity_resolve", &p));
    }

    #[test]
    fn admin_does_not_substitute_for_explicit_grants_externally() {
        let p = ext(&[Scope::Admin]);
        // Whitelist baseline still works…
        assert!(external_tool_allowed("memory_store", &p));
        // …but Admin alone does not unlock the wider families.
        assert!(!external_tool_allowed("memory_alias_add", &p));
        assert!(!external_tool_allowed("working_state_set", &p));
    }
}
