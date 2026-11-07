//! Durable replay admission; a lost actor cannot be restarted by an HTTP retry.
use std::{path::Path, sync::{Arc, Mutex}, time::Duration};
use rusqlite::{Connection, OptionalExtension, params};

const RETENTION_SECONDS: i64 = 24 * 60 * 60;
const THREAD_SECONDS: i64 = 15 * 60;
const MAX_ROWS: i64 = 4096;
const MAX_PRINCIPAL_ROWS: i64 = 256;
const MAX_PRINCIPAL_PER_MINUTE: i64 = 32;

#[derive(Clone)]
pub(super) struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Call on a blocking worker (or during synchronous initialization).
    pub(super) fn open(home: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(home).map_err(|e| format!("resolve AG-UI home: {e}"))?
            .join("ag_ui.db");
        let mut conn = Connection::open_with_flags(path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .map_err(|e| format!("open AG-UI journal: {e}"))?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ag_ui_runs (
                session_id TEXT NOT NULL, run_id TEXT NOT NULL, started INTEGER NOT NULL,
                PRIMARY KEY(session_id, run_id));
             CREATE INDEX IF NOT EXISTS idx_ag_ui_runs_started ON ag_ui_runs(started);
             CREATE TABLE IF NOT EXISTS ag_ui_threads (
                session_id TEXT PRIMARY KEY, turn_id TEXT NOT NULL, deadline INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('active','finished')));",
        ).map_err(|e| format!("initialize AG-UI journal: {e}"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AG-UI migration: {e}"))?;
        for (table, migration) in [
            ("ag_ui_runs", "ALTER TABLE ag_ui_runs ADD COLUMN principal TEXT NOT NULL DEFAULT ''"),
            ("ag_ui_threads", "ALTER TABLE ag_ui_threads ADD COLUMN principal TEXT NOT NULL DEFAULT ''"),
        ] {
            let has_principal: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='principal')",
                [table], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if !has_principal { tx.execute_batch(migration).map_err(|e| e.to_string())?; }
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_ag_ui_runs_principal ON ag_ui_runs(principal,started);
             CREATE INDEX IF NOT EXISTS idx_ag_ui_threads_principal ON ag_ui_threads(principal);",
        ).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| format!("commit AG-UI migration: {e}"))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub(super) async fn admit(&self, principal: &str, sid: &str, run_id: &str, has_live_thread: bool) -> Result<String, String> {
        let store = self.clone();
        let (principal, sid, run_id) = (principal.to_owned(), sid.to_owned(), run_id.to_owned());
        tokio::task::spawn_blocking(move || store.admit_at(&principal, &sid, &run_id, has_live_thread, chrono::Utc::now().timestamp()))
            .await.map_err(|e| format!("AG-UI admission task: {e}"))?
    }

    fn admit_at(&self, principal: &str, sid: &str, run_id: &str, has_live_thread: bool, now: i64) -> Result<String, String> {
        if principal.is_empty() || sid.is_empty() || run_id.is_empty() { return Err("invalid_run_identity".into()); }
        let mut conn = self.conn.lock().map_err(|_| "AG-UI journal lock unavailable")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AG-UI admission: {e}"))?;
        tx.execute("DELETE FROM ag_ui_threads WHERE state='finished'", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM ag_ui_runs WHERE started<?1", [now - RETENTION_SECONDS])
            .map_err(|e| e.to_string())?;
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ag_ui_runs WHERE session_id=?1 AND run_id=?2)",
            params![sid, run_id], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        if duplicate { return Err("run_id_reused".into()); }
        let existing: Option<(String, String)> = tx.query_row(
            "SELECT turn_id,principal FROM ag_ui_threads WHERE session_id=?1", [sid], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().map_err(|e| e.to_string())?;
        if existing.as_ref().is_some_and(|(_, owner)| owner != principal) {
            return Err("thread_principal_mismatch".into());
        }
        let existing_turn = existing.map(|(turn, _)| turn);
        let count: i64 = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM ag_ui_runs)+(SELECT COUNT(*) FROM ag_ui_threads)",
            [], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        let additional = if existing_turn.is_none() { 2 } else { 1 };
        if count + additional > MAX_ROWS { return Err("run_journal_full".into()); }
        let principal_count: i64 = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM ag_ui_runs WHERE principal=?1)
                +(SELECT COUNT(*) FROM ag_ui_threads WHERE principal=?1)",
            [principal], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        if principal_count + additional > MAX_PRINCIPAL_ROWS { return Err("principal_journal_full".into()); }
        let recent: i64 = tx.query_row(
            "SELECT COUNT(*) FROM ag_ui_runs WHERE principal=?1 AND started>?2",
            params![principal, now - 60], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        if recent >= MAX_PRINCIPAL_PER_MINUTE { return Err("principal_rate_limit".into()); }
        tx.execute("INSERT INTO ag_ui_runs(session_id,run_id,started,principal) VALUES (?1,?2,?3,?4)",
            params![sid, run_id, now, principal]).map_err(|e| e.to_string())?;
        let orphaned = !has_live_thread && existing_turn.is_some();
        let missing_live_origin = has_live_thread && existing_turn.is_none();
        let turn = existing_turn.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if existing_turn.is_none() && !missing_live_origin {
            tx.execute("INSERT INTO ag_ui_threads(session_id,turn_id,deadline,state,principal) VALUES (?1,?2,?3,'active',?4)",
                params![sid, turn, now + THREAD_SECONDS, principal]).map_err(|e| e.to_string())?;
        }
        // A refused retry still consumes its run id. Never extend an actor's deadline.
        tx.commit().map_err(|e| format!("commit AG-UI admission: {e}"))?;
        if orphaned { Err("orphaned_thread".into()) }
        else if missing_live_origin { Err("missing_live_thread".into()) }
        else { Ok(turn) }
    }

    pub(super) async fn expired_turn(&self, sid: &str) -> Result<Option<String>, String> {
        let store = self.clone();
        let sid = sid.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().map_err(|_| "AG-UI journal lock unavailable")?;
            conn.query_row(
                "SELECT turn_id FROM ag_ui_threads WHERE session_id=?1 AND state='active' AND deadline<=?2",
                params![sid, chrono::Utc::now().timestamp()], |r| r.get(0),
            ).optional().map_err(|e| format!("read expired AG-UI turn: {e}"))
        }).await.map_err(|e| format!("AG-UI expiry task: {e}"))?
    }

    pub(super) async fn finish(&self, sid: &str, turn_id: &str) -> Result<(), String> {
        let store = self.clone();
        let (sid, turn_id) = (sid.to_owned(), turn_id.to_owned());
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().map_err(|_| "AG-UI journal lock unavailable")?;
            conn.execute("UPDATE ag_ui_threads SET state='finished' WHERE session_id=?1 AND turn_id=?2", params![sid, turn_id])
                .map_err(|e| format!("finish AG-UI thread: {e}"))?;
            Ok(())
        }).await.map_err(|e| format!("AG-UI finish task: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_rejects_duplicate_run_and_restart_orphan_durably() {
        let home = tempfile::tempdir().unwrap();
        let first = Store::open(home.path()).unwrap();
        first.admit_at("user:one", "s", "r1", false, 1000).unwrap();
        assert_eq!(first.admit_at("user:one", "s", "r1", true, 1001).unwrap_err(), "run_id_reused");
        drop(first);
        let restarted = Store::open(home.path()).unwrap();
        assert_eq!(restarted.admit_at("user:one", "s", "r2", false, 1002).unwrap_err(), "orphaned_thread");
        assert_eq!(restarted.admit_at("user:one", "s", "r2", true, 1003).unwrap_err(), "run_id_reused");
    }

    #[test]
    fn admission_resume_keeps_deadline_and_expired_actor_remains_fenced() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let turn = store.admit_at("user:one", "s", "r1", false, 1000).unwrap();
        assert!(uuid::Uuid::parse_str(&turn).is_ok());
        assert_eq!(store.admit_at("user:one", "s", "r2", true, 1200).unwrap(), turn);
        let deadline: i64 = store.conn.lock().unwrap().query_row(
            "SELECT deadline FROM ag_ui_threads WHERE session_id='s'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(deadline, 1900);
        assert_eq!(store.admit_at("user:one", "s", "late-resume", true, 1900).unwrap(), turn);
        let deadline: i64 = store.conn.lock().unwrap().query_row(
            "SELECT deadline FROM ag_ui_threads WHERE session_id='s'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(deadline, 1900);
        assert_eq!(store.admit_at("user:one", "s", "r3", false, 1900).unwrap_err(), "orphaned_thread");
        assert_eq!(store.admit_at("user:one", "s", "r1", true, 1901).unwrap_err(), "run_id_reused");
        store.admit_at("user:one", "other", "new", false, 1902).unwrap();
        assert_eq!(store.admit_at("user:one", "s", "r4", false, 1903).unwrap_err(), "orphaned_thread");
    }

    #[tokio::test]
    async fn admission_finish_allows_new_run_without_forgetting_replay_ids() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let first = store.admit("user:one", "s", "r1", false).await.unwrap();
        store.finish("s", &first).await.unwrap();
        let second = store.admit("user:one", "s", "r2", false).await.unwrap();
        assert_ne!(first, second);
        store.finish("s", &first).await.unwrap();
        assert_eq!(store.admit("user:one", "s", "r3", false).await.unwrap_err(), "orphaned_thread");
        assert_eq!(store.admit("user:one", "s", "r1", true).await.unwrap_err(), "run_id_reused");
    }

    #[tokio::test]
    async fn admission_expiry_requires_explicit_owner_finish_before_replacement() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let first = store.admit_at("user:one", "s", "r1", false, 0).unwrap();
        assert_eq!(store.expired_turn("s").await.unwrap().as_deref(), Some(first.as_str()));
        assert!(store.expired_turn("unknown").await.unwrap().is_none());
        assert_eq!(store.admit("user:one", "s", "r2", false).await.unwrap_err(), "orphaned_thread");
        store.finish("s", "wrong-turn").await.unwrap();
        assert_eq!(store.expired_turn("s").await.unwrap().as_deref(), Some(first.as_str()));
        store.finish("s", &first).await.unwrap();
        assert!(store.expired_turn("s").await.unwrap().is_none());
        let second = store.admit("user:one", "s", "r3", false).await.unwrap();
        assert_ne!(first, second);
        assert!(store.expired_turn("s").await.unwrap().is_none());
    }

    #[test]
    fn admission_caps_total_rows_and_reclaims_expired_history() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        store.admit_at("user:one", "s", "r1", false, 1000).unwrap();
        store.conn.lock().unwrap().execute(
            "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<4094)
             INSERT INTO ag_ui_runs(session_id,run_id,started) SELECT 's','extra-'||n,1000 FROM ids", [],
        ).unwrap();
        assert_eq!(store.admit_at("user:one", "s", "full", true, 1001).unwrap_err(), "run_journal_full");
        store.admit_at("user:one", "s", "r1", true, 1001 + RETENTION_SECONDS).unwrap();
        let count: i64 = store.conn.lock().unwrap().query_row(
            "SELECT (SELECT COUNT(*) FROM ag_ui_runs)+(SELECT COUNT(*) FROM ag_ui_threads)", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn admission_principal_capacity_survives_restart_and_leaves_room_for_others() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        store.admit_at("user:one", "s", "r0", false, 1000).unwrap();
        // Seed history in one commit: hundreds of durable admissions make this
        // boundary test depend on disk fsync latency under builder contention.
        {
            let mut conn = store.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 1..MAX_PRINCIPAL_ROWS - 2 {
                tx.execute(
                    "INSERT INTO ag_ui_runs(session_id,run_id,started,principal) VALUES ('s',?1,?2,'user:one')",
                    params![format!("r{i}"), 1000 + i * 3],
                ).unwrap();
            }
            tx.commit().unwrap();
        }
        let last = MAX_PRINCIPAL_ROWS - 2;
        store.admit_at("user:one", "s", &format!("r{last}"), true, 1000 + last * 3).unwrap();
        drop(store);
        let restarted = Store::open(home.path()).unwrap();
        assert_eq!(restarted.admit_at("user:one", "s", "overflow", true, 1800).unwrap_err(), "principal_journal_full");
        for i in 0..100 {
            assert_eq!(restarted.admit_at("user:one", &format!("new-thread-{i}"), "new", false, 1800).unwrap_err(), "principal_journal_full");
        }
        let count: i64 = restarted.conn.lock().unwrap().query_row(
            "SELECT (SELECT COUNT(*) FROM ag_ui_runs)+(SELECT COUNT(*) FROM ag_ui_threads)", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, MAX_PRINCIPAL_ROWS);
        restarted.admit_at("user:two", "other", "allowed", false, 1800).unwrap();
        restarted.admit_at("user:one", "s", "after-retention", true, 1800 + RETENTION_SECONDS).unwrap();
    }

    #[test]
    fn admission_principal_rate_is_durable_and_shared_across_threads() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        for i in 0..MAX_PRINCIPAL_PER_MINUTE {
            store.admit_at("user:one", &format!("thread-{i}"), "run", false, 1000).unwrap();
        }
        let other_process = Store::open(home.path()).unwrap();
        assert_eq!(other_process.admit_at("user:one", "another-agent-thread", "run", false, 1059).unwrap_err(), "principal_rate_limit");
        other_process.admit_at("user:two", "other", "run", false, 1059).unwrap();
        other_process.admit_at("user:one", "another-agent-thread", "run", false, 1060).unwrap();
    }

    #[test]
    fn admission_refuses_principal_changes_without_allocating_rows() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        store.admit_at("user:one", "s", "original", false, 1000).unwrap();
        assert_eq!(store.admit_at("user:two", "s", "new", true, 1001).unwrap_err(), "thread_principal_mismatch");
        let count: i64 = store.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM ag_ui_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn admission_migrates_legacy_schema_without_dropping_replay_fences() {
        let home = tempfile::tempdir().unwrap();
        let conn = Connection::open(home.path().join("ag_ui.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE ag_ui_runs(session_id TEXT NOT NULL,run_id TEXT NOT NULL,started INTEGER NOT NULL,PRIMARY KEY(session_id,run_id));
             CREATE TABLE ag_ui_threads(session_id TEXT PRIMARY KEY,turn_id TEXT NOT NULL,deadline INTEGER NOT NULL,state TEXT NOT NULL);
             INSERT INTO ag_ui_runs VALUES ('legacy','old',1000);
             INSERT INTO ag_ui_threads VALUES ('legacy','old-turn',1900,'active');",
        ).unwrap();
        drop(conn);
        let store = Store::open(home.path()).unwrap();
        assert_eq!(store.admit_at("user:one", "legacy", "old", true, 1001).unwrap_err(), "run_id_reused");
        assert_eq!(store.admit_at("user:one", "legacy", "new", true, 1001).unwrap_err(), "thread_principal_mismatch");
        store.admit_at("user:one", "new-thread", "run", false, 1001).unwrap();
    }

}
