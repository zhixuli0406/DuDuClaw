//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::KNOWN_MCP_SCOPES;

/// P0-S3 (2026-08 audit): `mcp_keys.create` used to reject 12 of the 22
/// real scopes ("Unknown scope") because this list was a hand-copied
/// 10-entry duplicate that fell out of sync with
/// `duduclaw-cli::mcp_auth::Scope`. It is now a direct alias of
/// `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS`, so this test is really
/// asserting the shared list itself — but it pins the count at the call
/// site the dashboard actually validates against, catching a future
/// accidental re-introduction of a local override here.
///
/// 25 since W3-3b split `team_handoff` onto its own internal-only
/// `team:handoff` scope (2026-09-28), on top of WP-F2's `files:read`
/// (§14.2 local data-file tools) and WP-D's `db:read` (§13.7 read-only
/// SQL data sources).
#[test]
fn known_mcp_scopes_has_all_25_entries() {
    assert_eq!(KNOWN_MCP_SCOPES.len(), 25);
}

/// Spot-check a sample of the 12 scopes that were previously missing —
/// these used to make `mcp_keys.create` fail with "Unknown scope".
#[test]
fn previously_missing_scopes_are_now_known() {
    for s in [
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
    ] {
        assert!(KNOWN_MCP_SCOPES.contains(&s), "{s} should be a known scope");
    }
}
