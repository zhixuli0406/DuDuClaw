//! SQLite-based session management with 50k token compression.
//!
//! Uses a connection pool (multiple connections) to avoid the single-Mutex
//! bottleneck (BE-H1). Each operation acquires a connection from the pool,
//! runs the blocking SQLite call via `spawn_blocking`, and returns it.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::info;

/// Threshold (in tokens) above which a session should be compressed.
const COMPRESSION_THRESHOLD: u32 = 50_000;

/// Number of connections in the pool.
const POOL_SIZE: usize = 4;

/// A conversation session for a specific agent.
pub struct Session {
    pub id: String,
    pub agent_id: String,
    pub summary: String,
    pub total_tokens: u32,
    pub last_active: String,
    pub model: String,
}

/// A single message within a session.
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    pub tokens: u32,
    pub timestamp: String,
}

/// SQLite-backed session manager with a simple connection pool (BE-H1).
pub struct SessionManager {
    pool: Vec<Mutex<Connection>>,
    #[allow(dead_code)] // Retained for future diagnostics/reconnect
    db_path: PathBuf,
}

impl SessionManager {
    /// Open (or create) a session database at `db_path` and initialize tables.
    pub fn new(db_path: &Path) -> Result<Self> {
        let first = Connection::open(db_path).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Self::init_tables(&first)?;

        let mut pool = vec![Mutex::new(first)];
        for _ in 1..POOL_SIZE {
            let conn = Connection::open(db_path)
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            // Enable WAL mode for better concurrent read performance
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
            pool.push(Mutex::new(conn));
        }

        info!(?db_path, pool_size = POOL_SIZE, "Session manager initialized with connection pool");
        Ok(Self { pool, db_path: db_path.to_path_buf() })
    }

    /// Acquire a connection from the pool.
    ///
    /// Tries non-blocking acquisition across all connections first. If all busy,
    /// sleeps 1ms before retrying to avoid CPU spin (R4-M2). Logs a warning
    /// after 1000 consecutive misses to surface pool exhaustion.
    async fn acquire(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        let mut attempts = 0u32;
        loop {
            for conn in &self.pool {
                if let Ok(guard) = conn.try_lock() {
                    return guard;
                }
            }
            attempts += 1;
            if attempts >= 1000 {
                tracing::error!("Session pool exhausted after 1000 attempts — possible deadlock");
                attempts = 0;
            }
            // Short sleep instead of yield_now to avoid CPU spin-loop (R4-M2)
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                summary TEXT DEFAULT '',
                total_tokens INTEGER DEFAULT 0,
                last_active TEXT NOT NULL,
                model TEXT DEFAULT 'claude-sonnet-4-6',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tokens INTEGER DEFAULT 0,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON session_messages(session_id);",
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        Ok(())
    }

    /// Get an existing session or create a new one.
    ///
    /// Uses INSERT OR IGNORE + SELECT to avoid the TOCTOU race where two
    /// concurrent callers both see "not found" and both attempt INSERT,
    /// causing one to fail with a UNIQUE constraint error.
    pub async fn get_or_create(&self, session_id: &str, agent_id: &str) -> Result<Session> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();

        // Atomic upsert: INSERT OR IGNORE is a no-op if the row already exists.
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, agent_id, summary, total_tokens, last_active, model, created_at)
             VALUES (?1, ?2, '', 0, ?3, 'claude-sonnet-4-6', ?3)",
            params![session_id, agent_id, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        // Authoritative SELECT — reads whichever row won the race.
        let session = conn
            .query_row(
                "SELECT id, agent_id, summary, total_tokens, last_active, model
                 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        agent_id: row.get(1)?,
                        summary: row.get(2)?,
                        total_tokens: row.get(3)?,
                        last_active: row.get(4)?,
                        model: row.get(5)?,
                    })
                },
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        // Verify the session belongs to the expected agent to prevent session
        // hijacking across agents (e.g. two agents racing with the same session_id).
        if session.agent_id != agent_id {
            return Err(DuDuClawError::Gateway(format!(
                "Session '{session_id}' belongs to agent '{}', not '{agent_id}'",
                session.agent_id
            )));
        }

        Ok(session)
    }

    /// Append a message to the session and update token count.
    pub async fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tokens: u32,
    ) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, role, content, tokens, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        conn.execute(
            "UPDATE sessions SET total_tokens = total_tokens + ?1, last_active = ?2 WHERE id = ?3",
            params![tokens, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        Ok(())
    }

    /// Retrieve all messages for a session, ordered by timestamp.
    pub async fn get_messages(&self, session_id: &str) -> Result<Vec<SessionMessage>> {
        let conn = self.acquire().await;

        let mut stmt = conn
            .prepare(
                "SELECT role, content, tokens, timestamp
                 FROM session_messages
                 WHERE session_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(SessionMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    tokens: row.get(2)?,
                    timestamp: row.get(3)?,
                })
            })
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let mut messages = Vec::new();
        for row in rows {
            let msg = row.map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            messages.push(msg);
        }

        Ok(messages)
    }

    /// Check whether the session's token count exceeds the compression threshold.
    pub async fn should_compress(&self, session_id: &str) -> bool {
        let conn = self.acquire().await;

        conn.query_row(
            "SELECT total_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, u32>(0),
        )
        .map(|tokens| tokens > COMPRESSION_THRESHOLD)
        .unwrap_or(false)
    }

    /// Replace all messages with a summary and reset the token count.
    ///
    /// Uses a SQLite transaction to ensure atomicity (BE-M7).
    pub async fn compress(&self, session_id: &str, summary: &str) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();

        let tx = conn.unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
             VALUES (?1, 'system', ?2, 0, ?3)",
            params![session_id, summary, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "UPDATE sessions SET summary = ?1, total_tokens = 0, last_active = ?2 WHERE id = ?3",
            params![summary, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        info!(session_id, "Session compressed");
        Ok(())
    }

    /// Force-compress a session immediately, truncating message history and resetting
    /// the token counter. Returns the number of tokens freed.
    ///
    /// Uses a 10-second timeout to prevent long-running compression from starving the
    /// connection pool (SEC2-M25).
    pub async fn force_compress(&self, session_id: &str) -> Result<u32> {
        let compress_result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                let tokens: u32 = {
                    let conn = self.acquire().await;
                    conn.query_row(
                        "SELECT total_tokens FROM sessions WHERE id = ?1",
                        params![session_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0)
                };

                let summary = "[Session force-compressed — history cleared to free token budget]";
                self.compress(session_id, summary).await?;
                Ok::<u32, DuDuClawError>(tokens)
            },
        )
        .await;

        match compress_result {
            Ok(Ok(tokens)) => Ok(tokens),
            Ok(Err(e)) => {
                tracing::warn!(session_id, error = %e, "Session force_compress failed");
                Err(e)
            }
            Err(_) => {
                tracing::warn!(session_id, "Session force_compress timed out after 10s");
                Err(DuDuClawError::Gateway("compression timed out".to_string()))
            }
        }
    }

    /// Remove sessions that have been inactive for longer than `max_age_hours`.
    /// Returns the number of sessions removed.
    /// Delete a session and all its messages. Used by /reset commands.
    /// Uses a transaction to ensure atomicity (consistent with compress()).
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.acquire().await;
        let tx = conn.unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        info!(session_id, "Session deleted");
        Ok(())
    }

    pub async fn cleanup_inactive(&self, max_age_hours: u64) -> Result<u64> {
        let conn = self.acquire().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(max_age_hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        conn.execute(
            "DELETE FROM session_messages WHERE session_id IN (
                SELECT id FROM sessions WHERE last_active < ?1
            )",
            params![cutoff_str],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let deleted = conn
            .execute(
                "DELETE FROM sessions WHERE last_active < ?1",
                params![cutoff_str],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        if deleted > 0 {
            info!(deleted, max_age_hours, "Cleaned up inactive sessions");
        }

        Ok(deleted as u64)
    }
}
