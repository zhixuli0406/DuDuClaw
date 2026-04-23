//! SQLite-backed event bus replacing the `events.jsonl` file bus.
//!
//! Purpose: transport events from the MCP subprocess(es) into the
//! gateway's in-process `AutopilotEngine` broadcast channel. The old
//! file-based bus had several correctness issues (rotation race,
//! permission concerns, partial-line reads, unbounded growth) — SQLite
//! eliminates all of them by design:
//!
//! * Atomic per-row INSERT (no partial writes).
//! * WAL mode + 5s busy_timeout → safe concurrent writers from multiple
//!   MCP subprocesses + the gateway reader.
//! * Monotonic `id INTEGER PRIMARY KEY AUTOINCREMENT` → reader simply
//!   tracks `last_seen_id` and queries `WHERE id > ?`.
//! * Built-in retention: `DELETE WHERE ts < ?` purges old rows.
//! * File permissions are managed by SQLite (0600 via umask).
//!
//! Schema is self-healing; `open()` creates the table if missing so MCP
//! subprocesses can write before the gateway's own `open()` runs.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::info;

/// One row in the `events` table. `payload` is a JSON blob serialized
/// as text. `id` is assigned by SQLite on INSERT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub id: i64,
    pub event: String,
    pub payload: String,
    pub ts: String,
}

/// Thread-safe SQLite event bus.
///
/// Writers (MCP) call [`append`]. Readers (AutopilotEngine tail task)
/// call [`fetch_since`] repeatedly with a monotonically-increasing
/// `last_seen_id`.
pub struct EventBusStore {
    conn: tokio::sync::Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl EventBusStore {
    /// Open (or create) the event bus at `<home>/events.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("events.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open event bus: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "EventBusStore initialized");
        Ok(Self {
            conn: tokio::sync::Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS events (
                 id       INTEGER PRIMARY KEY AUTOINCREMENT,
                 event    TEXT NOT NULL,
                 payload  TEXT NOT NULL,
                 ts       TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_events_id ON events(id);
             CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);",
        )
        .map_err(|e| format!("init event bus schema: {e}"))?;
        Ok(())
    }

    /// Append one event. Payload is an already-serialized JSON string.
    pub async fn append(&self, event: &str, payload: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO events (event, payload, ts) VALUES (?1, ?2, ?3)",
            params![event, payload, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("append event: {e}"))?;
        Ok(())
    }

    /// Fetch up to `limit` events with `id > last_seen_id`, ordered by id.
    pub async fn fetch_since(
        &self,
        last_seen_id: i64,
        limit: i64,
    ) -> Result<Vec<EventRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, event, payload, ts FROM events
                 WHERE id > ?1
                 ORDER BY id ASC
                 LIMIT ?2",
            )
            .map_err(|e| format!("prepare fetch_since: {e}"))?;
        let rows = stmt
            .query_map(params![last_seen_id, limit], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    event: r.get(1)?,
                    payload: r.get(2)?,
                    ts: r.get(3)?,
                })
            })
            .map_err(|e| format!("query events: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect events: {e}"))?;
        Ok(rows)
    }

    /// Return the current MAX(id) — used on startup so historical events
    /// aren't replayed.
    pub async fn max_id(&self) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        let id: i64 = conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
            .map_err(|e| format!("max_id: {e}"))?;
        Ok(id)
    }

    /// Delete events older than `cutoff_iso` (RFC3339 timestamp string).
    /// Returns the number of rows deleted.
    pub async fn prune_before(&self, cutoff_iso: &str) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute("DELETE FROM events WHERE ts < ?1", params![cutoff_iso])
            .map_err(|e| format!("prune events: {e}"))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "duduclaw-eventbus-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_then_fetch_since_0_returns_all() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        store.append("task.created", r#"{"id":"t1"}"#).await.unwrap();
        store.append("task.updated", r#"{"id":"t1"}"#).await.unwrap();

        let rows = store.fetch_since(0, 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].event, "task.created");
        assert_eq!(rows[1].event, "task.updated");
        // IDs are monotonically increasing
        assert!(rows[1].id > rows[0].id);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_since_skips_already_seen() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        store.append("a", "{}").await.unwrap();
        store.append("b", "{}").await.unwrap();
        store.append("c", "{}").await.unwrap();

        let first_batch = store.fetch_since(0, 100).await.unwrap();
        let last_id = first_batch.last().unwrap().id;

        // No new events since last_id
        assert!(store.fetch_since(last_id, 100).await.unwrap().is_empty());

        // Append more → should pick up only those
        store.append("d", "{}").await.unwrap();
        let delta = store.fetch_since(last_id, 100).await.unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].event, "d");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn max_id_seeds_correctly_for_startup() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        assert_eq!(store.max_id().await.unwrap(), 0);
        store.append("seed", "{}").await.unwrap();
        store.append("seed2", "{}").await.unwrap();
        let max = store.max_id().await.unwrap();
        assert!(max >= 2);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prune_before_deletes_old_rows() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        // Insert rows with explicit timestamps via raw SQL to sidestep
        // append()'s Utc::now() — simulate older rows.
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "INSERT INTO events (event, payload, ts) VALUES ('old', '{}', '2020-01-01T00:00:00Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO events (event, payload, ts) VALUES ('new', '{}', '2099-01-01T00:00:00Z')",
                [],
            ).unwrap();
        }
        let deleted = store.prune_before("2025-01-01T00:00:00Z").await.unwrap();
        assert_eq!(deleted, 1);
        let remaining = store.fetch_since(0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event, "new");

        let _ = std::fs::remove_dir_all(&home);
    }
}
