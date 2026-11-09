//! RM-166: the AI-employee identity-token transition.
//!
//! v1.80.0 made `[delegation] require_identity_token` strict by default, but
//! the Claude CLI spawn never gave the `agent-file-guard` hook the employee's
//! identity, so every employee was refused when writing its own folder. The
//! spawn now carries it (`agent_hook_installer::apply_hook_identity_env`).
//!
//! Owner decision 2026-11-09 (security review, round 2): the transition is a
//! **notice**, never an automatic weakening. Nothing in this module writes
//! `config.toml` on its own; strict mode stays on unless an Admin turns on a
//! bounded manual grace from the dashboard.
//!
//! - [`record_first_start`] — at gateway start, records once when this
//!   release first started on this home, in the gateway-owned
//!   `<home>/identity-transition.json` (`duduclaw_core::IDENTITY_TRANSITION_FILE`,
//!   refused for every caller by the file guard on the Write/Edit and Bash
//!   lanes). It only drives the notice: deleting it at worst shows the notice
//!   again, and it never changes verification.
//! - [`status_json`] — `system.status.identity_token` (Admin-only field): the
//!   effective mode, any grace deadline, whether the 30-day notice window is
//!   open, and the identity refusals the hook recorded in the last 24 h.
//! - [`manual_grace_deadline`] / [`commit_grace_edit`] — the dashboard's
//!   one-click manual grace (`require_identity_token = false` +
//!   `identity_token_grace_until`, ≤ 30 days, the existing bounded mechanism)
//!   and "end grace now", through the locked, comment-preserving config
//!   commit. Admin + fresh step-up + audit live in
//!   `handlers::identity_grace_rpc`.

use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, SecondsFormat, SubsecRound, Utc};
use tracing::{info, warn};

/// Length of the post-upgrade notice window.
pub const NOTICE_DAYS: i64 = 30;

/// Longest manual grace the dashboard may set; equals the reader's limit
/// (`identity_token_grace_until` at most 30 days ahead).
pub const MAX_MANUAL_GRACE_DAYS: i64 = 30;

/// Largest state file read (it holds two short fields).
const STATE_MAX_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FirstStart {
    /// RFC 3339 UTC.
    first_started_at: String,
    /// A gateway had already booted on this home before this release.
    existing_install: bool,
}

fn state_path(home: &Path) -> PathBuf {
    home.join(duduclaw_core::IDENTITY_TRANSITION_FILE)
}

/// A gateway already booted on this home before this release: only gateway
/// startup (and the operator-run `duduclaw mcp init`) creates the identity
/// key and its initialization marker, and [`record_first_start`] runs before
/// this boot creates them.
fn has_prior_install(home: &Path) -> bool {
    home.join("config.toml").exists()
        && [
            duduclaw_core::IDENTITY_KEY_FILE,
            duduclaw_core::identity_token::IDENTITY_INITIALIZED_FILE,
        ]
        .iter()
        .any(|name| std::fs::symlink_metadata(home.join(name)).is_ok())
}

/// The recorded first start, if the file is a small regular file (never a
/// link) holding valid JSON.
fn read_state(home: &Path) -> Option<FirstStart> {
    let path = state_path(home);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() || meta.len() > STATE_MAX_BYTES {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Record, once, when this release first started on `home`. Call at gateway
/// start before the identity key is prepared (the key's presence is the
/// "existing install" signal). Never touches `config.toml`; failures only
/// log. A missing or unreadable record is simply written again with the
/// current time, which can only reopen the notice.
pub fn record_first_start(home: &Path) {
    record_first_start_at(home, Utc::now());
}

fn record_first_start_at(home: &Path, now: DateTime<Utc>) -> Option<FirstStart> {
    if let Some(state) = read_state(home) {
        return Some(state);
    }
    let state = FirstStart {
        first_started_at: now.trunc_subsecs(0).to_rfc3339_opts(SecondsFormat::Secs, true),
        existing_install: has_prior_install(home),
    };
    let body = match serde_json::to_string(&state) {
        Ok(body) => body,
        Err(e) => {
            warn!("identity-transition record not serialised: {e}");
            return None;
        }
    };
    match crate::handlers::config_commit::replace_owner_only(&state_path(home), &body) {
        Ok(()) => Some(state),
        Err(e) => {
            warn!("identity-transition record not written: {e}");
            None
        }
    }
}

/// End of the notice window, while it is open: the install existed before
/// this release and its first start was less than [`NOTICE_DAYS`] ago. A
/// first start in the future (clock change, hand edit) does not open it.
fn notice_until(home: &Path, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let state = read_state(home).filter(|s| s.existing_install)?;
    let first = DateTime::parse_from_rfc3339(&state.first_started_at)
        .ok()?
        .with_timezone(&Utc);
    if first > now + Duration::days(1) {
        return None;
    }
    let until = first + Duration::days(NOTICE_DAYS);
    (now < until).then_some(until)
}

fn delegation_section(home: &Path) -> Option<toml::Table> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let table: toml::Table = raw.parse().ok()?;
    table.get("delegation")?.as_table().cloned()
}

/// Whether `config.toml [delegation]` sets `require_identity_token` at all.
/// Unreadable / invalid config ⇒ `false` (the reader rejects it anyway).
pub fn explicit_setting_present(home: &Path) -> bool {
    delegation_section(home).is_some_and(|s| s.contains_key("require_identity_token"))
}

/// Effective identity mode: `strict` / `grace` / `error` (the setting cannot
/// be read — identities are refused).
pub fn effective_mode(home: &Path) -> &'static str {
    match duduclaw_core::require_identity_token_from_home(home) {
        Ok(true) => "strict",
        Ok(false) => "grace",
        Err(_) => "error",
    }
}

/// `system.status.identity_token` (Admin-only; stripped for everyone else
/// by the RPC and the push filter):
///
/// - `mode`: `strict` / `grace` / `error`;
/// - `explicit_setting`: `require_identity_token` is written in config.toml;
/// - `grace_until` / `days_left`: only while `mode == "grace"`;
/// - `notice_active` / `notice_until`: the 30-day "strict mode is on" notice
///   after this release first started on an existing install;
/// - `untrusted_refusals_24h`: identity refusals the hook recorded in the
///   last 24 hours (one row per claimed id per minute);
/// - `manual_grace_max_days`: the longest grace the dashboard may set.
pub fn status_json(home: &Path) -> serde_json::Value {
    status_json_at(home, Utc::now())
}

fn status_json_at(home: &Path, now: DateTime<Utc>) -> serde_json::Value {
    let mode = effective_mode(home);
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
    let notice = notice_until(home, now);
    serde_json::json!({
        "mode": mode,
        "explicit_setting": explicit,
        "grace_until": deadline.map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "days_left": deadline.map(|d| days_left(d, now)),
        "notice_active": notice.is_some(),
        "notice_until": notice.map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "untrusted_refusals_24h": refusals_24h(home, now),
        "manual_grace_max_days": MAX_MANUAL_GRACE_DAYS,
    })
}

/// Whole days left, rounded up (a deadline 1 hour away is "1 day").
fn days_left(deadline: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let secs = (deadline - now).num_seconds();
    if secs <= 0 { 0 } else { (secs + 86_399) / 86_400 }
}

/// `system.status` is read on every dashboard load and re-broadcast; the
/// count (itself a bounded tail read) is cached for a minute per home.
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

/// Startup notice for the effective identity mode (called once at boot).
/// Strict-by-default gets its own line so an operator who sees refusals in
/// the logs knows why.
pub fn log_startup_mode(home: &Path, strict: bool) {
    if strict && !explicit_setting_present(home) {
        info!(
            "AI employee identity verification: strict mode (default; config.toml [delegation] \
             require_identity_token is not set). Every employee tool call must carry its \
             DuDuClaw-issued identity token."
        );
    }
}

// ── Manual grace (dashboard, Admin + fresh step-up + audit) ──────────────────

/// `identity_token_grace_until` for a manual grace of `days` (1..=30) from
/// `now`: whole seconds, UTC `Z` — exactly what the reader accepts.
pub(crate) fn manual_grace_deadline(now: DateTime<Utc>, days: i64) -> Result<String, String> {
    if !(1..=MAX_MANUAL_GRACE_DAYS).contains(&days) {
        return Err(format!("寬限天數必須是 1 到 {MAX_MANUAL_GRACE_DAYS} 天。"));
    }
    Ok((now + Duration::days(days))
        .trunc_subsecs(0)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// One dashboard edit of `config.toml [delegation]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GraceEdit {
    /// `require_identity_token = false` + `identity_token_grace_until`.
    Start { grace_until: String },
    /// `require_identity_token = true`, `identity_token_grace_until` removed.
    End,
}

/// Apply `edit` through the locked, comment-preserving config commit (the
/// same writer as every dashboard config save). `guard` re-checks the
/// caller's authority under the file lock. `Ok(false)`: [`GraceEdit::End`]
/// found nothing to end, and nothing was written.
pub(crate) async fn commit_grace_edit(
    home: &Path,
    edit: GraceEdit,
    guard: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<bool, String> {
    use crate::handlers::config_commit::{commit_table_locked_with_guard, content_hash, read_text_or_empty};
    let path = home.join("config.toml");
    let raw = read_text_or_empty(&path)?;
    let mut table: toml::Table = if raw.trim().is_empty() {
        toml::Table::new()
    } else {
        raw.parse()
            .map_err(|_| "config.toml 不是有效的 TOML，無法變更；請先修正設定檔。".to_string())?
    };
    let section = table
        .entry("delegation")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("config.toml 的 [delegation] 格式不正確，無法變更。")?;
    match edit {
        GraceEdit::Start { grace_until } => {
            section.insert("require_identity_token".into(), toml::Value::Boolean(false));
            section.insert("identity_token_grace_until".into(), toml::Value::String(grace_until));
        }
        GraceEdit::End => {
            let weakened = section.get("require_identity_token") != Some(&toml::Value::Boolean(true))
                && section.contains_key("require_identity_token");
            if !weakened && !section.contains_key("identity_token_grace_until") {
                return Ok(false);
            }
            section.insert("require_identity_token".into(), toml::Value::Boolean(true));
            section.remove("identity_token_grace_until");
        }
    }
    commit_table_locked_with_guard(&path, content_hash(&raw), &table, guard).await?;
    Ok(true)
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
        delegation_section(home).unwrap_or_default()
    }

    /// An install a previous gateway already booted on.
    fn existing_install(config: &str) -> tempfile::TempDir {
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", config);
        duduclaw_core::ensure_identity_key(h.path()).unwrap();
        h
    }

    /// Round 2: no boot step ever weakens strict mode — the first-start
    /// record leaves config.toml byte-identical on every kind of install.
    #[test]
    fn rm166b_first_start_never_weakens_strict_mode() {
        for config in [
            "# operator note\n[gateway]\nport = 18789\n",
            "[delegation]\npolicy = \"open\"\n",
            "",
        ] {
            let h = existing_install(config);
            let before = std::fs::read(h.path().join("config.toml")).unwrap();
            let state = record_first_start_at(h.path(), Utc::now()).unwrap();
            assert!(state.existing_install);
            assert_eq!(std::fs::read(h.path().join("config.toml")).unwrap(), before);
            assert!(!delegation(h.path()).contains_key("require_identity_token"));
            assert!(duduclaw_core::require_identity_token_from_home(h.path()).unwrap(), "{config:?}");
            assert_eq!(status_json(h.path())["mode"], "strict");
        }
        // Fresh install: no record of a prior boot, no config written either.
        let h = tempfile::tempdir().unwrap();
        assert!(!record_first_start_at(h.path(), Utc::now()).unwrap().existing_install);
        assert!(!h.path().join("config.toml").exists());
        assert!(duduclaw_core::require_identity_token_from_home(h.path()).unwrap());
    }

    #[test]
    fn rm166b_notice_window_is_thirty_days_after_first_start_on_an_existing_install() {
        let h = existing_install("[gateway]\nport = 18789\n");
        let t0 = Utc::now();
        record_first_start_at(h.path(), t0).unwrap();
        // A later boot never moves the first start.
        let again = record_first_start_at(h.path(), t0 + Duration::days(10)).unwrap();
        assert_eq!(
            DateTime::parse_from_rfc3339(&again.first_started_at).unwrap().timestamp(),
            t0.timestamp()
        );
        let s = status_json_at(h.path(), t0 + Duration::days(1));
        assert_eq!(s["notice_active"], true);
        assert_eq!(s["mode"], "strict");
        assert!(s["grace_until"].is_null() && s["days_left"].is_null());
        assert_eq!(s["manual_grace_max_days"], MAX_MANUAL_GRACE_DAYS);
        assert!(status_json_at(h.path(), t0 + Duration::days(29))["notice_active"] == true);
        let after = status_json_at(h.path(), t0 + Duration::days(31));
        assert_eq!(after["notice_active"], false);
        assert!(after["notice_until"].is_null());

        // A fresh install gets no notice.
        let fresh = tempfile::tempdir().unwrap();
        record_first_start_at(fresh.path(), t0).unwrap();
        assert_eq!(status_json_at(fresh.path(), t0)["notice_active"], false);
    }

    /// Deleting (or corrupting) the record cannot weaken anything: the next
    /// boot just records a new first start and the notice shows again.
    #[test]
    fn rm166b_deleting_the_record_only_reopens_the_notice() {
        let h = existing_install("[gateway]\nport = 18789\n");
        let t0 = Utc::now() - Duration::days(40);
        record_first_start_at(h.path(), t0).unwrap();
        assert_eq!(status_json(h.path())["notice_active"], false);
        let config = std::fs::read(h.path().join("config.toml")).unwrap();
        for damage in ["delete", "garbage"] {
            if damage == "delete" {
                std::fs::remove_file(state_path(h.path())).unwrap();
            } else {
                std::fs::write(state_path(h.path()), "not json").unwrap();
            }
            assert_eq!(status_json(h.path())["notice_active"], false);
            record_first_start(h.path());
            let s = status_json(h.path());
            assert_eq!(s["notice_active"], true, "{damage}");
            assert_eq!(s["mode"], "strict", "{damage}");
            assert_eq!(std::fs::read(h.path().join("config.toml")).unwrap(), config);
            std::fs::write(state_path(h.path()), serde_json::to_string(&FirstStart {
                first_started_at: t0.to_rfc3339_opts(SecondsFormat::Secs, true),
                existing_install: true,
            }).unwrap()).unwrap();
        }
        // A forged far-future first start does not open a never-ending notice.
        std::fs::write(state_path(h.path()), r#"{"first_started_at":"2999-01-01T00:00:00Z","existing_install":true}"#).unwrap();
        assert_eq!(status_json(h.path())["notice_active"], false);
    }

    #[test]
    fn rm166b_manual_grace_is_bounded_to_thirty_days() {
        let now = Utc::now();
        for days in [0, -1, 31, 365] {
            assert!(manual_grace_deadline(now, days).is_err(), "{days}");
        }
        for days in [1, 7, 30] {
            let d = manual_grace_deadline(now, days).unwrap();
            assert!(d.ends_with('Z'), "{d}");
            let parsed = DateTime::parse_from_rfc3339(&d).unwrap().with_timezone(&Utc);
            assert!(parsed > now && parsed - now <= Duration::days(MAX_MANUAL_GRACE_DAYS));
        }
    }

    #[tokio::test]
    async fn rm166b_grace_edit_start_and_end_preserve_the_file() {
        let h = existing_install("# operator note\n[gateway]\nport = 18789 # keep\n");
        let deadline = manual_grace_deadline(Utc::now(), 30).unwrap();
        assert!(commit_grace_edit(h.path(), GraceEdit::Start { grace_until: deadline.clone() }, || Ok(())).await.unwrap());
        let d = delegation(h.path());
        assert_eq!(d.get("require_identity_token").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(d.get("identity_token_grace_until").and_then(|v| v.as_str()), Some(deadline.as_str()));
        let text = std::fs::read_to_string(h.path().join("config.toml")).unwrap();
        assert!(text.contains("# operator note") && text.contains("# keep"), "{text}");
        let s = status_json(h.path());
        assert_eq!(s["mode"], "grace");
        assert_eq!(s["grace_until"], deadline);
        assert_eq!(s["days_left"], 30);

        // A refused guard writes nothing.
        let before = std::fs::read(h.path().join("config.toml")).unwrap();
        assert!(commit_grace_edit(h.path(), GraceEdit::End, || Err("no".into())).await.is_err());
        assert_eq!(std::fs::read(h.path().join("config.toml")).unwrap(), before);

        // End grace now: strict again, deadline gone, comments kept.
        assert!(commit_grace_edit(h.path(), GraceEdit::End, || Ok(())).await.unwrap());
        let d = delegation(h.path());
        assert_eq!(d.get("require_identity_token").and_then(|v| v.as_bool()), Some(true));
        assert!(!d.contains_key("identity_token_grace_until"));
        assert_eq!(status_json(h.path())["mode"], "strict");
        assert!(std::fs::read_to_string(h.path().join("config.toml")).unwrap().contains("# operator note"));
        // Nothing left to end.
        assert!(!commit_grace_edit(h.path(), GraceEdit::End, || Ok(())).await.unwrap());
    }

    #[test]
    fn rm166b_status_reports_refusals_and_error_mode() {
        let h = existing_install("[gateway]\nport = 18789\n");
        duduclaw_security::audit::log_hook_untrusted_caller(h.path(), "alice", "Write");
        duduclaw_security::audit::log_hook_untrusted_caller(h.path(), "bob", "Bash");
        assert_eq!(status_json(h.path())["untrusted_refusals_24h"], 2);
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
