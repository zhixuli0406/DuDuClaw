//! SQLite-backed persistent store for tasks and activity events.
//!
//! Provides CRUD operations for the Task Board (Kanban) and an append-only
//! activity feed. WAL mode + 5s busy_timeout for multi-process safety.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

// ── Task row ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,       // todo | in_progress | done | blocked
    pub priority: String,     // low | medium | high | urgent
    pub assigned_to: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub blocked_reason: Option<String>,
    pub parent_task_id: Option<String>,
    pub tags: String, // comma-separated
    pub message_id: Option<String>,
}

impl TaskRow {
    pub fn new(
        id: String,
        title: String,
        description: String,
        priority: String,
        assigned_to: String,
        created_by: String,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id,
            title,
            description,
            status: "todo".into(),
            priority,
            assigned_to,
            created_by,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
            blocked_reason: None,
            parent_task_id: None,
            tags: String::new(),
            message_id: None,
        }
    }
}

// ── Activity row ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRow {
    pub id: String,
    pub event_type: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub summary: String,
    pub timestamp: String,
    pub metadata: Option<String>, // JSON string
}

// ── Store ───────────────────────────────────────────────────

pub struct TaskStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl TaskStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("tasks.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open task store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "TaskStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS tasks (
                 id              TEXT PRIMARY KEY,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 status          TEXT NOT NULL DEFAULT 'todo',
                 priority        TEXT NOT NULL DEFAULT 'medium',
                 assigned_to     TEXT NOT NULL,
                 created_by      TEXT NOT NULL DEFAULT 'system',
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL,
                 completed_at    TEXT,
                 blocked_reason  TEXT,
                 parent_task_id  TEXT,
                 tags            TEXT NOT NULL DEFAULT '',
                 message_id      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_to);
             CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority);

             CREATE TABLE IF NOT EXISTS activity (
                 id          TEXT PRIMARY KEY,
                 event_type  TEXT NOT NULL,
                 agent_id    TEXT NOT NULL,
                 task_id     TEXT,
                 summary     TEXT NOT NULL,
                 timestamp   TEXT NOT NULL,
                 metadata    TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_activity_agent ON activity(agent_id);
             CREATE INDEX IF NOT EXISTS idx_activity_type  ON activity(event_type);
             CREATE INDEX IF NOT EXISTS idx_activity_ts    ON activity(timestamp DESC);",
        )
        .map_err(|e| format!("init task store schema: {e}"))?;
        Ok(())
    }

    // ── Task CRUD ───────────────────────────────────────────

    pub async fn list_tasks(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
    ) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut sql = "SELECT id, title, description, status, priority, assigned_to, created_by,
                               created_at, updated_at, completed_at, blocked_reason,
                               parent_task_id, tags, message_id
                        FROM tasks WHERE 1=1".to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            sql.push_str(&format!(" AND assigned_to = ?{}", binds.len()));
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            sql.push_str(&format!(" AND priority = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY updated_at DESC");

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare list: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list: {e}"))?;
        Ok(rows)
    }

    pub async fn get_task(&self, id: &str) -> Result<Option<TaskRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, title, description, status, priority, assigned_to, created_by,
                    created_at, updated_at, completed_at, blocked_reason,
                    parent_task_id, tags, message_id
             FROM tasks WHERE id = ?1",
            params![id],
            row_to_task,
        )
        .optional()
        .map_err(|e| format!("get task: {e}"))
    }

    pub async fn insert_task(&self, row: &TaskRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO tasks
                (id, title, description, status, priority, assigned_to, created_by,
                 created_at, updated_at, completed_at, blocked_reason,
                 parent_task_id, tags, message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                row.id,
                row.title,
                row.description,
                row.status,
                row.priority,
                row.assigned_to,
                row.created_by,
                row.created_at,
                row.updated_at,
                row.completed_at,
                row.blocked_reason,
                row.parent_task_id,
                row.tags,
                row.message_id,
            ],
        )
        .map_err(|e| format!("insert task: {e}"))?;
        Ok(())
    }

    pub async fn update_task(&self, id: &str, fields: &serde_json::Value) -> Result<Option<TaskRow>, String> {
        // Scoped block ensures all non-Send refs are dropped before the next await.
        {
            let conn = self.conn.lock().await;
            let now = Utc::now().to_rfc3339();
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![now];

            macro_rules! opt_field {
                ($key:expr, $col:expr) => {
                    if let Some(v) = fields.get($key).and_then(|v| v.as_str()) {
                        binds.push(v.to_string());
                        sets.push(format!("{} = ?{}", $col, binds.len()));
                    }
                };
            }
            opt_field!("title", "title");
            opt_field!("description", "description");
            opt_field!("status", "status");
            opt_field!("priority", "priority");
            opt_field!("assigned_to", "assigned_to");
            opt_field!("blocked_reason", "blocked_reason");
            if let Some(v) = fields.get("tags").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("tags = ?{}", binds.len()));
            }

            // Auto-set completed_at when status changes to done
            if fields.get("status").and_then(|v| v.as_str()) == Some("done") {
                binds.push(Utc::now().to_rfc3339());
                sets.push(format!("completed_at = ?{}", binds.len()));
            }

            binds.push(id.to_string());
            let sql = format!(
                "UPDATE tasks SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            conn.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update task: {e}"))?;
        }

        self.get_task(id).await
    }

    pub async fn remove_task(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let count = conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .map_err(|e| format!("remove task: {e}"))?;
        Ok(count > 0)
    }

    // ── Activity feed ───────────────────────────────────────

    pub async fn append_activity(&self, row: &ActivityRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO activity (id, event_type, agent_id, task_id, summary, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id,
                row.event_type,
                row.agent_id,
                row.task_id,
                row.summary,
                row.timestamp,
                row.metadata,
            ],
        )
        .map_err(|e| format!("append activity: {e}"))?;
        Ok(())
    }

    pub async fn list_activity(
        &self,
        agent_id: Option<&str>,
        event_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        let conn = self.conn.lock().await;

        // Count total
        let mut count_sql = "SELECT COUNT(*) FROM activity WHERE 1=1".to_string();
        let mut query_sql = "SELECT id, event_type, agent_id, task_id, summary, timestamp, metadata
                             FROM activity WHERE 1=1".to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            let clause = format!(" AND agent_id = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(t) = event_type {
            binds.push(t.to_string());
            let clause = format!(" AND event_type = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        query_sql.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT {} OFFSET {}",
            limit, offset
        ));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();

        let total: i64 = conn
            .query_row(&count_sql, params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count activity: {e}"))?;

        let mut stmt = conn.prepare(&query_sql).map_err(|e| format!("prepare activity: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), |r| {
                Ok(ActivityRow {
                    id: r.get(0)?,
                    event_type: r.get(1)?,
                    agent_id: r.get(2)?,
                    task_id: r.get(3)?,
                    summary: r.get(4)?,
                    timestamp: r.get(5)?,
                    metadata: r.get(6)?,
                })
            })
            .map_err(|e| format!("query activity: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect activity: {e}"))?;

        Ok((rows, total))
    }
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        assigned_to: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        completed_at: row.get(9)?,
        blocked_reason: row.get(10)?,
        parent_task_id: row.get(11)?,
        tags: row.get(12)?,
        message_id: row.get(13)?,
    })
}
