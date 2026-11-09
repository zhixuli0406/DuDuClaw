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

/// An exclusive cross-process advisory lock held for as long as the guard
/// lives (same sidecar `<path>.lock` scheme as [`with_file_lock`]). For a
/// critical section that cannot be one closure — e.g. one spanning `.await`
/// points. Unlocked on drop.
#[derive(Debug)]
pub struct FileLockGuard {
    file: File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Take the lock [`with_file_lock`] would take, returning a guard instead of
/// running a closure. Blocks until the lock can be acquired. Two guards for
/// the same path conflict even inside one process (each holds its own open
/// file description), so the lock is not re-entrant.
pub fn lock_exclusive(path: &Path) -> io::Result<FileLockGuard> {
    Ok(FileLockGuard { file: acquire(path)? })
}

/// [`lock_exclusive`] with a **hard upper bound** on the wait: when the lock
/// is not acquired within `wait`, an [`io::ErrorKind::TimedOut`] error
/// (message starting `lock_wait_timeout`) is returned and nothing is held.
/// RM-027 round 6: for a holder of the global org lock that must keep a
/// per-agent file lock across several steps.
pub fn lock_exclusive_within(path: &Path, wait: std::time::Duration) -> io::Result<FileLockGuard> {
    Ok(FileLockGuard { file: acquire_within(path, wait)? })
}

/// [`with_file_lock`] with a **hard upper bound** on the wait: when the lock
/// is not acquired within `wait`, `f` does not run and an
/// [`io::ErrorKind::TimedOut`] error is returned. For a caller that must not
/// wait unboundedly while it holds another lock (RM-027 round 5: the
/// `agent.toml` mirror write under the global org lock).
pub fn with_file_lock_timeout<T>(
    path: &Path,
    wait: std::time::Duration,
    f: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let lock = acquire_within(path, wait)?;
    let result = f();
    let _ = FileExt::unlock(&lock);
    result
}

/// Is `error` the "someone else holds it" answer of a non-blocking lock try?
fn is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
}

/// Unbounded acquire, unless the thread is inside [`with_lock_deadline`]: then
/// the wait is bounded by that caller-owned deadline.
fn acquire(path: &Path) -> io::Result<File> {
    let file = open_lock_file(path)?;
    if let Some(deadline) = LOCK_DEADLINE.with(Cell::get) {
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "file lock deadline exceeded"));
            }
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error) if is_contended(&error) => {
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

/// Poll a non-blocking exclusive try every 10 ms until `wait` has passed.
/// An enclosing [`with_lock_deadline`] can only shorten the bound.
fn acquire_within(path: &Path, wait: std::time::Duration) -> io::Result<File> {
    let file = open_lock_file(path)?;
    let own_deadline = Instant::now() + wait;
    let scoped = LOCK_DEADLINE.with(Cell::get).filter(|scope| *scope < own_deadline);
    let deadline = scoped.unwrap_or(own_deadline);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(e) if is_contended(&e) => {}
            Err(e) => return Err(e),
        }
        if Instant::now() >= deadline {
            if scoped.is_some() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "file lock deadline exceeded"));
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("lock_wait_timeout: {} still held after {} ms", lock_path_for(path).display(), wait.as_millis()),
            ));
        }
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(10)));
    }
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
    fn a_guard_excludes_a_second_holder_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("locks").join("guarded");
        let guard = lock_exclusive(&target).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let t2 = target.clone();
        let waiter = std::thread::spawn(move || {
            let _g = lock_exclusive(&t2).unwrap();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "a second holder must wait while the guard lives"
        );
        drop(guard);
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn a_bounded_wait_times_out_without_running_the_closure() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("bounded");
        let held = lock_exclusive(&target).unwrap();
        let started = std::time::Instant::now();
        let mut ran = false;
        let err = with_file_lock_timeout(&target, std::time::Duration::from_millis(150), || {
            ran = true;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(!ran, "the closure must not run without the lock");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        drop(held);
        assert_eq!(with_file_lock_timeout(&target, std::time::Duration::from_millis(150), || Ok(7)).unwrap(), 7);
    }

    #[test]
    fn a_bounded_guard_times_out_then_succeeds_once_free() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("bounded-guard");
        let held = lock_exclusive(&target).unwrap();
        let err = lock_exclusive_within(&target, std::time::Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("lock_wait_timeout"), "{err}");
        drop(held);
        let guard = lock_exclusive_within(&target, std::time::Duration::from_millis(100)).unwrap();
        // The bounded guard excludes a closure-style holder too.
        assert!(with_file_lock_timeout(&target, std::time::Duration::from_millis(50), || Ok(())).is_err());
        drop(guard);
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
