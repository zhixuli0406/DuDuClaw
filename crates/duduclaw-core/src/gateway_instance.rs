//! One gateway per data directory (F5-A, R-M1).
//!
//! Boot reconciliation releases every workflow run lease and puts unsettled
//! workflow queue messages back to pending. That is only safe when no other
//! gateway is executing runs out of the same home, so the gateway takes an
//! exclusive lock on `<home>/locks/gateway.lock` for its whole lifetime and
//! does the reconciliation, the workflow dispatch and the workflow sweep only
//! while it holds it. A second gateway started on the same home does none of
//! those, logs why, and leaves `<home>/locks/gateway.lock.refused` for
//! `duduclaw doctor`. The lock is an advisory `flock` held through one open
//! file for the life of the process; it is released when the process exits,
//! even on a crash.
//!
//! Locks are per home: two homes in one process (tests) never block each
//! other, and the same home locked twice in one process is refused like a
//! second process would be.
//!
//! The holder line (`pid=… since=…`) lives in the sidecar
//! `<home>/locks/gateway.lock.holder`, not in the lock file itself: on
//! Windows `LockFileEx` guards the locked byte range against every other
//! handle, so a holder line written into the lock file could not be read by
//! `duduclaw doctor`, by a refused second gateway, or by this module's own
//! `status` (the 2026-10-06 Windows CI failure of
//! `one_holder_per_home_and_homes_do_not_block_each_other`). The lock file
//! stays empty.
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::OpenOptions;
use crate::owner_lock::OwnerLock;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const LOCK_FILE: &str = "gateway.lock";
const HOLDER_FILE: &str = "gateway.lock.holder";
const REFUSED_FILE: &str = "gateway.lock.refused";

fn held_locks() -> &'static Mutex<HashMap<PathBuf, OwnerLock>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, OwnerLock>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(home: &Path) -> PathBuf {
    std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())
}

pub fn lock_path(home: &Path) -> PathBuf {
    home.join("locks").join(LOCK_FILE)
}

pub fn refused_marker_path(home: &Path) -> PathBuf {
    home.join("locks").join(REFUSED_FILE)
}

/// Where the current (or last) holder's `pid=… since=…` line is written.
pub fn holder_path(home: &Path) -> PathBuf {
    home.join("locks").join(HOLDER_FILE)
}

/// Take this home's gateway lock for the rest of the process. `Ok` when this
/// process now holds it (or already did); `Err` names why not, and leaves the
/// refusal marker for `duduclaw doctor`.
pub fn acquire(home: &Path) -> Result<(), String> {
    acquire_inner(home, true)
}

/// Take the same process-lifetime singleton lock before applying a restore.
/// A competing holder must leave the pending restore and live home untouched,
/// including the diagnostic refusal marker. The restore must retain `locks/`
/// in place so this handle continues to exclude later gateway starts.
pub fn acquire_for_restore(home: &Path) -> Result<(), String> {
    acquire_inner(home, false)
}

fn acquire_inner(home: &Path, record_refusal: bool) -> Result<(), String> {
    // Canonicalize only after creation. A missing home under a relative or
    // symlinked parent otherwise gets a different registry key after locking,
    // so held()/the next acquire would fail to recognize our own handle.
    crate::config::create_home_dir(home).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(home.join("locks")).map_err(|e| e.to_string())?;
    let key = key(home);
    let mut held = held_locks()
        .lock()
        .map_err(|_| "gateway lock registry poisoned")?;
    if held.contains_key(&key) {
        return Ok(());
    }
    let result = (|| -> Result<std::fs::File, String> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path(home))
            .map_err(|e| e.to_string())?;
        // A read-only probe may briefly hold a shared lock. Give it time to
        // finish before refusing this gateway (five attempts over 250 ms).
        for attempt in 0..5 {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) => {
                    let contended = error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error();
                    if contended && attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_micros(62_500));
                        continue;
                    }
                    if !contended {
                        return Err(error.to_string());
                    }
                    let holder = std::fs::read_to_string(holder_path(home)).unwrap_or_default();
                    return Err(format!(
                        "another gateway already runs on this data directory ({})",
                        holder.trim()
                    ));
                }
            }
        }
        // The holder line goes to the sidecar (see the module doc): readers
        // on Windows cannot see bytes inside the locked file.
        std::fs::write(
            holder_path(home),
            format!(
                "pid={} since={}\n",
                std::process::id(),
                chrono::Utc::now().to_rfc3339()
            ),
        )
        .map_err(|e| e.to_string())?;
        Ok(file)
    })();
    match result {
        Ok(file) => {
            let _ = std::fs::remove_file(refused_marker_path(home));
            held.insert(key, OwnerLock::new(file));
            Ok(())
        }
        Err(error) => {
            if record_refusal {
                let _ = std::fs::create_dir_all(home.join("locks"));
                let _ = std::fs::write(
                    refused_marker_path(home),
                    format!(
                        "pid={} at={} reason={error}\n",
                        std::process::id(),
                        chrono::Utc::now().to_rfc3339()
                    ),
                );
            }
            Err(error)
        }
    }
}

/// Whether this process holds the gateway lock of `home`.
pub fn held(home: &Path) -> bool {
    held_locks()
        .lock()
        .map(|h| h.contains_key(&key(home)))
        .unwrap_or(false)
}

/// Whether some process currently holds `home`'s gateway lock (checked from
/// another process, e.g. `duduclaw doctor`).
pub fn locked_elsewhere(home: &Path) -> bool {
    probe(home).unwrap_or(false)
}

/// Probe whether the gateway lock exists and is held, without creating files.
/// Open read-only and briefly share the lock, never taking an exclusive lock.
/// `None` means the lock could not be inspected for a reason other than absence.
pub fn probe(home: &Path) -> Option<bool> {
    let file = match OpenOptions::new().read(true).open(lock_path(home)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {}
        _ => return None,
    }
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            Some(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Some(true),
        Err(_) => None,
    }
}

/// What `duduclaw doctor` reports: the current holder line (if any) and the
/// last refusal (if any). A refusal older than the current holder's start is
/// stale but still shown, so the operator learns a second start happened.
pub fn status(home: &Path) -> (Option<String>, Option<String>) {
    let read = |p: PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    (read(holder_path(home)), read(refused_marker_path(home)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_absent_unlocked_and_exclusively_held_locks() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(probe(home.path()), Some(false));
        assert!(!lock_path(home.path()).exists());
        std::fs::create_dir_all(home.path().join("locks")).unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path(home.path()))
            .unwrap();
        assert_eq!(probe(home.path()), Some(false));
        assert!(!locked_elsewhere(home.path()));
        file.try_lock_exclusive().unwrap();
        assert_eq!(probe(home.path()), Some(true));
        assert!(locked_elsewhere(home.path()));
        fs2::FileExt::unlock(&file).unwrap();
        assert_eq!(probe(home.path()), Some(false));
    }

    #[test]
    fn probe_reports_unknown_when_lock_path_is_a_directory() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("locks/gateway.lock")).unwrap();

        assert_eq!(probe(home.path()), None);
    }

    #[test]
    fn acquire_waits_for_a_brief_shared_probe_without_recording_refusal() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("locks")).unwrap();
        std::fs::File::create(lock_path(home.path())).unwrap();
        let reader = OpenOptions::new().read(true).open(lock_path(home.path())).unwrap();
        fs2::FileExt::try_lock_shared(&reader).unwrap();
        // Concurrent probes can share the same read-only lock.
        assert_eq!(probe(home.path()), Some(false));
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            fs2::FileExt::unlock(&reader).unwrap();
        });
        let result = acquire(home.path());
        release.join().unwrap();
        result.unwrap();
        assert!(held(home.path()));
        assert!(!refused_marker_path(home.path()).exists());
    }

    #[test]
    fn a_new_home_is_registered_under_its_created_path() {
        let parent = tempfile::tempdir().unwrap();
        let home = parent.path().join("new-home");
        acquire_for_restore(&home).unwrap();
        assert!(held(&home));
        let canonical = std::fs::canonicalize(&home).unwrap();
        assert!(held(&canonical));
        acquire(&canonical).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_new_home_under_a_symlink_keeps_one_registry_key() {
        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        let alias = parent.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let home = alias.join("new-home");
        acquire_for_restore(&home).unwrap();
        assert!(held(&home));
        assert!(held(&real.join("new-home")));
        acquire(&real.join("new-home")).unwrap();
    }

    #[test]
    fn one_holder_per_home_and_homes_do_not_block_each_other() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        acquire(a.path()).unwrap();
        acquire(a.path()).unwrap(); // idempotent in the holding process
        acquire(b.path()).unwrap();
        assert!(held(a.path()) && held(b.path()));
        // A second open file description on the same lock is refused, exactly
        // as a second gateway process would be.
        let other = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path(a.path()))
            .unwrap();
        assert!(other.try_lock_exclusive().is_err());
        let (holder, refused) = status(a.path());
        assert!(
            holder
                .unwrap()
                .contains(&format!("pid={}", std::process::id()))
        );
        assert!(refused.is_none());
    }

    #[test]
    fn a_home_held_elsewhere_is_refused_and_marked_for_doctor() {
        let c = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(c.path().join("locks")).unwrap();
        // Stands in for the first gateway process.
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path(c.path()))
            .unwrap();
        first.try_lock_exclusive().unwrap();
        assert_eq!(probe(c.path()), Some(true));
        let err = acquire(c.path()).unwrap_err();
        assert!(err.contains("another gateway"), "{err}");
        assert!(!held(c.path()));
        assert_eq!(probe(c.path()), Some(true));
        assert!(status(c.path()).1.unwrap().contains("another gateway"));
        drop(first);
        // A forked test child can briefly retain an inherited copy of the flock until exec.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let result = acquire(c.path());
            if result.is_ok() || std::time::Instant::now() >= deadline {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        result.unwrap();
        assert!(
            status(c.path()).1.is_none(),
            "a later successful start clears the marker"
        );
    }
}
