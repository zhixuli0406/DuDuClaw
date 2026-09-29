//! Authentication-flow regressions for [`super`].
//!
//! Split out of `mcp_auth.rs` on 2026-09-29 (audit O14). Bodies are
//! unchanged; the scope-table half lives in [`scope_table`] so neither file
//! exceeds the project's 800-line ceiling. Shared fixtures (the env lock and
//! the config-dir builders) stay here — the submodule reaches them through
//! `use super::*`.

use super::*;
use std::io::Write;
use std::sync::Mutex;
use tempfile::TempDir;

// Global mutex to serialize tests that manipulate environment variables.
// env::set_var / remove_var are inherently process-global; running them
// concurrently across threads is UB in Rust 2024.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── Helpers ──────────────────────────────────────────────────────────────

fn make_config_dir_with_key(
    key: &str,
    client_id: &str,
    scopes: &[&str],
    is_external: bool,
    created_at: &str,
) -> TempDir {
    let dir = TempDir::new().unwrap();
    let scopes_toml = scopes
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let content = format!(
        r#"
[mcp_keys."{key}"]
client_id = "{client_id}"
scopes = [{scopes_toml}]
created_at = "{created_at}"
is_external = {is_external}
"#
    );
    let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    dir
}

fn fresh_key(env_suffix: &str) -> String {
    // Generate a valid-format key with fresh created_at
    format!("ddc_{env_suffix}_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4")
}

/// Today's date in RFC-3339 form, for tests that need a fresh `created_at`.
///
/// Replaces the hardcoded `2026-04-29T00:00:00Z` string that was used
/// across these tests pre-2026-06-01 and which became a time-bomb: once
/// the wall-clock crossed 30 days past 2026-04-29, every test that
/// expected `Ok(Principal)` started failing with `KeyExpired`. Calling
/// `Utc::now()` keeps the suite robust to time.
fn fresh_today_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

// ── Test 1: valid key returns correct Principal ───────────────────────────
#[test]
fn test_valid_key_returns_principal() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(
        &key,
        "claude-desktop",
        &["memory:read", "wiki:read"],
        true,
        &today,
    );
    // SAFETY: protected by ENV_LOCK — no concurrent env mutation.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    let principal = result.expect("should authenticate successfully");
    assert_eq!(principal.client_id, "claude-desktop");
    assert!(principal.is_external);
    assert!(principal.scopes.contains(&Scope::MemoryRead));
    assert!(principal.scopes.contains(&Scope::WikiRead));
}

// ── Test 2: missing env var → MissingKey (registry has entries) ──────────
#[test]
fn test_missing_env_var_returns_missing_key() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(&key, "claude-desktop", &["memory:read"], true, &today);
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
    let result = authenticate_from_env(dir.path());
    assert_eq!(result.unwrap_err(), AuthError::MissingKey);
}

// ── Test 3: key format error (too short) → InvalidFormat ─────────────────
#[test]
fn test_invalid_format_too_short() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", "ddc_prod_tooshort") };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
    assert_eq!(result.unwrap_err(), AuthError::InvalidFormat);
}

// ── Test 4: valid format but not in registry → UnknownKey ────────────────
#[test]
fn test_unknown_key_not_in_registry() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    // Empty config (no mcp_keys section)
    std::fs::write(dir.path().join("config.toml"), "[settings]\nfoo = 1\n").unwrap();
    let key = fresh_key("prod");
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
    assert_eq!(result.unwrap_err(), AuthError::UnknownKey);
}

// ── Test 5: key older than 30 days → KeyExpired ───────────────────────────
#[test]
fn test_expired_key_31_days_old() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    // Use a date clearly more than 30 days in the past relative to any
    // reasonable "now" during CI — 2025-01-01 is well over 90 days before
    // the earliest possible test run date.
    let old_date = "2025-01-01T00:00:00Z";
    let dir =
        make_config_dir_with_key(&key, "claude-desktop", &["memory:read"], true, old_date);
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    match result.unwrap_err() {
        AuthError::KeyExpired { days_old } => {
            assert!(days_old >= 31, "expected at least 31 days, got {days_old}");
        }
        other => panic!("expected KeyExpired, got {other:?}"),
    }
}

// ── Test 6: parse_scopes happy path ───────────────────────────────────────
#[test]
fn test_parse_scopes_memory_read_wiki_write() {
    let scopes = parse_scopes("memory:read,wiki:write").expect("should parse");
    assert!(scopes.contains(&Scope::MemoryRead));
    assert!(scopes.contains(&Scope::WikiWrite));
    assert_eq!(scopes.len(), 2);
}

// ── Test 7: parse_scopes unknown scope → InvalidScope ────────────────────
#[test]
fn test_parse_scopes_unknown_returns_invalid_scope() {
    let result = parse_scopes("unknown:scope");
    assert!(matches!(result, Err(AuthError::InvalidScope(_))));
}

/// Scope vocabulary, the tool → scope table, and the cross-crate drift
/// guards that pin both against `duduclaw_core`.
mod scope_table;
