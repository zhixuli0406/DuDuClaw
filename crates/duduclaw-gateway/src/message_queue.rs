//! SQLite-backed message queue for inter-agent communication.
//!
//! Replaces `bus_queue.jsonl` with a durable store that tracks message
//! lifecycle (pending → acked → processing → done/failed), supports
//! timeout-based retry, and provides receipt verification.
//!
//! Multi-process safe: WAL mode + busy_timeout allows both the gateway
//! dispatcher and MCP subprocess to access concurrently (same pattern
//! as `CronStore`).

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

/// Message status in the queue lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Acked,
    Processing,
    Done,
    Failed,
}

impl MessageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acked => "acked",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "acked" => Self::Acked,
            "processing" => Self::Processing,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// One row in the `message_queue` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMessage {
    pub id: String,
    pub sender: String,
    pub target: String,
    pub payload: String,
    pub status: MessageStatus,
    pub retry_count: i32,
    pub delegation_depth: i32,
    pub origin_agent: Option<String>,
    pub sender_agent: Option<String>,
    pub error: Option<String>,
    pub response: Option<String>,
    pub created_at: String,
    pub acked_at: Option<String>,
    pub completed_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Thread-safe SQLite message queue.
pub struct MessageQueue {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl MessageQueue {
    /// Open (or create) the message queue at `<home>/message_queue.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("message_queue.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open message queue: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "MessageQueue initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS message_queue (
                 id              TEXT PRIMARY KEY,
                 sender          TEXT NOT NULL,
                 target          TEXT NOT NULL,
                 payload         TEXT NOT NULL,
                 status          TEXT NOT NULL DEFAULT 'pending',
                 retry_count     INTEGER NOT NULL DEFAULT 0,
                 delegation_depth INTEGER NOT NULL DEFAULT 0,
                 origin_agent    TEXT,
                 sender_agent    TEXT,
                 error           TEXT,
                 response        TEXT,
                 created_at      TEXT NOT NULL,
                 acked_at        TEXT,
                 completed_at    TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_mq_status ON message_queue(status);
             CREATE INDEX IF NOT EXISTS idx_mq_target ON message_queue(target);
             CREATE INDEX IF NOT EXISTS idx_mq_created ON message_queue(created_at);",
        )
        .map_err(|e| format!("init message_queue schema: {e}"))
    }

    // ── Write operations ──────────────────────────────────────────

    /// Insert a new message into the queue.
    pub async fn enqueue(&self, msg: &QueueMessage) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO message_queue \
             (id, sender, target, payload, status, retry_count, delegation_depth, \
              origin_agent, sender_agent, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                msg.id,
                msg.sender,
                msg.target,
                msg.payload,
                msg.status.as_str(),
                msg.retry_count,
                msg.delegation_depth,
                msg.origin_agent,
                msg.sender_agent,
                msg.created_at,
            ],
        )
        .map_err(|e| format!("enqueue: {e}"))?;
        Ok(())
    }

    /// Mark a message as acknowledged (dispatcher has picked it up).
    pub async fn ack(&self, message_id: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE message_queue SET status = 'acked', acked_at = ?1 WHERE id = ?2",
            params![now, message_id],
        )
        .map_err(|e| format!("ack: {e}"))?;
        Ok(())
    }

    /// Mark a message as successfully completed with a response.
    pub async fn complete(&self, message_id: &str, response: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE message_queue SET status = 'done', response = ?1, completed_at = ?2 \
             WHERE id = ?3",
            params![response, now, message_id],
        )
        .map_err(|e| format!("complete: {e}"))?;
        Ok(())
    }

    /// Mark a message as failed with an error message.
    pub async fn fail(&self, message_id: &str, error: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE message_queue SET status = 'failed', error = ?1, completed_at = ?2 \
             WHERE id = ?3",
            params![error, now, message_id],
        )
        .map_err(|e| format!("fail: {e}"))?;
        Ok(())
    }

    /// Reset a stale message back to pending for retry.
    pub async fn reset_to_pending(&self, message_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE message_queue SET status = 'pending', retry_count = retry_count + 1, \
             acked_at = NULL WHERE id = ?1",
            params![message_id],
        )
        .map_err(|e| format!("reset_to_pending: {e}"))?;
        Ok(())
    }

    // ── Read operations ───────────────────────────────────────────

    /// Fetch up to `limit` pending messages, ordered by creation time.
    pub async fn pending_messages(&self, limit: usize) -> Result<Vec<QueueMessage>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, sender, target, payload, status, retry_count, delegation_depth, \
                 origin_agent, sender_agent, error, response, created_at, acked_at, completed_at \
                 FROM message_queue WHERE status = 'pending' \
                 ORDER BY created_at ASC LIMIT ?1",
            )
            .map_err(|e| format!("prepare pending: {e}"))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(QueueMessage {
                    id: row.get(0)?,
                    sender: row.get(1)?,
                    target: row.get(2)?,
                    payload: row.get(3)?,
                    status: MessageStatus::from_str(
                        &row.get::<_, String>(4).unwrap_or_default(),
                    ),
                    retry_count: row.get(5)?,
                    delegation_depth: row.get(6)?,
                    origin_agent: row.get(7)?,
                    sender_agent: row.get(8)?,
                    error: row.get(9)?,
                    response: row.get(10)?,
                    created_at: row.get(11)?,
                    acked_at: row.get(12)?,
                    completed_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("query pending: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            match row {
                Ok(msg) => result.push(msg),
                Err(e) => warn!("skip malformed queue row: {e}"),
            }
        }
        Ok(result)
    }

    /// Find messages that were acked but not completed within `timeout_secs`.
    pub async fn stale_messages(&self, timeout_secs: i64) -> Result<Vec<QueueMessage>, String> {
        let cutoff = (Utc::now() - chrono::Duration::seconds(timeout_secs)).to_rfc3339();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, sender, target, payload, status, retry_count, delegation_depth, \
                 origin_agent, sender_agent, error, response, created_at, acked_at, completed_at \
                 FROM message_queue WHERE status = 'acked' AND acked_at < ?1",
            )
            .map_err(|e| format!("prepare stale: {e}"))?;

        let rows = stmt
            .query_map(params![cutoff], |row| {
                Ok(QueueMessage {
                    id: row.get(0)?,
                    sender: row.get(1)?,
                    target: row.get(2)?,
                    payload: row.get(3)?,
                    status: MessageStatus::from_str(
                        &row.get::<_, String>(4).unwrap_or_default(),
                    ),
                    retry_count: row.get(5)?,
                    delegation_depth: row.get(6)?,
                    origin_agent: row.get(7)?,
                    sender_agent: row.get(8)?,
                    error: row.get(9)?,
                    response: row.get(10)?,
                    created_at: row.get(11)?,
                    acked_at: row.get(12)?,
                    completed_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("query stale: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            match row {
                Ok(msg) => result.push(msg),
                Err(e) => warn!("skip malformed stale row: {e}"),
            }
        }
        Ok(result)
    }

    /// Look up a single message by ID.
    pub async fn get_by_id(&self, message_id: &str) -> Result<Option<QueueMessage>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, sender, target, payload, status, retry_count, delegation_depth, \
             origin_agent, sender_agent, error, response, created_at, acked_at, completed_at \
             FROM message_queue WHERE id = ?1",
            params![message_id],
            |row| {
                Ok(QueueMessage {
                    id: row.get(0)?,
                    sender: row.get(1)?,
                    target: row.get(2)?,
                    payload: row.get(3)?,
                    status: MessageStatus::from_str(
                        &row.get::<_, String>(4).unwrap_or_default(),
                    ),
                    retry_count: row.get(5)?,
                    delegation_depth: row.get(6)?,
                    origin_agent: row.get(7)?,
                    sender_agent: row.get(8)?,
                    error: row.get(9)?,
                    response: row.get(10)?,
                    created_at: row.get(11)?,
                    acked_at: row.get(12)?,
                    completed_at: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("get_by_id: {e}"))
    }

    /// Count messages by status.
    pub async fn count_by_status(&self) -> Result<Vec<(String, i64)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT status, COUNT(*) FROM message_queue GROUP BY status")
            .map_err(|e| format!("prepare count: {e}"))?;

        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| format!("query count: {e}"))?;

        let mut result = Vec::new();
        for row in rows.flatten() {
            result.push(row);
        }
        Ok(result)
    }
}
