//! Bounded garbage collection for `<home>/session_pending/`.
//!
//! Runs from the gateway maintenance loop (`server.rs`, every 10 minutes) and
//! only in the process holding `<home>/locks/gateway.lock`. One pass handles
//! no more than [`MAX_GC_ENTRIES`] directory entries and never waits for a
//! lock: a ledger or sidecar somebody is using right now is left for the next
//! pass. Passes resume: each reads the entries whose names sort after the
//! last name the previous pass handled ([`SafeDir::list_after`] — only the
//! kept names are allocated and inspected), and wraps to the start once the
//! end is reached, so successive passes cover the whole directory however
//! large it is. A directory within one pass's bound is covered by every pass.
//! The cursor lives in memory per home; a restart begins at the start again.
//! Removed: ledgers whose items have all expired (or that no longer parse),
//! ledgers of the earlier unreleased name format (owned by nobody and read
//! by no code path), lock sidecars nobody holds (orphans left by a crash,
//! including the admission lock), and write temp files older than
//! [`TEMP_MAX_AGE`]. Entries the safe open refuses (symlink, FIFO, hard link,
//! foreign owner) are counted and left alone.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::{prune, store};
use crate::fs_safe::{EntryKind, SafeDir};

pub(super) const MAX_GC_ENTRIES: usize = store::MAX_SCAN_ENTRIES;
const TEMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Where the next pass for a home resumes (absent: from the start).
static CURSORS: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GcReport {
    pub removed_ledgers: usize,
    pub rewritten_ledgers: usize,
    pub removed_locks: usize,
    pub removed_temps: usize,
    pub busy: usize,
    pub refused: usize,
}

/// One GC pass at `now` (unix seconds), resuming where the previous pass for
/// `home` stopped. No-op when the directory is absent.
pub(super) fn collect(home: &Path, now: i64) -> io::Result<GcReport> {
    let start = CURSORS.lock().ok().and_then(|mut cursors| cursors.remove(home));
    let (report, next) = pass(home, now, MAX_GC_ENTRIES, start.as_deref())?;
    if let Some(next) = next {
        if let Ok(mut cursors) = CURSORS.lock() {
            cursors.insert(home.to_path_buf(), next);
        }
    }
    Ok(report)
}

/// One pass over at most `limit` entries sorting after `cursor`. Returns the
/// report and the cursor for the next pass (`None`: the end was reached).
pub(super) fn pass(home: &Path, now: i64, limit: usize, cursor: Option<&str>) -> io::Result<(GcReport, Option<String>)> {
    let mut report = GcReport::default();
    let Some(dir) = store::open_dir(home, false)? else { return Ok((report, None)) };
    let (entries, next) = dir.list_after(cursor, limit)?;
    if next.is_some() {
        tracing::warn!(limit, "pending ledger GC read only part of the directory; the next pass resumes after it");
    }
    for entry in entries {
        if entry.kind != EntryKind::File {
            continue;
        }
        let name = entry.name.as_str();
        if store::is_ledger_name(name) {
            ledger(&dir, name, now, false, &mut report);
        } else if store::is_legacy_ledger_name(name) {
            ledger(&dir, name, now, true, &mut report);
        } else if name == store::ADMISSION_LOCK || store::is_lock_name(name) {
            orphan_lock(&dir, name, &mut report);
        } else if store::is_temp_name(name) {
            temp(&dir, name, &mut report);
        }
    }
    if report.refused > 0 {
        tracing::warn!(refused = report.refused, "pending ledger GC left entries that failed the safe-open checks");
    }
    Ok((report, next))
}

fn busy_or_refused(error: &io::Error, report: &mut GcReport) {
    if error.kind() == io::ErrorKind::TimedOut {
        report.busy += 1;
    } else {
        report.refused += 1;
    }
}

/// Expire one ledger under its lock (zero wait). `legacy`: a name of the
/// earlier format, which no reader can reach, so it is removed whatever it
/// holds (still only after the safe open accepted it).
fn ledger(dir: &SafeDir, name: &str, now: i64, legacy: bool, report: &mut GcReport) {
    let lock = match store::lock(dir, &store::lock_name(name), true, Duration::ZERO) {
        Ok(Some(lock)) => lock,
        Ok(None) => return,
        Err(error) => return busy_or_refused(&error, report),
    };
    let outcome = (|| -> io::Result<()> {
        let mut items = match store::read_items(dir, name) {
            Ok(Some(items)) => items,
            Ok(None) => return Ok(()),
            // Our own verified file that no longer parses holds nothing usable.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                store::remove_ledger(dir, name)?;
                report.removed_ledgers += 1;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let before = items.len();
        prune(&mut items, now);
        if legacy || items.is_empty() {
            store::remove_ledger(dir, name)?;
            report.removed_ledgers += 1;
        } else if items.len() != before {
            store::write_items(dir, name, &items)?;
            report.rewritten_ledgers += 1;
        }
        Ok(())
    })();
    lock.release(dir);
    if outcome.is_err() {
        report.refused += 1;
    }
}

/// A sidecar is live exactly while someone holds it; a non-blocking try that
/// succeeds proves it is an orphan, and release unlinks it.
fn orphan_lock(dir: &SafeDir, name: &str, report: &mut GcReport) {
    match store::lock(dir, name, false, Duration::ZERO) {
        Ok(Some(lock)) => {
            lock.release(dir);
            if dir.entry(name).ok().flatten().is_none() {
                report.removed_locks += 1;
            }
        }
        Ok(None) => {}
        Err(error) => busy_or_refused(&error, report),
    }
}

fn temp(dir: &SafeDir, name: &str, report: &mut GcReport) {
    let modified = match store::open_private_file(dir, name) {
        Ok(Some(file)) => file.metadata().and_then(|m| m.modified()),
        Ok(None) => return,
        Err(_) => {
            report.refused += 1;
            return;
        }
    };
    let Ok(modified) = modified else { return };
    let old = SystemTime::now().duration_since(modified).is_ok_and(|age| age >= TEMP_MAX_AGE);
    if old && dir.remove_file(name).is_ok() {
        report.removed_temps += 1;
    }
}
