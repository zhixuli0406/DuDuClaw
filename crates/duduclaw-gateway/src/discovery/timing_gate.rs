//! Cross-run serialization for operator-declared timing-sensitive scorers.
use std::{fs::File, path::Path, time::Duration};

pub async fn acquire(home: &Path, scorer_hash: &str, timeout: Duration) -> Result<Option<File>, String> {
    #[cfg(not(unix))]
    return Err("timing-sensitive scorer locking requires a verified platform backend".into());
    #[cfg(unix)]
    {
        use std::{fs::OpenOptions, os::unix::fs::{MetadataExt, OpenOptionsExt}, time::Instant};
        if timeout.is_zero() || scorer_hash.len() != 64 || !scorer_hash.bytes().all(|c|c.is_ascii_hexdigit()) {
            return Err("invalid timing-sensitive scorer deadline or identity".into());
        }
        let directory = home.join("discovery/timing-locks");
        super::workspace::create_private_directory(&directory).map_err(|e|e.to_string())?;
        let path = directory.join(format!("{}.lock", scorer_hash.to_ascii_lowercase()));
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path).map_err(|e|e.to_string())?;
        let metadata = file.metadata().map_err(|e|e.to_string())?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600 {
            return Err("scorer lock ownership rejected".into());
        }
        let deadline = Instant::now().checked_add(timeout).ok_or("scorer deadline overflow")?;
        loop {
            if Instant::now() >= deadline { return Err("timing-sensitive scorer lock timed out".into()); }
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(5))).await;
                },
                Err(error) => return Err(error.to_string()),
            }
        }
        let current = std::fs::symlink_metadata(&path).map_err(|e|e.to_string())?;
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Err("scorer lock replaced during wait".into());
        }
        Ok(Some(file))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn same_scorer_wait_consumes_timeout_and_different_scorer_is_independent() {
        let home = tempfile::tempdir().unwrap();
        let held = acquire(home.path(), &"a".repeat(64), Duration::from_secs(1)).await.unwrap();
        assert!(acquire(home.path(), &"b".repeat(64), Duration::from_millis(30)).await.is_ok());
        let started = std::time::Instant::now();
        assert!(acquire(home.path(), &"a".repeat(64), Duration::from_millis(60)).await.is_err(),
            "two runs must not time the same scorer concurrently");
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_millis(300));
        drop(held);
        assert!(acquire(home.path(), &"a".repeat(64), Duration::from_millis(30)).await.is_ok());
    }
}
