//! Shared input validation for values that flow into SQL, log lines, and
//! URL path segments (`device_id`).
//!
//! `validate_device_id` (and its length constants) now live in
//! `duduclaw_core::relay_protocol` (WP-E2) — the box-side relay client must
//! derive a `device_id` that satisfies the exact same rule, so there is one
//! definition, not two that could drift. Re-exported here so every existing
//! `crate::validate::*` reference in this crate keeps compiling unchanged.
//! `sanitize_device_name` stays local: it is a relay-only display concern
//! (rendered on the `/v1/find` HTML page), not part of the wire protocol.

pub use duduclaw_core::relay_protocol::{validate_device_id, MAX_DEVICE_ID_LEN, MIN_DEVICE_ID_LEN};

pub const MAX_DEVICE_NAME_CHARS: usize = 80;

/// Trim and cap a device display name. Never fails — an over-long or
/// oddly-formed name is just truncated, never rejected, since it is purely
/// cosmetic (shown only on the `/v1/find` page, HTML-escaped there).
pub fn sanitize_device_name(name: &str) -> String {
    duduclaw_core::truncate_chars(name.trim(), MAX_DEVICE_NAME_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        assert!(validate_device_id("box-alpha-01").is_ok());
        assert!(validate_device_id("a_b_c_1234").is_ok());
    }

    #[test]
    fn rejects_too_short() {
        assert!(validate_device_id("ab").is_err());
    }

    #[test]
    fn rejects_too_long() {
        assert!(validate_device_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn rejects_uppercase_and_symbols() {
        assert!(validate_device_id("Box-Alpha").is_err());
        assert!(validate_device_id("box alpha").is_err());
        assert!(validate_device_id("box/alpha").is_err());
        assert!(validate_device_id("../etc/passwd").is_err());
    }

    #[test]
    fn sanitize_truncates_and_trims() {
        let long = "學".repeat(200);
        let out = sanitize_device_name(&format!("  {long}  "));
        assert_eq!(out.chars().count(), MAX_DEVICE_NAME_CHARS);
    }
}
