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
    file.lock_exclusive()?;
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
