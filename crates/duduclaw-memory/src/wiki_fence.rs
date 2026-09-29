//! Cooperative delivery fence for local Wiki pages and live trust state.
//!
//! The fence is **per directory**: each agent Wiki root, the shared Wiki root
//! and the home directory that owns `wiki_trust.db` each get their own lock
//! and their own durable epoch. Agent A's page write therefore no longer
//! blocks agent B's page write, trust update or source delivery.
//!
//! A reader takes the shared lock only for the read-and-verify window and
//! records the epoch it observed; delivery is re-checked against that epoch
//! instead of pinning the lock for the whole turn (see
//! `wiki_mcp_source::WikiBoundDeliveryLease`). Writers take a **bounded
//! blocking** exclusive lock and advance the epoch before mutating, so a
//! short reader window delays a write instead of failing it outright.
//! Direct filesystem or SQLite writes still cannot be fenced.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use duduclaw_core::error::{DuDuClawError, Result};
use fs2::FileExt;

const LOCK_FILE: &str = ".delivery_fence.lock";
const EPOCH_FILE: &str = ".delivery_fence.epoch";

/// Stable, anchored prefix for a fence-contention failure. `is_fence_busy`
/// matches on this prefix rather than searching the message for a substring.
pub const FENCE_BUSY_PREFIX: &str = "wiki delivery Busy:";

/// Bounded wait for an ordinary page / trust-row write. A reader now holds
/// the fence for milliseconds, so a writer that waits almost always wins.
pub const WRITE_FENCE_WAIT: Duration = Duration::from_secs(5);

/// Bounded wait for the trust-database open probe and schema migration. Short
/// on purpose: opening is a startup-shaped path whose caller can retry, and a
/// migration must not sit on the fence behind an in-flight delivery read.
pub const OPEN_FENCE_WAIT: Duration = Duration::from_millis(750);

/// Poll interval while waiting. `fs2`'s blocking `lock_exclusive` cannot be
/// cancelled, so a bounded poll is what keeps the timeout honest.
const FENCE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Fence acquisition failure. `Busy` is a distinct kind so callers can retry
/// or degrade instead of reporting an unrelated "invalid input".
#[derive(Debug)]
pub enum WikiFenceError {
    /// Another participant holds the fence in a conflicting mode.
    Busy {
        operation: &'static str,
        scope: String,
    },
    /// The lock or epoch file itself could not be read or written.
    Fs(DuDuClawError),
}

impl WikiFenceError {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

impl std::fmt::Display for WikiFenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy { operation, scope } => {
                write!(f, "{FENCE_BUSY_PREFIX} {operation} fence at {scope} is held")
            }
            Self::Fs(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for WikiFenceError {}

impl From<WikiFenceError> for DuDuClawError {
    fn from(error: WikiFenceError) -> Self {
        match error {
            busy @ WikiFenceError::Busy { .. } => DuDuClawError::Memory(busy.to_string()),
            WikiFenceError::Fs(inner) => inner,
        }
    }
}

/// `true` when `error` came from fence contention rather than a real fault.
///
/// The check is anchored on the prefix this module itself writes — never an
/// unanchored substring search (project convention 2).
pub fn is_fence_busy(error: &DuDuClawError) -> bool {
    matches!(error, DuDuClawError::Memory(message) if message.starts_with(FENCE_BUSY_PREFIX))
}

type FenceResult<T> = std::result::Result<T, WikiFenceError>;

#[derive(Debug, Clone)]
pub struct WikiDeliveryFence {
    /// Directory that owns this fence's lock and epoch files. One per agent
    /// Wiki root, one per shared Wiki root, one for the trust-database home.
    home_dir: PathBuf,
}

#[derive(Debug)]
pub struct WikiDeliveryLease {
    _lock: File,
    epoch: u64,
}

#[derive(Debug)]
pub struct WikiMutationGuard {
    _lock: File,
    epoch: u64,
}

impl WikiDeliveryFence {
    pub fn new(home_dir: impl AsRef<Path>) -> Self {
        Self {
            home_dir: home_dir.as_ref().to_path_buf(),
        }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// The fence that owns a Wiki root. Each agent Wiki directory and the
    /// shared Wiki directory own their own lock and epoch, so one agent's
    /// delivery window cannot fail another agent's write.
    pub fn for_wiki_dir(wiki_dir: &Path) -> Self {
        Self::new(wiki_dir)
    }

    /// The fence that owns the home directory holding `wiki_trust.db`. Used
    /// for trust writes that cannot be attributed to a single Wiki root and
    /// for the trust-database open probe / schema migration.
    pub fn for_trust_home(home_dir: &Path) -> Self {
        Self::new(home_dir)
    }

    /// The fence that owns one agent's Wiki root under `home_dir`.
    pub fn for_agent_wiki(home_dir: &Path, agent_id: &str) -> Self {
        Self::new(home_dir.join("agents").join(agent_id).join("wiki"))
    }

    /// Acquire a nonblocking shared lease. Readers hold this only for the
    /// read-and-verify window; delivery re-checks `epoch()` instead.
    pub fn try_shared(&self) -> Result<WikiDeliveryLease> {
        Ok(self.lock_shared_with_timeout(Duration::ZERO)?)
    }

    /// Acquire a nonblocking exclusive mutation guard. Advancing the epoch
    /// before mutation means a crash cannot leave a changed page under the
    /// previous epoch. The caller must retain this guard through all writes.
    pub fn try_exclusive(&self) -> Result<WikiMutationGuard> {
        Ok(self.lock_exclusive_with_timeout(Duration::ZERO)?)
    }

    /// Bounded blocking shared acquisition. `Duration::ZERO` is a single
    /// nonblocking attempt.
    pub fn lock_shared_with_timeout(&self, wait: Duration) -> FenceResult<WikiDeliveryLease> {
        let lock = self.open_lock().map_err(WikiFenceError::Fs)?;
        self.wait_for_lock(&lock, wait, "read", FileExt::try_lock_shared)?;
        let epoch = self.read_epoch().map_err(WikiFenceError::Fs)?;
        Ok(WikiDeliveryLease { _lock: lock, epoch })
    }

    /// Bounded blocking exclusive acquisition. A writer that loses a race
    /// against a short reader window waits instead of failing, which is what
    /// stops one in-flight delivery from rejecting every concurrent write.
    /// `Duration::ZERO` is a single nonblocking attempt.
    pub fn lock_exclusive_with_timeout(&self, wait: Duration) -> FenceResult<WikiMutationGuard> {
        let lock = self.open_lock().map_err(WikiFenceError::Fs)?;
        self.wait_for_lock(&lock, wait, "write", FileExt::try_lock_exclusive)?;
        let epoch = self
            .read_epoch()
            .map_err(WikiFenceError::Fs)?
            .checked_add(1)
            .ok_or_else(|| {
                WikiFenceError::Fs(DuDuClawError::Memory("wiki delivery epoch overflow".into()))
            })?;
        self.write_epoch(epoch).map_err(WikiFenceError::Fs)?;
        Ok(WikiMutationGuard { _lock: lock, epoch })
    }

    /// Exclusive guard for an ordinary page / trust-row write.
    pub fn exclusive_for_write(&self) -> Result<WikiMutationGuard> {
        Ok(self.lock_exclusive_with_timeout(WRITE_FENCE_WAIT)?)
    }

    /// `fs2` offers no cancellable blocking lock, so poll until the deadline.
    /// The caller is already on a blocking (non-reactor) path: every writer
    /// here goes on to do synchronous file or SQLite work.
    fn wait_for_lock(
        &self,
        lock: &File,
        wait: Duration,
        operation: &'static str,
        attempt: fn(&File) -> std::io::Result<()>,
    ) -> FenceResult<()> {
        let deadline = Instant::now() + wait;
        loop {
            if attempt(lock).is_ok() {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WikiFenceError::Busy {
                    operation,
                    scope: self.home_dir.display().to_string(),
                });
            }
            std::thread::sleep(FENCE_POLL_INTERVAL.min(deadline - now));
        }
    }

    fn open_lock(&self) -> Result<File> {
        std::fs::create_dir_all(&self.home_dir).map_err(|e| {
            DuDuClawError::Memory(format!(
                "create wiki fence home {}: {e}",
                self.home_dir.display()
            ))
        })?;
        let path = self.home_dir.join(LOCK_FILE);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| DuDuClawError::Memory(format!("open wiki fence {}: {e}", path.display())))
    }

    fn read_epoch(&self) -> Result<u64> {
        let path = self.home_dir.join(EPOCH_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => raw.trim().parse::<u64>().map_err(|e| {
                DuDuClawError::Memory(format!(
                    "invalid wiki delivery epoch {}: {e}",
                    path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(DuDuClawError::Memory(format!(
                "read wiki delivery epoch {}: {e}",
                path.display()
            ))),
        }
    }

    fn write_epoch(&self, epoch: u64) -> Result<()> {
        // The exclusive fence serialises writers, so a stable temporary name
        // is safe. Rename keeps concurrent readers from observing partial data.
        let tmp = self.home_dir.join(format!("{EPOCH_FILE}.tmp"));
        let path = self.home_dir.join(EPOCH_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| {
                DuDuClawError::Memory(format!("open wiki epoch temp {}: {e}", tmp.display()))
            })?;
        file.write_all(epoch.to_string().as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| {
                DuDuClawError::Memory(format!("sync wiki epoch temp {}: {e}", tmp.display()))
            })?;
        // Windows may refuse to rename an open file handle.
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|e| {
            DuDuClawError::Memory(format!("commit wiki epoch {}: {e}", path.display()))
        })?;
        // Unix allows fsync on a directory to persist the rename. Windows
        // rejects ordinary directory opens; the synced file and rename are
        // retained without that additional directory flush.
        #[cfg(unix)]
        {
            File::open(&self.home_dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|e| {
                    DuDuClawError::Memory(format!(
                        "sync wiki fence home {}: {e}",
                        self.home_dir.display()
                    ))
                })?;
        }
        Ok(())
    }

}

impl WikiDeliveryLease {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl WikiMutationGuard {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_lease_blocks_writes_and_epoch_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let fence = WikiDeliveryFence::new(temp.path());
        let first = fence.try_shared().unwrap();
        let second = WikiDeliveryFence::new(temp.path()).try_shared().unwrap();
        assert_eq!(first.epoch(), 0);
        assert!(
            fence
                .try_exclusive()
                .unwrap_err()
                .to_string()
                .contains("Busy")
        );
        drop(first);
        drop(second);
        assert_eq!(fence.try_exclusive().unwrap().epoch(), 1);
        assert_eq!(
            WikiDeliveryFence::new(temp.path())
                .try_shared()
                .unwrap()
                .epoch(),
            1
        );
        let writer = fence.try_exclusive().unwrap();
        assert!(fence.try_shared().unwrap_err().to_string().contains("Busy"));
        drop(writer);
    }

    #[test]
    fn wiki_layouts_get_independent_per_directory_fences() {
        let home = Path::new("/tmp/wiki-fence-test");
        // W2-B: the fence is the Wiki root itself, no longer folded back to
        // the common home — agent and shared roots must not share a lock.
        assert_eq!(
            WikiDeliveryFence::for_wiki_dir(&home.join("agents/a/wiki")).home_dir(),
            home.join("agents/a/wiki")
        );
        assert_eq!(
            WikiDeliveryFence::for_wiki_dir(&home.join("shared/wiki")).home_dir(),
            home.join("shared/wiki")
        );
        assert_ne!(
            WikiDeliveryFence::for_agent_wiki(home, "a").home_dir(),
            WikiDeliveryFence::for_agent_wiki(home, "b").home_dir()
        );
        assert_eq!(
            WikiDeliveryFence::for_trust_home(home).home_dir(),
            home
        );
    }

    /// Regression (W2-B/a): one agent's delivery window must not fail another
    /// agent's page write, and must not fail a trust write for that other
    /// agent's page. Before the per-directory split both were instant `Busy`.
    #[test]
    fn delivery_lease_on_one_agent_never_blocks_another_agents_page_or_trust_write() {
        let temp = tempfile::tempdir().unwrap();
        let held = crate::wiki::WikiStore::new(temp.path().join("agents/a/wiki"));
        held.ensure_scaffold().unwrap();
        held.write_page("concepts/a.md", "# Before\n").unwrap();
        let other = crate::wiki::WikiStore::new(temp.path().join("agents/b/wiki"));
        other.ensure_scaffold().unwrap();
        let trust =
            crate::trust_store::WikiTrustStore::open(temp.path().join("wiki_trust.db")).unwrap();

        let lease = WikiDeliveryFence::for_agent_wiki(temp.path(), "a")
            .try_shared()
            .unwrap();

        other.write_page("concepts/b.md", "# B\n").unwrap();
        assert_eq!(other.read_raw("concepts/b.md").unwrap(), "# B\n");
        trust
            .manual_set("concepts/b.md", "b", 0.0, false, Some(true), None)
            .unwrap();
        assert!(
            trust
                .get("concepts/b.md", "b")
                .unwrap()
                .unwrap()
                .do_not_inject
        );
        drop(lease);
    }

    /// Regression (W2-B/b): a same-directory write used to fail instantly
    /// while any reader held the fence. It now waits out the read window and
    /// commits, and nothing partial is written while it waits.
    #[test]
    fn same_directory_write_waits_out_the_read_window_instead_of_failing() {
        let temp = tempfile::tempdir().unwrap();
        let wiki = crate::wiki::WikiStore::new(temp.path().join("agents/a/wiki"));
        wiki.ensure_scaffold().unwrap();
        wiki.write_page("concepts/a.md", "# Before\n").unwrap();
        let fence = WikiDeliveryFence::for_agent_wiki(temp.path(), "a");
        let lease = fence.try_shared().unwrap();

        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(lease);
        });
        // A nonblocking attempt still reports Busy while the reader holds it.
        assert!(matches!(
            fence.lock_exclusive_with_timeout(Duration::ZERO),
            Err(WikiFenceError::Busy { .. })
        ));
        assert_eq!(wiki.read_raw("concepts/a.md").unwrap(), "# Before\n");

        wiki.write_page("concepts/a.md", "# After\n").unwrap();
        holder.join().unwrap();
        assert_eq!(wiki.read_raw("concepts/a.md").unwrap(), "# After\n");
    }

    /// Regression (W2-B/b): fence contention is its own error kind, and a
    /// `DuDuClawError` that came from it is recognisable without an
    /// unanchored substring search.
    #[test]
    fn fence_busy_is_a_distinct_error_kind_recognisable_after_conversion() {
        let temp = tempfile::tempdir().unwrap();
        let fence = WikiDeliveryFence::new(temp.path());
        let writer = fence.lock_exclusive_with_timeout(Duration::ZERO).unwrap();
        let error = fence
            .lock_exclusive_with_timeout(Duration::from_millis(10))
            .unwrap_err();
        assert!(error.is_busy());
        let converted: DuDuClawError = error.into();
        assert!(is_fence_busy(&converted));
        assert!(!is_fence_busy(&DuDuClawError::Memory("unrelated".into())));
        drop(writer);
        assert!(
            fence
                .lock_exclusive_with_timeout(WRITE_FENCE_WAIT)
                .is_ok()
        );
    }

    /// Regression (W2-B/a): a trust write is fenced by the Wiki root of the
    /// page it touches, not by a single home-wide lock.
    #[test]
    fn trust_write_takes_the_fence_of_its_own_page_directory() {
        let temp = tempfile::tempdir().unwrap();
        crate::wiki::WikiStore::new(temp.path().join("agents/a/wiki"))
            .ensure_scaffold()
            .unwrap();
        let trust =
            crate::trust_store::WikiTrustStore::open(temp.path().join("wiki_trust.db")).unwrap();
        let own = WikiDeliveryFence::for_agent_wiki(temp.path(), "a")
            .lock_exclusive_with_timeout(Duration::ZERO)
            .unwrap();
        // Holding agent a's Wiki fence blocks a trust write for agent a's
        // page (short bounded wait keeps the test fast) ...
        let blocked = trust
            .manual_set_with_wait(
                "concepts/a.md",
                "a",
                0.0,
                false,
                Some(true),
                None,
                Duration::from_millis(30),
            )
            .unwrap_err();
        assert!(is_fence_busy(&blocked));
        // ... and leaves a different agent's trust row writable.
        trust
            .manual_set("concepts/a.md", "b", 0.0, false, Some(true), None)
            .unwrap();
        drop(own);
        trust
            .manual_set("concepts/a.md", "a", 0.0, false, Some(true), None)
            .unwrap();
    }

    #[test]
    fn initialized_trust_db_reopens_during_shared_delivery_without_advancing_epoch() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("wiki_trust.db");
        let fence = WikiDeliveryFence::new(temp.path());
        let initial = crate::trust_store::WikiTrustStore::open(&db).unwrap();
        drop(initial);
        let lease = fence.try_shared().unwrap();
        let initialized_epoch = lease.epoch();
        assert!(initialized_epoch > 0);

        let reopened = crate::trust_store::WikiTrustStore::open(&db).unwrap();
        assert!(reopened.get("missing.md", "a").unwrap().is_none());
        assert_eq!(fence.try_shared().unwrap().epoch(), initialized_epoch);
        drop(reopened);
        drop(lease);

        let reopened_again = crate::trust_store::WikiTrustStore::open(&db).unwrap();
        drop(reopened_again);
        assert_eq!(fence.try_shared().unwrap().epoch(), initialized_epoch);
    }

    #[test]
    fn missing_schema_trigger_requires_exclusive_migration_and_advances_epoch() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("wiki_trust.db");
        let fence = WikiDeliveryFence::new(temp.path());
        let initial = crate::trust_store::WikiTrustStore::open(&db).unwrap();
        drop(initial);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TRIGGER wiki_trust_ccr_after_update")
            .unwrap();
        drop(conn);
        let lease = fence.try_shared().unwrap();
        let before = lease.epoch();
        match crate::trust_store::WikiTrustStore::open(&db) {
            Err(error) => assert!(error.to_string().contains("Busy")),
            Ok(_) => panic!("migration must wait for the delivery lease"),
        }
        drop(lease);

        let migrated = crate::trust_store::WikiTrustStore::open(&db).unwrap();
        drop(migrated);
        assert_eq!(fence.try_shared().unwrap().epoch(), before + 1);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let trigger_count: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='wiki_trust_ccr_after_update'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(trigger_count, 1);
    }
}
