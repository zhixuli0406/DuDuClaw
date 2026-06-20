//! `ForkStore` — cross-process SQLite source of truth for fork state (RFC-26 P5
//! follow-up).
//!
//! Forks execute in the **MCP-server** process, but the **gateway** serves
//! `/metrics` and the dashboard RPC. An in-process registry can't span both, so
//! fork + branch state lives in a WAL SQLite DB under `~/.duduclaw/fork_store.db`
//! that both processes open. WAL mode + a busy timeout make concurrent
//! reader/writer access safe (same pattern as `SqliteMemoryEngine`).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::error::{ForkError, Result};

fn map_err(e: rusqlite::Error) -> ForkError {
    ForkError::Executor(format!("fork store: {e}"))
}

/// One row in the `forks` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ForkRow {
    pub fork_id: String,
    pub agent_id: String,
    pub prompt: String,
    pub merge_mode: String,
    pub resolved: bool,
    pub winner: Option<String>,
    pub promoted: bool,
    pub aggregate_spent_usd: f64,
    pub created_at: String,
}

/// One row in the `fork_branches` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BranchRow {
    pub branch_id: String,
    pub fork_id: String,
    pub steering: Option<String>,
    pub budget_usd: f64,
    pub state: String,
    pub spent_usd: f64,
    pub output: String,
    pub test_exit_code: Option<i64>,
}

/// Aggregate counts for `/metrics` and dashboards.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct ForkStoreMetrics {
    pub forks_total: u64,
    pub forks_resolved: u64,
    pub forks_promoted: u64,
    pub branches_total: u64,
    pub branches_finished: u64,
    pub branches_budget_killed: u64,
    pub branches_failed: u64,
    pub aggregate_spent_usd: f64,
}

/// WAL SQLite-backed fork store.
pub struct ForkStore {
    conn: Mutex<Connection>,
}

impl ForkStore {
    /// Open (creating + migrating) the store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_err)?;
        Self::init(conn)
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_err)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS forks (
                 fork_id TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL,
                 prompt TEXT NOT NULL,
                 merge_mode TEXT NOT NULL,
                 resolved INTEGER NOT NULL DEFAULT 0,
                 winner TEXT,
                 promoted INTEGER NOT NULL DEFAULT 0,
                 aggregate_spent_usd REAL NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS fork_branches (
                 branch_id TEXT PRIMARY KEY,
                 fork_id TEXT NOT NULL,
                 steering TEXT,
                 budget_usd REAL NOT NULL DEFAULT 0,
                 state TEXT NOT NULL,
                 spent_usd REAL NOT NULL DEFAULT 0,
                 output TEXT NOT NULL DEFAULT '',
                 test_exit_code INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_fork_branches_fork ON fork_branches(fork_id);
             CREATE INDEX IF NOT EXISTS idx_forks_created ON forks(created_at);",
        )
        .map_err(map_err)?;
        Ok(ForkStore { conn: Mutex::new(conn) })
    }

    /// Insert a new fork plus its branches in one transaction.
    pub fn insert_fork(&self, fork: &ForkRow, branches: &[BranchRow]) -> Result<()> {
        let mut conn = self.conn.lock().expect("fork store poisoned");
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT OR REPLACE INTO forks
               (fork_id, agent_id, prompt, merge_mode, resolved, winner, promoted, aggregate_spent_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                fork.fork_id, fork.agent_id, fork.prompt, fork.merge_mode,
                fork.resolved as i64, fork.winner, fork.promoted as i64,
                fork.aggregate_spent_usd, fork.created_at,
            ],
        )
        .map_err(map_err)?;
        for b in branches {
            tx.execute(
                "INSERT OR REPLACE INTO fork_branches
                   (branch_id, fork_id, steering, budget_usd, state, spent_usd, output, test_exit_code)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    b.branch_id, b.fork_id, b.steering, b.budget_usd, b.state,
                    b.spent_usd, b.output, b.test_exit_code,
                ],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Update one branch's mutable fields (state / spend / output / test result).
    pub fn update_branch(
        &self,
        branch_id: &str,
        state: &str,
        spent_usd: f64,
        output: &str,
        test_exit_code: Option<i64>,
    ) -> Result<bool> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let n = conn
            .execute(
                "UPDATE fork_branches SET state=?2, spent_usd=?3, output=?4, test_exit_code=?5 WHERE branch_id=?1",
                params![branch_id, state, spent_usd, output, test_exit_code],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Set the resolution of a fork (winner / promoted / resolved / spend).
    pub fn set_resolution(
        &self,
        fork_id: &str,
        winner: Option<&str>,
        promoted: bool,
        resolved: bool,
        aggregate_spent_usd: f64,
    ) -> Result<bool> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let n = conn
            .execute(
                "UPDATE forks SET winner=?2, promoted=?3, resolved=?4, aggregate_spent_usd=?5 WHERE fork_id=?1",
                params![fork_id, winner, promoted as i64, resolved as i64, aggregate_spent_usd],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Mark every branch of a fork as `state` (used for Running transition).
    pub fn set_all_branch_states(&self, fork_id: &str, state: &str) -> Result<()> {
        let conn = self.conn.lock().expect("fork store poisoned");
        conn.execute(
            "UPDATE fork_branches SET state=?2 WHERE fork_id=?1",
            params![fork_id, state],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn get_fork(&self, fork_id: &str) -> Result<Option<ForkRow>> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let row = conn
            .query_row(
                "SELECT fork_id, agent_id, prompt, merge_mode, resolved, winner, promoted, aggregate_spent_usd, created_at
                 FROM forks WHERE fork_id=?1",
                params![fork_id],
                Self::map_fork_row,
            )
            .ok();
        Ok(row)
    }

    pub fn list_branches(&self, fork_id: &str) -> Result<Vec<BranchRow>> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT branch_id, fork_id, steering, budget_usd, state, spent_usd, output, test_exit_code
                 FROM fork_branches WHERE fork_id=?1 ORDER BY branch_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![fork_id], Self::map_branch_row)
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Most recent forks, newest first.
    pub fn list_forks(&self, limit: usize) -> Result<Vec<ForkRow>> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT fork_id, agent_id, prompt, merge_mode, resolved, winner, promoted, aggregate_spent_usd, created_at
                 FROM forks ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![limit as i64], Self::map_fork_row)
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Aggregate metrics for `/metrics` exposition.
    pub fn metrics(&self) -> Result<ForkStoreMetrics> {
        let conn = self.conn.lock().expect("fork store poisoned");
        let (forks_total, forks_resolved, forks_promoted, spent): (u64, u64, u64, f64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(resolved),0), COALESCE(SUM(promoted),0), COALESCE(SUM(aggregate_spent_usd),0) FROM forks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(map_err)?;
        let count_state = |state: &str| -> Result<u64> {
            conn.query_row(
                "SELECT COUNT(*) FROM fork_branches WHERE state=?1",
                params![state],
                |r| r.get(0),
            )
            .map_err(map_err)
        };
        let branches_total: u64 = conn
            .query_row("SELECT COUNT(*) FROM fork_branches", [], |r| r.get(0))
            .map_err(map_err)?;
        Ok(ForkStoreMetrics {
            forks_total,
            forks_resolved,
            forks_promoted,
            branches_total,
            branches_finished: count_state("finished")?,
            branches_budget_killed: count_state("budget_killed")?,
            branches_failed: count_state("failed")?,
            aggregate_spent_usd: spent,
        })
    }

    fn map_fork_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForkRow> {
        Ok(ForkRow {
            fork_id: row.get(0)?,
            agent_id: row.get(1)?,
            prompt: row.get(2)?,
            merge_mode: row.get(3)?,
            resolved: row.get::<_, i64>(4)? != 0,
            winner: row.get(5)?,
            promoted: row.get::<_, i64>(6)? != 0,
            aggregate_spent_usd: row.get(7)?,
            created_at: row.get(8)?,
        })
    }

    fn map_branch_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BranchRow> {
        Ok(BranchRow {
            branch_id: row.get(0)?,
            fork_id: row.get(1)?,
            steering: row.get(2)?,
            budget_usd: row.get(3)?,
            state: row.get(4)?,
            spent_usd: row.get(5)?,
            output: row.get(6)?,
            test_exit_code: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork_row(id: &str) -> ForkRow {
        ForkRow {
            fork_id: id.into(),
            agent_id: "a1".into(),
            prompt: "solve".into(),
            merge_mode: "auto".into(),
            resolved: false,
            winner: None,
            promoted: false,
            aggregate_spent_usd: 0.0,
            created_at: "2026-06-19T00:00:00Z".into(),
        }
    }
    fn branch_row(bid: &str, fid: &str, state: &str) -> BranchRow {
        BranchRow {
            branch_id: bid.into(),
            fork_id: fid.into(),
            steering: Some("s".into()),
            budget_usd: 0.5,
            state: state.into(),
            spent_usd: 0.0,
            output: String::new(),
            test_exit_code: None,
        }
    }

    #[test]
    fn insert_and_read_back() {
        let s = ForkStore::open_in_memory().unwrap();
        s.insert_fork(
            &fork_row("f1"),
            &[branch_row("b1", "f1", "pending"), branch_row("b2", "f1", "pending")],
        )
        .unwrap();

        let f = s.get_fork("f1").unwrap().unwrap();
        assert_eq!(f.agent_id, "a1");
        let branches = s.list_branches("f1").unwrap();
        assert_eq!(branches.len(), 2);
    }

    #[test]
    fn unknown_fork_is_none() {
        let s = ForkStore::open_in_memory().unwrap();
        assert!(s.get_fork("nope").unwrap().is_none());
        assert!(s.list_branches("nope").unwrap().is_empty());
    }

    #[test]
    fn update_branch_and_resolution() {
        let s = ForkStore::open_in_memory().unwrap();
        s.insert_fork(&fork_row("f1"), &[branch_row("b1", "f1", "running")]).unwrap();

        assert!(s.update_branch("b1", "finished", 0.12, "done", Some(0)).unwrap());
        let b = &s.list_branches("f1").unwrap()[0];
        assert_eq!(b.state, "finished");
        assert_eq!(b.spent_usd, 0.12);
        assert_eq!(b.test_exit_code, Some(0));

        assert!(s.set_resolution("f1", Some("b1"), true, true, 0.12).unwrap());
        let f = s.get_fork("f1").unwrap().unwrap();
        assert_eq!(f.winner.as_deref(), Some("b1"));
        assert!(f.promoted);
        assert!(f.resolved);
    }

    #[test]
    fn update_unknown_branch_returns_false() {
        let s = ForkStore::open_in_memory().unwrap();
        assert!(!s.update_branch("ghost", "finished", 0.0, "", None).unwrap());
    }

    #[test]
    fn set_all_branch_states() {
        let s = ForkStore::open_in_memory().unwrap();
        s.insert_fork(
            &fork_row("f1"),
            &[branch_row("b1", "f1", "pending"), branch_row("b2", "f1", "pending")],
        )
        .unwrap();
        s.set_all_branch_states("f1", "running").unwrap();
        assert!(s.list_branches("f1").unwrap().iter().all(|b| b.state == "running"));
    }

    #[test]
    fn list_forks_newest_first() {
        let s = ForkStore::open_in_memory().unwrap();
        let mut f1 = fork_row("f1");
        f1.created_at = "2026-06-19T01:00:00Z".into();
        let mut f2 = fork_row("f2");
        f2.created_at = "2026-06-19T02:00:00Z".into();
        s.insert_fork(&f1, &[]).unwrap();
        s.insert_fork(&f2, &[]).unwrap();
        let forks = s.list_forks(10).unwrap();
        assert_eq!(forks[0].fork_id, "f2"); // newest first
        assert_eq!(forks.len(), 2);
    }

    #[test]
    fn metrics_aggregate() {
        let s = ForkStore::open_in_memory().unwrap();
        s.insert_fork(
            &fork_row("f1"),
            &[
                branch_row("b1", "f1", "finished"),
                branch_row("b2", "f1", "failed"),
                branch_row("b3", "f1", "budget_killed"),
            ],
        )
        .unwrap();
        s.set_resolution("f1", Some("b1"), true, true, 0.3).unwrap();

        let m = s.metrics().unwrap();
        assert_eq!(m.forks_total, 1);
        assert_eq!(m.forks_resolved, 1);
        assert_eq!(m.forks_promoted, 1);
        assert_eq!(m.branches_total, 3);
        assert_eq!(m.branches_finished, 1);
        assert_eq!(m.branches_failed, 1);
        assert_eq!(m.branches_budget_killed, 1);
        assert!((m.aggregate_spent_usd - 0.3).abs() < 1e-9);
    }

    #[test]
    fn cross_connection_visibility() {
        // Two ForkStore handles on the same file see each other's writes (WAL).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fork_store.db");
        let writer = ForkStore::open(&path).unwrap();
        writer.insert_fork(&fork_row("f1"), &[branch_row("b1", "f1", "running")]).unwrap();

        let reader = ForkStore::open(&path).unwrap();
        assert!(reader.get_fork("f1").unwrap().is_some());
        assert_eq!(reader.list_branches("f1").unwrap().len(), 1);
    }
}
