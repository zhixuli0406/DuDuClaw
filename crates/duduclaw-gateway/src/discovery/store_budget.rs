//! Durable whole-run accounting, including calls that never produce a node.
use super::{DiscoveryStore, StoreError};
use crate::discovery::budget::BudgetSnapshot;
use rusqlite::{params, OptionalExtension};

impl DiscoveryStore {
    pub fn load_run_budget_snapshot(&self, run_id: &str) -> Result<Option<BudgetSnapshot>, StoreError> {
        let json: Option<String> = self.conn.query_row(
            "SELECT snapshot FROM discovery_run_budget WHERE run_id=?1", [run_id], |r| r.get(0)
        ).optional()?;
        json.map(|json| {
            let snapshot: BudgetSnapshot = serde_json::from_str(&json)?;
            if !snapshot.valid() { return Err(StoreError::Corrupt("invalid run budget snapshot".into())); }
            Ok(snapshot)
        }).transpose()
    }
}

pub(crate) fn persist(home: &std::path::Path, run_id: &str, limits: crate::discovery::contracts::RunBudget,
    sequence: u64, snapshot: BudgetSnapshot) -> Result<(), StoreError> {
    if !snapshot.valid() { return Err(StoreError::Corrupt("invalid run budget snapshot".into())); }
    let conn = super::private_connection(&home.join(super::DB_FILE))?;
    conn.busy_timeout(std::time::Duration::from_millis(200))?;
    let configured: Option<(u32, f64, u64)> = conn.query_row(
        "SELECT budget_calls,budget_usd,budget_secs FROM discovery_runs WHERE run_id=?1",
        [run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    if configured != Some((limits.max_agent_calls, limits.max_usd, limits.max_wall_secs)) {
        return Err(StoreError::Corrupt("budget does not match its durable run".into()));
    }
    let changed = conn.execute("INSERT INTO discovery_run_budget(run_id,sequence,snapshot,updated_at)
        VALUES(?1,?2,?3,?4) ON CONFLICT(run_id) DO UPDATE SET sequence=excluded.sequence,
        snapshot=excluded.snapshot,updated_at=excluded.updated_at
        WHERE excluded.sequence > discovery_run_budget.sequence",
        params![run_id, super::to_i64(sequence,"budget sequence")?, serde_json::to_string(&snapshot)?,
            chrono::Utc::now().to_rfc3339()])?;
    if changed != 1 { return Err(StoreError::Corrupt("stale or reused run budget binding".into())); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{budget::SharedBudget, contracts::RunBudget, tree::{CostSource, Direction}};
    fn fixture() -> (tempfile::TempDir, SharedBudget) {
        let home = tempfile::tempdir().unwrap();
        let limits = RunBudget { max_agent_calls:3, max_usd:3.0, max_wall_secs:30, max_rounds:1 };
        DiscoveryStore::open(home.path()).unwrap().create_run("run-1", "goal", "agent", "score",
            &"a".repeat(64), Direction::Max, &limits).unwrap();
        (home, SharedBudget::new(limits).unwrap())
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn run_budget_survives_reopen_and_includes_pending_retry_and_development_calls_without_nodes() {
        let (home, budget) = fixture();
        budget.bind_run(home.path(), "run-1").unwrap();
        let attempt = budget.reserve_call().unwrap();
        assert!(budget.observe_cost(attempt, 0.05));
        let pending = DiscoveryStore::open(home.path()).unwrap().load_run_budget_snapshot("run-1")
            .unwrap().expect("a pending reservation must be durable before process spawn");
        assert_eq!((pending.agent_calls,pending.pending_calls), (1,1));
        budget.finish_accounted_call(attempt, 0.1, CostSource::Reported);
        let retry = budget.reserve_call().unwrap();
        budget.finish_accounted_call(retry, 0.2, CostSource::Estimated);
        let development = budget.reserve_call().unwrap();
        budget.finish_accounted_call(development, f64::NAN, CostSource::Unknown);
        let settled = DiscoveryStore::open(home.path()).unwrap().load_run_budget_snapshot("run-1")
            .unwrap().unwrap();
        assert_eq!((settled.agent_calls, settled.pending_calls, settled.unknown_calls), (3,0,1));
        assert_eq!(settled.reported_usd, 0.1);
        assert_eq!(settled.estimated_usd, 0.2);
        assert!(settled.unknown_reserved_usd > 0.0);
        assert!(settled.spent_usd >= settled.reported_usd + settled.estimated_usd + settled.unknown_reserved_usd);
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn a_shared_budget_cannot_rebind_to_another_run_or_home() {
        let (home, budget) = fixture();
        budget.bind_run(home.path(), "run-1").unwrap();
        assert!(budget.clone().bind_run(home.path(), "different-run").is_err());
        let (foreign, _) = fixture();
        assert!(budget.bind_run(foreign.path(), "run-1").is_err());
    }
}

#[cfg(all(test, unix))]
mod private_database_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn database_and_wal_are_private_even_when_the_home_is_traversable() {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        store.conn.execute("INSERT INTO discovery_runs(run_id,direction,created_at) VALUES('private','max','now')", []).unwrap();
        for file in ["discovery.db", "discovery.db-wal", "discovery.db-shm"] {
            let mode = std::fs::metadata(home.path().join(file)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "raw frozen policy data requires a private SQLite file: {file}");
        }
    }
    #[test]
    fn database_entry_points_refuse_shared_linked_and_hardlinked_files() {
        let home = tempfile::tempdir().unwrap();
        let original = home.path().join("original.db");
        drop(DiscoveryStore::open_path(&original).unwrap());
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DiscoveryStore::open_path(&original).is_err(), "existing shared databases must be rejected");
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
        let linked = home.path().join("linked.db");
        std::os::unix::fs::symlink(&original, &linked).unwrap();
        assert!(DiscoveryStore::open_path(&linked).is_err(), "SQLite must not follow an alias");
        std::fs::remove_file(&linked).unwrap();
        std::fs::hard_link(&original, &linked).unwrap();
        assert!(DiscoveryStore::open_path(&linked).is_err(), "multiple names invalidate the private file authority");
    }

    /// Whether some process other than the caller's child holds a lock on
    /// SQLite's SHARED byte range of `path`. Asked from a forked child,
    /// because POSIX locks are per process: only another process sees them.
    fn shared_range_locked_by_parent(path: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the child only calls async-signal-safe functions (open,
        // fcntl, _exit) before exiting.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                let fd = libc::open(c_path.as_ptr(), libc::O_RDONLY);
                if fd < 0 { libc::_exit(2); }
                let mut lock: libc::flock = std::mem::zeroed();
                lock.l_type = libc::F_WRLCK as _;
                lock.l_whence = libc::SEEK_SET as _;
                lock.l_start = 0x4000_0002; // PENDING_BYTE + 2: SQLite SHARED range
                lock.l_len = 510;
                if libc::fcntl(fd, libc::F_GETLK, &mut lock) != 0 { libc::_exit(2); }
                let unlocked = i32::from(lock.l_type) == i32::from(libc::F_UNLCK);
                libc::_exit(if unlocked { 1 } else { 0 });
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "lock probe child crashed");
        match libc::WEXITSTATUS(status) {
            0 => true,
            1 => false,
            _ => panic!("lock probe child could not inspect the database"),
        }
    }

    /// Opening a second store on the same database must not strip the SQLite
    /// locks a live store in this process holds: closing any descriptor on
    /// the file drops all of this process's POSIX locks on it.
    #[test]
    fn opening_a_second_store_keeps_the_live_stores_sqlite_locks() {
        let home = tempfile::tempdir().unwrap();
        let live = DiscoveryStore::open(home.path()).unwrap();
        let _: i64 = live.conn.query_row("SELECT count(*) FROM discovery_runs", [], |r| r.get(0)).unwrap();
        let db = live.db_path().to_path_buf();
        assert!(shared_range_locked_by_parent(&db), "a WAL connection holds its SHARED lock between reads");
        drop(DiscoveryStore::open(home.path()).unwrap());
        drop(open_budget_connection_for_test(home.path()));
        assert!(shared_range_locked_by_parent(&db), "a later open in this process dropped the live store's lock");
    }

    fn open_budget_connection_for_test(home: &std::path::Path) -> rusqlite::Connection {
        super::super::private_connection(&home.join(super::super::DB_FILE)).unwrap()
    }
}
