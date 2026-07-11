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

/// Escape LIKE metacharacters so a contact/session prefix containing `%`/`_`
/// matches literally under an `ESCAPE '\'` clause (GDPR prefix matching).
fn gdpr_like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

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
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                summary TEXT DEFAULT '',
                total_tokens INTEGER DEFAULT 0,
                last_active TEXT NOT NULL,
                model TEXT DEFAULT 'auto',
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

        // Phase 4: Add hidden column for hide/restore mechanism (Sculptor, arXiv 2508.04664)
        let _ = conn.execute(
            "ALTER TABLE session_messages ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Instruction Pinning: persist extracted task instructions across turns
        // (arXiv 2505.06120 — combats U-shaped attention degradation in multi-turn)
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN pinned_instructions TEXT DEFAULT ''",
            [],
        );

        // #13 (2026-05-12): async summarization columns. The background
        // task in `session_summarizer_task::tick` writes Haiku-generated
        // bullet summaries of older turns; channel_reply reads them to
        // substitute for the verbatim history slice. All three columns
        // start NULL — pre-13 sessions keep their existing behaviour
        // because the consumer falls back to verbatim assembly when
        // `summary_of_prior` is NULL/empty.
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN summary_of_prior TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN summarized_through_turn INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN last_summarized_at TEXT",
            [],
        );

        // WP5 T5.3: soft-delete. `archived_at` NULL = live; set = archived
        // (hidden from normal listings, still replayable / searchable). Real
        // deletion is `purge_session`.
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN archived_at TEXT",
            [],
        );

        // WP5 T5.1: reply-to → session mapping. When the bot sends a reply we
        // record which session that outbound message belongs to; when the user
        // replies to that message, we resume the same session instead of the
        // per-channel main session.
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_session_map (
                channel TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (channel, message_id)
            );",
        );
        // Ignore errors — columns already exist on subsequent runs

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
             VALUES (?1, ?2, '', 0, ?3, 'auto', ?3)",
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
                 WHERE session_id = ?1 AND hidden = 0
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
    /// WP5 T5.3: "delete" now ARCHIVES (soft delete). Messages are retained and
    /// the session stays replayable/searchable; it just drops out of normal
    /// listings. Use [`Self::purge_session`] for a real, irreversible delete.
    /// Idempotent — archiving an already-archived session is a no-op.
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions SET archived_at = ?2 WHERE id = ?1 AND archived_at IS NULL",
            params![session_id, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        info!(session_id, "Session archived (soft delete)");
        Ok(())
    }

    /// Un-archive a session so it reappears in normal listings.
    pub async fn unarchive_session(&self, session_id: &str) -> Result<()> {
        let conn = self.acquire().await;
        conn.execute(
            "UPDATE sessions SET archived_at = NULL WHERE id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Hard delete — irreversibly removes the session and its messages. This is
    /// the pre-WP5 `delete_session` behaviour, now behind an explicit name.
    pub async fn purge_session(&self, session_id: &str) -> Result<()> {
        let conn = self.acquire().await;
        let tx = conn.unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "DELETE FROM message_session_map WHERE session_id = ?1",
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

        info!(session_id, "Session purged (hard delete)");
        Ok(())
    }

    // ── WP5 T5.1: reply-to → session mapping ───────────────────────

    /// Record that an outbound bot message belongs to `session_id`, so a later
    /// user reply to that message can resume the same session.
    pub async fn record_message_session(
        &self,
        channel: &str,
        message_id: &str,
        session_id: &str,
    ) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO message_session_map (channel, message_id, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![channel, message_id, session_id, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Resolve the session a user is replying to, given the replied-to message
    /// id on a channel. `None` ⇒ not a tracked reply ⇒ caller uses the main
    /// per-channel session.
    pub async fn session_for_reply(
        &self,
        channel: &str,
        message_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.acquire().await;
        let r: std::result::Result<String, _> = conn.query_row(
            "SELECT session_id FROM message_session_map WHERE channel = ?1 AND message_id = ?2",
            params![channel, message_id],
            |row| row.get(0),
        );
        match r {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(DuDuClawError::Gateway(format!("session_for_reply: {e}"))),
        }
    }

    /// GDPR: session ids belonging to a contact. A "contact" here is the
    /// `"<channel>:<chat_id>"` session-id prefix (sessions are keyed by that
    /// string; threads add a `:<tid>` suffix). Matches the exact id and any
    /// `<contact>:*`. LIKE wildcards in the contact are escaped so a literal
    /// `%`/`_` cannot widen the match.
    pub async fn sessions_for_contact(&self, contact: &str) -> Result<Vec<String>> {
        let conn = self.acquire().await;
        let like = format!("{}:%", gdpr_like_escape(contact));
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE id = ?1 OR id LIKE ?2 ESCAPE '\\'")
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt
            .query_map(params![contact, like], |r| r.get::<_, String>(0))
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }

    /// GDPR erase: hard-delete every session (and its messages) belonging to a
    /// contact prefix, in one transaction. Returns `(sessions_deleted,
    /// messages_deleted)`. Messages are deleted first via a subquery that still
    /// sees the `sessions` rows.
    pub async fn erase_sessions_for_contact(&self, contact: &str) -> Result<(u64, u64)> {
        let conn = self.acquire().await;
        let like = format!("{}:%", gdpr_like_escape(contact));
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        let messages = tx
            .execute(
                "DELETE FROM session_messages WHERE session_id IN (
                     SELECT id FROM sessions WHERE id = ?1 OR id LIKE ?2 ESCAPE '\\'
                 )",
                params![contact, like],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let sessions = tx
            .execute(
                "DELETE FROM sessions WHERE id = ?1 OR id LIKE ?2 ESCAPE '\\'",
                params![contact, like],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        if sessions > 0 {
            info!(contact, sessions, messages, "GDPR: erased contact sessions");
        }
        Ok((sessions as u64, messages as u64))
    }

    /// WP5 T5.3: inactive sessions are now ARCHIVED (not hard-deleted), and only
    /// purged after a retention period. Archives sessions whose `last_active` is
    /// older than `max_age_hours`; then hard-purges sessions that have been
    /// archived longer than `purge_after_days` (default 90). Returns the number
    /// newly archived (messages are preserved until purge).
    pub async fn cleanup_inactive(&self, max_age_hours: u64) -> Result<u64> {
        self.cleanup_inactive_with_retention(max_age_hours, 90).await
    }

    /// Retention-aware variant of [`Self::cleanup_inactive`].
    pub async fn cleanup_inactive_with_retention(
        &self,
        max_age_hours: u64,
        purge_after_days: u64,
    ) -> Result<u64> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now();
        let archive_cutoff = (now - chrono::Duration::hours(max_age_hours as i64)).to_rfc3339();
        let purge_cutoff = (now - chrono::Duration::days(purge_after_days as i64)).to_rfc3339();
        let now_str = now.to_rfc3339();

        // Step 1: archive inactive live sessions (soft).
        let archived = conn
            .execute(
                "UPDATE sessions SET archived_at = ?2
                 WHERE last_active < ?1 AND archived_at IS NULL",
                params![archive_cutoff, now_str],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        // Step 2: purge sessions archived beyond the retention window.
        conn.execute(
            "DELETE FROM session_messages WHERE session_id IN (
                SELECT id FROM sessions WHERE archived_at IS NOT NULL AND archived_at < ?1
            )",
            params![purge_cutoff],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let purged = conn
            .execute(
                "DELETE FROM sessions WHERE archived_at IS NOT NULL AND archived_at < ?1",
                params![purge_cutoff],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        if archived > 0 || purged > 0 {
            info!(archived, purged, max_age_hours, purge_after_days, "Session cleanup: archived + purged");
        }
        Ok(archived as u64)
    }

    // ── Instruction Pinning ────────────────────────────────────

    /// Get pinned task instructions for a session.
    ///
    /// Returns empty string if no instructions are pinned.
    pub async fn get_pinned(&self, session_id: &str) -> Result<String> {
        let conn = self.acquire().await;
        let result: std::result::Result<String, _> = conn.query_row(
            "SELECT COALESCE(pinned_instructions, '') FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        );
        match result {
            Ok(s) => Ok(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(String::new()),
            Err(e) => Err(DuDuClawError::Gateway(format!("get_pinned: {e}"))),
        }
    }

    /// Set (or replace) pinned task instructions for a session.
    pub async fn set_pinned(&self, session_id: &str, instructions: &str) -> Result<()> {
        let conn = self.acquire().await;
        conn.execute(
            "UPDATE sessions SET pinned_instructions = ?1 WHERE id = ?2",
            params![instructions, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("set_pinned: {e}")))?;
        Ok(())
    }

    /// Read the cached summary of older turns for this session (#13).
    ///
    /// Returns `(summary_text, summarized_through_turn)`. When no
    /// summary has been generated yet, summary_text is empty and
    /// summarized_through_turn is 0 — caller falls back to verbatim
    /// history.
    pub async fn get_summary(&self, session_id: &str) -> Result<(String, u32)> {
        let conn = self.acquire().await;
        let result: std::result::Result<(Option<String>, i64), _> = conn.query_row(
            "SELECT summary_of_prior, summarized_through_turn \
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((s, t)) => Ok((s.unwrap_or_default(), t.max(0) as u32)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((String::new(), 0)),
            Err(e) => Err(DuDuClawError::Gateway(format!("get_summary: {e}"))),
        }
    }

    /// Write (or overwrite) the summary for a session (#13).
    ///
    /// Caller has the responsibility to set `through_turn` to the
    /// inclusive turn index covered by `summary`. The background
    /// summarizer task is the only writer in production; tests can
    /// invoke this directly.
    pub async fn set_summary(
        &self,
        session_id: &str,
        summary: &str,
        through_turn: u32,
    ) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions
             SET summary_of_prior = ?1,
                 summarized_through_turn = ?2,
                 last_summarized_at = ?3
             WHERE id = ?4",
            params![summary, through_turn as i64, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("set_summary: {e}")))?;
        Ok(())
    }

    /// Scan all sessions and emit summarization candidates (#13).
    ///
    /// One row per session — the background task then runs
    /// `session_summarizer::decide_summarization` to filter / order /
    /// quota-limit them. We intentionally don't filter at the SQL
    /// level: the policy lives in the pure module and is easier to
    /// test there.
    pub async fn list_summary_candidates(
        &self,
    ) -> Result<Vec<crate::session_summarizer::SummaryCandidate>> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.agent_id,
                        (SELECT COUNT(*) FROM session_messages m WHERE m.session_id = s.id) AS turn_count,
                        COALESCE(s.summarized_through_turn, 0) AS summarized_through_turn,
                        s.last_summarized_at
                 FROM sessions s",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("list_summary_candidates prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let agent_id: String = row.get(1)?;
                let turn_count: i64 = row.get(2)?;
                let summarized_through_turn: i64 = row.get(3)?;
                let last: Option<String> = row.get(4)?;
                Ok((id, agent_id, turn_count, summarized_through_turn, last))
            })
            .map_err(|e| DuDuClawError::Gateway(format!("list_summary_candidates query: {e}")))?;

        let now = chrono::Utc::now();
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (id, agent_id, turn_count, summarized_through_turn, last) = row;
            let seconds_since_last_summary = last
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64);
            out.push(crate::session_summarizer::SummaryCandidate {
                session_id: id,
                agent_id,
                turn_count: turn_count.max(0) as u32,
                summarized_through_turn: summarized_through_turn.max(0) as u32,
                seconds_since_last_summary,
            });
        }
        Ok(out)
    }

    /// Fetch the first N turns of a session for summarization, in order
    /// (#13). Returns the rendered transcript ready for the prompt.
    pub async fn read_first_n_turns_text(
        &self,
        session_id: &str,
        n: u32,
    ) -> Result<String> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM session_messages
                 WHERE session_id = ?1 AND hidden = 0
                 ORDER BY id ASC
                 LIMIT ?2",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("read_first_n prepare: {e}")))?;
        let rows = stmt
            .query_map(params![session_id, n as i64], |row| {
                let role: String = row.get(0)?;
                let content: String = row.get(1)?;
                Ok((role, content))
            })
            .map_err(|e| DuDuClawError::Gateway(format!("read_first_n query: {e}")))?;
        let mut out = String::new();
        for (role, content) in rows.flatten() {
            out.push_str(&role);
            out.push_str(": ");
            out.push_str(&content);
            out.push('\n');
        }
        Ok(out)
    }

    /// Hide a message from the active context (Sculptor hide/restore).
    ///
    /// The message remains in the database and can be restored later.
    pub async fn hide_message(&self, session_id: &str, message_id: i64) -> Result<bool> {
        let conn = self.acquire().await;
        let rows = conn
            .execute(
                "UPDATE session_messages SET hidden = 1 WHERE id = ?1 AND session_id = ?2",
                params![message_id, session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Restore a previously hidden message back to active context.
    pub async fn restore_message(&self, session_id: &str, message_id: i64) -> Result<bool> {
        let conn = self.acquire().await;
        let rows = conn
            .execute(
                "UPDATE session_messages SET hidden = 0 WHERE id = ?1 AND session_id = ?2",
                params![message_id, session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Search hidden messages for potential restoration.
    pub async fn search_hidden_messages(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(i64, SessionMessage)>> {
        let conn = self.acquire().await;
        // Escape LIKE wildcards to prevent wildcard injection
        let escaped_query = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped_query}%");
        let safe_limit = limit.min(200); // Cap at 200 to prevent memory abuse

        let mut stmt = conn
            .prepare(
                "SELECT id, role, content, tokens, timestamp
                 FROM session_messages
                 WHERE session_id = ?1 AND hidden = 1 AND content LIKE ?2 ESCAPE '\\'
                 ORDER BY id DESC
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let rows = stmt
            .query_map(params![session_id, pattern, safe_limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    SessionMessage {
                        role: row.get(1)?,
                        content: row.get(2)?,
                        tokens: row.get(3)?,
                        timestamp: row.get(4)?,
                    },
                ))
            })
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn wp5_soft_delete_archives_then_purge_removes() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("s1", "a").await.unwrap();
        mgr.append_message("s1", "user", "hi", 2).await.unwrap();

        // delete = archive: messages survive, replayable.
        mgr.delete_session("s1").await.unwrap();
        assert_eq!(mgr.get_messages("s1").await.unwrap().len(), 1, "archived session keeps messages");

        // unarchive brings it back to live.
        mgr.unarchive_session("s1").await.unwrap();

        // purge = hard delete: messages gone.
        mgr.purge_session("s1").await.unwrap();
        assert_eq!(mgr.get_messages("s1").await.unwrap().len(), 0, "purge removes messages");
    }

    #[tokio::test]
    async fn wp5_reply_to_session_mapping() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("telegram:42:task-A", "a").await.unwrap();
        mgr.record_message_session("telegram", "msg-777", "telegram:42:task-A")
            .await
            .unwrap();

        // Replying to msg-777 resumes the mapped session.
        assert_eq!(
            mgr.session_for_reply("telegram", "msg-777").await.unwrap().as_deref(),
            Some("telegram:42:task-A")
        );
        // Unknown message / wrong channel ⇒ None (caller uses main session).
        assert_eq!(mgr.session_for_reply("telegram", "nope").await.unwrap(), None);
        assert_eq!(mgr.session_for_reply("slack", "msg-777").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_hide_restore_message() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Create session and add messages
        mgr.get_or_create("test-session", "test-agent").await.unwrap();
        mgr.append_message("test-session", "user", "hello", 5).await.unwrap();
        mgr.append_message("test-session", "assistant", "world", 5).await.unwrap();

        // Should have 2 visible messages
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 2);

        // Hide first message (id=1)
        let hidden = mgr.hide_message("test-session", 1).await.unwrap();
        assert!(hidden);

        // Should have 1 visible message
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");

        // Restore
        let restored = mgr.restore_message("test-session", 1).await.unwrap();
        assert!(restored);

        // Should have 2 visible messages again
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn test_gdpr_erase_sessions_for_contact() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // A contact's base chat + a thread of it, plus an unrelated contact.
        mgr.get_or_create("telegram:12345", "a").await.unwrap();
        mgr.append_message("telegram:12345", "user", "hi", 1).await.unwrap();
        mgr.get_or_create("telegram:12345:77", "a").await.unwrap();
        mgr.append_message("telegram:12345:77", "user", "thread", 1).await.unwrap();
        mgr.get_or_create("telegram:99999", "a").await.unwrap();
        mgr.append_message("telegram:99999", "user", "other", 1).await.unwrap();
        // A prefix-collision guard: `telegram:123456` must NOT match `telegram:12345`.
        mgr.get_or_create("telegram:123456", "a").await.unwrap();

        let ids = mgr.sessions_for_contact("telegram:12345").await.unwrap();
        assert_eq!(ids.len(), 2, "exact id + its thread, not the 99999 or 123456");

        let (sessions, messages) =
            mgr.erase_sessions_for_contact("telegram:12345").await.unwrap();
        assert_eq!(sessions, 2);
        assert_eq!(messages, 2);

        // The unrelated contacts survive.
        assert_eq!(mgr.get_messages("telegram:99999").await.unwrap().len(), 1);
        assert!(mgr.sessions_for_contact("telegram:123456").await.unwrap().len() == 1);
        // The erased contact is gone.
        assert!(mgr.sessions_for_contact("telegram:12345").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_search_hidden_messages() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        mgr.get_or_create("test-session", "test-agent").await.unwrap();
        mgr.append_message("test-session", "tool", "search result: found 5 items", 10)
            .await
            .unwrap();
        mgr.append_message("test-session", "assistant", "I found 5 items", 5)
            .await
            .unwrap();

        // Hide the tool result
        mgr.hide_message("test-session", 1).await.unwrap();

        // Search hidden messages
        let found = mgr
            .search_hidden_messages("test-session", "search result", 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].1.content.contains("search result"));
    }

    #[tokio::test]
    async fn test_get_messages_excludes_hidden() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        mgr.get_or_create("s1", "agent").await.unwrap();
        mgr.append_message("s1", "user", "msg1", 5).await.unwrap();
        mgr.append_message("s1", "assistant", "msg2", 5).await.unwrap();
        mgr.append_message("s1", "user", "msg3", 5).await.unwrap();

        // Hide middle message
        mgr.hide_message("s1", 2).await.unwrap();

        let msgs = mgr.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "msg1");
        assert_eq!(msgs[1].content, "msg3");
    }

    #[tokio::test]
    async fn test_pinned_instructions_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("pin-test", "agent").await.unwrap();

        // Initially empty
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.is_empty());

        // Set pinned
        mgr.set_pinned("pin-test", "- Goal: build two teams\n- Constraint: use Rust").await.unwrap();
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.contains("build two teams"));
        assert!(pinned.contains("use Rust"));

        // Update pinned (accumulate)
        let updated = format!("{pinned}\n- 用戶確認：PM 每天 8:00 回報");
        mgr.set_pinned("pin-test", &updated).await.unwrap();
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.contains("PM 每天 8:00 回報"));
    }

    #[tokio::test]
    async fn test_pinned_survives_compression() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("compress-pin", "agent").await.unwrap();

        // Set pinned + add messages
        mgr.set_pinned("compress-pin", "- Goal: deploy v2.0").await.unwrap();
        mgr.append_message("compress-pin", "user", "hello", 5).await.unwrap();
        mgr.append_message("compress-pin", "assistant", "world", 5).await.unwrap();

        // Compress — should delete messages but keep pinned
        mgr.compress("compress-pin", "Summary: user said hello").await.unwrap();

        // Messages gone (replaced with summary)
        let msgs = mgr.get_messages("compress-pin").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");

        // Pinned survived!
        let pinned = mgr.get_pinned("compress-pin").await.unwrap();
        assert_eq!(pinned, "- Goal: deploy v2.0");
    }

    #[tokio::test]
    async fn test_pinned_nonexistent_session() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Non-existent session returns empty (not error)
        let pinned = mgr.get_pinned("does-not-exist").await.unwrap();
        assert!(pinned.is_empty());
    }
}
