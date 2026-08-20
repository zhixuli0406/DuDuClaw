//! WP-G1 — scheduled device backups.
//!
//! Packs the writable data partition on a timer instead of only on a manual
//! dashboard click — same `tar -czf` shape as `device.backup_create`
//! (`crate::device_ops::DeviceOps::backup_create`), but run directly via
//! [`tar_backup_excluding`] rather than through that shared trait, so the
//! scheduler alone can pass `--exclude` patterns
//! ([`schedule_backup_excludes`]) without touching the manual path at all —
//! see that function's doc comment for why unattended recurring packing
//! needs exclusions a one-off manual click does not. Deliberately does not
//! reach for a generic cron dependency —
//! `config.toml [backup]` declares a single fixed interval, so a plain
//! `tokio::time::interval` tick loop (mirroring every other single-purpose
//! background driver in this crate: `notify_digest::DailyDigestScheduler`,
//! `channel_alerts::ChannelAlertMonitor`, `self_study::SelfStudyScheduler`)
//! is the whole mechanism this needs.
//!
//! ## Why a dedicated `<home>/backups/` directory, not `attachments/`
//!
//! `device.backup_create` (the manual, dashboard-button path) stages its
//! archive under the shared attachments directory so the existing
//! `GET /api/files/download` route can serve it with zero new code. Scheduled
//! backups run unattended and accumulate — mixing them into `attachments/`
//! would pollute the same listing task/channel-delivered artifacts show up
//! in (`crate::files_api::list_files`, the I-2b provenance ledger). They get
//! their own directory ([`backups_dir`]) with their own listing
//! (`device.backup_list`) and download route
//! (`GET /api/device/backups/download`, `server.rs`).
//!
//! ## Fail-open discipline
//!
//! A scheduled run that fails (disk full, `tar` missing, …) logs a `warn`,
//! bumps `duduclaw_backup_schedule_fail_total`, and tries again on the next
//! tick — it never panics, never aborts the scheduler loop, and never blocks
//! gateway startup. The tick cadence is a short fixed interval (5 minutes,
//! set by the `server.rs` caller); [`is_due`] is what actually decides
//! whether `interval_hours` has elapsed since the last successful run, so a
//! failure only costs a 5-minute retry delay, not a full missed interval.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// `backups_dir`'s literal dirname — also referenced by
/// [`schedule_backup_excludes`] to build the tar exclusion pattern, so the
/// two can never drift apart.
const BACKUPS_DIRNAME: &str = "backups";

/// `<home>/backups` — dedicated, never shared with task/channel attachments.
pub fn backups_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(BACKUPS_DIRNAME)
}

const STATE_FILE: &str = "backup_schedule_state.json";
const BACKUP_PREFIX: &str = "device-backup-";
const BACKUP_SUFFIX: &str = ".tar.gz";
/// Basename glob for this scheduler's own in-flight staging file (see
/// [`schedule_backup_excludes`]'s doc comment on why this is excluded too).
const STAGING_BASENAME_GLOB: &str = "duduclaw-schedule-*";

// ── Config: `config.toml [backup]` ──────────────────────────────────────

/// `config.toml [backup]`. Mirrors `document_limits::DocumentLimits`'s
/// discipline: absent file/section ⇒ [`Default`] (feature off), and a `0`
/// read back from a hand-edited config means "use the default", never
/// "unlimited"/"instant" — see [`Self::sanitized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupScheduleConfig {
    pub enabled: bool,
    pub interval_hours: u32,
    pub retention_count: u32,
}

impl Default for BackupScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 24,
            retention_count: 7,
        }
    }
}

impl BackupScheduleConfig {
    /// Load `[backup]` from `<home>/config.toml`. Missing file / missing
    /// section / malformed section ⇒ [`Default`] (off).
    pub fn from_home(home_dir: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        Self::from_toml_str(&content)
    }

    /// Parse a whole `config.toml` body. Public for tests and for
    /// `handlers.rs`'s read-modify-write RPC glue.
    pub fn from_toml_str(content: &str) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        let Some(section) = table.get("backup").and_then(|v| v.as_table()) else {
            return Self::default();
        };
        let d = Self::default();
        Self {
            enabled: section
                .get("schedule_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(d.enabled),
            interval_hours: section
                .get("interval_hours")
                .and_then(|v| v.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.interval_hours),
            retention_count: section
                .get("retention_count")
                .and_then(|v| v.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.retention_count),
        }
        .sanitized()
    }

    /// `0` (or a value that failed to parse, already folded to the default
    /// above) reads as "use the default", never as "unlimited" / "every
    /// tick" / "keep nothing" — a config file can tune this schedule but
    /// can never turn it into a footgun.
    fn sanitized(mut self) -> Self {
        let d = Self::default();
        if self.interval_hours == 0 {
            self.interval_hours = d.interval_hours;
        }
        if self.retention_count == 0 {
            self.retention_count = d.retention_count;
        }
        self
    }
}

// ── Pure scheduling logic (unit-tested directly) ────────────────────────

/// Has `interval_hours` elapsed since `last_backup_at`? `None` (never run
/// yet) is always due.
pub fn is_due(interval_hours: u32, last_backup_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last_backup_at {
        None => true,
        Some(last) => now.signed_duration_since(last) >= chrono::Duration::hours(interval_hours as i64),
    }
}

/// Given every backup file's `(name, modified_time)`, return the names that
/// exceed `retention_count` and should be deleted — the `retention_count`
/// most recent are always kept. Pure: callers pass an already-listed
/// snapshot, so this is unit-testable without a real filesystem.
pub fn files_to_prune(mut entries: Vec<(String, DateTime<Utc>)>, retention_count: u32) -> Vec<String> {
    let keep = retention_count.max(1) as usize;
    if entries.len() <= keep {
        return Vec::new();
    }
    // Newest first; stable tie-break on name so equal mtimes don't reorder
    // between runs (matters for the "which N survive" test to be
    // deterministic).
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    entries.split_off(keep).into_iter().map(|(name, _)| name).collect()
}

/// tar `--exclude` patterns for a SCHEDULED backup run. Pure — a fixed set
/// derived only from `home_dir`'s own basename, never from RPC/user input.
///
/// ## Why this exists (compounding-archive fix)
///
/// `device.backup_create`'s underlying packer (`device_ops::DeviceOps::
/// backup_create`, still used byte-identically by the manual dashboard
/// button) archives `home_dir.parent()` — the whole writable data partition
/// — so a from-scratch rebuild can restore the OS-level layout too. But
/// [`backups_dir`] and the restore-related directories
/// (`crate::backup_restore::staging_dir`, and every
/// `{crate::backup_restore::RESTORE_BACKUP_PREFIX}<ts>` preserved-data dir)
/// all live INSIDE `home_dir`, which is itself inside the archived tree.
/// Without exclusion, every scheduled run would pack every backup and
/// restore artifact from every PRIOR run into itself — a one-off manual
/// click pays that cost once; an unattended recurring job pays it every
/// cycle and grows without bound. These patterns are ONLY applied to the
/// scheduler's own packing call ([`tar_backup_excluding`]) — the manual
/// `device.backup_create` RPC/handler is untouched by this fix.
///
/// Patterns are relative to `source_dir` (`home_dir.parent()`), matching
/// what `tar -C source_dir .` walks — e.g. for a home dir named `duduclaw`:
/// `./duduclaw/backups`, `./duduclaw/restore-staging`,
/// `./duduclaw/restore-backup-*`. A trailing pattern also excludes this
/// scheduler's own in-flight staging-file basename
/// ([`STAGING_BASENAME_GLOB`]) as defense in depth: that file normally
/// lives under `std::env::temp_dir()`, entirely outside `source_dir`, but a
/// deployment where `TMPDIR` happens to resolve inside the data partition
/// should not be able to smuggle a half-written archive into itself either.
pub fn schedule_backup_excludes(home_dir: &Path) -> Vec<String> {
    let base = home_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty());
    let mut out = Vec::with_capacity(4);
    if let Some(base) = base {
        out.push(format!("./{base}/{BACKUPS_DIRNAME}"));
        out.push(format!("./{base}/{}", crate::backup_restore::RESTORE_STAGING_DIRNAME));
        out.push(format!("./{base}/{}*", crate::backup_restore::RESTORE_BACKUP_PREFIX));
    }
    out.push(STAGING_BASENAME_GLOB.to_string());
    out
}

// ── Once-per-interval state ──────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduleState {
    #[serde(default)]
    last_backup_at: Option<DateTime<Utc>>,
}

fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STATE_FILE)
}

fn read_last_backup_at(home_dir: &Path) -> Option<DateTime<Utc>> {
    std::fs::read_to_string(state_path(home_dir))
        .ok()
        .and_then(|b| serde_json::from_str::<ScheduleState>(&b).ok())
        .and_then(|s| s.last_backup_at)
}

/// Record a successful run. Best-effort atomic write (temp + rename); a
/// failure here only means the next tick might re-run slightly early — it
/// must never block or fail the backup that already succeeded.
fn write_last_backup_at(home_dir: &Path, at: DateTime<Utc>) {
    let path = state_path(home_dir);
    let body = serde_json::to_string_pretty(&ScheduleState { last_backup_at: Some(at) }).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ── Outcome (for tests + logging — never surfaced to a dashboard RPC) ───

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupTickOutcome {
    /// `[backup] schedule_enabled` is not `true`.
    Disabled,
    /// Enabled, but `interval_hours` has not elapsed since the last run.
    NotDue,
    /// A backup was created and rotation ran.
    Ran { filename: String, pruned: Vec<String> },
    /// The backup attempt failed. The caller (metrics/log) is responsible
    /// for surfacing this — the scheduler itself never propagates it.
    Failed(String),
}

// ── Scheduler ─────────────────────────────────────────────────────────

/// Ticks on a short fixed interval (5 minutes, set by the `server.rs`
/// caller) and does nothing unless `[backup] schedule_enabled = true` AND
/// `interval_hours` has elapsed since the last successful run.
pub struct BackupScheduler {
    home_dir: PathBuf,
}

impl BackupScheduler {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    /// One evaluation. Returns the outcome for tests/logging.
    pub async fn tick(&self) -> BackupTickOutcome {
        let cfg = BackupScheduleConfig::from_home(&self.home_dir);
        if !cfg.enabled {
            return BackupTickOutcome::Disabled;
        }
        let now = Utc::now();
        let last = read_last_backup_at(&self.home_dir);
        if !is_due(cfg.interval_hours, last, now) {
            return BackupTickOutcome::NotDue;
        }

        match self.run_backup_and_rotate(&cfg, now).await {
            Ok((filename, pruned)) => {
                write_last_backup_at(&self.home_dir, now);
                crate::metrics::global_metrics().backup_schedule_ok();
                info!(filename, pruned = pruned.len(), "backup-schedule: 已建立排程備份");
                BackupTickOutcome::Ran { filename, pruned }
            }
            Err(e) => {
                crate::metrics::global_metrics().backup_schedule_fail();
                warn!(error = %e, "backup-schedule: 排程備份失敗，將於下次檢查時重試");
                BackupTickOutcome::Failed(e)
            }
        }
    }

    async fn run_backup_and_rotate(
        &self,
        cfg: &BackupScheduleConfig,
        now: DateTime<Utc>,
    ) -> Result<(String, Vec<String>), String> {
        let home = self.home_dir.clone();
        let source_dir = home.parent().map(Path::to_path_buf).unwrap_or_else(|| home.clone());
        let dest_dir = backups_dir(&home);
        tokio::fs::create_dir_all(&dest_dir)
            .await
            .map_err(|e| format!("建立備份目錄失敗: {e}"))?;

        let filename = format!("{BACKUP_PREFIX}{}{BACKUP_SUFFIX}", now.format("%Y%m%dT%H%M%SZ"));
        // The staging name carries a uuid, not just the second-precision
        // timestamp: two independent `duduclaw-schedule-` runs (this
        // scheduler + a concurrent test in the same process, or — in
        // principle — two gateways sharing an OS temp dir) landing in the
        // same wall-clock second must never collide on one path.
        let staging = std::env::temp_dir().join(format!("duduclaw-schedule-{}-{filename}", uuid::Uuid::new_v4()));

        let excludes = schedule_backup_excludes(&home);
        let result = tar_backup_excluding(&source_dir, &staging, &excludes).await;
        match result {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let _ = std::fs::remove_file(&staging);
                return Err(format!("tar 執行失敗（exit: {status}）"));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&staging);
                return Err(format!("tar 無法啟動: {e}"));
            }
        }

        let dest_path = dest_dir.join(&filename);
        if std::fs::rename(&staging, &dest_path).is_err() {
            // Cross-device staging dir (e.g. /tmp on a different filesystem).
            std::fs::copy(&staging, &dest_path).map_err(|e| format!("搬移備份檔失敗: {e}"))?;
            let _ = std::fs::remove_file(&staging);
        }

        let pruned = self.rotate(&dest_dir, cfg.retention_count);
        Ok((filename, pruned))
    }

    /// Delete files beyond `retention_count`, oldest first. Best-effort per
    /// file — one stubborn file (permissions, in-use) does not stop the
    /// rest from being pruned, and never turns a rotation failure into a
    /// backup failure (the new backup already succeeded by this point).
    fn rotate(&self, dest_dir: &Path, retention_count: u32) -> Vec<String> {
        let entries = list_backup_files(dest_dir);
        let to_prune = files_to_prune(entries, retention_count);
        for name in &to_prune {
            let _ = std::fs::remove_file(dest_dir.join(name));
        }
        to_prune
    }

    /// Long-running task: evaluate every `interval` until cancelled.
    pub async fn run(self: std::sync::Arc<Self>, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            self.tick().await;
        }
    }
}

/// List `(name, modified_time)` for every regular file directly under `dir`.
/// A missing directory yields an empty vec (nothing to rotate yet).
fn list_backup_files(dir: &Path) -> Vec<(String, DateTime<Utc>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(modified) = meta.modified() else { continue };
        out.push((name, DateTime::<Utc>::from(modified)));
    }
    out
}

/// `tar -czf dest_path --exclude=<pattern> ... -C source_dir .` — the
/// SCHEDULED-backup packer, run directly rather than through
/// `device_ops::DeviceOps::backup_create`.
///
/// Packing never needs root (`device_ops.rs`'s own doc comment on
/// `SysdDeviceOps::backup_create`: "only reads/writes paths the
/// unprivileged `duduclaw` user already owns, so it is delegated straight
/// to `SystemDeviceOps`'s local shell-out rather than duplicated" — i.e.
/// even the privilege-separated path ends up running this exact same local
/// `tar` shell-out), so there is no privilege-separation reason to route
/// through that trait here. Doing so would have meant either adding an
/// `--exclude`-aware method to a trait the existing manual
/// `device.backup_create` RPC also depends on (risking that path), or
/// duplicating the whole trait for one flag — shelling out directly in this
/// module, which owns the one caller that needs exclusions, is the smaller
/// surface. Every `--exclude` value comes from [`schedule_backup_excludes`]
/// (derived only from `home_dir`'s own path, never RPC/user input) and is
/// passed as a discrete `Command::arg()`, never shell-interpolated — the
/// same "no injection surface" property `device_ops.rs` documents for its
/// own shell-outs.
async fn tar_backup_excluding(
    source_dir: &Path,
    dest_path: &Path,
    excludes: &[String],
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = tokio::process::Command::new("tar");
    cmd.arg("-czf").arg(dest_path);
    for pattern in excludes {
        cmd.arg("--exclude").arg(pattern);
    }
    cmd.arg("-C").arg(source_dir).arg(".");
    cmd.status().await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── config ───────────────────────────────────────────────────

    #[test]
    fn schedule_is_off_by_default() {
        let cfg = BackupScheduleConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.interval_hours, 24);
        assert_eq!(cfg.retention_count, 7);
    }

    #[test]
    fn missing_file_or_section_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(BackupScheduleConfig::from_home(dir.path()), BackupScheduleConfig::default());
        assert_eq!(
            BackupScheduleConfig::from_toml_str("[general]\nname = \"x\"\n"),
            BackupScheduleConfig::default()
        );
        assert_eq!(BackupScheduleConfig::from_toml_str("not [ toml"), BackupScheduleConfig::default());
    }

    #[test]
    fn config_reads_all_three_fields() {
        let cfg = BackupScheduleConfig::from_toml_str(
            "[backup]\nschedule_enabled = true\ninterval_hours = 6\nretention_count = 3\n",
        );
        assert!(cfg.enabled);
        assert_eq!(cfg.interval_hours, 6);
        assert_eq!(cfg.retention_count, 3);
    }

    #[test]
    fn zero_in_config_means_default_not_unlimited_or_instant() {
        let cfg = BackupScheduleConfig::from_toml_str(
            "[backup]\nschedule_enabled = true\ninterval_hours = 0\nretention_count = 0\n",
        );
        assert_eq!(cfg.interval_hours, 24);
        assert_eq!(cfg.retention_count, 7);
    }

    #[test]
    fn malformed_field_type_falls_back_to_default_for_that_field() {
        let cfg = BackupScheduleConfig::from_toml_str(
            "[backup]\nschedule_enabled = true\ninterval_hours = \"soon\"\n",
        );
        assert!(cfg.enabled, "a bad interval must not silently disable the feature");
        assert_eq!(cfg.interval_hours, 24);
    }

    // ── is_due ───────────────────────────────────────────────────

    #[test]
    fn never_run_before_is_always_due() {
        assert!(is_due(24, None, Utc::now()));
    }

    #[test]
    fn not_due_until_the_interval_elapses() {
        let now = Utc::now();
        let last = now - chrono::Duration::hours(23);
        assert!(!is_due(24, Some(last), now));
        let last = now - chrono::Duration::hours(24);
        assert!(is_due(24, Some(last), now));
        let last = now - chrono::Duration::hours(25);
        assert!(is_due(24, Some(last), now));
    }

    // ── files_to_prune ───────────────────────────────────────────

    fn t(hours_ago: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::hours(hours_ago)
    }

    #[test]
    fn under_the_cap_prunes_nothing() {
        let entries = vec![("a".to_string(), t(1)), ("b".to_string(), t(2))];
        assert!(files_to_prune(entries, 7).is_empty());
    }

    #[test]
    fn keeps_the_n_most_recent_prunes_the_rest() {
        let entries = vec![
            ("newest".to_string(), t(1)),
            ("mid".to_string(), t(2)),
            ("oldest".to_string(), t(3)),
        ];
        let pruned = files_to_prune(entries, 2);
        assert_eq!(pruned, vec!["oldest".to_string()]);
    }

    #[test]
    fn retention_zero_still_keeps_at_least_one() {
        // Config-level zero already sanitizes to the default before reaching
        // here, but the pure function itself must not delete everything on
        // a stray zero — losing every backup is never the right outcome.
        let entries = vec![("newest".to_string(), t(1)), ("oldest".to_string(), t(2))];
        let pruned = files_to_prune(entries, 0);
        assert_eq!(pruned, vec!["oldest".to_string()]);
    }

    #[test]
    fn equal_mtimes_break_ties_deterministically() {
        let same = t(1);
        let entries = vec![("b".to_string(), same), ("a".to_string(), same), ("c".to_string(), same)];
        // retention 1 ⇒ keep exactly one, deterministically (name-sorted
        // descending as the tie-break), not a different survivor every run.
        let pruned1 = files_to_prune(entries.clone(), 1);
        let pruned2 = files_to_prune(entries, 1);
        assert_eq!(pruned1, pruned2);
        assert_eq!(pruned1.len(), 2);
    }

    // ── scheduler (tempdir + a stubbed device_ops-free path) ───────

    #[tokio::test]
    async fn tick_is_a_noop_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[backup]\nschedule_enabled = false\n").unwrap();
        let sched = BackupScheduler::new(dir.path().to_path_buf());
        assert_eq!(sched.tick().await, BackupTickOutcome::Disabled);
        assert!(!backups_dir(dir.path()).exists());
    }

    /// `run_backup_and_rotate` tars `home.parent()` (mirrors
    /// `handle_device_backup_create`'s manual-path convention) — so the test
    /// home dir must be a subdirectory of a tempdir root that contains
    /// NOTHING else, never the bare tempdir itself. Using the bare tempdir
    /// as `home` would make `home.parent()` resolve to the OS temp root and
    /// `tar` would try to archive it.
    fn scoped_home() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("duduclaw");
        std::fs::create_dir_all(&home).unwrap();
        (root, home)
    }

    #[tokio::test]
    async fn tick_creates_a_backup_and_records_state_when_due() {
        let (_root, home) = scoped_home();
        std::fs::write(home.join("config.toml"), "[backup]\nschedule_enabled = true\ninterval_hours = 24\nretention_count = 7\n").unwrap();

        let sched = BackupScheduler::new(home.clone());
        let outcome = sched.tick().await;
        match outcome {
            BackupTickOutcome::Ran { filename, pruned } => {
                assert!(filename.starts_with(BACKUP_PREFIX));
                assert!(filename.ends_with(BACKUP_SUFFIX));
                assert!(pruned.is_empty());
                assert!(backups_dir(&home).join(&filename).exists());
            }
            other => panic!("expected Ran, got {other:?}"),
        }
        assert!(read_last_backup_at(&home).is_some());

        // Second tick immediately after: not due yet (interval_hours=24).
        let outcome2 = sched.tick().await;
        assert_eq!(outcome2, BackupTickOutcome::NotDue);
    }

    #[tokio::test]
    async fn rotation_deletes_beyond_retention_after_a_run() {
        let (_root, home) = scoped_home();
        std::fs::write(
            home.join("config.toml"),
            "[backup]\nschedule_enabled = true\ninterval_hours = 24\nretention_count = 2\n",
        )
        .unwrap();
        let backups = backups_dir(&home);
        std::fs::create_dir_all(&backups).unwrap();
        // Two pre-existing "old" backups already over the cap once a third
        // lands.
        std::fs::write(backups.join("device-backup-old1.tar.gz"), b"x").unwrap();
        std::fs::write(backups.join("device-backup-old2.tar.gz"), b"x").unwrap();

        let sched = BackupScheduler::new(home.clone());
        let outcome = sched.tick().await;
        match outcome {
            BackupTickOutcome::Ran { pruned, .. } => {
                assert_eq!(pruned.len(), 1, "3 files, keep 2 ⇒ prune 1: {pruned:?}");
            }
            other => panic!("expected Ran, got {other:?}"),
        }
        let remaining: Vec<_> = std::fs::read_dir(&backups).unwrap().collect();
        assert_eq!(remaining.len(), 2, "retention_count=2 must leave exactly 2 files");
    }

    // ── schedule_backup_excludes / compounding-archive fix ──────────

    #[test]
    fn schedule_backup_excludes_covers_backups_staging_and_preserved_dirs() {
        let home = Path::new("/data/duduclaw");
        let excludes = schedule_backup_excludes(home);
        assert!(excludes.contains(&"./duduclaw/backups".to_string()), "{excludes:?}");
        assert!(excludes.contains(&"./duduclaw/restore-staging".to_string()), "{excludes:?}");
        assert!(excludes.contains(&"./duduclaw/restore-backup-*".to_string()), "{excludes:?}");
        assert!(excludes.iter().any(|p| p == STAGING_BASENAME_GLOB), "{excludes:?}");
    }

    #[test]
    fn schedule_backup_excludes_degrades_gracefully_on_a_root_home_dir() {
        // `home_dir.file_name()` is `None` for `/` — must not panic, and
        // must still carry the staging-file defense-in-depth pattern.
        let excludes = schedule_backup_excludes(Path::new("/"));
        assert_eq!(excludes, vec![STAGING_BASENAME_GLOB.to_string()]);
    }

    /// List entry names inside a `.tar.gz` — test-only helper for asserting
    /// on what a scheduled backup actually packed.
    fn list_tar_gz_entries(path: &Path) -> Vec<String> {
        let f = std::fs::File::open(path).unwrap();
        let gz = flate2::read::GzDecoder::new(f);
        let mut archive = tar::Archive::new(gz);
        archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// The compounding-archive fix, exercised end-to-end with the real
    /// `tar` binary: run the scheduler twice in a row (bypassing the
    /// interval due-check — `run_backup_and_rotate` is called directly,
    /// which is what `tick()` calls once it decides a run is due) and
    /// assert round 2's archive contains neither round 1's backup file nor
    /// the `backups/` directory at all.
    #[tokio::test]
    async fn second_scheduled_round_does_not_pack_the_first_rounds_backup_file() {
        let (_root, home) = scoped_home();
        std::fs::write(home.join("config.toml"), "[backup]\nschedule_enabled = true\n").unwrap();
        // A real sibling file so the archive isn't trivially empty.
        std::fs::write(home.join("agent.toml"), b"[agent]\nname=\"kiki\"\n").unwrap();

        let cfg = BackupScheduleConfig::from_home(&home);
        let sched = BackupScheduler::new(home.clone());

        let (filename1, pruned1) = sched.run_backup_and_rotate(&cfg, Utc::now()).await.unwrap();
        assert!(pruned1.is_empty());
        assert!(backups_dir(&home).join(&filename1).exists());

        let (filename2, _pruned2) = sched
            .run_backup_and_rotate(&cfg, Utc::now() + chrono::Duration::seconds(1))
            .await
            .unwrap();
        assert_ne!(filename1, filename2, "the two runs must produce distinct filenames");

        let entries2 = list_tar_gz_entries(&backups_dir(&home).join(&filename2));

        assert!(
            !entries2.iter().any(|e| e.contains(&filename1)),
            "round 2's archive must not contain round 1's backup file: {entries2:?}"
        );
        assert!(
            !entries2.iter().any(|e| e.contains("backups")),
            "round 2's archive must not contain the backups/ directory at all: {entries2:?}"
        );
        // A restore-staging leftover and a preserved-restore dir must be
        // excluded too, not just `backups/`.
        let staging = crate::backup_restore::staging_dir(&home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("leftover.txt"), b"x").unwrap();
        let preserved = home.join(format!("{}20260101T000000Z", crate::backup_restore::RESTORE_BACKUP_PREFIX));
        std::fs::create_dir_all(&preserved).unwrap();
        std::fs::write(preserved.join("old-config.toml"), b"x").unwrap();

        let (filename3, _) = sched
            .run_backup_and_rotate(&cfg, Utc::now() + chrono::Duration::seconds(2))
            .await
            .unwrap();
        let entries3 = list_tar_gz_entries(&backups_dir(&home).join(&filename3));
        assert!(
            !entries3.iter().any(|e| e.contains("restore-staging") || e.contains("leftover.txt")),
            "restore-staging must be excluded: {entries3:?}"
        );
        assert!(
            !entries3.iter().any(|e| e.contains("restore-backup-") || e.contains("old-config.toml")),
            "a preserved restore-backup-* dir must be excluded: {entries3:?}"
        );
        // The ordinary sibling file must still be there — exclusion is
        // targeted, not a blanket "pack nothing" regression.
        assert!(entries3.iter().any(|e| e.contains("agent.toml")), "{entries3:?}");
    }
}
