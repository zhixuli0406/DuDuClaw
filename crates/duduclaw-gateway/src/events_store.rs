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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Marker stamped on rows written by
/// [`crate::os_events::spawn_os_event_persistence`] (P4-1 integration-gap
/// closer) — an `os_file` / `os_frontmost` event that has ALREADY been
/// broadcast in-process by its originating forwarder (`OsWatcherRegistry`'s
/// watcher task, or `os_frontmost::spawn_agent_poll`). These rows exist in
/// `events.db` purely so out-of-process / batch readers (today:
/// [`crate::rule_induction::RuleInductor`], via [`EventBusStore::fetch_since`])
/// can see perception history — they must NOT be re-broadcast onto the
/// autopilot bus a second time. `crate::autopilot_engine::spawn_events_db_poll`
/// checks this marker and skips those rows; every other producer (MCP
/// subprocess `task.created`/`activity.new`/`task.updated`, or any future
/// out-of-process `os_file` writer) leaves `source` unset and is rebroadcast
/// as before.
pub const SOURCE_INTERNAL_BROADCAST: &str = "internal_broadcast";

/// One row in the `events` table. `payload` is a JSON blob serialized
/// as text. `id` is assigned by SQLite on INSERT. `source` is `None` for
/// every event bus write except the internal os-event persistence bridge
/// (see [`SOURCE_INTERNAL_BROADCAST`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub id: i64,
    pub event: String,
    pub payload: String,
    pub ts: String,
    #[serde(default)]
    pub source: Option<String>,
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
        Self::migrate_add_source_column(&conn)?;
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

    /// Idempotently add the `source` column (P4-1 integration-gap closer).
    /// Safe to call on every `open()` — a `PRAGMA table_info` existence check
    /// guards the `ALTER TABLE`, same convention as `task_store::add_dispatch_columns`
    /// / `cost_telemetry`'s additive migrations.
    fn migrate_add_source_column(conn: &Connection) -> Result<(), String> {
        let existing: HashSet<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(events)")
                .map_err(|e| format!("pragma table_info: {e}"))?;
            stmt.query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("query table_info: {e}"))?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|e| format!("collect table_info: {e}"))?
        };
        if !existing.contains("source") {
            conn.execute("ALTER TABLE events ADD COLUMN source TEXT", [])
                .map_err(|e| format!("add column source: {e}"))?;
        }
        Ok(())
    }

    /// Append one event. Payload is an already-serialized JSON string.
    pub async fn append(&self, event: &str, payload: &str) -> Result<(), String> {
        self.append_with_ts(event, payload, &Utc::now().to_rfc3339())
            .await
    }

    /// Append one event with an explicit RFC3339 timestamp. Lets an
    /// out-of-process producer preserve the *original* event time rather than
    /// the write time (used by ts-sensitive consumers such as `rule_induction`
    /// pattern detection, and by tests that need deterministic timestamps).
    /// `source` is left unset — see [`Self::append_with_source`] for the
    /// internal os-event persistence bridge.
    pub async fn append_with_ts(
        &self,
        event: &str,
        payload: &str,
        ts_rfc3339: &str,
    ) -> Result<(), String> {
        self.append_with_source(event, payload, ts_rfc3339, None)
            .await
    }

    /// Append one event with an explicit timestamp AND `source` marker (see
    /// [`SOURCE_INTERNAL_BROADCAST`]). `source = None` is byte-identical to
    /// [`Self::append_with_ts`]'s behavior — every existing caller is
    /// unaffected.
    pub async fn append_with_source(
        &self,
        event: &str,
        payload: &str,
        ts_rfc3339: &str,
        source: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO events (event, payload, ts, source) VALUES (?1, ?2, ?3, ?4)",
            params![event, payload, ts_rfc3339, source],
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
                "SELECT id, event, payload, ts, source FROM events
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
                    source: r.get(4)?,
                })
            })
            .map_err(|e| format!("query events: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect events: {e}"))?;
        Ok(rows)
    }

    /// Fetch the most recent up-to-`limit` events whose `event` name starts
    /// with `prefix` (e.g. `"os_"` for OS-native perception events), newest
    /// first. Used by the dashboard `os.events.recent` RPC. `limit` is applied
    /// in SQL (`ORDER BY id DESC LIMIT`) so this stays O(limit) regardless of
    /// table size — no full-table scan even under the 7-day retention window.
    pub async fn fetch_recent_by_prefix(
        &self,
        prefix: &str,
        limit: i64,
    ) -> Result<Vec<EventRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, event, payload, ts, source FROM events
                 WHERE event LIKE ?1 ESCAPE '\\'
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|e| format!("prepare fetch_recent_by_prefix: {e}"))?;
        // Escape LIKE metacharacters in the caller-supplied prefix so a literal
        // `_` / `%` (e.g. "os_") matches literally, then append the wildcard.
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{escaped}%");
        let rows = stmt
            .query_map(params![pattern, limit], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    event: r.get(1)?,
                    payload: r.get(2)?,
                    ts: r.get(3)?,
                    source: r.get(4)?,
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

    #[tokio::test(flavor = "current_thread")]
    async fn append_without_source_round_trips_none() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        store.append("task.created", "{}").await.unwrap();
        let rows = store.fetch_since(0, 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_with_source_round_trips_marker() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        store
            .append_with_source(
                "os_file",
                r#"{"agent_id":"a1","path":"/inbox/x.pdf","kind":"created"}"#,
                &Utc::now().to_rfc3339(),
                Some(SOURCE_INTERNAL_BROADCAST),
            )
            .await
            .unwrap();
        let rows = store.fetch_since(0, 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source.as_deref(), Some(SOURCE_INTERNAL_BROADCAST));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_recent_by_prefix_filters_and_orders_newest_first() {
        let home = fresh_home();
        let store = EventBusStore::open(&home).unwrap();
        store.append("task.created", "{}").await.unwrap();
        store.append("os_file", r#"{"n":1}"#).await.unwrap();
        store.append("os_frontmost", r#"{"n":2}"#).await.unwrap();
        store.append("activity.new", "{}").await.unwrap();
        store.append("os_file", r#"{"n":3}"#).await.unwrap();

        // Only os_* rows, newest first, capped by limit.
        let rows = store.fetch_recent_by_prefix("os_", 10).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].event, "os_file"); // most recent os_ row
        assert_eq!(rows[0].payload, r#"{"n":3}"#);
        assert_eq!(rows[1].event, "os_frontmost");
        assert_eq!(rows[2].event, "os_file");
        assert!(rows.iter().all(|r| r.event.starts_with("os_")));

        // Limit clamps the tail.
        let one = store.fetch_recent_by_prefix("os_", 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].payload, r#"{"n":3}"#);

        // The `_` in the prefix is a literal, not a LIKE wildcard: a
        // hypothetical "osXfile" event must NOT match "os_".
        store.append("osXfile", "{}").await.unwrap();
        let rows2 = store.fetch_recent_by_prefix("os_", 10).await.unwrap();
        assert_eq!(rows2.len(), 3, "os_ must not match osXfile");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The `source` column migration must be idempotent — re-opening an
    /// already-migrated database (a second gateway start, or a warm restart)
    /// must not error on the `ALTER TABLE`.
    #[tokio::test(flavor = "current_thread")]
    async fn reopen_after_migration_is_idempotent() {
        let home = fresh_home();
        {
            let store = EventBusStore::open(&home).unwrap();
            store
                .append_with_source("os_file", "{}", &Utc::now().to_rfc3339(), Some("x"))
                .await
                .unwrap();
        }
        // Re-open the same DB file — migrate_add_source_column must be a no-op.
        let store2 = EventBusStore::open(&home).unwrap();
        let rows = store2.fetch_since(0, 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source.as_deref(), Some("x"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
