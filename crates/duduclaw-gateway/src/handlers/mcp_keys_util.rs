//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── P0 dashboard-config helpers (CAP / CON / RED / MK / KS) ───────────────────
//
// These are module-level pure functions so the validation + TOML round-trip
// logic can be unit-tested directly without spinning up a full MethodHandler.
// They never touch the network or the filesystem; the async `handle_*` wrappers
// own the read → mutate → atomic-write + encryption side of things.

/// Known MCP scope strings — the single authority is
/// `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS`. The gateway crate cannot
/// depend on `duduclaw-cli` (the dependency runs the other way), so it can't
/// read `duduclaw-cli::mcp_auth::Scope` directly; that shared module is what
/// both crates read instead, replacing a hand-copied 10-of-22 list that had
/// silently drifted (2026-08 audit — see that module's doc comment).
pub(crate) const KNOWN_MCP_SCOPES: &[&str] = duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS;

// RM-134 (v1.82): key generation and the format check moved to the hashed
// key store (`duduclaw_auth::mcp_keys` / `duduclaw_core::mcp_key_store`).

/// Mask an MCP key for display: keep the `ddc_<env>_` prefix + first 4 hex of
/// the suffix, replace the rest with `…`. NEVER returns the full key.
pub(crate) fn mask_mcp_key(key: &str) -> String {
    // Find the second underscore (after the env segment) to keep the prefix.
    let parts: Vec<&str> = key.splitn(3, '_').collect();
    if parts.len() == 3 {
        let suffix = parts[2];
        let head: String = suffix.chars().take(4).collect();
        format!("{}_{}_{}…", parts[0], parts[1], head)
    } else {
        // Unrecognised shape — mask aggressively.
        let head: String = key.chars().take(6).collect();
        format!("{head}…")
    }
}
