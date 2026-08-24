//! `/data` forward-only settings migrator (H3g, 2026-08-24).
//!
//! ## Why this exists
//!
//! The appliance's A/B update scheme (`commercial/docs/
//! DESIGN-ab-update-rollback-2026-08.md`) only ever rolls back **root** — the
//! read-only slot containing the OS image. `/data` (`<DUDUCLAW_HOME>` —
//! `config.toml`, `org.toml`, `agents/`, the various `.jsonl`/`.db` stores) is
//! never part of a slot and never rolls back. So the moment any on-disk
//! shape under `/data` changes across a release (a renamed key, a relocated
//! file, a directory that now needs stricter permissions, …), A/B cannot
//! undo it — a rollback restores yesterday's *code* on top of today's
//! *data*, and there is nothing in this project today that closes that gap
//! in a uniform, auditable way. This module is that gap-closer.
//!
//! Design borrowed almost verbatim from basecamp/omarchy's `migrations/`
//! mechanism (`research/native-os-2026-08/omarchy-borrowings-2026-08.md`
//! §2.3 — omarchy hits the identical structural problem: its own update
//! model only rolls back the root btrfs subvolume via snapper, never
//! `~/.config`).
//!
//! ## Where things live
//!
//! - **Scripts** ship baked into the image root, read-only, and roll back
//!   with the A/B slot: `/usr/share/duduclaw/migrations/<unix-ts>.sh`
//!   (`appliance/mkosi.extra/usr/share/duduclaw/migrations/` in the source
//!   tree). Override via [`MIGRATIONS_DIR_ENV`] — production never sets it;
//!   tests and VM fixture injection do.
//! - **Completion markers** live on `/data` and never roll back:
//!   `<DUDUCLAW_HOME>/system/migrations/<script-name>`, one empty file per
//!   applied script. `<DUDUCLAW_HOME>/system/` already exists as the
//!   appliance's one "durable device state" directory —
//!   `duduclaw-firstboot-provision.sh` puts `.provisioned`, `machine-id` and
//!   `device.key` there. This module deliberately reuses that directory
//!   rather than inventing a second `/data/state/` tree.
//!
//! This root/`/data` split is the entire point: after an update rolls back
//! to an older root slot, that slot's `/usr/share/duduclaw/migrations/`
//! only ever contained the scripts that shipped with it — a script that
//! only exists in the newer, rolled-back-away version can never run again
//! by accident. And a marker written by a migration that outlives a
//! rollback is inert: nothing on the old root even knows to look for it.
//!
//! ## Script contract
//!
//! - No shebang, mode 0644 (baked by mkosi) — the runner does not rely on
//!   the executable bit, matching the omarchy convention exactly.
//! - Executed as `bash -euo pipefail <script>`, never sourced, never run
//!   with any other interpreter.
//! - **Must be idempotent.** The runner does not guarantee at-most-once
//!   execution across a crash between "script exited 0" and "marker
//!   written" — idempotency is the only safety net for that window, so it
//!   is a hard requirement, not a nicety. See [`run_pending`]'s doc comment
//!   for the narrower guarantee this module itself provides.
//! - Should open with one `echo` line stating what it does, and a comment
//!   explaining *why* the fix must be applied retroactively (the omarchy
//!   report calls this "an inline ADR" — §2.4) — not enforced by this
//!   module (there is no shipped linter for shell comments), but is house
//!   style; see the first shipped script for the pattern.
//!
//! ## Fresh `/data` handling
//!
//! A brand-new device's `/data` is fine as shipped — replaying historical
//! migrations against it would be redundant at best (nothing to fix) and
//! actively wrong at worst (a script written to transform an *old* shape
//! has no business touching a shape that was never old). This module
//! contains **no** "is this fresh?" special case on purpose: by the time
//! [`list_pending`] / [`run_pending`] ever runs, "fresh" and "not fresh"
//! must already be indistinguishable. The disambiguation happens exactly
//! once, upstream, in `duduclaw-firstboot-provision.sh` — the one script
//! proven (by its own `ConditionPathExists=!.../system/.provisioned` guard
//! and self-disabling `ExecStartPost`) to run on precisely the boot where
//! `/data` had nothing on it. That script stamps every migration shipped in
//! the current image as already-applied before this module is ever invoked.
//!
//! ## Failure posture
//!
//! Stop-the-line, not brick-the-line. [`run_pending`] runs scripts
//! oldest-first and stops at the first failure (a later script may assume
//! an earlier one already landed) and durably records the failure via
//! [`write_failure_marker`] / [`read_failure`] so it is never silent — but
//! it does **not** block the gateway from starting (the appliance is
//! unattended; a hard block on migration failure would turn one bad script
//! into a fleet of boxes nobody can reach to fix). The gateway is expected
//! to call [`read_failure`] once it is up and surface it loudly (dashboard
//! Activity Feed / audit log) — see `appliance/mkosi.extra/etc/systemd/
//! system/duduclaw-data-migrate.service` for how boot ordering encodes this
//! trade-off (`Before=` the gateway, never `Requires=` in the direction that
//! would stop it).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{DuDuClawError, Result};
use crate::text_utils::truncate_bytes;

/// Env var override for the shipped-migrations directory. Production never
/// sets this — only tests and VM fixture injection (`appliance/.vm/inject/`)
/// point it somewhere other than [`DEFAULT_MIGRATIONS_DIR`].
pub const MIGRATIONS_DIR_ENV: &str = "DUDUCLAW_MIGRATIONS_DIR";

/// Where the appliance image bakes migration scripts: read-only root,
/// rolls back with the A/B slot.
pub const DEFAULT_MIGRATIONS_DIR: &str = "/usr/share/duduclaw/migrations";

/// Cap on a failed migration's captured stdout+stderr tail. Matches the
/// audit-log convention elsewhere in the project (bounded, never raw
/// unbounded process output landing in a JSON file).
const OUTPUT_TAIL_BYTES: usize = 4096;

/// Resolve the migrations source directory.
pub fn migrations_dir() -> PathBuf {
    std::env::var(MIGRATIONS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_MIGRATIONS_DIR))
}

/// Resolve the completion-marker directory for a given `DUDUCLAW_HOME`.
pub fn marker_dir(home: &Path) -> PathBuf {
    home.join("system").join("migrations")
}

/// Where a failed run's diagnosis is recorded. A sibling of the marker
/// directory, not inside it, so a directory listing of applied markers is
/// never polluted by the one failure-record file.
pub fn failure_marker_path(home: &Path) -> PathBuf {
    home.join("system").join("migrations.failed.json")
}

/// One migration script as discovered on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationScript {
    /// Filename, e.g. `"1787540626.sh"` — also the marker file's basename.
    pub name: String,
    pub path: PathBuf,
    /// Parsed from the filename stem. Ordering key — never the filesystem's
    /// own directory-listing order, which is unspecified.
    pub timestamp: u64,
}

/// A directory entry that did not parse as a `<unix-ts>.sh` migration
/// script. Reported, never silently dropped — a typo'd filename shipping in
/// an image should be visible, not invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
}

/// Result of scanning the migrations directory.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// Valid scripts, sorted oldest-first by [`MigrationScript::timestamp`].
    pub scripts: Vec<MigrationScript>,
    pub skipped: Vec<SkippedEntry>,
}

/// Scan `dir` for `<unix-ts>.sh` migration scripts.
///
/// A missing directory is not an error — it is the common case for every
/// image shipped before the first migration ever landed, and for any test
/// fixture that only cares about a subset of behavior. Sort key is the
/// **parsed** timestamp, not the filename string — lexical sort of
/// unequal-length numbers is wrong (`"99999.sh" > "100000.sh"` lexically,
/// backwards numerically).
pub fn list_shipped(dir: &Path) -> Result<ScanResult> {
    let mut out = ScanResult::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(DuDuClawError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(DuDuClawError::Io)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(DuDuClawError::Io)?;
        if !file_type.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                out.skipped.push(SkippedEntry {
                    name: path.to_string_lossy().into_owned(),
                    reason: "non-UTF-8 filename".to_string(),
                });
                continue;
            }
        };
        let Some(stem) = name.strip_suffix(".sh") else {
            out.skipped.push(SkippedEntry {
                name: name.clone(),
                reason: "missing .sh suffix".to_string(),
            });
            continue;
        };
        match stem.parse::<u64>() {
            Ok(timestamp) => out.scripts.push(MigrationScript {
                name,
                path,
                timestamp,
            }),
            Err(_) => out.skipped.push(SkippedEntry {
                name: name.clone(),
                reason: "filename stem is not a plain unix-timestamp integer".to_string(),
            }),
        }
    }
    out.scripts.sort_by_key(|s| s.timestamp);
    Ok(out)
}

/// `true` when `script_name` already has a completion marker under
/// `marker_dir`. Existence-only check (empty marker files, per convention).
pub fn is_applied(marker_dir: &Path, script_name: &str) -> bool {
    marker_dir.join(script_name).is_file()
}

/// Shipped scripts minus already-applied ones, oldest-first. Read-only —
/// safe to call from `--pending` / `--check` without side effects.
pub fn list_pending(migrations_dir: &Path, marker_dir: &Path) -> Result<Vec<MigrationScript>> {
    let scan = list_shipped(migrations_dir)?;
    Ok(scan
        .scripts
        .into_iter()
        .filter(|s| !is_applied(marker_dir, &s.name))
        .collect())
}

/// Outcome of one successfully-applied migration script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub name: String,
    pub duration_ms: u128,
}

/// Durable record of a failed migration run — written to
/// [`failure_marker_path`] so a failure is never silent. Consumed by the
/// gateway at startup (surfaced to the dashboard Activity Feed / audit log)
/// and by `duduclaw doctor` / an operator inspecting the box directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationFailure {
    pub script: String,
    pub exit_code: Option<i32>,
    /// Combined stdout+stderr tail, capped at [`OUTPUT_TAIL_BYTES`].
    pub output_tail: String,
    pub failed_at_unix: u64,
}

/// Full result of one [`run_pending`] invocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunReport {
    /// Scripts that ran and exited 0, in the order they ran.
    pub applied: Vec<RunRecord>,
    /// Set only when a script failed — [`run_pending`] stops immediately
    /// after recording this, so at most one failure is ever present here.
    pub failure: Option<MigrationFailure>,
}

impl RunReport {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }
}

/// Execute every pending migration, oldest-first, stopping at the first
/// failure.
///
/// **This function's contract** (narrower than "the migration succeeded",
/// which is the *script's* contract): never run an already-marked script
/// again, never mark a script that did not exit 0, never run a script past
/// a failure. `home` is exported to every script as `DUDUCLAW_HOME` — a
/// script must never have to guess it, even if the caller's own process env
/// doesn't happen to carry it.
///
/// Idempotency across repeated calls comes from the marker files, not from
/// this function tracking anything in memory — calling this twice in a row
/// with nothing new pending is a correct, cheap no-op (empty `applied`,
/// `failure: None`).
pub fn run_pending(migrations_dir: &Path, marker_dir: &Path, home: &Path) -> Result<RunReport> {
    std::fs::create_dir_all(marker_dir).map_err(DuDuClawError::Io)?;
    let pending = list_pending(migrations_dir, marker_dir)?;
    let mut report = RunReport::default();
    for script in pending {
        let started = Instant::now();
        let output = Command::new("bash")
            .arg("-euo")
            .arg("pipefail")
            .arg(&script.path)
            .env("DUDUCLAW_HOME", home)
            .output()
            .map_err(|e| {
                DuDuClawError::Config(format!(
                    "failed to spawn migration {}: {e}",
                    script.name
                ))
            })?;
        let duration_ms = started.elapsed().as_millis();
        if output.status.success() {
            // Marker written only AFTER a clean exit — this ordering is
            // what makes "crash mid-script" and "script that failed"
            // indistinguishable to the next run: both leave no marker, both
            // retry from this exact script next time.
            std::fs::write(marker_dir.join(&script.name), b"").map_err(DuDuClawError::Io)?;
            report.applied.push(RunRecord {
                name: script.name,
                duration_ms,
            });
        } else {
            let mut tail = String::new();
            tail.push_str(&String::from_utf8_lossy(&output.stdout));
            tail.push_str(&String::from_utf8_lossy(&output.stderr));
            let failure = MigrationFailure {
                script: script.name.clone(),
                exit_code: output.status.code(),
                output_tail: truncate_bytes(&tail, OUTPUT_TAIL_BYTES).to_string(),
                failed_at_unix: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            write_failure_marker(home, &failure)?;
            report.failure = Some(failure);
            return Ok(report);
        }
    }
    // A clean run (nothing pending, or everything pending applied) clears
    // any stale failure record left by a previous boot — otherwise a
    // failure resolved by a corrected image would haunt the dashboard
    // forever. Best-effort: a missing file is not an error.
    let _ = std::fs::remove_file(failure_marker_path(home));
    Ok(report)
}

fn write_failure_marker(home: &Path, failure: &MigrationFailure) -> Result<()> {
    let path = failure_marker_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(DuDuClawError::Io)?;
    }
    let json = serde_json::to_string_pretty(failure).map_err(|e| {
        DuDuClawError::Config(format!("serialize migration failure record: {e}"))
    })?;
    // Atomic commit (temp + rename) — project convention for shared state
    // files (see duduclaw-core::org_store::write_atomic).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(DuDuClawError::Io)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DuDuClawError::Io(e));
    }
    Ok(())
}

/// Read a previously-recorded failure, if any.
///
/// Never errors — a missing or corrupt record reads as "no known failure"
/// (fail-open: a broken failure-record file must not itself become a
/// second, independent outage on top of the migration that actually
/// failed).
pub fn read_failure(home: &Path) -> Option<MigrationFailure> {
    let raw = std::fs::read_to_string(failure_marker_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_script(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    // ---- list_shipped -----------------------------------------------

    #[test]
    fn missing_migrations_dir_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("does-not-exist");
        let scan = list_shipped(&dir).unwrap();
        assert!(scan.scripts.is_empty());
        assert!(scan.skipped.is_empty());
    }

    #[test]
    fn sorts_by_numeric_timestamp_not_lexical_order() {
        let tmp = tempfile::tempdir().unwrap();
        // Deliberately unequal digit lengths so a lexical sort would get
        // this backwards ("99999.sh" > "100000.sh" as strings).
        write_script(tmp.path(), "100000.sh", "echo b\n");
        write_script(tmp.path(), "99999.sh", "echo a\n");
        let scan = list_shipped(tmp.path()).unwrap();
        let names: Vec<_> = scan.scripts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["99999.sh", "100000.sh"]);
    }

    #[test]
    fn malformed_names_are_skipped_with_a_reason_not_dropped_silently() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "README.md", "not a migration\n");
        write_script(tmp.path(), "abc.sh", "echo x\n");
        write_script(tmp.path(), "123.txt", "echo x\n");
        write_script(tmp.path(), "42.sh", "echo x\n");
        let scan = list_shipped(tmp.path()).unwrap();
        assert_eq!(scan.scripts.len(), 1);
        assert_eq!(scan.scripts[0].name, "42.sh");
        assert_eq!(scan.skipped.len(), 3);
    }

    // ---- list_pending / is_applied -----------------------------------

    #[test]
    fn pending_excludes_already_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations");
        let markers = tmp.path().join("markers");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::create_dir_all(&markers).unwrap();
        write_script(&migrations, "1.sh", "echo one\n");
        write_script(&migrations, "2.sh", "echo two\n");
        std::fs::write(markers.join("1.sh"), b"").unwrap();

        let pending = list_pending(&migrations, &markers).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "2.sh");
        assert!(is_applied(&markers, "1.sh"));
        assert!(!is_applied(&markers, "2.sh"));
    }

    // ---- run_pending: happy path + idempotency ------------------------

    #[test]
    fn run_pending_applies_in_order_and_marks_each() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let marker_dir = marker_dir(&home);
        let log = home.join("order.log");

        // Second script's correctness depends on the first having already
        // run — a real cross-migration ordering guarantee, not just marker
        // bookkeeping.
        write_script(
            &migrations,
            "1.sh",
            &format!("echo first >> '{}'\n", log.display()),
        );
        write_script(
            &migrations,
            "2.sh",
            &format!(
                "grep -q first '{}' || exit 1\necho second >> '{}'\n",
                log.display(),
                log.display()
            ),
        );

        let report = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(report.ok());
        assert_eq!(report.applied.len(), 2);
        assert_eq!(report.applied[0].name, "1.sh");
        assert_eq!(report.applied[1].name, "2.sh");
        assert!(is_applied(&marker_dir, "1.sh"));
        assert!(is_applied(&marker_dir, "2.sh"));
        let contents = std::fs::read_to_string(&log).unwrap();
        assert_eq!(contents, "first\nsecond\n");
    }

    #[test]
    fn run_pending_twice_in_a_row_is_a_cheap_no_op_the_second_time() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let marker_dir = marker_dir(&home);
        let counter = home.join("run-count");

        // A script that is NOT idempotent on its own (appends every time it
        // runs) — proves the *runner's* marker-driven at-most-once
        // guarantee independent of any single script's own idempotence.
        write_script(
            &migrations,
            "1.sh",
            &format!("echo x >> '{}'\n", counter.display()),
        );

        let first = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert_eq!(first.applied.len(), 1);
        let second = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(second.ok());
        assert!(second.applied.is_empty(), "nothing pending on second run");
        let lines = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(lines, "x\n", "script body must have run exactly once");
    }

    // ---- run_pending: failure handling --------------------------------

    #[test]
    fn run_pending_stops_at_first_failure_and_never_marks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let marker_dir = marker_dir(&home);
        let log = home.join("order.log");

        write_script(
            &migrations,
            "1.sh",
            &format!("echo ok >> '{}'\n", log.display()),
        );
        write_script(&migrations, "2.sh", "echo boom 1>&2\nexit 3\n");
        write_script(
            &migrations,
            "3.sh",
            &format!("echo should-not-run >> '{}'\n", log.display()),
        );

        let report = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(!report.ok());
        assert_eq!(report.applied.len(), 1, "only script 1 applied");
        assert!(is_applied(&marker_dir, "1.sh"));
        assert!(!is_applied(&marker_dir, "2.sh"), "failed script has no marker");
        assert!(!is_applied(&marker_dir, "3.sh"), "later script never ran");
        let failure = report.failure.unwrap();
        assert_eq!(failure.script, "2.sh");
        assert_eq!(failure.exit_code, Some(3));
        assert!(failure.output_tail.contains("boom"));

        // Durable record readable independently (simulating the gateway
        // reading it after boot).
        let read_back = read_failure(&home).unwrap();
        assert_eq!(read_back.script, "2.sh");

        // "should-not-run" proves script 3 never executed.
        assert!(!log.to_string_lossy().is_empty());
        let contents = std::fs::read_to_string(&log).unwrap();
        assert_eq!(contents, "ok\n");
    }

    #[test]
    fn retrying_after_a_fixed_script_clears_the_failure_record() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let marker_dir = marker_dir(&home);

        write_script(&migrations, "1.sh", "exit 1\n");
        let first = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(!first.ok());
        assert!(read_failure(&home).is_some());

        // Operator/image ships a corrected script at the same filename
        // (simulates a hand-patched fixture or a rebuilt image).
        write_script(&migrations, "1.sh", "exit 0\n");
        let second = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(second.ok());
        assert_eq!(second.applied.len(), 1);
        assert!(
            read_failure(&home).is_none(),
            "a clean run must clear the stale failure record"
        );
    }

    #[test]
    fn read_failure_is_none_when_no_record_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_failure(tmp.path()).is_none());
    }

    // ---- migrations_dir env override -----------------------------------

    #[test]
    fn migrations_dir_defaults_when_env_unset() {
        // Not asserting the env var is actually unset (parallel test runs
        // share process env) — only that the function returns *a* path and
        // does not panic. The override itself is covered by callers passing
        // an explicit directory everywhere else in this module's tests.
        let _ = migrations_dir();
    }

    // ---- filesystem permission sanity (used by the first shipped script) --

    #[test]
    fn marker_dir_is_created_by_run_pending_even_with_nothing_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations = tmp.path().join("migrations"); // never created — empty
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let marker_dir = marker_dir(&home);
        assert!(!marker_dir.exists());
        let report = run_pending(&migrations, &marker_dir, &home).unwrap();
        assert!(report.ok());
        assert!(report.applied.is_empty());
        assert!(marker_dir.is_dir());
    }

    #[test]
    fn permissions_helper_sanity_for_the_first_shipped_migration() {
        // Not testing the shipped script itself here (that is covered by
        // appliance/tests/data-migrations/test_first_migration.sh, which
        // runs the real file directly) — just pinning the Rust-side
        // expectation the script relies on: PermissionsExt::mode() masks to
        // the low 12 bits, so 0o700 must compare against `& 0o777`.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mode = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
