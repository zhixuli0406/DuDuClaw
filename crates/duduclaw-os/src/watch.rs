//! Cross-platform filesystem watcher with burst debouncing and sliding-window
//! rate limiting.
//!
//! Built on `notify-debouncer-mini` (FSEvents on macOS, inotify on Linux,
//! ReadDirectoryChangesW on Windows). The mini debouncer coalesces bursts but
//! collapses every backend event kind into `Any`, so we re-derive a
//! Created/Modified/Removed classification from filesystem state at emit time
//! (see [`classify_kind`]). Renames surface as a Removed + Created pair.
//!
//! Safety / discipline notes:
//! - Ignore matching is **exact path-component / extension** equality, never a
//!   substring `contains` (project convention #2).
//! - Rate limiting drops events past `max_events_per_min` but **counts** every
//!   drop and logs a per-minute summary — no silent caps.
//! - Missing watch paths are warned + skipped, never fatal to the watcher.
//! - Watch paths should point at local disks: FSEvents (macOS) is known to
//!   behave inconsistently over network mounts (NAS/SMB) and iCloud Drive —
//!   events may be delayed, coalesced away, or not fire at all. `duduclaw os
//!   doctor` surfaces this as an operator-facing tip; it is not detectable
//!   programmatically, so no runtime check is added here.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use serde::Serialize;
use tokio::sync::mpsc::{self, Receiver};
use tracing::{info, warn};

/// Default debounce window — coalesce a burst of writes to the same file.
pub const DEFAULT_DEBOUNCE_MS: u64 = 2000;
/// Default sliding-window rate cap (events emitted per rolling minute).
pub const DEFAULT_MAX_EVENTS_PER_MIN: u32 = 30;

/// Bounded event channel capacity. The sliding-window limiter keeps the steady
/// rate well under this; a transient overflow is counted as a drop (not silent).
const EVENT_CHANNEL_CAP: usize = 256;
/// Rolling window length for both the rate limiter and the drop-summary log.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Soft cap on the tracked-paths set used for kind inference — bounds memory
/// under long-running churn. Clearing is safe: a subsequent event re-seeds the
/// path, and `classify_kind` consults the file's birth/mtime relative to the
/// watcher start so an already-existing file is still classified `Modified`
/// (not spuriously `Created`) after a clear.
const SEEN_SET_SOFT_CAP: usize = 10_000;

/// Watcher configuration, typically sourced from `agent.toml [os_watch]`.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Paths to watch (recursively). Canonicalized at start; missing → skipped.
    pub paths: Vec<PathBuf>,
    /// User ignore patterns, ADDITIVE on top of the built-in ignore set.
    /// `*.ext` → extension match; anything else → exact path-component / filename.
    pub ignore: Vec<String>,
    /// Debounce window in milliseconds.
    pub debounce_ms: u64,
    /// Max events emitted per rolling minute before dropping (counted).
    pub max_events_per_min: u32,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            ignore: Vec::new(),
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            max_events_per_min: DEFAULT_MAX_EVENTS_PER_MIN,
        }
    }
}

/// Classification of a filesystem change, re-derived from fs state (the mini
/// debouncer does not preserve backend event kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEventKind {
    Created,
    Modified,
    Removed,
    /// Reserved: renames surface as Removed+Created via the mini debouncer, but
    /// the variant is kept so downstream rule authors can match on it if a
    /// future backend distinguishes renames.
    Renamed,
}

impl FileEventKind {
    /// Stable lowercase name used in autopilot rule fields and JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            FileEventKind::Created => "created",
            FileEventKind::Modified => "modified",
            FileEventKind::Removed => "removed",
            FileEventKind::Renamed => "renamed",
        }
    }
}

/// A single debounced, filtered, rate-limited filesystem event.
#[derive(Debug, Clone, Serialize)]
pub struct OsFileEvent {
    /// Absolute path (lossy UTF-8) of the changed entry.
    pub path: String,
    /// Change classification.
    pub kind: FileEventKind,
}

/// Live counters for a running watcher, shared with the owner via the handle.
#[derive(Debug, Default)]
pub struct WatchStats {
    /// Events successfully forwarded to the receiver.
    pub emitted: AtomicU64,
    /// Events dropped by the rate limiter or a full channel.
    pub dropped: AtomicU64,
}

/// Handle that keeps a watcher alive. Dropping it stops the watch (the inner
/// `Debouncer` unregisters its OS watches on drop).
pub struct WatchHandle {
    // Field order matters only for Drop clarity; the Debouncer stops on drop.
    _debouncer: Debouncer<RecommendedWatcher>,
    stats: Arc<WatchStats>,
    watched_paths: Vec<String>,
}

impl WatchHandle {
    /// Shared live counters (emitted / dropped).
    pub fn stats(&self) -> &Arc<WatchStats> {
        &self.stats
    }

    /// The canonicalized paths actually being watched.
    pub fn watched_paths(&self) -> &[String] {
        &self.watched_paths
    }
}

/// Errors starting a watcher.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("filesystem watch backend error: {0}")]
    Backend(#[from] notify_debouncer_mini::notify::Error),
    #[error("no valid watch paths (all missing or unwatchable)")]
    NoValidPaths,
}

/// Namespace type for the watcher entry point.
pub struct OsWatcher;

impl OsWatcher {
    /// Start watching. Returns a bounded receiver of [`OsFileEvent`] plus a
    /// [`WatchHandle`] that must be kept alive for the watch to persist.
    ///
    /// Paths that do not exist (or cannot be watched) are warned and skipped; if
    /// no path can be watched the call fails with [`WatchError::NoValidPaths`].
    pub fn start(config: WatchConfig) -> Result<(Receiver<OsFileEvent>, WatchHandle), WatchError> {
        let (tx, rx) = mpsc::channel::<OsFileEvent>(EVENT_CHANNEL_CAP);
        let stats = Arc::new(WatchStats::default());
        let ignore = IgnoreMatcher::build(&config.ignore);
        let max_per_min = config.max_events_per_min.max(1) as usize;
        let debounce = Duration::from_millis(config.debounce_ms.max(1));
        let stats_for_cb = Arc::clone(&stats);

        // Wall-clock instant the watcher began. Used by `classify_kind` to tell
        // a genuinely-new file (born after we started) from a pre-existing file
        // whose first observed event is really a modification (see below).
        let watcher_start = SystemTime::now();

        // Per-callback mutable state owned by the FnMut handler.
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut window: VecDeque<Instant> = VecDeque::new();
        let mut dropped_since_log: u64 = 0;
        let mut last_drop_log = Instant::now();

        let handler = move |res: DebounceEventResult| {
            let events = match res {
                Ok(evs) => evs,
                Err(e) => {
                    warn!(error = %e, "os watcher backend error");
                    return;
                }
            };
            let now = Instant::now();
            // Prune the sliding window of timestamps older than one minute.
            while let Some(&front) = window.front() {
                if now.duration_since(front) >= RATE_WINDOW {
                    window.pop_front();
                } else {
                    break;
                }
            }

            for ev in events {
                let path = ev.path;
                if ignore.is_ignored(&path) {
                    continue;
                }
                let kind = classify_kind(&path, &mut seen, watcher_start);

                if window.len() >= max_per_min {
                    stats_for_cb.dropped.fetch_add(1, Ordering::Relaxed);
                    dropped_since_log += 1;
                    continue;
                }

                let out = OsFileEvent {
                    path: path.to_string_lossy().into_owned(),
                    kind,
                };
                match tx.try_send(out) {
                    Ok(()) => {
                        window.push_back(now);
                        stats_for_cb.emitted.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        stats_for_cb.dropped.fetch_add(1, Ordering::Relaxed);
                        dropped_since_log += 1;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Receiver gone — nothing more to do; the handle drop
                        // will tear down the debouncer shortly.
                    }
                }
            }

            // No-silent-caps: summarize drops at most once per minute.
            if dropped_since_log > 0 && now.duration_since(last_drop_log) >= RATE_WINDOW {
                warn!(
                    dropped = dropped_since_log,
                    "os watcher rate-limited / overflowed filesystem events in the last minute"
                );
                dropped_since_log = 0;
                last_drop_log = now;
            }
        };

        let mut debouncer = new_debouncer(debounce, handler)?;

        let mut watched_paths = Vec::new();
        for p in &config.paths {
            let canon = match p.canonicalize() {
                Ok(c) => c,
                Err(e) => {
                    warn!(path = %p.display(), error = %e, "os watch path not found — skipping");
                    continue;
                }
            };
            match debouncer.watcher().watch(&canon, RecursiveMode::Recursive) {
                Ok(()) => {
                    info!(path = %canon.display(), "os watcher watching");
                    watched_paths.push(canon.to_string_lossy().into_owned());
                }
                Err(e) => {
                    warn!(path = %canon.display(), error = %e, "failed to watch path — skipping");
                }
            }
        }

        if watched_paths.is_empty() {
            return Err(WatchError::NoValidPaths);
        }

        Ok((
            rx,
            WatchHandle {
                _debouncer: debouncer,
                stats,
                watched_paths,
            },
        ))
    }
}

/// Derive a Created/Modified/Removed classification from filesystem state and a
/// running seen-set. The mini debouncer collapses all backend kinds to `Any`.
///
/// The seen-set alone can't distinguish "file created after we started
/// watching" from "file that already existed and was just modified" — on a cold
/// start (and after a `SEEN_SET_SOFT_CAP` clear) both look like a first sighting
/// and would naively report `Created`. To fix that, a first-sighting of an
/// existing path consults the file's **birth time** (`metadata.created()`,
/// available on macOS/Windows and on Linux via `statx` for most filesystems):
/// born at/after `watcher_start` → `Created`, else `Modified`. When birth time
/// is unavailable the modification time is used with the same cutoff; when
/// neither timestamp can be read we fall back to the historical `Created`.
fn classify_kind(
    path: &Path,
    seen: &mut HashSet<PathBuf>,
    watcher_start: SystemTime,
) -> FileEventKind {
    if !path.exists() {
        seen.remove(path);
        return FileEventKind::Removed;
    }
    if seen.contains(path) {
        return FileEventKind::Modified;
    }
    // First sighting since (re)start. Bound the tracked set, then decide
    // Created-vs-Modified from the file's own timestamps relative to start.
    if seen.len() >= SEEN_SET_SOFT_CAP {
        seen.clear();
    }
    seen.insert(path.to_path_buf());
    first_sighting_kind(path, watcher_start)
}

/// Classify the first-since-start sighting of an existing path using its birth
/// time (preferred) or modification time relative to `watcher_start`. Anything
/// born/modified at or after start is a genuine `Created`; older files whose
/// first event we only now observe are really a `Modified`. Falls back to
/// `Created` (the historical behavior) when no timestamp is readable.
fn first_sighting_kind(path: &Path, watcher_start: SystemTime) -> FileEventKind {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileEventKind::Created;
    };
    // `duration_since(watcher_start).is_ok()` ⇔ timestamp >= watcher_start.
    if let Ok(birth) = meta.created() {
        return if birth.duration_since(watcher_start).is_ok() {
            FileEventKind::Created
        } else {
            FileEventKind::Modified
        };
    }
    if let Ok(mtime) = meta.modified() {
        return if mtime.duration_since(watcher_start).is_ok() {
            FileEventKind::Created
        } else {
            FileEventKind::Modified
        };
    }
    FileEventKind::Created
}

/// Exact-match ignore filter (path-component / filename equality + extension
/// equality). Built-ins plus user-supplied additive patterns. No substring
/// `contains` — a routing decision (project convention #2).
struct IgnoreMatcher {
    /// Directory / file names that mask any path containing them as a component.
    components: HashSet<String>,
    /// File extensions (without the leading dot) that mask matching files.
    extensions: HashSet<String>,
}

impl IgnoreMatcher {
    fn build(user: &[String]) -> Self {
        let mut components = HashSet::new();
        let mut extensions = HashSet::new();

        // Built-in noise + self-loop guard (`.duduclaw` prevents an agent's own
        // writes from re-triggering its watcher — see the design risk memo).
        for c in [".git", "node_modules", "target", ".duduclaw", ".DS_Store"] {
            components.insert(c.to_string());
        }
        for e in ["tmp", "swp"] {
            extensions.insert(e.to_string());
        }

        for pat in user {
            let pat = pat.trim().trim_end_matches('/');
            if let Some(ext) = pat.strip_prefix("*.") {
                if !ext.is_empty() {
                    extensions.insert(ext.to_string());
                }
            } else if !pat.is_empty() {
                components.insert(pat.to_string());
            }
        }

        Self {
            components,
            extensions,
        }
    }

    fn is_ignored(&self, path: &Path) -> bool {
        for comp in path.components() {
            if let std::path::Component::Normal(os) = comp
                && let Some(s) = os.to_str()
                && self.components.contains(s)
            {
                return true;
            }
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && self.extensions.contains(ext)
        {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_matches_builtin_components() {
        let m = IgnoreMatcher::build(&[]);
        assert!(m.is_ignored(Path::new("/home/u/proj/.git/HEAD")));
        assert!(m.is_ignored(Path::new("/home/u/proj/node_modules/x/index.js")));
        assert!(m.is_ignored(Path::new("/home/u/proj/target/debug/app")));
        assert!(m.is_ignored(Path::new("/home/u/.duduclaw/agents/a/agent.toml")));
        assert!(m.is_ignored(Path::new("/home/u/proj/.DS_Store")));
    }

    #[test]
    fn ignore_matches_builtin_extensions() {
        let m = IgnoreMatcher::build(&[]);
        assert!(m.is_ignored(Path::new("/tmp/foo.tmp")));
        assert!(m.is_ignored(Path::new("/tmp/.bar.swp")));
    }

    #[test]
    fn ignore_does_not_substring_match() {
        // "target" is an ignored component, but "targeting.txt" must NOT match:
        // exact component equality, not substring contains (convention #2).
        let m = IgnoreMatcher::build(&[]);
        assert!(!m.is_ignored(Path::new("/home/u/targeting.txt")));
        assert!(!m.is_ignored(Path::new("/home/u/my-node_modules-notes.md")));
    }

    #[test]
    fn ignore_additive_user_patterns() {
        let m = IgnoreMatcher::build(&["*.part".to_string(), "vendor".to_string()]);
        assert!(m.is_ignored(Path::new("/dl/file.part")));
        assert!(m.is_ignored(Path::new("/proj/vendor/lib.rs")));
        // built-ins still apply
        assert!(m.is_ignored(Path::new("/proj/.git/config")));
        // unrelated file passes
        assert!(!m.is_ignored(Path::new("/proj/src/main.rs")));
    }

    #[test]
    fn classify_kind_transitions() {
        // Anchor the watcher start before creating the file so its first
        // sighting classifies as Created (born after start).
        let start = SystemTime::now();
        let mut seen = HashSet::new();
        // A path that does not exist → Removed.
        let ghost = Path::new("/nonexistent/duduclaw-os/ghost-xyz");
        assert_eq!(
            classify_kind(ghost, &mut seen, start),
            FileEventKind::Removed
        );

        // Use a real temp file to exercise Created → Modified.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hi").unwrap();
        assert_eq!(classify_kind(&f, &mut seen, start), FileEventKind::Created);
        assert_eq!(classify_kind(&f, &mut seen, start), FileEventKind::Modified);

        // Delete → Removed, and the seen-set forgets it.
        std::fs::remove_file(&f).unwrap();
        assert_eq!(classify_kind(&f, &mut seen, start), FileEventKind::Removed);
        assert!(!seen.contains(&f));
    }

    #[test]
    fn classify_kind_preexisting_file_is_modified_on_cold_start() {
        // A file that existed BEFORE the watcher started: its first observed
        // event must be Modified, not Created — even though the seen-set is
        // empty (cold start). Create the file, then anchor `start` after it so
        // the file's birth/mtime precede start.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("preexisting.txt");
        std::fs::write(&f, b"old").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let start = SystemTime::now();

        let mut seen = HashSet::new();
        assert_eq!(
            classify_kind(&f, &mut seen, start),
            FileEventKind::Modified,
            "a pre-existing file's first sighting is a modification, not a create"
        );
        // Still tracked, so a subsequent event is also Modified.
        assert_eq!(classify_kind(&f, &mut seen, start), FileEventKind::Modified);
    }

    #[test]
    fn classify_kind_new_file_after_start_is_created() {
        // Anchor start first, then create the file AFTER it: born after start
        // → Created, even on a fresh seen-set.
        let start = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("fresh.txt");
        std::fs::write(&f, b"new").unwrap();

        let mut seen = HashSet::new();
        assert_eq!(classify_kind(&f, &mut seen, start), FileEventKind::Created);
    }

    #[test]
    fn kind_as_str_is_stable() {
        assert_eq!(FileEventKind::Created.as_str(), "created");
        assert_eq!(FileEventKind::Modified.as_str(), "modified");
        assert_eq!(FileEventKind::Removed.as_str(), "removed");
        assert_eq!(FileEventKind::Renamed.as_str(), "renamed");
    }

    #[test]
    fn start_with_no_valid_paths_errors() {
        let cfg = WatchConfig {
            paths: vec![PathBuf::from("/definitely/not/here/duduclaw-os-xyz")],
            ..Default::default()
        };
        let r = OsWatcher::start(cfg);
        assert!(matches!(r, Err(WatchError::NoValidPaths)));
    }
}
