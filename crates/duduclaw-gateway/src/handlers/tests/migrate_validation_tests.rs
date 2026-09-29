//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

#[test]
fn platform_allowlist_only_accepts_known_platforms() {
    assert!(migrate_platform_allowed("openclaw"));
    assert!(migrate_platform_allowed("hermes"));
    assert!(migrate_platform_allowed("paperclip"));
    assert!(migrate_platform_allowed("claude-code"));
    // aliases / unknowns are rejected at the RPC boundary (fail-closed)
    assert!(!migrate_platform_allowed("moltbot"));
    assert!(!migrate_platform_allowed("openai"));
    assert!(!migrate_platform_allowed(""));
    assert!(!migrate_platform_allowed("OpenClaw")); // case-sensitive
    assert!(!migrate_platform_allowed("claudecode")); // alias handled CLI-side only
}

#[test]
fn source_none_is_allowed() {
    assert!(validate_migrate_source(None).is_ok());
}

#[test]
fn source_absolute_is_allowed() {
    // `is_absolute` is platform-semantic: on Windows a path without a drive
    // prefix (like `/Users/x`) is NOT absolute, so probe the native form.
    #[cfg(windows)]
    assert!(validate_migrate_source(Some("C:\\Users\\x\\.openclaw")).is_ok());
    #[cfg(not(windows))]
    assert!(validate_migrate_source(Some("/Users/x/.openclaw")).is_ok());
}

#[test]
fn source_relative_is_rejected() {
    let err = validate_migrate_source(Some("relative/path")).unwrap_err();
    assert!(err.contains("absolute"));
    assert!(validate_migrate_source(Some("./x")).is_err());
    assert!(validate_migrate_source(Some("../x")).is_err());
}

#[test]
fn source_empty_is_rejected() {
    assert!(validate_migrate_source(Some("")).is_err());
    assert!(validate_migrate_source(Some("   ")).is_err());
}
