//! Request context, the API-key authentication entry point, and key rotation.
//!
//! # What this replaced (audit O14, 2026-09-29)
//!
//! This used to be a separate top-level module, `mcp_auth_strategy.rs`, built
//! as a Strategy Pattern "upgrade path to JWT / OAuth2 in P2": a
//! `dyn AuthStrategy` trait, an `ApiKeyAuthStrategy` that forwarded straight
//! to [`super::authenticate_with_key`] / [`super::authenticate_from_env`],
//! two placeholder strategies (`JwtAuthStrategy` / `OAuth2AuthStrategy`) that
//! returned `InvalidFormat` for every request, a `KeyRotationPolicy` trait
//! with exactly one implementation, and a `McpAuthMiddleware` that held one
//! boxed value of each and forwarded to both.
//!
//! Nothing in the workspace ever constructed any of it — the MCP dispatcher,
//! the HTTP server and the stdio server all call [`super::authenticate_*`]
//! directly. A speculative indirection that is never exercised is not an
//! upgrade path; it is a second description of the same behavior that can
//! drift from the first. The **behavior** it carried (credential-or-env
//! selection, the 30-day rotation window, and the registry-wide rotation
//! scan) is kept here as plain functions and constants, which is what the
//! P2 work would have wanted anyway: adding a JWT path then means adding a
//! function next to [`authenticate`], not implementing a trait for it.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::{authenticate_from_env, authenticate_with_key, AuthError, Principal};

// ── Auth context ─────────────────────────────────────────────────────────────

/// Request context for [`authenticate`].
pub struct AuthContext<'a> {
    /// Directory where `config.toml` (containing `[mcp_keys]`) is stored.
    pub config_dir: &'a Path,
    /// Raw bearer credential from the request (e.g. `Authorization: Bearer …`).
    /// Left `None` on the stdio path — the key is then read from the
    /// `DUDUCLAW_MCP_API_KEY` environment variable to preserve the existing
    /// stdio startup contract.
    pub credential: Option<&'a str>,
}

/// Human-readable identifier for the active authentication mode (logging /
/// metrics). One mode exists today; a second would add a sibling function and
/// its own name rather than a trait implementation.
pub const STRATEGY_NAME: &str = "api_key";

/// Authenticate a request and resolve its [`Principal`].
///
/// If `ctx.credential` is `Some(key)` the key is used directly (HTTP
/// transport and tests); otherwise `DUDUCLAW_MCP_API_KEY` is read from the
/// environment (stdio transport). Key expiry, constant-time comparison and
/// scope binding are all handled by [`super`].
///
/// # Errors
///
/// Returns [`AuthError::MissingKey`] (→ HTTP 401) when no credential is
/// present, [`AuthError::UnknownKey`] / [`AuthError::KeyExpired`] for
/// invalid / stale credentials, and [`AuthError::InvalidScope`] (→ HTTP 403)
/// when scope parsing fails.
pub fn authenticate(ctx: &AuthContext<'_>) -> Result<Principal, AuthError> {
    match ctx.credential {
        Some(key) => authenticate_with_key(key, ctx.config_dir),
        None => authenticate_from_env(ctx.config_dir),
    }
}

// ── Key rotation ─────────────────────────────────────────────────────────────

/// Maximum age before a key **must** be rotated (SDD §7 and
/// `decisions/tl-decision-2026-04-29-mcp-server-p0.md`).
pub const MAX_KEY_AGE_DAYS: u64 = 30;

/// Days before hard expiry at which rotation warnings begin.
pub const ROTATION_WARN_DAYS: u64 = 7;

/// Rotation status for a single credential.
#[derive(Debug, PartialEq)]
pub enum RotationStatus {
    /// Key is within valid lifetime; no action needed.
    Ok,
    /// Key is nearing expiry; rotation recommended.
    WarningSoon {
        /// Days remaining until hard expiry.
        days_remaining: u64,
    },
    /// Key has exceeded the maximum age and must be rotated immediately.
    Expired {
        /// How many days old the key is.
        days_old: u64,
    },
}

/// Evaluate the rotation status of a credential created at `created_at`.
///
/// Warning window: strictly more than `MAX_KEY_AGE_DAYS - ROTATION_WARN_DAYS`
/// days old. Example: max=30, warn=7 → days 24–30 trigger `WarningSoon`.
pub fn rotation_status(created_at: &DateTime<Utc>) -> RotationStatus {
    let age_days = Utc::now()
        .signed_duration_since(*created_at)
        .num_days()
        .max(0) as u64;

    if age_days > MAX_KEY_AGE_DAYS {
        RotationStatus::Expired { days_old: age_days }
    } else if age_days > MAX_KEY_AGE_DAYS - ROTATION_WARN_DAYS {
        let days_remaining = MAX_KEY_AGE_DAYS.saturating_sub(age_days);
        RotationStatus::WarningSoon { days_remaining }
    } else {
        RotationStatus::Ok
    }
}

/// `true` when any configured credential is within the default warning window
/// before expiry. Used by startup health checks to emit a `WARN` log before
/// hard expiry.
pub fn is_rotation_due(config_dir: &Path) -> bool {
    check_any_key_rotation_due(config_dir, ROTATION_WARN_DAYS)
}

/// Returns `true` if any `[mcp_keys]` entry in `config.toml` has a
/// `created_at` within `warn_before_days` of the hard expiry.
///
/// Takes the window explicitly so it can be unit-tested independently of
/// [`is_rotation_due`]'s default.
pub fn check_any_key_rotation_due(config_dir: &Path, warn_before_days: u64) -> bool {
    let config_path = config_dir.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let doc: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let mcp_keys = match doc.get("mcp_keys").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return false,
    };

    for (_key, val) in mcp_keys {
        let tbl = match val.as_table() {
            Some(t) => t,
            None => continue,
        };

        let created_at_str = match tbl.get("created_at").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };

        let created_at = match DateTime::parse_from_rfc3339(created_at_str) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => continue,
        };

        let age_days = Utc::now()
            .signed_duration_since(created_at)
            .num_days()
            .max(0) as u64;

        // Warn if strictly within `warn_before_days` of expiry, OR already
        // expired. E.g. max=30, warn=7: trigger when age > 23 (days 24+).
        if age_days > MAX_KEY_AGE_DAYS - warn_before_days {
            return true;
        }
    }

    false
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_config(key: &str, created_at: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let content = format!(
            r#"
[mcp_keys."{key}"]
client_id = "test-client"
scopes = ["memory:read", "wiki:read"]
created_at = "{created_at}"
is_external = true
"#
        );
        let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        dir
    }

    fn valid_key() -> &'static str {
        "ddc_prod_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"
    }

    // ── API-key authentication ───────────────────────────────────────────────

    /// TC-STRAT-01: a valid, in-date key authenticates via `ctx.credential`
    /// (no env var dependency — thread-safe).
    #[test]
    fn test_api_key_authenticates_valid_key() {
        let key = valid_key();
        let dir = make_config(key, &Utc::now().to_rfc3339());

        let ctx = AuthContext {
            config_dir: dir.path(),
            credential: Some(key), // Inject key directly — no env var needed
        };
        let principal = authenticate(&ctx).expect("valid key should authenticate");
        assert_eq!(principal.client_id, "test-client");
        assert!(principal.is_external);
    }

    /// TC-STRAT-02: a key not in the registry → `UnknownKey` (uses the
    /// `ctx.credential` path — no env var race condition).
    #[test]
    fn test_api_key_unknown_key_returns_unknown_key() {
        let dir = TempDir::new().unwrap();
        // Write empty config with no mcp_keys section
        std::fs::write(dir.path().join("config.toml"), "[settings]\nfoo = 1\n").unwrap();

        let ctx = AuthContext {
            config_dir: dir.path(),
            credential: Some(valid_key()),
        };
        assert_eq!(authenticate(&ctx).unwrap_err(), AuthError::UnknownKey);
    }

    /// TC-STRAT-02b: `credential: None` falls through to the env-var path
    /// without panicking. The outcome depends on the ambient
    /// `DUDUCLAW_MCP_API_KEY`, so this asserts liveness only — the
    /// deterministic cases are covered by the `credential: Some(..)` tests
    /// above and by `super::tests`.
    #[test]
    fn test_api_key_no_credential_falls_through_to_env() {
        let dir = make_config(valid_key(), &Utc::now().to_rfc3339());
        let ctx = AuthContext {
            config_dir: dir.path(),
            credential: None,
        };
        let _ = authenticate(&ctx); // result may vary; just ensure no panic
    }

    /// TC-STRAT-03: the observability name is stable.
    #[test]
    fn test_strategy_name_is_api_key() {
        assert_eq!(STRATEGY_NAME, "api_key");
    }

    // ── Rotation window ──────────────────────────────────────────────────────

    /// TC-ROT-01: Fresh key (today) → `RotationStatus::Ok`.
    #[test]
    fn test_rotation_fresh_key_is_ok() {
        assert_eq!(rotation_status(&Utc::now()), RotationStatus::Ok);
    }

    /// TC-ROT-02: Key at exactly 23 days → still Ok (7 days before expiry is
    /// the warning threshold).
    #[test]
    fn test_rotation_23_days_old_is_ok() {
        let created_at = Utc::now() - chrono::Duration::days(23);
        assert_eq!(rotation_status(&created_at), RotationStatus::Ok);
    }

    /// TC-ROT-03: Key at 24 days → `WarningSoon` (within the 7-day window).
    #[test]
    fn test_rotation_24_days_old_warns() {
        let created_at = Utc::now() - chrono::Duration::days(24);
        match rotation_status(&created_at) {
            RotationStatus::WarningSoon { days_remaining } => {
                assert!(
                    days_remaining <= 7,
                    "expected ≤7 days remaining, got {days_remaining}"
                );
            }
            other => panic!("expected WarningSoon, got {other:?}"),
        }
    }

    /// TC-ROT-04: Key at 31 days → `Expired`.
    #[test]
    fn test_rotation_31_days_old_expired() {
        let created_at = Utc::now() - chrono::Duration::days(31);
        match rotation_status(&created_at) {
            RotationStatus::Expired { days_old } => {
                assert!(days_old >= 31, "expected ≥31 days_old, got {days_old}");
            }
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    /// TC-ROT-05 / TC-MW-06: constants match SDD §7 (30-day max, 7-day warn).
    #[test]
    fn test_rotation_constants() {
        assert_eq!(MAX_KEY_AGE_DAYS, 30);
        assert_eq!(ROTATION_WARN_DAYS, 7);
    }

    // ── Registry-wide rotation scan ──────────────────────────────────────────

    /// TC-ROTCHECK-01: No config file → returns false (safe default).
    #[test]
    fn test_rotation_check_no_config_returns_false() {
        let dir = TempDir::new().unwrap();
        assert!(!check_any_key_rotation_due(dir.path(), 7));
    }

    /// TC-ROTCHECK-02: Fresh key → not rotation due.
    #[test]
    fn test_rotation_check_fresh_key_not_due() {
        let key = valid_key();
        let dir = make_config(key, &Utc::now().to_rfc3339());
        assert!(!check_any_key_rotation_due(dir.path(), 7));
    }

    /// TC-ROTCHECK-03: Key at 28 days (within the 7-day window) → due.
    #[test]
    fn test_rotation_check_28_day_old_key_is_due() {
        let key = valid_key();
        let dir = TempDir::new().unwrap();
        let old_date = (Utc::now() - chrono::Duration::days(28)).to_rfc3339();
        let content = format!(
            r#"
[mcp_keys."{key}"]
client_id = "aging-client"
scopes = ["memory:read"]
created_at = "{old_date}"
is_external = true
"#
        );
        std::fs::write(dir.path().join("config.toml"), &content).unwrap();
        assert!(
            check_any_key_rotation_due(dir.path(), 7),
            "28-day-old key should trigger rotation due"
        );
    }

    /// TC-MW-04: the default-window helper reports a fresh key as not due.
    #[test]
    fn test_is_rotation_due_false_for_fresh_key() {
        let key = valid_key();
        let dir = make_config(key, &Utc::now().to_rfc3339());
        assert!(!is_rotation_due(dir.path()));
    }

    /// TC-MW-05: the default-window helper reports a 25-day-old key as due
    /// (30 − 7 = 23-day threshold).
    #[test]
    fn test_is_rotation_due_true_for_25_day_old_key() {
        let key = valid_key();
        let dir = TempDir::new().unwrap();
        let old_date = (Utc::now() - chrono::Duration::days(25)).to_rfc3339();
        let content = format!(
            r#"
[mcp_keys."{key}"]
client_id = "old-client"
scopes = ["memory:read"]
created_at = "{old_date}"
is_external = true
"#
        );
        std::fs::write(dir.path().join("config.toml"), &content).unwrap();
        assert!(
            is_rotation_due(dir.path()),
            "25-day-old key should trigger rotation warning"
        );
    }
}
