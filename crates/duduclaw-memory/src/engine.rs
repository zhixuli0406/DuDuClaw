use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::{info, warn};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::{MemoryEntry, TimeWindow};

/// SQLite-backed memory engine with FTS5 full-text search.
pub struct SqliteMemoryEngine {
    conn: Mutex<Connection>,
}

impl SqliteMemoryEngine {
    /// Open (or create) a database at `db_path` and initialise tables.
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Self::init_tables(&conn)?;
        info!(?db_path, "SQLite memory engine initialised");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory database (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Self::init_tables(&conn)?;
        info!("SQLite in-memory memory engine initialised");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_memories_agent
                ON memories(agent_id);

            CREATE INDEX IF NOT EXISTS idx_memories_timestamp
                ON memories(timestamp);

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                agent_id UNINDEXED,
                memory_id UNINDEXED,
                tokenize='unicode61'
            );
            ",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    /// Parse a row from the `memories` table into a [`MemoryEntry`].
    fn row_to_entry(row: &rusqlite::Row<'_>) -> std::result::Result<MemoryEntry, rusqlite::Error> {
        let id: String = row.get(0)?;
        let agent_id: String = row.get(1)?;
        let content: String = row.get(2)?;
        let timestamp_str: String = row.get(3)?;
        let tags_json: String = row.get(4)?;

        let timestamp: DateTime<Utc> = timestamp_str.parse().unwrap_or_else(|e| {
            warn!(
                timestamp = %timestamp_str,
                error = %e,
                "failed to parse memory timestamp, falling back to now"
            );
            Utc::now()
        });

        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_else(|e| {
            warn!(
                tags_json = %tags_json,
                error = %e,
                "failed to parse memory tags JSON, falling back to empty"
            );
            Vec::new()
        });

        Ok(MemoryEntry {
            id,
            agent_id,
            content,
            timestamp,
            tags,
            embedding: None,
        })
    }
}

#[async_trait]
impl MemoryEngine for SqliteMemoryEngine {
    async fn store(&self, agent_id: &str, entry: MemoryEntry) -> Result<()> {
        let conn = self.conn.lock().await;

        let tags_json =
            serde_json::to_string(&entry.tags).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let timestamp_str = entry.timestamp.to_rfc3339();

        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry.id, agent_id, entry.content, timestamp_str, tags_json],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        conn.execute(
            "INSERT INTO memories_fts (content, agent_id, memory_id) VALUES (?1, ?2, ?3)",
            params![entry.content, agent_id, entry.id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        info!(agent_id, entry_id = %entry.id, "memory stored");
        Ok(())
    }

    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let conn = self.conn.lock().await;

        // Use FTS5 MATCH to find relevant memory ids, then join back for full rows.
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.agent_id, m.content, m.timestamp, m.tags
                 FROM memories_fts AS f
                 JOIN memories AS m ON m.id = f.memory_id
                 WHERE f.memories_fts MATCH ?1
                   AND f.agent_id = ?2
                 ORDER BY rank
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![query, agent_id, limit as i64], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            results.push(entry);
        }

        Ok(results)
    }

    async fn summarize(&self, agent_id: &str, window: TimeWindow) -> Result<String> {
        let conn = self.conn.lock().await;

        let start_str = window.start.to_rfc3339();
        let end_str = window.end.to_rfc3339();

        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.agent_id, m.content, m.timestamp, m.tags
                 FROM memories AS m
                 WHERE m.agent_id = ?1
                   AND m.timestamp >= ?2
                   AND m.timestamp <= ?3
                 ORDER BY m.timestamp ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![agent_id, start_str, end_str], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries: Vec<MemoryEntry> = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            entries.push(entry);
        }

        if entries.is_empty() {
            return Ok(format!(
                "No memories found for agent '{agent_id}' in the given time window."
            ));
        }

        // Simple concatenation summary (AI-powered summarisation is planned for Phase 4).
        let summary = entries
            .iter()
            .map(|e| {
                format!(
                    "[{}] {}",
                    e.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    e.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!(
            "Summary for agent '{agent_id}' ({} memories, {} to {}):\n{summary}",
            entries.len(),
            window.start.format("%Y-%m-%d"),
            window.end.format("%Y-%m-%d"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn make_entry(agent_id: &str, content: &str, tags: Vec<String>) -> MemoryEntry {
        MemoryEntry {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags,
            embedding: None,
        }
    }

    #[tokio::test]
    async fn store_and_search() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "test-agent";

        engine
            .store(agent, make_entry(agent, "hello world of rust", vec![]))
            .await
            .unwrap();
        engine
            .store(agent, make_entry(agent, "goodbye world", vec![]))
            .await
            .unwrap();

        let results = engine.search(agent, "rust", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("rust"));
    }

    #[tokio::test]
    async fn search_isolates_agents() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();

        engine
            .store("a", make_entry("a", "secret data for agent a", vec![]))
            .await
            .unwrap();
        engine
            .store("b", make_entry("b", "secret data for agent b", vec![]))
            .await
            .unwrap();

        let results = engine.search("a", "secret", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id, "a");
    }

    #[tokio::test]
    async fn summarize_returns_content() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "sum-agent";

        engine
            .store(agent, make_entry(agent, "first memory", vec![]))
            .await
            .unwrap();
        engine
            .store(agent, make_entry(agent, "second memory", vec![]))
            .await
            .unwrap();

        let window = TimeWindow {
            start: Utc::now() - Duration::hours(1),
            end: Utc::now() + Duration::hours(1),
        };
        let summary = engine.summarize(agent, window).await.unwrap();
        assert!(summary.contains("first memory"));
        assert!(summary.contains("second memory"));
    }

    #[tokio::test]
    async fn summarize_empty_window() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let window = TimeWindow {
            start: Utc::now() - Duration::hours(2),
            end: Utc::now() - Duration::hours(1),
        };
        let summary = engine.summarize("nobody", window).await.unwrap();
        assert!(summary.contains("No memories found"));
    }
}
