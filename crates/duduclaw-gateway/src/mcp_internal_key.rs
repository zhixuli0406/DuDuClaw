//! Startup provisioning of the gateway-internal MCP API key.
//!
//! Since the M6 fail-closed change (v1.31), `duduclaw mcp-server` refuses to
//! start unless `DUDUCLAW_MCP_API_KEY` carries a key registered in
//! `config.toml [mcp_keys]`. Nothing ever provisioned that key for the
//! gateway's own CLI children, so every runtime whose CLI spawns MCP servers
//! with a sanitized env (Grok, and any setup where the gateway env itself
//! lacks the key) silently lost the whole duduclaw tool surface.
//!
//! This module closes the loop: at gateway startup, ensure ONE *currently
//! valid* internal key (client_id = `gateway-internal`, scope `admin`,
//! `is_external = false`) exists in `[mcp_keys]` and return its cleartext so
//! the caller can export it into the gateway process env — from where
//! `mcp_forward_env_vars()` carries it into every MCP env assembly point.
//!
//! Rotation (2026-09 production defect): the MCP authenticator hard-expires
//! any key older than [`HARD_EXPIRY_DAYS`], but this module used to reuse the
//! existing `gateway-internal` entry forever. Exactly 30 days after first
//! boot, every CLI-spawned `duduclaw mcp-server` started failing auth and
//! agents silently lost their entire tool surface again — the same outage M6
//! provisioning was written to prevent. Provisioning now checks the entry's
//! age and mints a replacement at [`ROTATE_AFTER_DAYS`]; the boot fixup
//! `duduclaw_agent::mcp_template::ensure_mcp_absolute_paths_all` rewrites the
//! forward-set env of every agent's `.mcp.json`, so a key rotated at boot
//! propagates to the CLI children on that same boot.
//!
//! Security posture: this restores the pre-M6 *local* convenience (any
//! gateway-spawned child can reach the MCP server) but keeps M6's actual
//! goal — an EXTERNAL/stdio caller without the key still gets denied, and the
//! key is per-install (never leaves `config.toml` + local process envs).
//! Concurrency: writes hold `duduclaw_core::with_file_lock` on `config.toml`
//! (multi-instance gateways race on first boot otherwise).

use chrono::{DateTime, Utc};
use std::path::Path;
use tracing::info;

/// Reserved `client_id` marking the auto-provisioned internal key.
pub const INTERNAL_CLIENT_ID: &str = "gateway-internal";

/// Hard expiry enforced by the MCP server's own authenticator — see
/// `duduclaw-cli/src/mcp_auth.rs::authenticate_against_registry`, which
/// returns `AuthError::KeyExpired` for `days_old > 30`. Mirrored as a local
/// constant because `duduclaw-gateway` does not depend on `duduclaw-cli`
/// (the dependency runs the other way); `rotate_margin_leaves_headroom`
/// below ties the two numbers together so drift shows up as a test failure.
pub const HARD_EXPIRY_DAYS: i64 = 30;

/// Rotate the internal key once it reaches this age, leaving 5 days of margin
/// before [`HARD_EXPIRY_DAYS`] so a gateway that reboots only occasionally
/// still hands its children a key that authenticates.
pub const ROTATE_AFTER_DAYS: i64 = 25;

/// Whole-day age of `created_at` as of `now`, clamped at 0.
///
/// The clamp mirrors the authenticator's L12 fix: a future-dated `created_at`
/// (clock skew, mis-set system time) yields a negative duration there and is
/// treated as age 0 rather than an absurd expiry, so rotation must agree —
/// otherwise the gateway would mint a fresh key on every single boot.
fn age_in_days(created_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(created_at).num_days().max(0)
}

/// Mint a fresh internal key. `Uuid::simple()` renders 32 lowercase hex
/// chars — the exact suffix `is_valid_key_format` requires.
fn mint_key() -> String {
    format!("ddc_prod_{}", uuid::Uuid::new_v4().simple())
}

/// The `[mcp_keys.<key>]` table body for a freshly minted internal key.
fn internal_key_entry(now: DateTime<Utc>) -> toml::Value {
    let mut entry = toml::map::Map::new();
    entry.insert(
        "client_id".into(),
        toml::Value::String(INTERNAL_CLIENT_ID.into()),
    );
    entry.insert("is_external".into(), toml::Value::Boolean(false));
    entry.insert("created_at".into(), toml::Value::String(now.to_rfc3339()));
    entry.insert(
        "scopes".into(),
        toml::Value::Array(vec![toml::Value::String("admin".into())]),
    );
    toml::Value::Table(entry)
}

/// Ensure a *valid* internal MCP key exists in `<home>/config.toml
/// [mcp_keys]`, creating it on first boot and rotating it before the
/// authenticator's hard expiry. Returns the cleartext key.
///
/// Rules (all scoped to `client_id = "gateway-internal"` — foreign keys are
/// never read for rotation and never modified):
/// - Reuse the newest entry whose age is `< ROTATE_AFTER_DAYS` (25 d).
/// - Otherwise mint a new key in the same format and insert it.
/// - Prune internal entries that provably cannot authenticate: age
///   `> HARD_EXPIRY_DAYS` (30 d), or a missing/unparseable `created_at`
///   (`load_key_registry_checked` skips such rows outright).
/// - Keep younger extras (e.g. the 26-day-old key we just rotated away from)
///   so an in-flight MCP child spawned with the previous key keeps working
///   until its own process ends.
/// - Write only when something actually changed, so the steady state is a
///   read-only boot (and `config.toml`'s mtime — which the MCP auth registry
///   cache keys off — stays put).
pub fn ensure_internal_mcp_key(home_dir: &Path) -> Result<String, String> {
    let config_path = home_dir.join("config.toml");
    duduclaw_core::with_file_lock(&config_path, || {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut table: toml::Table = toml::from_str(&content)
            .map_err(|e| std::io::Error::other(format!("malformed config.toml: {e}")))?;

        let now = Utc::now();

        // Survey every existing `gateway-internal` entry.
        let mut internal_seen = 0usize;
        let mut undatable = 0usize;
        // Newest entry that can still authenticate (age <= HARD_EXPIRY_DAYS).
        let mut newest_valid: Option<(String, DateTime<Utc>, i64)> = None;
        // Age of the newest *datable* internal entry, expired ones included —
        // logging only, so a rotation line says what it rotated away from.
        let mut newest_age: Option<i64> = None;
        let mut prune: Vec<String> = Vec::new();

        if let Some(keys) = table.get("mcp_keys").and_then(|v| v.as_table()) {
            for (key, val) in keys {
                if val.get("client_id").and_then(|v| v.as_str()) != Some(INTERNAL_CLIENT_ID) {
                    continue;
                }
                internal_seen += 1;

                let created_at = val
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                let Some(created_at) = created_at else {
                    // The authenticator skips a row with a missing/unparseable
                    // `created_at` at registry-load time, so this key can never
                    // authenticate — treat it as expired and drop it.
                    undatable += 1;
                    prune.push(key.clone());
                    continue;
                };

                let age = age_in_days(created_at, now);
                if newest_age.is_none_or(|a| age < a) {
                    newest_age = Some(age);
                }

                if age > HARD_EXPIRY_DAYS {
                    prune.push(key.clone());
                    continue;
                }
                let is_newer = match &newest_valid {
                    None => true,
                    Some((_, best, _)) => created_at > *best,
                };
                if is_newer {
                    newest_valid = Some((key.clone(), created_at, age));
                }
            }
        }

        let reused = newest_valid
            .as_ref()
            .filter(|(_, _, age)| *age < ROTATE_AFTER_DAYS)
            .map(|(k, _, _)| k.clone());

        let (key, minted) = match reused {
            Some(k) => (k, false),
            None => (mint_key(), true),
        };

        // Steady state: a young key and nothing to prune ⇒ no write at all.
        if !minted && prune.is_empty() {
            return Ok(key);
        }

        let mcp_keys = table
            .entry("mcp_keys")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let mcp_keys = mcp_keys
            .as_table_mut()
            .ok_or_else(|| std::io::Error::other("config.toml [mcp_keys] is not a table"))?;
        for stale in &prune {
            mcp_keys.remove(stale);
        }
        if minted {
            mcp_keys.insert(key.clone(), internal_key_entry(now));
        }

        // Atomic write (temp + rename) so a crash never truncates config.toml.
        let rendered = toml::to_string_pretty(&table)
            .map_err(|e| std::io::Error::other(format!("serialize config.toml: {e}")))?;
        let tmp = config_path.with_extension("toml.tmp");
        std::fs::write(&tmp, &rendered)?;
        std::fs::rename(&tmp, &config_path)?;
        duduclaw_core::platform::set_owner_only(&config_path).ok();

        // Never log the key itself — only ages and counts.
        let previous = match newest_age {
            Some(a) => format!("{a}d"),
            None if internal_seen > 0 => "undatable".to_string(),
            None => "none".to_string(),
        };
        if minted && internal_seen == 0 {
            info!(
                client_id = INTERNAL_CLIENT_ID,
                "Provisioned internal MCP API key in config.toml [mcp_keys]"
            );
        } else if minted {
            info!(
                client_id = INTERNAL_CLIENT_ID,
                previous_key_age = %previous,
                rotate_after_days = ROTATE_AFTER_DAYS,
                hard_expiry_days = HARD_EXPIRY_DAYS,
                pruned = prune.len(),
                undatable = undatable,
                "Rotated internal MCP API key (previous one too old to authenticate)"
            );
        } else {
            info!(
                client_id = INTERNAL_CLIENT_ID,
                pruned = prune.len(),
                undatable = undatable,
                "Pruned expired internal MCP API keys from config.toml [mcp_keys]"
            );
        }
        Ok(key)
    })
    .map_err(|e| format!("internal MCP key provisioning failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a `config.toml` containing one `gateway-internal` entry whose
    /// `created_at` is `age_days` in the past (or a literal string when
    /// `created_at` is given verbatim).
    fn write_internal_key(dir: &Path, key: &str, created_at: &str) {
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[general]\ndefault_agent = \"anna\"\n\n\
                 [mcp_keys.\"{key}\"]\n\
                 client_id = \"{INTERNAL_CLIENT_ID}\"\n\
                 is_external = false\n\
                 created_at = \"{created_at}\"\n\
                 scopes = [\"admin\"]\n"
            ),
        )
        .unwrap();
    }

    fn days_ago(days: i64) -> String {
        (Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }

    fn config_text(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("config.toml")).unwrap()
    }

    #[test]
    fn provisions_once_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = ensure_internal_mcp_key(dir.path()).unwrap();
        assert!(k1.starts_with("ddc_prod_"), "canonical format: {k1}");
        assert_eq!(k1.len(), "ddc_prod_".len() + 32);
        let k2 = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_eq!(k1, k2, "second boot must reuse the same key, not mint");

        let content = config_text(dir.path());
        assert!(content.contains(INTERNAL_CLIENT_ID));
        assert!(content.contains(&k1));
    }

    #[test]
    fn young_key_is_reused_without_rewriting_config() {
        let dir = tempfile::tempdir().unwrap();
        let existing = "ddc_prod_11111111111111111111111111111111";
        write_internal_key(dir.path(), existing, &days_ago(10));
        let before = config_text(dir.path());

        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_eq!(key, existing, "a 10-day-old key still authenticates");
        assert_eq!(
            config_text(dir.path()),
            before,
            "steady state must not rewrite config.toml (mtime feeds the auth cache)"
        );
    }

    #[test]
    fn key_past_rotation_age_is_replaced_but_kept() {
        let dir = tempfile::tempdir().unwrap();
        let old = "ddc_prod_22222222222222222222222222222222";
        write_internal_key(dir.path(), old, &days_ago(26));

        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_ne!(key, old, "26d > ROTATE_AFTER_DAYS ⇒ must mint a fresh key");
        assert!(key.starts_with("ddc_prod_"));

        let content = config_text(dir.path());
        assert!(content.contains(&key), "new key written");
        assert!(
            content.contains(old),
            "the 26-day key still authenticates (< 30d) and must stay for \
             in-flight MCP children spawned with it"
        );

        // The fresh key is now the newest valid one — next boot reuses it.
        assert_eq!(ensure_internal_mcp_key(dir.path()).unwrap(), key);
    }

    #[test]
    fn hard_expired_key_is_rotated_and_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let old = "ddc_prod_33333333333333333333333333333333";
        write_internal_key(dir.path(), old, &days_ago(40));

        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_ne!(key, old);
        let content = config_text(dir.path());
        assert!(content.contains(&key));
        assert!(
            !content.contains(old),
            "a 40-day key can never authenticate again — prune it"
        );
    }

    #[test]
    fn unparseable_created_at_is_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let old = "ddc_prod_44444444444444444444444444444444";
        write_internal_key(dir.path(), old, "not-a-timestamp");

        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_ne!(key, old, "an undatable key must never be trusted as current");
        let content = config_text(dir.path());
        assert!(content.contains(&key));
        assert!(
            !content.contains(old),
            "the authenticator skips rows it cannot date, so this key could \
             never have authenticated — prune it"
        );
    }

    #[test]
    fn newest_valid_key_wins_over_older_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let older = "ddc_prod_55555555555555555555555555555555";
        let newer = "ddc_prod_66666666666666666666666666666666";
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "[mcp_keys.\"{older}\"]\n\
                 client_id = \"{INTERNAL_CLIENT_ID}\"\n\
                 is_external = false\n\
                 created_at = \"{}\"\n\
                 scopes = [\"admin\"]\n\n\
                 [mcp_keys.\"{newer}\"]\n\
                 client_id = \"{INTERNAL_CLIENT_ID}\"\n\
                 is_external = false\n\
                 created_at = \"{}\"\n\
                 scopes = [\"admin\"]\n",
                days_ago(20),
                days_ago(2),
            ),
        )
        .unwrap();

        assert_eq!(ensure_internal_mcp_key(dir.path()).unwrap(), newer);
    }

    #[test]
    fn preserves_existing_config_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[general]
default_agent = "anna"

[mcp_keys."ddc_prod_00000000000000000000000000000000"]
client_id = "claude-desktop"
is_external = true
created_at = "2026-01-01T00:00:00Z"
scopes = ["memory:read"]
"#,
        )
        .unwrap();
        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        let content = config_text(dir.path());
        assert!(content.contains("default_agent = \"anna\""));
        assert!(content.contains("claude-desktop"));
        assert!(content.contains(&key));
    }

    /// A foreign key that is itself long expired must survive rotation
    /// untouched — pruning is scoped to `gateway-internal` rows only.
    #[test]
    fn rotation_never_prunes_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let foreign = "ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let stale_internal = "ddc_prod_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "[mcp_keys.\"{foreign}\"]\n\
                 client_id = \"claude-desktop\"\n\
                 is_external = true\n\
                 created_at = \"2020-01-01T00:00:00+00:00\"\n\
                 scopes = [\"memory:read\"]\n\n\
                 [mcp_keys.\"{stale_internal}\"]\n\
                 client_id = \"{INTERNAL_CLIENT_ID}\"\n\
                 is_external = false\n\
                 created_at = \"{}\"\n\
                 scopes = [\"admin\"]\n",
                days_ago(90),
            ),
        )
        .unwrap();

        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        let content = config_text(dir.path());
        assert!(content.contains(foreign), "foreign key must survive");
        assert!(content.contains("claude-desktop"));
        assert!(content.contains("2020-01-01T00:00:00+00:00"));
        assert!(content.contains("memory:read"));
        assert!(!content.contains(stale_internal), "expired internal pruned");
        assert!(content.contains(&key));
    }

    /// Future-dated `created_at` (clock skew) is age-0 for the authenticator,
    /// so rotation must agree — otherwise every boot mints a new key.
    #[test]
    fn future_dated_key_is_reused_not_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let existing = "ddc_prod_cccccccccccccccccccccccccccccccc";
        write_internal_key(dir.path(), existing, &days_ago(-10));
        assert_eq!(ensure_internal_mcp_key(dir.path()).unwrap(), existing);
    }

    /// The contract with `duduclaw-cli/src/mcp_auth.rs`: that module refuses
    /// any key with `days_old > 30`. Rotating strictly earlier is what keeps
    /// a running install's CLI children authenticating; if either number ever
    /// moves, this assertion is the tripwire.
    #[test]
    fn rotate_margin_leaves_headroom() {
        assert_eq!(HARD_EXPIRY_DAYS, 30, "mirrors mcp_auth's `days_old > 30`");
        assert!(
            ROTATE_AFTER_DAYS < HARD_EXPIRY_DAYS,
            "rotation must happen before the authenticator's hard expiry"
        );
        assert!(
            HARD_EXPIRY_DAYS - ROTATE_AFTER_DAYS >= 5,
            "keep at least 5 days of margin for installs that reboot rarely"
        );
    }
}
