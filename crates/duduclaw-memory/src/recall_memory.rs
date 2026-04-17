//! Recall Memory (L2) — Cross-session conversation log.
//!
//! Records ALL messages (inbound and outbound) regardless of source:
//! interactive replies, cron task results, reminders, proactive messages,
//! and sub-agent delegations. This unified log enables context retrieval
//! across session boundaries.
//!
//! Key differences from SessionManager:
//! - **Cross-session**: queries by (agent, channel, chat_id), not session_id
//! - **All sources**: captures cron/reminder/proactive that bypass SessionManager
//! - **FTS5 search**: full-text search for keyword-based retrieval
//! - **Source tracking**: records who sent what and why (source + source_agent)

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use duduclaw_core::error::{DuDuClawError, Result};

// ── Token estimation (same as core_memory.rs) ───────────────────

fn estimate_tokens(text: &str) -> u32 {
    let mut cjk: u32 = 0;
    let mut total: u32 = 0;
    for c in text.chars() {
        total += 1;
        if matches!(c,
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' |
            '\u{3000}'..='\u{303F}' | '\u{3040}'..='\u{309F}' |
            '\u{30A0}'..='\u{30FF}' | '\u{FF00}'..='\u{FFEF}'
        ) {
            cjk += 1;
        }
    }
    let non_cjk = total - cjk;
    let cjk_tokens = ((cjk as f64) / 1.5).ceil() as u32;
    let ascii_tokens = non_cjk / 4;
    (cjk_tokens + ascii_tokens).max(1)
}

// ── Types ───────────────────────────────────────────────────────

/// A single recall log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallEntry {
    pub id: Option<i64>,
    pub agent_id: String,
    pub channel: String,
    pub chat_id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    /// Source of this message: "interactive", "cron", "reminder", "proactive", "sub_agent", "agent_mcp"
    pub source: String,
    /// Which agent produced this message (for multi-agent tracing).
    pub source_agent: String,
    pub token_count: u32,
    pub timestamp: String,
    /// Arbitrary JSON metadata (e.g., reply_to_message_id, platform_message_id).
    pub metadata: serde_json::Value,
}

impl Default for RecallEntry {
    fn default() -> Self {
        Self {
            id: None,
            agent_id: String::new(),
            channel: String::new(),
            chat_id: String::new(),
            session_id: String::new(),
            role: String::new(),
            content: String::new(),
            source: "interactive".to_string(),
            source_agent: String::new(),
            token_count: 0,
            timestamp: Utc::now().to_rfc3339(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

// ── Manager ─────────────────────────────────────────────────────

/// Manages the recall log in SQLite.
pub struct RecallMemoryManager {
    conn: Mutex<Connection>,
}

impl RecallMemoryManager {
    /// Open or create the recall log database at `db_path`.
    pub fn new(db_path: &std::path::Path) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory database (for testing).
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS recall_log (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id     TEXT NOT NULL,
                channel      TEXT NOT NULL,
                chat_id      TEXT NOT NULL,
                session_id   TEXT NOT NULL DEFAULT '',
                role         TEXT NOT NULL,
                content      TEXT NOT NULL,
                source       TEXT NOT NULL DEFAULT 'interactive',
                source_agent TEXT NOT NULL DEFAULT '',
                token_count  INTEGER NOT NULL DEFAULT 0,
                timestamp    TEXT NOT NULL,
                metadata     TEXT DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_recall_agent_chat
                ON recall_log(agent_id, channel, chat_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_recall_session
                ON recall_log(session_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_recall_timestamp
                ON recall_log(timestamp DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS recall_log_fts USING fts5(
                content,
                content='recall_log',
                content_rowid='id',
                tokenize='unicode61'
            );

            -- Triggers to keep FTS5 in sync
            CREATE TRIGGER IF NOT EXISTS recall_log_ai AFTER INSERT ON recall_log BEGIN
                INSERT INTO recall_log_fts(rowid, content) VALUES (new.id, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS recall_log_ad AFTER DELETE ON recall_log BEGIN
                INSERT INTO recall_log_fts(recall_log_fts, rowid, content) VALUES('delete', old.id, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS recall_log_au AFTER UPDATE ON recall_log BEGIN
                INSERT INTO recall_log_fts(recall_log_fts, rowid, content) VALUES('delete', old.id, old.content);
                INSERT INTO recall_log_fts(rowid, content) VALUES (new.id, new.content);
            END;
            ",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    /// Record a message (any direction, any source).
    pub async fn record(&self, entry: RecallEntry) -> Result<i64> {
        let conn = self.conn.lock().await;
        let token_count = if entry.token_count > 0 {
            entry.token_count
        } else {
            estimate_tokens(&entry.content)
        };
        let metadata_str = entry.metadata.to_string();

        conn.execute(
            "INSERT INTO recall_log (agent_id, channel, chat_id, session_id, role, content, source, source_agent, token_count, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.agent_id,
                entry.channel,
                entry.chat_id,
                entry.session_id,
                entry.role,
                entry.content,
                entry.source,
                entry.source_agent,
                token_count,
                entry.timestamp,
                metadata_str,
            ],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Get recent messages for a conversation (cross-session).
    pub async fn get_recent(
        &self,
        agent_id: &str,
        channel: &str,
        chat_id: &str,
        limit: u32,
    ) -> Result<Vec<RecallEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, channel, chat_id, session_id, role, content, source, source_agent, token_count, timestamp, metadata
                 FROM recall_log
                 WHERE agent_id = ?1 AND channel = ?2 AND chat_id = ?3
                 ORDER BY timestamp DESC
                 LIMIT ?4",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![agent_id, channel, chat_id, limit], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries: Vec<RecallEntry> = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        // Reverse to chronological order
        entries.reverse();
        Ok(entries)
    }

    /// Search past conversations by keyword using FTS5.
    pub async fn search(
        &self,
        agent_id: &str,
        channel: &str,
        chat_id: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<RecallEntry>> {
        let conn = self.conn.lock().await;

        // Sanitize FTS5 query: escape special characters
        let sanitized = sanitize_fts_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = conn
            .prepare(
                "SELECT r.id, r.agent_id, r.channel, r.chat_id, r.session_id, r.role, r.content, r.source, r.source_agent, r.token_count, r.timestamp, r.metadata
                 FROM recall_log r
                 JOIN recall_log_fts f ON r.id = f.rowid
                 WHERE recall_log_fts MATCH ?1
                   AND r.agent_id = ?2
                   AND (r.channel = ?3 OR ?3 = '')
                   AND (r.chat_id = ?4 OR ?4 = '')
                 ORDER BY rank
                 LIMIT ?5",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(
                params![sanitized, agent_id, channel, chat_id, limit],
                Self::row_to_entry,
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(entries)
    }

    /// Get messages around a specific timestamp (context window).
    pub async fn get_around(
        &self,
        agent_id: &str,
        channel: &str,
        chat_id: &str,
        timestamp: &str,
        window: u32,
    ) -> Result<Vec<RecallEntry>> {
        let conn = self.conn.lock().await;
        let half = window / 2;

        // Get messages before and after the timestamp
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, channel, chat_id, session_id, role, content, source, source_agent, token_count, timestamp, metadata
                 FROM (
                     SELECT * FROM recall_log
                     WHERE agent_id = ?1 AND channel = ?2 AND chat_id = ?3 AND timestamp <= ?4
                     ORDER BY timestamp DESC LIMIT ?5
                 )
                 UNION ALL
                 SELECT id, agent_id, channel, chat_id, session_id, role, content, source, source_agent, token_count, timestamp, metadata
                 FROM (
                     SELECT * FROM recall_log
                     WHERE agent_id = ?1 AND channel = ?2 AND chat_id = ?3 AND timestamp > ?4
                     ORDER BY timestamp ASC LIMIT ?5
                 )
                 ORDER BY timestamp ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(
                params![agent_id, channel, chat_id, timestamp, half],
                Self::row_to_entry,
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(entries)
    }

    /// Purge entries older than `days` (for maintenance).
    pub async fn purge_older_than(&self, days: u32) -> Result<u64> {
        let conn = self.conn.lock().await;
        let cutoff = (Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        let count = conn
            .execute(
                "DELETE FROM recall_log WHERE timestamp < ?1",
                params![cutoff],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(count as u64)
    }

    /// Render recent recall entries as a prompt section.
    pub fn render_prompt_section(entries: &[RecallEntry], budget_tokens: u32) -> String {
        if entries.is_empty() {
            return String::new();
        }

        let header = "## Recent Conversations (cross-session recall)";
        let mut parts = Vec::with_capacity(entries.len() + 1);
        parts.push(header.to_string());
        let mut used_tokens = estimate_tokens(header);

        for entry in entries {
            let source_tag = if entry.source != "interactive" {
                format!(" ({})", entry.source)
            } else {
                String::new()
            };
            let line = format!(
                "[{}] {}{}: {}",
                &entry.timestamp[..19.min(entry.timestamp.len())],
                entry.role,
                source_tag,
                entry.content
            );
            let line_tokens = estimate_tokens(&line);
            if used_tokens + line_tokens > budget_tokens {
                break;
            }
            used_tokens += line_tokens;
            parts.push(line);
        }

        if parts.len() <= 1 {
            return String::new();
        }

        parts.join("\n")
    }

    fn row_to_entry(row: &rusqlite::Row<'_>) -> std::result::Result<RecallEntry, rusqlite::Error> {
        let metadata_str: String = row.get(11)?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_str).unwrap_or_default();

        Ok(RecallEntry {
            id: Some(row.get(0)?),
            agent_id: row.get(1)?,
            channel: row.get(2)?,
            chat_id: row.get(3)?,
            session_id: row.get(4)?,
            role: row.get(5)?,
            content: row.get(6)?,
            source: row.get(7)?,
            source_agent: row.get(8)?,
            token_count: row.get(9)?,
            timestamp: row.get(10)?,
            metadata,
        })
    }
}

/// Sanitize a user query for FTS5 (remove special chars that break queries).
fn sanitize_fts_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len());
    for c in query.chars() {
        match c {
            '"' | '\'' | '*' | '(' | ')' | ':' | '^' | '{' | '}' | '[' | ']' | '~' | '!' => {}
            _ => result.push(c),
        }
    }
    result.trim().to_string()
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(role: &str, content: &str, source: &str) -> RecallEntry {
        RecallEntry {
            agent_id: "agent-1".into(),
            channel: "telegram".into(),
            chat_id: "12345".into(),
            role: role.into(),
            content: content.into(),
            source: source.into(),
            source_agent: "agent-1".into(),
            timestamp: Utc::now().to_rfc3339(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_record_and_get_recent() {
        let mgr = RecallMemoryManager::in_memory().unwrap();

        mgr.record(make_entry("user", "Hello", "interactive"))
            .await
            .unwrap();
        mgr.record(make_entry("assistant", "Hi there!", "interactive"))
            .await
            .unwrap();
        mgr.record(make_entry("assistant", "Trigger report", "cron"))
            .await
            .unwrap();

        let recent = mgr
            .get_recent("agent-1", "telegram", "12345", 10)
            .await
            .unwrap();
        assert_eq!(recent.len(), 3);
        // Should be in chronological order
        assert_eq!(recent[0].content, "Hello");
        assert_eq!(recent[2].content, "Trigger report");
        assert_eq!(recent[2].source, "cron");
    }

    #[tokio::test]
    async fn test_search_fts() {
        let mgr = RecallMemoryManager::in_memory().unwrap();

        mgr.record(make_entry("assistant", "I found 3 disabled triggers", "cron"))
            .await
            .unwrap();
        mgr.record(make_entry("user", "enable all", "interactive"))
            .await
            .unwrap();
        mgr.record(make_entry("assistant", "Unrelated weather info", "interactive"))
            .await
            .unwrap();

        let results = mgr
            .search("agent-1", "telegram", "12345", "triggers", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("triggers"));
    }

    #[tokio::test]
    async fn test_cross_session_isolation() {
        let mgr = RecallMemoryManager::in_memory().unwrap();

        // Two different chats
        let mut e1 = make_entry("user", "Chat 1 msg", "interactive");
        e1.chat_id = "111".into();
        mgr.record(e1).await.unwrap();

        let mut e2 = make_entry("user", "Chat 2 msg", "interactive");
        e2.chat_id = "222".into();
        mgr.record(e2).await.unwrap();

        let chat1 = mgr.get_recent("agent-1", "telegram", "111", 10).await.unwrap();
        assert_eq!(chat1.len(), 1);
        assert_eq!(chat1[0].content, "Chat 1 msg");

        let chat2 = mgr.get_recent("agent-1", "telegram", "222", 10).await.unwrap();
        assert_eq!(chat2.len(), 1);
        assert_eq!(chat2[0].content, "Chat 2 msg");
    }

    #[tokio::test]
    async fn test_render_prompt_section() {
        let entries = vec![
            RecallEntry {
                role: "assistant".into(),
                content: "Found 3 disabled triggers".into(),
                source: "cron".into(),
                timestamp: "2026-04-17T15:26:00+08:00".into(),
                ..Default::default()
            },
            RecallEntry {
                role: "user".into(),
                content: "Enable all".into(),
                source: "interactive".into(),
                timestamp: "2026-04-17T15:27:00+08:00".into(),
                ..Default::default()
            },
        ];

        let prompt = RecallMemoryManager::render_prompt_section(&entries, 3000);
        assert!(prompt.contains("## Recent Conversations"));
        assert!(prompt.contains("(cron)"));
        assert!(prompt.contains("Found 3 disabled triggers"));
        assert!(prompt.contains("Enable all"));
    }

    #[tokio::test]
    async fn test_purge() {
        let mgr = RecallMemoryManager::in_memory().unwrap();
        let mut old_entry = make_entry("user", "old message", "interactive");
        old_entry.timestamp = "2020-01-01T00:00:00Z".into();
        mgr.record(old_entry).await.unwrap();
        mgr.record(make_entry("user", "new message", "interactive"))
            .await
            .unwrap();

        let purged = mgr.purge_older_than(30).await.unwrap();
        assert_eq!(purged, 1);

        let remaining = mgr.get_recent("agent-1", "telegram", "12345", 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].content, "new message");
    }
}
