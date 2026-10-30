//! Host-validated connector stamps, committed with source lineage.

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{params, Connection};
use super::db::OriginRow;

fn e(error: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(error.to_string())
}

pub(crate) fn valid_ref(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        && (matches!(value, "unknown" | "none" | "google:home" | "google:delegation")
            || ["google:person:", "db:", "remote_mcp:", "channel:"]
                .iter().any(|prefix| value.strip_prefix(*prefix).is_some_and(|rest| !rest.is_empty())))
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let current: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_meta WHERE key='connector_schema_version' AND value='2')",
        [], |r| r.get(0),
    ).map_err(e)?;
    if current { return Ok(()); }
    let result = (|| -> Result<()> {
        conn.execute_batch(
        "SAVEPOINT connector_schema_v2;
         CREATE TABLE IF NOT EXISTS memory_connector_stamps (
            memory_store TEXT NOT NULL CHECK(memory_store IN ('memories','key_facts')),
            memory_id TEXT NOT NULL, connector_ref TEXT NOT NULL, stamped_at TEXT NOT NULL,
            connector_observed_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY(memory_store,memory_id,connector_ref));
         CREATE INDEX IF NOT EXISTS idx_memory_connector_ref
            ON memory_connector_stamps(connector_ref,memory_store,memory_id);
         CREATE TABLE IF NOT EXISTS forgotten_connectors (
            agent_id TEXT NOT NULL, connector_ref TEXT NOT NULL, upto_time TEXT NOT NULL,
            plan_id TEXT NOT NULL, PRIMARY KEY(agent_id,connector_ref));
         CREATE TABLE IF NOT EXISTS memory_forget_apply_reports (
            plan_id TEXT PRIMARY KEY, report_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS disconnect_forget_batches (
            id TEXT PRIMARY KEY, connector_ref TEXT NOT NULL, display_name TEXT NOT NULL,
            created_at TEXT NOT NULL, expires_at TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'planned', approval_id TEXT, digest TEXT NOT NULL,
            approved_by TEXT, lease_holder TEXT, lease_until INTEGER NOT NULL DEFAULT 0,
            last_error TEXT);
         CREATE TABLE IF NOT EXISTS disconnect_forget_batch_plans (
            batch_id TEXT NOT NULL, agent_id TEXT NOT NULL, approved_plan_id TEXT NOT NULL,
            apply_plan_id TEXT, PRIMARY KEY(batch_id,agent_id));
         CREATE TABLE IF NOT EXISTS forget_receipts (
            id TEXT PRIMARY KEY, batch_id TEXT NOT NULL UNIQUE,
            completed_at TEXT NOT NULL, receipt_json TEXT NOT NULL);"
        ).map_err(e)?;
        let has_time: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('memory_connector_stamps') WHERE name='connector_observed_at')",
            [], |r| r.get(0),
        ).map_err(e)?;
        if !has_time {
            conn.execute_batch("ALTER TABLE memory_connector_stamps ADD COLUMN connector_observed_at TEXT NOT NULL DEFAULT '';").map_err(e)?;
        }
        conn.execute_batch(
            "UPDATE memory_connector_stamps SET connector_observed_at=COALESCE(
                (SELECT MIN(o.source_observed_at) FROM memory_origins o
                 WHERE o.memory_store=memory_connector_stamps.memory_store AND o.memory_id=memory_connector_stamps.memory_id), stamped_at)
             WHERE connector_observed_at='';
             DROP TRIGGER IF EXISTS trg_connector_stamp_fence;
             CREATE TRIGGER trg_connector_stamp_fence BEFORE INSERT ON memory_connector_stamps
             WHEN EXISTS (
                SELECT 1 FROM memory_origins o JOIN forgotten_connectors f
                  ON f.agent_id=o.agent_id AND f.connector_ref=NEW.connector_ref
                WHERE o.memory_store=NEW.memory_store AND o.memory_id=NEW.memory_id
                  AND NEW.connector_observed_at <= f.upto_time)
             BEGIN SELECT RAISE(ABORT,'duduclaw:connector_forgotten'); END;
             CREATE TRIGGER IF NOT EXISTS trg_connector_stamp_update_fence BEFORE UPDATE ON memory_connector_stamps
             WHEN EXISTS (
                SELECT 1 FROM memory_origins o JOIN forgotten_connectors f
                  ON f.agent_id=o.agent_id AND f.connector_ref=NEW.connector_ref
                WHERE o.memory_store=NEW.memory_store AND o.memory_id=NEW.memory_id
                  AND NEW.connector_observed_at <= f.upto_time)
             BEGIN SELECT RAISE(ABORT,'duduclaw:connector_forgotten'); END;
             INSERT OR REPLACE INTO memory_meta(key,value) VALUES('connector_schema_version','2');
             RELEASE connector_schema_v2;"
        ).map_err(e)
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK TO connector_schema_v2; RELEASE connector_schema_v2;");
    }
    result
}

pub(crate) fn load(conn: &Connection, store: &str, id: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT connector_ref,connector_observed_at FROM memory_connector_stamps WHERE memory_store=?1 AND memory_id=?2"
    ).map_err(e)?;
    let rows = stmt.query_map(params![store,id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
    rows.collect::<std::result::Result<Vec<_>,_>>().map_err(e)
}

pub(crate) fn fenced(conn: &Connection, agent: &str, rows: &[OriginRow]) -> Result<bool> {
    let references: std::collections::BTreeSet<&str> = rows.iter()
        .flat_map(|row| row.connectors.iter().map(|(reference, _)| reference.as_str())).collect();
    if references.len() > super::MAX_LINEAGE_SOURCES {
        return Err(e("connector stamp limit exceeded"));
    }
    for row in rows {
        for (reference, observed_at) in &row.connectors {
            let hit: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM forgotten_connectors
                 WHERE agent_id=?1 AND connector_ref=?2 AND upto_time>=?3)",
                params![agent,reference,observed_at], |r| r.get(0),
            ).map_err(e)?;
            if hit { return Ok(true); }
        }
    }
    Ok(false)
}

pub(crate) fn insert(conn: &Connection, store: &str, id: &str, rows: &[OriginRow]) -> Result<()> {
    let now = super::format_ts(chrono::Utc::now());
    for row in rows {
        for (reference, observed_at) in &row.connectors {
            conn.execute(
                "INSERT INTO memory_connector_stamps
                 (memory_store,memory_id,connector_ref,stamped_at,connector_observed_at) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(memory_store,memory_id,connector_ref) DO UPDATE SET
                    connector_observed_at=MIN(memory_connector_stamps.connector_observed_at,excluded.connector_observed_at)",
                params![store,id,reference,now,observed_at],
            ).map_err(e)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_stamps_gain_conservative_observation_time_without_losing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memory_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO memory_meta VALUES('connector_schema_version','1');
             CREATE TABLE memory_origins(memory_store TEXT,memory_id TEXT,agent_id TEXT,source_observed_at TEXT);
             INSERT INTO memory_origins VALUES('memories','m','bot','2026-10-01T00:00:00.000Z');
             CREATE TABLE memory_connector_stamps(memory_store TEXT,memory_id TEXT,connector_ref TEXT,stamped_at TEXT,
                PRIMARY KEY(memory_store,memory_id,connector_ref));
             INSERT INTO memory_connector_stamps VALUES('memories','m','google:home','2026-10-30T00:00:00.000Z');"
        ).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(load(&conn, "memories", "m").unwrap(), vec![("google:home".into(), "2026-10-01T00:00:00.000Z".into())]);
        migrate(&conn).unwrap();
        assert_eq!(load(&conn, "memories", "m").unwrap().len(), 1);
        let version: String = conn.query_row("SELECT value FROM memory_meta WHERE key='connector_schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(version, "2");
    }
}
