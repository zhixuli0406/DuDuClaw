//! SQLite-backed persistent store for cron tasks.
//!
//! Replaces the legacy `cron_tasks.jsonl` file with a proper relational store
//! that supports CRUD, run history, and concurrent access from both the gateway
//! process (`CronScheduler`, dashboard handlers) and the MCP subprocess
//! (`handle_schedule_task`). WAL mode + 5s busy_timeout enables multi-process
//! safety without explicit coordination.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// One row in the `cron_tasks` table.
///
/// `notify_channel` / `notify_chat_id` / `notify_thread_id` are optional
/// routing hints: when present, the scheduler delivers the agent's response
/// to that channel after a successful run. When absent, the run is recorded
/// silently (same as pre-v1.8.22 behaviour).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTaskRow {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub cron: String,
    pub task: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub run_count: i64,
    pub failure_count: i64,
    /// Target channel type: "discord" | "telegram" | "line" | "slack" |
    /// "whatsapp" | "feishu" | "webchat". `None` → no auto-delivery.
    #[serde(default)]
    pub notify_channel: Option<String>,
    /// Target chat / channel / room ID on the notify platform.
    #[serde(default)]
    pub notify_chat_id: Option<String>,
    /// Optional Discord thread ID (only meaningful when notify_channel="discord").
    #[serde(default)]
    pub notify_thread_id: Option<String>,
}

impl CronTaskRow {
    /// Build a fresh row with sensible defaults and `created_at`/`updated_at` set to now.
    pub fn new(id: String, name: String, agent_id: String, cron: String, task: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id,
            name,
            agent_id,
            cron,
            task,
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: 0,
            failure_count: 0,
            notify_channel: None,
            notify_chat_id: None,
            notify_thread_id: None,
        }
    }

    /// True iff this row is configured to deliver its response to a channel.
    pub fn has_notify_target(&self) -> bool {
        matches!(&self.notify_channel, Some(ch) if !ch.is_empty())
            && matches!(&self.notify_chat_id, Some(id) if !id.is_empty())
    }
}

/// Persistent, thread-safe cron task store.
pub struct CronStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl CronStore {
    /// Open (or create) the cron store at `<home>/cron_tasks.db` and initialize the schema.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("cron_tasks.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open cron store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "CronStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS cron_tasks (
                 id              TEXT PRIMARY KEY,
                 name            TEXT NOT NULL,
                 agent_id        TEXT NOT NULL DEFAULT 'default',
                 cron            TEXT NOT NULL,
                 task            TEXT NOT NULL,
                 enabled         INTEGER NOT NULL DEFAULT 1,
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL,
                 last_run_at     TEXT,
                 last_status     TEXT,
                 last_error      TEXT,
                 run_count       INTEGER NOT NULL DEFAULT 0,
                 failure_count   INTEGER NOT NULL DEFAULT 0
             );

             CREATE INDEX IF NOT EXISTS idx_cron_tasks_enabled ON cron_tasks(enabled);
             CREATE INDEX IF NOT EXISTS idx_cron_tasks_name    ON cron_tasks(name);",
        )
        .map_err(|e| format!("init cron store schema: {e}"))?;

        // v1.8.22 migration — add notify_* columns for cron-result channel
        // delivery (issue #15). SQLite does not support IF NOT EXISTS on
        // ALTER TABLE ADD COLUMN, so we attempt each one and ignore the
        // "duplicate column name" error. All other errors propagate.
        for col in [
            "notify_channel",
            "notify_chat_id",
            "notify_thread_id",
        ] {
            let sql = format!("ALTER TABLE cron_tasks ADD COLUMN {col} TEXT");
            if let Err(e) = conn.execute(&sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(format!("add column {col}: {msg}"));
                }
            }
        }
        Ok(())
    }

    // ── Reads ─────────────────────────────────────────────────────────

    pub async fn list_all(&self) -> Result<Vec<CronTaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, agent_id, cron, task, enabled,
                        created_at, updated_at, last_run_at, last_status, last_error,
                        run_count, failure_count,
                        notify_channel, notify_chat_id, notify_thread_id
                 FROM cron_tasks
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare list_all: {e}"))?;
        let rows = stmt
            .query_map([], row_to_cron_task)
            .map_err(|e| format!("query list_all: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_all: {e}"))?;
        Ok(rows)
    }

    pub async fn list_enabled(&self) -> Result<Vec<CronTaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, agent_id, cron, task, enabled,
                        created_at, updated_at, last_run_at, last_status, last_error,
                        run_count, failure_count,
                        notify_channel, notify_chat_id, notify_thread_id
                 FROM cron_tasks
                 WHERE enabled = 1
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare list_enabled: {e}"))?;
        let rows = stmt
            .query_map([], row_to_cron_task)
            .map_err(|e| format!("query list_enabled: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_enabled: {e}"))?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<CronTaskRow>, String> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, name, agent_id, cron, task, enabled,
                        created_at, updated_at, last_run_at, last_status, last_error,
                        run_count, failure_count,
                        notify_channel, notify_chat_id, notify_thread_id
                 FROM cron_tasks WHERE id = ?1",
                params![id],
                row_to_cron_task,
            )
            .optional()
            .map_err(|e| format!("get cron task: {e}"))?;
        Ok(row)
    }

    pub async fn get_by_name(&self, name: &str) -> Result<Option<CronTaskRow>, String> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, name, agent_id, cron, task, enabled,
                        created_at, updated_at, last_run_at, last_status, last_error,
                        run_count, failure_count,
                        notify_channel, notify_chat_id, notify_thread_id
                 FROM cron_tasks WHERE name = ?1 LIMIT 1",
                params![name],
                row_to_cron_task,
            )
            .optional()
            .map_err(|e| format!("get_by_name: {e}"))?;
        Ok(row)
    }

    // ── Writes ────────────────────────────────────────────────────────

    /// Insert a new row. Fails if `id` already exists.
    pub async fn insert(&self, row: &CronTaskRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO cron_tasks
                (id, name, agent_id, cron, task, enabled,
                 created_at, updated_at, last_run_at, last_status, last_error,
                 run_count, failure_count,
                 notify_channel, notify_chat_id, notify_thread_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                row.id,
                row.name,
                row.agent_id,
                row.cron,
                row.task,
                if row.enabled { 1 } else { 0 },
                row.created_at,
                row.updated_at,
                row.last_run_at,
                row.last_status,
                row.last_error,
                row.run_count,
                row.failure_count,
                row.notify_channel,
                row.notify_chat_id,
                row.notify_thread_id,
            ],
        )
        .map_err(|e| format!("insert cron task: {e}"))?;
        Ok(())
    }

    /// Update the editable fields of a task (name, agent_id, cron, task, enabled).
    /// `notify_*` fields are preserved — use [`Self::update_notify`] to change them.
    pub async fn update_fields(
        &self,
        id: &str,
        name: &str,
        agent_id: &str,
        cron: &str,
        task: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE cron_tasks
                 SET name = ?2, agent_id = ?3, cron = ?4, task = ?5,
                     enabled = ?6, updated_at = ?7
                 WHERE id = ?1",
                params![id, name, agent_id, cron, task, if enabled { 1 } else { 0 }, now],
            )
            .map_err(|e| format!("update cron task: {e}"))?;
        Ok(changed > 0)
    }

    /// Update the `notify_*` routing fields of a task. Pass `None` to clear a field.
    pub async fn update_notify(
        &self,
        id: &str,
        notify_channel: Option<&str>,
        notify_chat_id: Option<&str>,
        notify_thread_id: Option<&str>,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE cron_tasks
                 SET notify_channel = ?2, notify_chat_id = ?3, notify_thread_id = ?4,
                     updated_at = ?5
                 WHERE id = ?1",
                params![id, notify_channel, notify_chat_id, notify_thread_id, now],
            )
            .map_err(|e| format!("update_notify: {e}"))?;
        Ok(changed > 0)
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE cron_tasks SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, if enabled { 1 } else { 0 }, now],
            )
            .map_err(|e| format!("set_enabled: {e}"))?;
        Ok(changed > 0)
    }

    pub async fn set_enabled_by_name(&self, name: &str, enabled: bool) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE cron_tasks SET enabled = ?2, updated_at = ?3 WHERE name = ?1",
                params![name, if enabled { 1 } else { 0 }, now],
            )
            .map_err(|e| format!("set_enabled_by_name: {e}"))?;
        Ok(changed > 0)
    }

    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute("DELETE FROM cron_tasks WHERE id = ?1", params![id])
            .map_err(|e| format!("delete cron task: {e}"))?;
        Ok(changed > 0)
    }

    pub async fn delete_by_name(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute("DELETE FROM cron_tasks WHERE name = ?1", params![name])
            .map_err(|e| format!("delete_by_name: {e}"))?;
        Ok(changed > 0)
    }

    /// Record the outcome of a firing. Bumps `run_count` and (on failure) `failure_count`.
    pub async fn record_run(
        &self,
        id: &str,
        success: bool,
        error: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let status = if success { "success" } else { "failure" };
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE cron_tasks
             SET last_run_at = ?2,
                 last_status = ?3,
                 last_error = ?4,
                 run_count = run_count + 1,
                 failure_count = failure_count + CASE WHEN ?5 = 1 THEN 0 ELSE 1 END
             WHERE id = ?1",
            params![id, now, status, error, if success { 1 } else { 0 }],
        )
        .map_err(|e| format!("record_run: {e}"))?;
        Ok(())
    }

    // ── Migration ─────────────────────────────────────────────────────

    /// One-shot migration from the legacy `cron_tasks.jsonl` file. Skips rows
    /// whose `id` already exists in the DB. On success, renames the file to
    /// `cron_tasks.jsonl.migrated` so the migration does not re-run.
    ///
    /// Returns the number of rows inserted.
    pub async fn migrate_from_jsonl(&self, home_dir: &Path) -> Result<usize, String> {
        let jsonl_path = home_dir.join("cron_tasks.jsonl");
        if !jsonl_path.exists() {
            return Ok(0);
        }

        let content = tokio::fs::read_to_string(&jsonl_path)
            .await
            .map_err(|e| format!("read cron_tasks.jsonl: {e}"))?;

        let mut migrated = 0usize;
        for (lineno, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    warn!(line = lineno + 1, "skip invalid jsonl line: {e}");
                    continue;
                }
            };

            let id = value
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string();
            let agent_id = value
                .get("agent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            let cron = value
                .get("cron")
                .or_else(|| value.get("schedule"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task = value
                .get("task")
                .or_else(|| value.get("description"))
                .or_else(|| value.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = value
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let created_at = value
                .get("created_at")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| Utc::now().to_rfc3339());

            if cron.is_empty() || task.is_empty() {
                warn!(line = lineno + 1, id = %id, "skip row with empty cron/task");
                continue;
            }

            // Skip if this id already exists in the DB — makes migration idempotent.
            if self.get(&id).await.map_err(|e| e)?.is_some() {
                continue;
            }

            let notify_channel = value
                .get("notify_channel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let notify_chat_id = value
                .get("notify_chat_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let notify_thread_id = value
                .get("notify_thread_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);

            let row = CronTaskRow {
                id,
                name,
                agent_id,
                cron,
                task,
                enabled,
                created_at: created_at.clone(),
                updated_at: created_at,
                last_run_at: None,
                last_status: None,
                last_error: None,
                run_count: 0,
                failure_count: 0,
                notify_channel,
                notify_chat_id,
                notify_thread_id,
            };
            if let Err(e) = self.insert(&row).await {
                warn!(id = %row.id, "failed to migrate row: {e}");
                continue;
            }
            migrated += 1;
        }

        // Rename the file so we don't re-migrate on next startup.
        let archive = home_dir.join("cron_tasks.jsonl.migrated");
        if let Err(e) = tokio::fs::rename(&jsonl_path, &archive).await {
            warn!("failed to archive legacy cron_tasks.jsonl: {e}");
        } else {
            info!(migrated, "legacy cron_tasks.jsonl migrated to SQLite and archived");
        }

        Ok(migrated)
    }
}

fn row_to_cron_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronTaskRow> {
    let enabled_int: i64 = row.get(5)?;
    Ok(CronTaskRow {
        id: row.get(0)?,
        name: row.get(1)?,
        agent_id: row.get(2)?,
        cron: row.get(3)?,
        task: row.get(4)?,
        enabled: enabled_int != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        last_run_at: row.get(8)?,
        last_status: row.get(9)?,
        last_error: row.get(10)?,
        run_count: row.get(11)?,
        failure_count: row.get(12)?,
        notify_channel: row.get(13)?,
        notify_chat_id: row.get(14)?,
        notify_thread_id: row.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn insert_list_update_delete_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CronStore::open(dir.path()).unwrap();

        let row = CronTaskRow::new(
            "t1".into(),
            "Test Task".into(),
            "agnes".into(),
            "0 9 * * *".into(),
            "say hello".into(),
        );
        store.insert(&row).await.unwrap();

        let all = store.list_all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Test Task");

        store
            .update_fields("t1", "Renamed", "agnes", "0 10 * * *", "say hi", true)
            .await
            .unwrap();
        let got = store.get("t1").await.unwrap().unwrap();
        assert_eq!(got.name, "Renamed");
        assert_eq!(got.cron, "0 10 * * *");

        store.set_enabled("t1", false).await.unwrap();
        let enabled = store.list_enabled().await.unwrap();
        assert!(enabled.is_empty());

        store
            .record_run("t1", false, Some("boom"))
            .await
            .unwrap();
        let got = store.get("t1").await.unwrap().unwrap();
        assert_eq!(got.run_count, 1);
        assert_eq!(got.failure_count, 1);
        assert_eq!(got.last_status.as_deref(), Some("failure"));

        assert!(store.delete("t1").await.unwrap());
        assert!(store.list_all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn notify_fields_roundtrip_and_update() {
        let dir = tempdir().unwrap();
        let store = CronStore::open(dir.path()).unwrap();

        // Insert a row with notify fields set
        let mut row = CronTaskRow::new(
            "n1".into(),
            "Notify Task".into(),
            "agnes".into(),
            "0 9 * * *".into(),
            "daily brief".into(),
        );
        row.notify_channel = Some("discord".into());
        row.notify_chat_id = Some("1234567890".into());
        row.notify_thread_id = Some("9876543210".into());
        store.insert(&row).await.unwrap();

        // Round-trip through the DB
        let got = store.get("n1").await.unwrap().unwrap();
        assert!(got.has_notify_target());
        assert_eq!(got.notify_channel.as_deref(), Some("discord"));
        assert_eq!(got.notify_chat_id.as_deref(), Some("1234567890"));
        assert_eq!(got.notify_thread_id.as_deref(), Some("9876543210"));

        // update_fields preserves notify columns
        store
            .update_fields("n1", "Renamed", "agnes", "0 9 * * *", "brief", true)
            .await
            .unwrap();
        let got = store.get("n1").await.unwrap().unwrap();
        assert_eq!(got.name, "Renamed");
        assert_eq!(got.notify_channel.as_deref(), Some("discord"));

        // update_notify can clear a field with None
        store
            .update_notify("n1", Some("telegram"), Some("555"), None)
            .await
            .unwrap();
        let got = store.get("n1").await.unwrap().unwrap();
        assert_eq!(got.notify_channel.as_deref(), Some("telegram"));
        assert_eq!(got.notify_chat_id.as_deref(), Some("555"));
        assert_eq!(got.notify_thread_id, None);

        // has_notify_target is false when channel is empty
        store.update_notify("n1", None, None, None).await.unwrap();
        let got = store.get("n1").await.unwrap().unwrap();
        assert!(!got.has_notify_target());
    }

    #[tokio::test]
    async fn notify_columns_migration_is_idempotent() {
        // Simulate a pre-v1.8.22 DB by creating it, then re-opening — the
        // ALTER TABLE calls in init_schema must not fail on the second pass
        // when the columns already exist.
        let dir = tempdir().unwrap();
        let store1 = CronStore::open(dir.path()).unwrap();
        drop(store1);
        let store2 = CronStore::open(dir.path()).unwrap();
        // If open succeeded the ALTER idempotency contract holds.
        let _ = store2.list_all().await.unwrap();
    }

    #[tokio::test]
    async fn migrate_from_jsonl_archives_file() {
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("cron_tasks.jsonl");
        tokio::fs::write(
            &jsonl,
            r#"{"id":"a","name":"A","agent_id":"agnes","cron":"0 9 * * *","task":"hello","enabled":true}
{"id":"b","name":"B","agent_id":"agnes","cron":"0 10 * * *","task":"world","enabled":false}
"#,
        )
        .await
        .unwrap();

        let store = CronStore::open(dir.path()).unwrap();
        let migrated = store.migrate_from_jsonl(dir.path()).await.unwrap();
        assert_eq!(migrated, 2);
        assert_eq!(store.list_all().await.unwrap().len(), 2);
        assert!(!jsonl.exists());
        assert!(dir.path().join("cron_tasks.jsonl.migrated").exists());

        // Second run is a no-op (file already archived).
        let again = store.migrate_from_jsonl(dir.path()).await.unwrap();
        assert_eq!(again, 0);
    }
}
