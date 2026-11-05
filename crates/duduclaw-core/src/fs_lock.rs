//! Cross-process advisory file locking.
//!
//! Multiple processes (the Rust gateway plus Python channel adapters) append to
//! shared JSONL files such as `bus_queue.jsonl`. Records larger than `PIPE_BUF`
//! can interleave on concurrent `append`, producing malformed lines that the
//! consumer silently drops. Wrapping the critical section in a cross-process
//! advisory lock (via `fs2`) serializes writers without changing the file
//! format.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    static LOCK_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The active caller-owned bound, if this thread is inside a bounded operation.
pub fn current_lock_deadline() -> Option<Instant> { LOCK_DEADLINE.with(Cell::get) }

/// Bounds synchronous file-lock waits inside a caller-owned operation. The
/// migration bridge polls its future on this same blocking thread, so nested
/// normal product writers (including history writes) inherit the deadline.
/// Nested scopes can only shorten it. Nothing is detached on expiry.
pub fn with_lock_deadline<T>(deadline: Instant, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Instant>);
    impl Drop for Restore { fn drop(&mut self) { LOCK_DEADLINE.with(|slot| slot.set(self.0)); } }
    let previous = LOCK_DEADLINE.with(|slot| {
        let previous = slot.get();
        slot.set(Some(previous.map_or(deadline, |old| old.min(deadline))));
        previous
    });
    let _restore = Restore(previous);
    f()
}


/// Run `f` while holding an exclusive cross-process advisory lock keyed on
/// `path`. The lock is taken on a sidecar `<path>.lock` file (created if
/// absent) so it is independent of how the data file itself is opened. The lock
/// is always released when this function returns, including on error or panic
/// (the `File` guard unlocks on drop).
///
/// Blocks until the lock can be acquired.
pub fn with_file_lock<T>(path: &Path, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let lock = acquire(path)?;
    // `lock` (the File) holds the OS lock until dropped at end of scope.
    let result = f();
    // Best-effort explicit unlock; drop would also do it.
    let _ = FileExt::unlock(&lock);
    result
}

fn acquire(path: &Path) -> io::Result<File> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    if let Some(deadline) = LOCK_DEADLINE.with(Cell::get) {
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "file lock deadline exceeded"));
            }
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(5)));
                }
                Err(error) => return Err(error),
            }
        }
    } else {
        file.lock_exclusive()?;
    }
    Ok(file)
}

fn lock_path_for(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lock_serializes_and_runs_closure() {
        let dir = std::env::temp_dir();
        let target = dir.join("duduclaw_fs_lock_test.jsonl");
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(lock_path_for(&target));

        let res = with_file_lock(&target, || {
            let mut fobj = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target)?;
            fobj.write_all(b"{\"a\":1}\n")?;
            Ok::<_, io::Error>(42)
        })
        .unwrap();
        assert_eq!(res, 42);

        // Re-entrant acquire after release must succeed (lock was dropped).
        with_file_lock(&target, || Ok::<_, io::Error>(())).unwrap();

        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents, "{\"a\":1}\n");
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(lock_path_for(&target));
    }

    #[test]
    fn error_in_closure_still_releases_lock() {
        let target = std::env::temp_dir().join("duduclaw_fs_lock_err_test.jsonl");
        let _ = std::fs::remove_file(lock_path_for(&target));
        let r: io::Result<()> =
            with_file_lock(&target, || Err(io::Error::new(io::ErrorKind::Other, "boom")));
        assert!(r.is_err());
        // Lock must be free again.
        with_file_lock(&target, || Ok::<_, io::Error>(())).unwrap();
        let _ = std::fs::remove_file(lock_path_for(&target));
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn contended_writer_returns_at_deadline_without_running_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        let held = acquire(&path).unwrap();
        let deadline = Instant::now() + Duration::from_millis(20);
        let result = with_lock_deadline(deadline, || with_file_lock(&path, || {
            panic!("a contended migration writer must not run");
            #[allow(unreachable_code)] Ok(())
        }));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(Instant::now().duration_since(deadline) < Duration::from_secs(1));
        drop(held);
        with_file_lock(&path, || Ok(())).unwrap();
    }

    #[test]
    fn deadline_scope_restores_after_panic_and_nested_scope_cannot_extend_it() {
        let deadline = Instant::now() - Duration::from_millis(1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        assert!(std::panic::catch_unwind(|| with_lock_deadline(deadline, || {
            with_lock_deadline(Instant::now() + Duration::from_secs(60), || {
                assert_eq!(with_file_lock(&path, || Ok(())).unwrap_err().kind(), io::ErrorKind::TimedOut);
            });
            panic!("injected panic");
        })).is_err());
        with_file_lock(&path, || Ok(())).unwrap();
    }
}
