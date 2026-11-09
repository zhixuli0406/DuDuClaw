//! RM-166: the AI-employee identity-token transition.
//!
//! v1.80.0 made `[delegation] require_identity_token` strict by default, but
//! the Claude CLI spawn never gave the `agent-file-guard` hook the employee's
//! `DUDUCLAW_AGENT_TOKEN`, so every employee was refused when writing its own
//! folder. The spawn now carries the identity
//! (`agent_hook_installer::apply_hook_identity_env`); this module covers the
//! installs that were already running when that shipped:
//!
//! - [`run`] — a one-time upgrade migration at gateway start. An existing
//!   install (a `config.toml` plus evidence that a gateway already booted on
//!   this home: `identity.key` or its initialization marker) whose
//!   `[delegation]` has **no** `require_identity_token` key gets
//!   `require_identity_token = false` and `identity_token_grace_until =
//!   <now + 30 days>` (the existing ≤30-day rule), written through the same
//!   locked, comment-preserving commit as the dashboard, plus an audit event
//!   and a warning. A key the operator wrote is never touched; a new install
//!   stays strict. A marker under `<home>/migrations/` makes it run once, so
//!   the deadline is never pushed back by a later boot.
//! - [`status_json`] — the effective mode, deadline, days left and the number
//!   of identity refusals the hook recorded in the last 24 h, for
//!   `system.status` (`identity_token`), which drives the dashboard notice.
//!
//! Never blocks boot: every failure degrades to a `warn!` and, where nothing
//! was written, no marker (the next boot retries).

use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, SecondsFormat, SubsecRound, Utc};
use duduclaw_security::audit::{AuditEvent, Severity};
use tracing::{info, warn};

const MARKER_NAME: &str = "rm166-identity-token-grace.done";

/// Length of the upgrade grace window; equals the existing maximum.
pub const GRACE_DAYS: i64 = 30;

/// What one migration pass did (returned for tests and boot logging).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// A marker already existed; nothing was read or written.
    pub already_done: bool,
    /// No prior install (no `config.toml`, or no gateway ever booted here):
    /// strict mode is kept.
    pub fresh_install: bool,
    /// `require_identity_token` was already set by the operator; untouched.
    pub explicit_setting: bool,
    /// The deadline written by this pass.
    pub grace_until: Option<String>,
    pub error: Option<String>,
}

fn marker_path(home: &Path) -> PathBuf {
    home.join("migrations").join(MARKER_NAME)
}

/// A gateway already booted on this home before this release: only gateway
/// startup (and the operator-run `duduclaw mcp init`) creates the identity
/// key and its initialization marker, and this migration runs before this
/// boot creates them.
fn has_prior_install(home: &Path) -> bool {
    [
        duduclaw_core::IDENTITY_KEY_FILE,
        duduclaw_core::identity_token::IDENTITY_INITIALIZED_FILE,
    ]
    .iter()
    .any(|name| std::fs::symlink_metadata(home.join(name)).is_ok())
}

/// Run the upgrade migration once for `home`. Call before the identity key is
/// prepared (the key's presence is the "existing install" signal).
pub async fn run(home: &Path) -> MigrationReport {
    run_at(home, Utc::now()).await
}

async fn run_at(home: &Path, now: DateTime<Utc>) -> MigrationReport {
    let mut report = MigrationReport::default();
    if marker_path(home).exists() {
        report.already_done = true;
        return report;
    }
    let config_path = home.join("config.toml");
    let raw = match std::fs::read_to_string(&config_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.fresh_install = true;
            finish(home, &mut report, "fresh_install");
            return report;
        }
        Err(e) => {
            // Unreadable config: do not guess, do not write the marker.
            report.error = Some(format!("failed to read config.toml: {e}"));
            return report;
        }
    };
    if !has_prior_install(home) {
        report.fresh_install = true;
        finish(home, &mut report, "fresh_install");
        return report;
    }
    let mut table: toml::Table = match raw.parse() {
        Ok(table) => table,
        Err(_) => {
            report.error = Some("config.toml is not valid TOML".into());
            return report;
        }
    };
    let delegation = table
        .entry("delegation")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(section) = delegation.as_table_mut() else {
        // The identity reader rejects this shape too; leave it to the operator.
        report.error = Some("config.toml [delegation] is not a table".into());
        return report;
    };
    if section.contains_key("require_identity_token") {
        report.explicit_setting = true;
        finish(home, &mut report, "explicit_setting");
        return report;
    }
    let deadline = (now + Duration::days(GRACE_DAYS))
        .trunc_subsecs(0)
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    section.insert("require_identity_token".into(), toml::Value::Boolean(false));
    section.insert(
        "identity_token_grace_until".into(),
        toml::Value::String(deadline.clone()),
    );
    let hash = crate::handlers::config_commit::content_hash(&raw);
    if let Err(e) =
        crate::handlers::config_commit::commit_table_locked(&config_path, hash, &table).await
    {
        // config.toml changed under us (or the write failed): retry next boot.
        report.error = Some(e);
        return report;
    }
    audit(home, &deadline);
    warn!(
        grace_until = %deadline,
        "AI employee identity verification: this install was upgraded with the strict default \
         unset, so a {GRACE_DAYS}-day transition was enabled (config.toml [delegation] \
         require_identity_token = false, identity_token_grace_until = {deadline}). Until then \
         unverified employee identity claims are accepted; afterwards strict mode applies \
         automatically. Check on the dashboard that every AI employee can write its own folder, \
         then set require_identity_token = true (or remove both keys) to end the transition early."
    );
    report.grace_until = Some(deadline.clone());
    finish(home, &mut report, &format!("grace_until={deadline}"));
    report
}

fn finish(home: &Path, report: &mut MigrationReport, outcome: &str) {
    let path = marker_path(home);
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, format!("{outcome}\n")));
    if let Err(e) = result {
        warn!("identity-token transition marker not written: {e}");
        report.error = Some(e.to_string());
    }
}

fn audit(home: &Path, deadline: &str) {
    let event = AuditEvent::new(
        "identity_token_grace_auto_enabled",
        "system",
        Severity::Warning,
        serde_json::json!({
            "grace_until": deadline,
            "reason": "existing install without an explicit require_identity_token (upgrade migration)",
        }),
    );
    crate::security_autopilot::audit_and_emit(home, &event);
}

/// Whether `config.toml [delegation]` sets `require_identity_token` at all.
/// Unreadable / invalid config ⇒ `false` (the reader rejects it anyway).
pub fn explicit_setting_present(home: &Path) -> bool {
    delegation_section(home).is_some_and(|s| s.contains_key("require_identity_token"))
}

fn delegation_section(home: &Path) -> Option<toml::Table> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let table: toml::Table = raw.parse().ok()?;
    table.get("delegation")?.as_table().cloned()
}

/// `system.status.identity_token`:
///
/// - `mode`: `strict` / `grace` (transition active) / `error` (the setting
///   cannot be read — identities are refused);
/// - `explicit_setting`: the operator (or the upgrade migration) wrote the key;
/// - `grace_until` / `days_left`: only while `mode == "grace"`;
/// - `upgrade_grace`: the transition was enabled by the upgrade migration;
/// - `untrusted_refusals_24h`: identity refusals the hook recorded in the
///   last 24 hours.
pub fn status_json(home: &Path) -> serde_json::Value {
    status_json_at(home, Utc::now())
}

fn status_json_at(home: &Path, now: DateTime<Utc>) -> serde_json::Value {
    let mode = match duduclaw_core::require_identity_token_from_home(home) {
        Ok(true) => "strict",
        Ok(false) => "grace",
        Err(_) => "error",
    };
    let section = delegation_section(home);
    let explicit = section
        .as_ref()
        .is_some_and(|s| s.contains_key("require_identity_token"));
    let deadline = section
        .as_ref()
        .and_then(|s| s.get("identity_token_grace_until"))
        .and_then(toml::Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .filter(|_| mode == "grace");
    let upgrade_grace = std::fs::read_to_string(marker_path(home))
        .is_ok_and(|m| m.starts_with("grace_until="));
    serde_json::json!({
        "mode": mode,
        "explicit_setting": explicit,
        "grace_until": deadline.map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "days_left": deadline.map(|d| days_left(d, now)),
        "upgrade_grace": upgrade_grace,
        "untrusted_refusals_24h": refusals_24h(home, now),
    })
}

/// Whole days left, rounded up (a deadline 1 hour away is "1 day").
fn days_left(deadline: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let secs = (deadline - now).num_seconds();
    if secs <= 0 { 0 } else { (secs + 86_399) / 86_400 }
}

/// `system.status` is read on every dashboard load and re-broadcast; scanning
/// the audit log at most once a minute per home keeps that cheap.
fn refusals_24h(home: &Path, now: DateTime<Utc>) -> usize {
    static CACHE: std::sync::Mutex<Option<(PathBuf, Instant, usize)>> = std::sync::Mutex::new(None);
    const TTL: StdDuration = StdDuration::from_secs(60);
    if let Ok(guard) = CACHE.lock()
        && let Some((path, at, count)) = guard.as_ref()
        && path == home
        && at.elapsed() < TTL
    {
        return *count;
    }
    let count = duduclaw_security::audit::count_event_type_since(
        home,
        duduclaw_security::audit::EVENT_HOOK_UNTRUSTED_CALLER,
        now - Duration::hours(24),
    );
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((home.to_path_buf(), Instant::now(), count));
    }
    count
}

/// Startup notice for the effective identity mode (called once at boot,
/// after [`run`]). Strict-by-default gets its own line so an operator who
/// sees refusals in the logs knows why.
pub fn log_startup_mode(home: &Path, strict: bool) {
    if strict && !explicit_setting_present(home) {
        info!(
            "AI employee identity verification: strict mode (default; config.toml [delegation] \
             require_identity_token is not set). Every employee tool call must carry its \
             DuDuClaw-issued identity token."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        let p = home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn delegation(home: &Path) -> toml::Table {
        let t: toml::Table = std::fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        t.get("delegation").and_then(|v| v.as_table()).cloned().unwrap_or_default()
    }

    /// An install a previous gateway already booted on.
    fn existing_install(config: &str) -> tempfile::TempDir {
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", config);
        duduclaw_core::ensure_identity_key(h.path()).unwrap();
        h
    }

    #[tokio::test]
    async fn rm166_upgrade_grace_written_once_for_existing_install_without_the_key() {
        let h = existing_install("# operator note\n[gateway]\nport = 18789\n");
        let now = Utc::now();
        let r = run_at(h.path(), now).await;
        assert!(r.error.is_none() && !r.fresh_install && !r.explicit_setting, "{r:?}");
        let written = r.grace_until.clone().expect("grace written");
        let d = delegation(h.path());
        assert_eq!(d.get("require_identity_token").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(d.get("identity_token_grace_until").and_then(|v| v.as_str()), Some(written.as_str()));
        // Within the existing ≤30-day rule, so the reader accepts the grace.
        let deadline = DateTime::parse_from_rfc3339(&written).unwrap().with_timezone(&Utc);
        assert!(deadline > now && deadline - now <= Duration::days(GRACE_DAYS));
        assert!(!duduclaw_core::require_identity_token_from_home(h.path()).unwrap());
        // Comments survive (format-preserving commit).
        let text = std::fs::read_to_string(h.path().join("config.toml")).unwrap();
        assert!(text.contains("# operator note"));
        let audit = std::fs::read_to_string(h.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("identity_token_grace_auto_enabled"));

        // Never again: a later boot does not push the deadline back, even if
        // the operator removed the keys in the meantime.
        std::fs::write(h.path().join("config.toml"), "[gateway]\nport = 18789\n").unwrap();
        let again = run_at(h.path(), now + Duration::days(10)).await;
        assert!(again.already_done && again.grace_until.is_none());
        assert!(delegation(h.path()).is_empty());
    }

    #[tokio::test]
    async fn rm166_upgrade_grace_never_for_new_installs() {
        // No config.toml at all.
        let h = tempfile::tempdir().unwrap();
        let r = run_at(h.path(), Utc::now()).await;
        assert!(r.fresh_install && r.grace_until.is_none(), "{r:?}");
        assert!(!h.path().join("config.toml").exists());

        // Onboarding wrote config.toml but no gateway has booted yet.
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", "[gateway]\nport = 18789\n");
        let r = run_at(h.path(), Utc::now()).await;
        assert!(r.fresh_install && r.grace_until.is_none(), "{r:?}");
        assert!(delegation(h.path()).is_empty());
        assert!(duduclaw_core::require_identity_token_from_home(h.path()).unwrap());
        // The marker keeps it that way once the first boot creates the key.
        duduclaw_core::ensure_identity_key(h.path()).unwrap();
        assert!(run_at(h.path(), Utc::now()).await.already_done);
        assert!(delegation(h.path()).is_empty());
    }

    #[tokio::test]
    async fn rm166_upgrade_grace_never_touches_an_explicit_key() {
        for config in [
            "[delegation]\nrequire_identity_token = true\n",
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = \"2020-01-01T00:00:00Z\"\n",
        ] {
            let h = existing_install(config);
            let r = run_at(h.path(), Utc::now()).await;
            assert!(r.explicit_setting && r.grace_until.is_none(), "{r:?}");
            assert_eq!(std::fs::read_to_string(h.path().join("config.toml")).unwrap(), config);
        }
    }

    #[tokio::test]
    async fn rm166_upgrade_grace_leaves_broken_config_alone_and_retries() {
        let h = existing_install("[delegation\n");
        let r = run_at(h.path(), Utc::now()).await;
        assert!(r.error.is_some() && r.grace_until.is_none());
        assert!(!marker_path(h.path()).exists(), "retry on the next boot");
    }

    #[tokio::test]
    async fn rm166_status_reports_mode_deadline_days_and_refusals() {
        // Strict by default.
        let h = existing_install("[gateway]\nport = 18789\n");
        let s = status_json(h.path());
        assert_eq!(s["mode"], "strict");
        assert_eq!(s["explicit_setting"], false);
        assert!(s["grace_until"].is_null() && s["days_left"].is_null());
        assert_eq!(s["upgrade_grace"], false);
        assert_eq!(s["untrusted_refusals_24h"], 0);

        // After the upgrade migration: grace with 30 days left.
        let h = existing_install("[gateway]\nport = 18789\n");
        duduclaw_security::audit::log_hook_untrusted_caller(h.path(), "alice", "Write");
        duduclaw_security::audit::log_hook_untrusted_caller(h.path(), "bob", "Bash");
        let r = run_at(h.path(), Utc::now()).await;
        let s = status_json(h.path());
        assert_eq!(s["mode"], "grace");
        assert_eq!(s["explicit_setting"], true);
        assert_eq!(s["grace_until"].as_str(), r.grace_until.as_deref());
        assert_eq!(s["days_left"], GRACE_DAYS);
        assert_eq!(s["upgrade_grace"], true);
        assert_eq!(s["untrusted_refusals_24h"], 2);

        // Unreadable setting ⇒ error (identities are refused).
        let h = existing_install("[delegation]\nrequire_identity_token = \"no\"\n");
        assert_eq!(status_json(h.path())["mode"], "error");
    }

    #[test]
    fn days_left_rounds_up_and_never_goes_negative() {
        let now = Utc::now();
        assert_eq!(days_left(now + Duration::hours(1), now), 1);
        assert_eq!(days_left(now + Duration::days(30), now), 30);
        assert_eq!(days_left(now - Duration::hours(1), now), 0);
    }
}
