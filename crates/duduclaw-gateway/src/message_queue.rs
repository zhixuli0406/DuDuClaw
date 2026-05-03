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
    /// Originating channel context for delegation callback forwarding.
    ///
    /// Format: `<channel_type>:<channel_id>[:<thread_id>]` (same grammar as
    /// the `DUDUCLAW_REPLY_CHANNEL` env var). When the MCP `send_to_agent`
    /// tool inserts a row, it captures the caller's reply-channel context
    /// here so the dispatcher can scope `REPLY_CHANNEL` around the target
    /// agent's Claude CLI subprocess — otherwise nested sub-agent
    /// delegations (the ones spawned by the dispatcher, not by
    /// `channel_reply`) inherit no channel context and their callback
    /// is never registered, causing sub-agent replies to be silently
    /// dropped (v1.8.14 — v1.8.15 issue).
    pub reply_channel: Option<String>,
    /// Wiki RL trust feedback turn id (v1.10). Originating per-turn ULID
    /// so sub-agent RAG citations attribute back to the right prediction
    /// error. `None` for messages enqueued by callers that don't have
    /// trust-tracking context (cron, webhook, programmatic).
    pub turn_id: Option<String>,
    /// Wiki RL trust feedback session id (v1.10). Channel session id used
    /// as the per-conversation cap budget key.
    pub session_id: Option<String>,
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
             CREATE INDEX IF NOT EXISTS idx_mq_created ON message_queue(created_at);

             -- Delegation callbacks: maps bus message_id → originating channel context
             -- so the dispatcher can forward sub-agent responses back to the user.
             CREATE TABLE IF NOT EXISTS delegation_callbacks (
                 message_id      TEXT PRIMARY KEY,
                 agent_id        TEXT NOT NULL,
                 channel_type    TEXT NOT NULL,
                 channel_id      TEXT NOT NULL,
                 thread_id       TEXT,
                 retry_count     INTEGER NOT NULL DEFAULT 0,
                 created_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_dc_agent ON delegation_callbacks(agent_id);",
        )
        .map_err(|e| format!("init message_queue schema: {e}"))?;

        // v1.8.16 migration: propagate channel context through the delegation
        // chain so nested sub-agents spawned by the dispatcher inherit the
        // originating `DUDUCLAW_REPLY_CHANNEL`. Existing rows get NULL and
        // stay on the legacy (no-forward) path, which is what we want for
        // cleanup — new rows benefit from the fix.
        Self::ensure_column(conn, "message_queue", "reply_channel", "TEXT")?;

        // v1.10 migration: wiki RL trust feedback context. New rows
        // populated by `send_to_agent` MCP tool from `DUDUCLAW_TURN_ID` /
        // `DUDUCLAW_SESSION_ID` env vars; legacy rows stay NULL and skip
        // citation tracking (correct fallback — no signal to apply).
        Self::ensure_column(conn, "message_queue", "turn_id", "TEXT")?;
        Self::ensure_column(conn, "message_queue", "session_id", "TEXT")?;
        Ok(())
    }

    /// Idempotent column addition for SQLite — checks PRAGMA table_info
    /// before running ALTER TABLE ADD COLUMN, so this is safe to call on
    /// every startup. SQLite doesn't support `ADD COLUMN IF NOT EXISTS`.
    fn ensure_column(
        conn: &Connection,
        table: &str,
        column: &str,
        coltype: &str,
    ) -> Result<(), String> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut stmt = conn
            .prepare(&pragma)
            .map_err(|e| format!("prepare pragma for {table}: {e}"))?;
        let existing: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("query pragma for {table}: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        if existing.iter().any(|c| c == column) {
            return Ok(());
        }
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {coltype}"))
            .map_err(|e| format!("add {column} to {table}: {e}"))?;
        info!(table, column, "message_queue migration: added column");
        Ok(())
    }

    // ── Write operations ──────────────────────────────────────────

    /// Insert a new message into the queue.
    pub async fn enqueue(&self, msg: &QueueMessage) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO message_queue \
             (id, sender, target, payload, status, retry_count, delegation_depth, \
              origin_agent, sender_agent, created_at, reply_channel, turn_id, session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                msg.reply_channel,
                msg.turn_id,
                msg.session_id,
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
                 origin_agent, sender_agent, error, response, created_at, acked_at, completed_at, \
                 reply_channel, turn_id, session_id \
                 FROM message_queue WHERE status = 'pending' \
                 ORDER BY created_at ASC LIMIT ?1",
            )
            .map_err(|e| format!("prepare pending: {e}"))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| Self::row_to_message(row))
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
                 origin_agent, sender_agent, error, response, created_at, acked_at, completed_at, \
                 reply_channel, turn_id, session_id \
                 FROM message_queue WHERE status = 'acked' AND acked_at < ?1",
            )
            .map_err(|e| format!("prepare stale: {e}"))?;

        let rows = stmt
            .query_map(params![cutoff], |row| Self::row_to_message(row))
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
             origin_agent, sender_agent, error, response, created_at, acked_at, completed_at, \
             reply_channel, turn_id, session_id \
             FROM message_queue WHERE id = ?1",
            params![message_id],
            |row| Self::row_to_message(row),
        )
        .optional()
        .map_err(|e| format!("get_by_id: {e}"))
    }

    /// Single place to decode a `message_queue` row into a `QueueMessage`.
    /// Column order must match the SELECT statements above.
    fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueMessage> {
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
            reply_channel: row.get(14)?,
            turn_id: row.get(15)?,
            session_id: row.get(16)?,
        })
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

    // ── Delegation callbacks ─────────────────────────────────────

    /// Record a delegation callback so the dispatcher can forward
    /// the sub-agent's response back to the originating channel.
    pub async fn register_callback(&self, cb: &DelegationCallback) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO delegation_callbacks \
             (message_id, agent_id, channel_type, channel_id, thread_id, retry_count, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                cb.message_id,
                cb.agent_id,
                cb.channel_type,
                cb.channel_id,
                cb.thread_id,
                cb.retry_count,
                cb.created_at,
            ],
        )
        .map_err(|e| format!("register_callback: {e}"))?;
        Ok(())
    }

    /// Atomically consume a delegation callback by message_id (DELETE RETURNING).
    /// Returns `None` if no callback was registered for this message.
    pub async fn take_callback(&self, message_id: &str) -> Result<Option<DelegationCallback>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "DELETE FROM delegation_callbacks WHERE message_id = ?1 \
             RETURNING message_id, agent_id, channel_type, channel_id, thread_id, retry_count, created_at",
            params![message_id],
            |row| {
                Ok(DelegationCallback {
                    message_id: row.get(0)?,
                    agent_id: row.get(1)?,
                    channel_type: row.get(2)?,
                    channel_id: row.get(3)?,
                    thread_id: row.get(4)?,
                    retry_count: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("take_callback: {e}"))
    }

    /// Clean up callbacks older than 24 hours (orphans from crashed sessions).
    pub async fn cleanup_stale_callbacks(&self) -> Result<usize, String> {
        let cutoff = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM delegation_callbacks WHERE created_at < ?1",
            params![cutoff],
        )
        .map(|n| n)
        .map_err(|e| format!("cleanup_stale_callbacks: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Delegation callback row type
// ---------------------------------------------------------------------------

/// A callback record linking a bus message_id to the channel context
/// that should receive the sub-agent's response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationCallback {
    pub message_id: String,
    pub agent_id: String,
    pub channel_type: String,
    pub channel_id: String,
    pub thread_id: Option<String>,
    pub retry_count: i32,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_msg(id: &str, reply_channel: Option<&str>) -> QueueMessage {
        QueueMessage {
            id: id.into(),
            sender: "agnes".into(),
            target: "duduclaw-tl".into(),
            payload: "hello".into(),
            status: MessageStatus::Pending,
            retry_count: 0,
            delegation_depth: 1,
            origin_agent: Some("agnes".into()),
            sender_agent: Some("agnes".into()),
            error: None,
            response: None,
            created_at: "2026-04-21T00:00:00Z".into(),
            acked_at: None,
            completed_at: None,
            reply_channel: reply_channel.map(str::to_string),
            turn_id: None,
            session_id: None,
        }
    }

    #[tokio::test]
    async fn reply_channel_round_trips_through_pending() {
        let tmp = TempDir::new().unwrap();
        let queue = MessageQueue::open(tmp.path()).unwrap();
        queue
            .enqueue(&sample_msg("m1", Some("discord:1495790276264853625")))
            .await
            .unwrap();
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].reply_channel.as_deref(),
            Some("discord:1495790276264853625"),
        );
    }

    #[tokio::test]
    async fn reply_channel_null_is_preserved() {
        let tmp = TempDir::new().unwrap();
        let queue = MessageQueue::open(tmp.path()).unwrap();
        queue.enqueue(&sample_msg("m2", None)).await.unwrap();
        let fetched = queue.get_by_id("m2").await.unwrap().unwrap();
        assert_eq!(fetched.reply_channel, None);
    }

    #[tokio::test]
    async fn ensure_column_is_idempotent_on_legacy_db() {
        // Simulate a pre-v1.8.16 database where the table exists without
        // the `reply_channel` column. `init_schema` on startup must add
        // the column without data loss, and subsequent startups must
        // be no-ops.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("message_queue.db");

        {
            // Manually create the legacy schema (no reply_channel column).
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE message_queue (
                     id TEXT PRIMARY KEY, sender TEXT NOT NULL, target TEXT NOT NULL,
                     payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
                     retry_count INTEGER NOT NULL DEFAULT 0,
                     delegation_depth INTEGER NOT NULL DEFAULT 0,
                     origin_agent TEXT, sender_agent TEXT,
                     error TEXT, response TEXT,
                     created_at TEXT NOT NULL, acked_at TEXT, completed_at TEXT);
                 INSERT INTO message_queue (id, sender, target, payload, created_at)
                     VALUES ('legacy-1','agnes','tl','hi','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        // First open: column is added.
        let _ = MessageQueue::open(tmp.path()).unwrap();
        // Second open: ensure_column is a no-op, no error.
        let queue = MessageQueue::open(tmp.path()).unwrap();

        // Legacy row is intact and reads back with NULL reply_channel.
        let legacy = queue.get_by_id("legacy-1").await.unwrap().unwrap();
        assert_eq!(legacy.reply_channel, None);
        assert_eq!(legacy.sender, "agnes");
    }
}
