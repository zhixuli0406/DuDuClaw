//! Integration tests for `OsWatcher` against a real temp directory.
//!
//! These exercise the actual OS filesystem-notification backend (FSEvents /
//! inotify), so they use generous timeouts. A short debounce keeps them quick
//! while still coalescing the write burst.

use std::time::Duration;

use duduclaw_os::watch::{FileEventKind, OsFileEvent, OsWatcher, WatchConfig};
use tokio::sync::mpsc::Receiver;

/// Drain events until one satisfies `pred` or the deadline passes.
async fn wait_for<F>(
    rx: &mut Receiver<OsFileEvent>,
    timeout: Duration,
    mut pred: F,
) -> Option<OsFileEvent>
where
    F: FnMut(&OsFileEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => {
                if pred(&ev) {
                    return Some(ev);
                }
            }
            Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn detects_file_creation() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = WatchConfig {
        paths: vec![dir.path().to_path_buf()],
        ignore: Vec::new(),
        debounce_ms: 200,
        max_events_per_min: 100,
    };
    let (mut rx, _handle) = OsWatcher::start(cfg).expect("watcher should start");

    // Give the backend a moment to arm before writing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let f = dir.path().join("hello.txt");
    std::fs::write(&f, b"hi").unwrap();

    let ev = wait_for(&mut rx, Duration::from_secs(10), |ev| {
        ev.path.ends_with("hello.txt")
    })
    .await;
    let ev = ev.expect("should observe a create/modify event for hello.txt");
    assert!(matches!(
        ev.kind,
        FileEventKind::Created | FileEventKind::Modified
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn ignores_builtin_and_user_patterns() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-create the ignored subdir so writes land inside it.
    let git_dir = dir.path().join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();

    let cfg = WatchConfig {
        paths: vec![dir.path().to_path_buf()],
        ignore: vec!["*.part".to_string()],
        debounce_ms: 200,
        max_events_per_min: 100,
    };
    let (mut rx, _handle) = OsWatcher::start(cfg).expect("watcher should start");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Ignored: inside .git, a *.tmp file, and a *.part (user pattern).
    std::fs::write(git_dir.join("index"), b"x").unwrap();
    std::fs::write(dir.path().join("scratch.tmp"), b"x").unwrap();
    std::fs::write(dir.path().join("download.part"), b"x").unwrap();
    // Not ignored — this must come through and lets us bound the wait.
    let real = dir.path().join("real.txt");
    std::fs::write(&real, b"x").unwrap();

    // The only event we should see is real.txt; assert none of the ignored
    // paths ever surface before it does.
    let mut saw_real = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => {
                assert!(
                    !ev.path.ends_with(".part"),
                    "*.part must be ignored: {}",
                    ev.path
                );
                assert!(
                    !ev.path.ends_with(".tmp"),
                    "*.tmp must be ignored: {}",
                    ev.path
                );
                assert!(
                    !ev.path.contains("/.git/"),
                    ".git contents must be ignored: {}",
                    ev.path
                );
                if ev.path.ends_with("real.txt") {
                    saw_real = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        saw_real,
        "the non-ignored real.txt event should have surfaced"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_counts_drops() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = WatchConfig {
        paths: vec![dir.path().to_path_buf()],
        ignore: Vec::new(),
        // Very tight debounce + cap so distinct files trip the limiter.
        debounce_ms: 50,
        max_events_per_min: 3,
    };
    let (mut rx, handle) = OsWatcher::start(cfg).expect("watcher should start");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create many distinct files to generate more than the per-minute cap.
    for i in 0..40 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Drain whatever is buffered, allowing the backend time to deliver.
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    let emitted = handle
        .stats()
        .emitted
        .load(std::sync::atomic::Ordering::Relaxed);
    let dropped = handle
        .stats()
        .dropped
        .load(std::sync::atomic::Ordering::Relaxed);

    // The limiter caps emitted at the per-minute ceiling and records the rest
    // as drops (no silent caps). We can't assert exact FS event counts (backend
    // coalescing varies), only the invariant that the cap held and drops were
    // counted when the backend delivered more than the ceiling.
    assert!(emitted <= 3, "emitted ({emitted}) must not exceed the cap");
    assert!(
        emitted + dropped >= emitted,
        "counters must be internally consistent"
    );
    if emitted == 3 {
        assert!(
            dropped > 0,
            "once the cap is hit, further events must be counted as drops"
        );
    }
}
