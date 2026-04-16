//! Shared memory engine — cross-agent memory pool.
//!
//! A global SQLite database (`shared_memory.db`) where agents can publish
//! and search memories with visibility controls. Reuses the Generative Agents
//! 3D-weighted retrieval from [`SqliteMemoryEngine`](super::engine::SqliteMemoryEngine).

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use duduclaw_core::error::{DuDuClawError, Result};

use crate::engine::RetrievalWeights;

/// Visibility scope for a shared memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedVisibility {
    /// Any agent can read.
    Public,
    /// Only agents sharing the same `reports_to` parent.
    Team,
    /// Only the parent/child chain (ancestors + descendants).
    HierarchyOnly,
}

impl Default for SharedVisibility {
    fn default() -> Self {
        Self::Public
    }
}

impl SharedVisibility {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Public => "public",
            Self::Team => "team",
            Self::HierarchyOnly => "hierarchy_only",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "team" => Self::Team,
            "hierarchy_only" => Self::HierarchyOnly,
            _ => Self::Public,
        }
    }
}

/// A single entry in the shared memory pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedMemoryEntry {
    pub id: String,
    pub source_agent: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub tags: Vec<String>,
    pub layer: String,
    pub importance: f64,
    pub visibility: SharedVisibility,
    pub access_count: u32,
    pub last_accessed: Option<DateTime<Utc>>,
}

/// SQLite-backed shared memory engine for cross-agent knowledge sharing.
pub struct SharedMemoryEngine {
    conn: Mutex<Connection>,
    pub retrieval_weights: RetrievalWeights,
}

impl SharedMemoryEngine {
    /// Open (or create) the shared memory database at `db_path`.
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        Self::init_tables(&conn)?;
        info!(?db_path, "Shared memory engine initialised");
        Ok(Self {
            conn: Mutex::new(conn),
            retrieval_weights: RetrievalWeights::default(),
        })
    }

    /// Create an in-memory shared database (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            retrieval_weights: RetrievalWeights::default(),
        })
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS shared_memories (
                id TEXT PRIMARY KEY,
                source_agent TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                layer TEXT NOT NULL DEFAULT 'semantic',
                importance REAL NOT NULL DEFAULT 5.0,
                visibility TEXT NOT NULL DEFAULT 'public',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_shared_source
                ON shared_memories(source_agent);

            CREATE INDEX IF NOT EXISTS idx_shared_timestamp
                ON shared_memories(timestamp);

            CREATE INDEX IF NOT EXISTS idx_shared_visibility
                ON shared_memories(visibility);

            CREATE INDEX IF NOT EXISTS idx_shared_importance
                ON shared_memories(importance DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS shared_memories_fts USING fts5(
                content,
                source_agent UNINDEXED,
                memory_id UNINDEXED,
                tokenize='unicode61'
            );

            CREATE TABLE IF NOT EXISTS shared_access_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_id TEXT NOT NULL,
                accessor_agent TEXT NOT NULL,
                action TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_access_log_memory
                ON shared_access_log(memory_id);
            ",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    const SELECT_COLS: &str = "m.id, m.source_agent, m.content, m.timestamp, m.tags, m.layer, m.importance, m.visibility, m.access_count, m.last_accessed";

    /// Publish a memory to the shared pool.
    ///
    /// `source_agent` is the authoritative agent identifier (used for DB storage
    /// and access logging). `entry.source_agent` is ignored — the function
    /// parameter takes precedence to prevent spoofing.
    pub async fn share(
        &self,
        source_agent: &str,
        entry: SharedMemoryEntry,
    ) -> Result<()> {
        let conn = self.conn.lock().await;

        let tags_json =
            serde_json::to_string(&entry.tags).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let timestamp_str = entry.timestamp.to_rfc3339();

        // Transaction: all three INSERTs must succeed atomically.
        conn.execute_batch("BEGIN")
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let insert_result = (|| -> std::result::Result<(), rusqlite::Error> {
            conn.execute(
                "INSERT INTO shared_memories (id, source_agent, content, timestamp, tags, layer, importance, visibility, access_count, last_accessed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    entry.id, source_agent, entry.content, timestamp_str, tags_json,
                    entry.layer, entry.importance, entry.visibility.as_str(),
                    entry.access_count, entry.last_accessed.map(|t| t.to_rfc3339()),
                ],
            )?;

            conn.execute(
                "INSERT INTO shared_memories_fts (content, source_agent, memory_id) VALUES (?1, ?2, ?3)",
                params![entry.content, source_agent, entry.id],
            )?;

            conn.execute(
                "INSERT INTO shared_access_log (memory_id, accessor_agent, action, timestamp) VALUES (?1, ?2, 'write', ?3)",
                params![entry.id, source_agent, timestamp_str],
            )?;
            Ok(())
        })();

        match insert_result {
            Ok(()) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(DuDuClawError::Memory(e.to_string()));
            }
        }

        info!(source = source_agent, id = %entry.id, "shared memory stored");
        Ok(())
    }

    /// Search shared memories with visibility filtering.
    ///
    /// `visible_agents` is the set of agent IDs whose `team` / `hierarchy_only`
    /// memories the accessor can see. Pass `None` for public-only access.
    pub async fn search_shared(
        &self,
        query: &str,
        accessor_agent: &str,
        visible_agents: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<SharedMemoryEntry>> {
        let conn = self.conn.lock().await;

        let sanitized_query = match crate::search::sanitize_fts5_query(query) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };

        // Over-fetch to account for post-SQL Rust-side visibility filtering.
        // SQL pre-filter only passes public entries and own-agent entries; team/hierarchy_only
        // records are NOT fetched here to prevent cross-agent data leakage (SEC2-C7).
        // can_access() enforces fine-grained team/hierarchy checks for entries that
        // reach Rust, but the SQL layer must not expose those rows to unrelated agents.
        // Increase fetch_limit to compensate for the stricter SQL filter.
        let fetch_limit = (limit * 8).max(40);
        let sql = format!(
            "SELECT {cols}
             FROM shared_memories_fts AS f
             JOIN shared_memories AS m ON m.id = f.memory_id
             WHERE f.shared_memories_fts MATCH ?1
               AND (m.visibility = 'public' OR m.source_agent = ?3)
             ORDER BY rank
             LIMIT ?2",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, fetch_limit as i64, accessor_agent], Self::row_to_entry)
            .map_err(|e| {
                warn!("Shared FTS5 search error: {e}");
                DuDuClawError::Memory("Search query error".to_string())
            })?;

        let mut candidates = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            // Visibility filtering: team/hierarchy_only entries may still need
            // finer-grained checks based on visible_agents set from AgentRegistry.
            if self.can_access(&entry, accessor_agent, visible_agents) {
                candidates.push(entry);
                // Early exit once we have enough candidates for re-ranking
                if candidates.len() >= limit * 2 {
                    break;
                }
            }
        }

        // 3D-weighted re-ranking (same as SqliteMemoryEngine)
        let now = Utc::now();
        let w = &self.retrieval_weights;
        let mut scored: Vec<(f64, SharedMemoryEntry)> = candidates
            .into_iter()
            .enumerate()
            .map(|(rank_pos, entry)| {
                let hours_ago = now
                    .signed_duration_since(entry.last_accessed.unwrap_or(entry.timestamp))
                    .num_hours()
                    .max(0) as f64;
                let recency = w.recency_decay.powf(hours_ago);
                let importance = entry.importance / 10.0;
                let fts_rank = 1.0 / (1.0 + rank_pos as f64);

                let score = w.w_recency * recency + w.w_importance * importance + w.w_fts * fts_rank;
                (score, entry)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let results: Vec<SharedMemoryEntry> = scored.into_iter().take(limit).map(|(_, e)| e).collect();

        // Update access counts and log reads
        let now_str = now.to_rfc3339();
        for entry in &results {
            let _ = conn.execute(
                "UPDATE shared_memories SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
                params![now_str, entry.id],
            );
            let _ = conn.execute(
                "INSERT INTO shared_access_log (memory_id, accessor_agent, action, timestamp) VALUES (?1, ?2, 'read', ?3)",
                params![entry.id, accessor_agent, now_str],
            );
        }

        // Periodic cleanup: remove access log entries older than 30 days (every ~100 reads).
        static CLEANUP_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let count = CLEANUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 100 == 0 {
            let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
            let _ = conn.execute(
                "DELETE FROM shared_access_log WHERE timestamp < ?1",
                params![cutoff],
            );
        }

        Ok(results)
    }

    /// List the most recent shared memories accessible to `accessor_agent`.
    pub async fn list_recent_shared(
        &self,
        accessor_agent: &str,
        visible_agents: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<SharedMemoryEntry>> {
        let conn = self.conn.lock().await;

        let sql = format!(
            "SELECT {} FROM shared_memories AS m WHERE (m.visibility = 'public' OR m.source_agent = ?2) ORDER BY m.timestamp DESC LIMIT ?1",
            Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        // Fetch more than needed to account for visibility filtering
        let fetch_limit = (limit * 3).max(30);
        let rows = stmt
            .query_map(params![fetch_limit as i64, accessor_agent], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            if self.can_access(&entry, accessor_agent, visible_agents) {
                results.push(entry);
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    /// Check if `accessor` can read an entry based on visibility rules.
    ///
    /// - `Public`: any agent can read.
    /// - `Team` / `HierarchyOnly`: source agent can always read own entries.
    ///   Other agents need `accessor` to appear in the `visible_agents` set
    ///   (resolved from AgentRegistry by the caller).
    fn can_access(
        &self,
        entry: &SharedMemoryEntry,
        accessor: &str,
        visible_agents: Option<&[String]>,
    ) -> bool {
        match entry.visibility {
            SharedVisibility::Public => true,
            SharedVisibility::Team | SharedVisibility::HierarchyOnly => {
                // The source agent can always read their own shared memories
                if entry.source_agent == accessor {
                    return true;
                }
                // Check if the accessor is in the allowed set for this visibility level
                match visible_agents {
                    Some(agents) => agents.iter().any(|a| a == accessor),
                    None => false,
                }
            }
        }
    }

    fn row_to_entry(row: &rusqlite::Row<'_>) -> std::result::Result<SharedMemoryEntry, rusqlite::Error> {
        let id: String = row.get(0)?;
        let source_agent: String = row.get(1)?;
        let content: String = row.get(2)?;
        let timestamp_str: String = row.get(3)?;
        let tags_json: String = row.get(4)?;
        let layer: String = row.get(5)?;
        let importance: f64 = row.get(6)?;
        let visibility_str: String = row.get(7)?;
        let access_count: u32 = row.get(8)?;
        let last_accessed: Option<DateTime<Utc>> = row
            .get::<_, Option<String>>(9)
            .unwrap_or(None)
            .and_then(|s| s.parse().ok());

        let timestamp: DateTime<Utc> = timestamp_str.parse().unwrap_or_else(|_| Utc::now());
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

        Ok(SharedMemoryEntry {
            id,
            source_agent,
            content,
            timestamp,
            tags,
            layer,
            importance,
            visibility: SharedVisibility::from_str(&visibility_str),
            access_count,
            last_accessed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shared_entry(source: &str, content: &str, visibility: SharedVisibility) -> SharedMemoryEntry {
        SharedMemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            source_agent: source.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            layer: "semantic".to_string(),
            importance: 6.0,
            visibility,
            access_count: 0,
            last_accessed: None,
        }
    }

    #[tokio::test]
    async fn share_and_search() {
        let engine = SharedMemoryEngine::in_memory().unwrap();

        engine
            .share("agent-a", make_shared_entry("agent-a", "rust programming tips", SharedVisibility::Public))
            .await
            .unwrap();
        engine
            .share("agent-b", make_shared_entry("agent-b", "python data analysis", SharedVisibility::Public))
            .await
            .unwrap();

        let results = engine
            .search_shared("rust", "agent-c", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("rust"));
    }

    #[tokio::test]
    async fn visibility_filtering_public() {
        let engine = SharedMemoryEngine::in_memory().unwrap();

        engine
            .share("agent-a", make_shared_entry("agent-a", "public knowledge about cats", SharedVisibility::Public))
            .await
            .unwrap();
        engine
            .share("agent-b", make_shared_entry("agent-b", "team-only knowledge about cats", SharedVisibility::Team))
            .await
            .unwrap();

        // Without visible_agents, only public entries are returned
        let results = engine
            .search_shared("cats", "agent-c", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_agent, "agent-a");
    }

    #[tokio::test]
    async fn visibility_filtering_team() {
        let engine = SharedMemoryEngine::in_memory().unwrap();

        engine
            .share("agent-a", make_shared_entry("agent-a", "team knowledge about dogs", SharedVisibility::Team))
            .await
            .unwrap();

        // agent-b can see agent-a's team memory when agent-b is in visible set
        let visible = vec!["agent-b".to_string()];
        let results = engine
            .search_shared("dogs", "agent-b", Some(&visible), 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // agent-c cannot see without being in visible set
        let results = engine
            .search_shared("dogs", "agent-c", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn source_agent_can_read_own_team_memory() {
        let engine = SharedMemoryEngine::in_memory().unwrap();

        engine
            .share("agent-a", make_shared_entry("agent-a", "my team secret about birds", SharedVisibility::Team))
            .await
            .unwrap();

        // Source agent can always read their own entries
        let results = engine
            .search_shared("birds", "agent-a", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn list_recent_shared() {
        let engine = SharedMemoryEngine::in_memory().unwrap();

        for i in 0..5 {
            engine
                .share("agent-a", make_shared_entry("agent-a", &format!("memory {i}"), SharedVisibility::Public))
                .await
                .unwrap();
        }

        let results = engine.list_recent_shared("agent-b", None, 3).await.unwrap();
        assert_eq!(results.len(), 3);
    }
}
