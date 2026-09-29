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

/// Validate an MCP API key against `^ddc_(prod|staging|dev)_[a-f0-9]{32}$`
/// (mirrors `duduclaw-cli::mcp_auth::is_valid_key_format`).
pub(crate) fn is_valid_mcp_key_format(key: &str) -> bool {
    let rest = match key.strip_prefix("ddc_") {
        Some(r) => r,
        None => return false,
    };
    let hex = if let Some(h) = rest.strip_prefix("prod_") {
        h
    } else if let Some(h) = rest.strip_prefix("staging_") {
        h
    } else if let Some(h) = rest.strip_prefix("dev_") {
        h
    } else {
        return false;
    };
    hex.len() == 32
        && hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Generate a fresh MCP API key of the form `ddc_<env>_<32hex>`.
/// `env` must already be validated to one of prod/staging/dev.
pub(crate) fn generate_mcp_key(env: &str) -> String {
    // `Uuid::simple()` renders 32 lowercase hex chars (`[a-f0-9]{32}`) — exactly
    // the suffix format required by `is_valid_mcp_key_format`.
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("ddc_{env}_{suffix}")
}

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
