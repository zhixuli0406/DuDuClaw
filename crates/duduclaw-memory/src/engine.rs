use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::{info, warn};

use serde::Serialize;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::{MemoryEntry, TimeWindow};

/// A lightweight cross-session key fact (P2 Key-Fact Accumulator).
///
/// Replaces MemGPT's 6,500-token Core Memory with <200 tokens of key facts.
/// Stored per-agent in memory.db, retrieved via FTS5 for system prompt injection.
#[derive(Debug, Clone, Serialize)]
pub struct KeyFact {
    pub id: String,
    pub agent_id: String,
    pub fact: String,
    pub channel: String,
    pub chat_id: String,
    pub source_session: String,
    pub timestamp: String,
    pub access_count: u32,
}

/// Configurable weights for the Generative Agents 3D retrieval re-ranking.
///
/// Adjustable per-engine instance. Defaults tuned for agents with daily conversations.
#[derive(Debug, Clone)]
pub struct RetrievalWeights {
    /// Recency decay base (default 0.995). Higher = slower decay.
    /// 0.99 → 7-day half-life; 0.995 → 14-day half-life; 0.999 → 69-day half-life.
    pub recency_decay: f64,
    /// Weight for recency dimension (default 0.25).
    pub w_recency: f64,
    /// Weight for importance dimension (default 0.35).
    pub w_importance: f64,
    /// Weight for FTS relevance dimension (default 0.40).
    pub w_fts: f64,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            recency_decay: 0.995,  // ~14-day half-life (better differentiation in 0-48h window)
            w_recency: 0.25,
            w_importance: 0.35,
            w_fts: 0.40,
        }
    }
}

/// SQLite-backed memory engine with FTS5 full-text search.
///
/// Note: `list_recent()` is an inherent method (not on the `MemoryEngine` trait)
/// that returns entries ordered by recency without requiring an FTS query.
pub struct SqliteMemoryEngine {
    conn: Mutex<Connection>,
    /// Configurable retrieval weights for search re-ranking.
    pub retrieval_weights: RetrievalWeights,
}

impl SqliteMemoryEngine {
    /// Open (or create) a database at `db_path` and initialise tables.
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        Self::init_tables(&conn)?;
        info!(?db_path, "SQLite memory engine initialised");
        Ok(Self {
            conn: Mutex::new(conn),
            retrieval_weights: RetrievalWeights::default(),
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
            retrieval_weights: RetrievalWeights::default(),
        })
    }

    /// Acquire the database connection for maintenance tasks (e.g., decay/archival).
    ///
    /// Callers hold the lock for the duration of their work, preventing concurrent
    /// writes during multi-statement maintenance operations.
    pub async fn conn_for_maintenance(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        self.conn.lock().await
    }

    /// Select clause for all memory columns (qualified with table alias `m.`).
    const SELECT_COLS: &str = "m.id, m.agent_id, m.content, m.timestamp, m.tags, m.layer, m.importance, m.access_count, m.last_accessed, m.source_event";


    /// Return up to `limit` most-recent memory entries for `agent_id`, newest first.
    pub async fn list_recent(&self, agent_id: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {} FROM memories AS m WHERE m.agent_id = ?1 ORDER BY m.timestamp DESC LIMIT ?2",
            Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![agent_id, limit as i64], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(entries)
    }

    /// Fetch a single memory entry by its UUID and owning agent.
    ///
    /// Returns `Ok(Some(entry))` when found, `Ok(None)` when the ID does not
    /// exist **or** belongs to a different agent (ownership enforcement).
    /// This is more efficient than `list_recent` + linear scan for point lookups.
    pub async fn get_by_id(
        &self,
        agent_id: &str,
        memory_id: &str,
    ) -> Result<Option<MemoryEntry>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {} FROM memories AS m WHERE m.id = ?1 AND m.agent_id = ?2 LIMIT 1",
            Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![memory_id, agent_id], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Search memories filtered by cognitive layer.
    ///
    /// Same as `search()` but restricted to a specific layer (episodic/semantic).
    pub async fn search_layer(
        &self,
        agent_id: &str,
        query: &str,
        layer: &duduclaw_core::types::MemoryLayer,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let conn = self.conn.lock().await;

        let cleaned: String = query
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ':' | '^' | '{' | '}' | '*' | '(' | ')'))
            .take(500)
            .collect();
        if cleaned.trim().is_empty() {
            return Ok(Vec::new());
        }
        let sanitized_query = format!("\"{}\"", cleaned.replace('"', ""));

        let sql = format!(
            "SELECT {cols}
             FROM memories_fts AS f
             JOIN memories AS m ON m.id = f.memory_id
             WHERE f.memories_fts MATCH ?1
               AND f.agent_id = ?2
               AND m.layer = ?3
             ORDER BY rank
             LIMIT ?4",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, agent_id, layer.as_str(), limit as i64], Self::row_to_entry)
            .map_err(|e| {
                tracing::warn!("FTS5 layer search error: {e}");
                DuDuClawError::Memory("Search query error".to_string())
            })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(results)
    }

    /// Search episodic memories by topic keywords, filtering for higher-importance entries.
    ///
    /// Used by the skill synthesizer to find successful conversation patterns
    /// related to a specific topic for auto-synthesis.
    pub async fn search_successful_conversations(
        &self,
        agent_id: &str,
        topic: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;

        let cleaned: String = topic
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ':' | '^' | '{' | '}' | '*' | '(' | ')'))
            .take(500)
            .collect();
        if cleaned.trim().is_empty() {
            return Ok(Vec::new());
        }
        let sanitized_query = format!("\"{}\"", cleaned.replace('"', ""));

        let sql = "SELECT m.content
             FROM memories_fts AS f
             JOIN memories AS m ON m.id = f.memory_id
             WHERE f.memories_fts MATCH ?1
               AND f.agent_id = ?2
               AND m.layer = 'episodic'
               AND m.importance >= 5.0
             ORDER BY m.timestamp DESC
             LIMIT ?3";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, agent_id, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| {
                tracing::warn!("FTS5 synthesis search error: {e}");
                DuDuClawError::Memory("Search query error".to_string())
            })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(results)
    }

    /// Calculate episodic memory pressure for an agent.
    ///
    /// Returns the sum of importance scores of episodic memories created
    /// since `since` timestamp, divided by 10. A value > 10.0 suggests
    /// the agent has accumulated enough observations to warrant a Meso reflection.
    pub async fn episodic_pressure(&self, agent_id: &str, since: DateTime<Utc>) -> f64 {
        let conn = self.conn.lock().await;
        let since_str = since.to_rfc3339();

        conn.query_row(
            "SELECT COALESCE(SUM(importance), 0.0) FROM memories
             WHERE agent_id = ?1 AND layer = 'episodic' AND timestamp >= ?2",
            params![agent_id, since_str],
            |row| row.get::<_, f64>(0),
        )
        .unwrap_or(0.0)
            / 10.0
    }

    /// Count high-importance episodic memories that have no corresponding semantic memory.
    ///
    /// Heuristic: counts episodic memories (importance >= 7, last 7 days) where
    /// the semantic memory layer has zero entries for the same agent.
    /// This indicates accumulated observations that haven't been consolidated
    /// into generalised knowledge yet.
    pub async fn semantic_conflict_count(&self, agent_id: &str) -> u32 {
        let conn = self.conn.lock().await;

        let semantic_count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE agent_id = ?1 AND layer = 'semantic'",
            params![agent_id],
            |row| row.get(0),
        ).unwrap_or(0);

        let high_episodic: u32 = conn.query_row(
            "SELECT COUNT(*) FROM memories
             WHERE agent_id = ?1 AND layer = 'episodic' AND importance >= 7.0
             AND timestamp >= datetime('now', '-7 days')",
            params![agent_id],
            |row| row.get(0),
        ).unwrap_or(0);

        // If there are high-importance episodic memories but few semantic memories,
        // this indicates unconsolidated knowledge (potential "conflicts")
        if semantic_count == 0 && high_episodic > 0 {
            high_episodic
        } else if semantic_count > 0 {
            // Ratio: how many high episodic per semantic
            // More than 3:1 suggests consolidation is needed
            high_episodic.saturating_sub(semantic_count * 3)
        } else {
            0
        }
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
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                layer TEXT NOT NULL DEFAULT 'episodic',
                importance REAL NOT NULL DEFAULT 5.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT,
                source_event TEXT DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_memories_agent
                ON memories(agent_id);

            CREATE INDEX IF NOT EXISTS idx_memories_timestamp
                ON memories(timestamp);

            CREATE INDEX IF NOT EXISTS idx_memories_layer
                ON memories(layer);

            CREATE INDEX IF NOT EXISTS idx_memories_importance
                ON memories(importance DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                agent_id UNINDEXED,
                memory_id UNINDEXED,
                tokenize='unicode61'
            );
            ",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        // Key-Fact Accumulator (P2): lightweight cross-session memory.
        // Stores extracted key facts per agent for future session context.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS key_facts (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                fact TEXT NOT NULL,
                channel TEXT DEFAULT '',
                chat_id TEXT DEFAULT '',
                source_session TEXT DEFAULT '',
                timestamp TEXT NOT NULL,
                access_count INTEGER DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_facts_agent
                ON key_facts(agent_id, timestamp DESC);
            ",
        )
        .map_err(|e| DuDuClawError::Memory(format!("key_facts table: {e}")))?;

        // FTS5 for key facts — separate virtual table for fact search
        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS key_facts_fts USING fts5(
                fact,
                tokenize='unicode61'
            );",
        );

        // Migration: add cognitive memory columns to existing databases (idempotent).
        // Each ALTER is run individually so that "duplicate column" errors on one
        // do not prevent subsequent columns from being added.
        let migrations = [
            "ALTER TABLE memories ADD COLUMN layer TEXT NOT NULL DEFAULT 'episodic'",
            "ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 5.0",
            "ALTER TABLE memories ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE memories ADD COLUMN last_accessed TEXT",
            "ALTER TABLE memories ADD COLUMN source_event TEXT DEFAULT ''",
        ];
        for sql in &migrations {
            match conn.execute_batch(sql) {
                Ok(()) => {}
                Err(e) if e.to_string().contains("duplicate column name") => {}
                Err(e) => {
                    tracing::warn!("Memory schema migration failed: {e} — SQL: {sql}");
                    return Err(DuDuClawError::Memory(format!("Migration failed: {e}")));
                }
            }
        }

        Ok(())
    }

    /// Parse a row from the `memories` table into a [`MemoryEntry`].
    ///
    /// Expects columns: id, agent_id, content, timestamp, tags, layer, importance,
    /// access_count, last_accessed, source_event.
    /// Gracefully handles missing cognitive columns (backward compat with old DBs).
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

        // Cognitive fields — gracefully default if columns missing (old DB)
        let layer_str: String = row.get(5).unwrap_or_else(|_| "episodic".to_string());
        let importance: f64 = row.get(6).unwrap_or(5.0);
        let access_count: u32 = row.get(7).unwrap_or(0);
        let last_accessed: Option<DateTime<Utc>> = row
            .get::<_, Option<String>>(8)
            .unwrap_or(None)
            .and_then(|s| s.parse().ok());
        let source_event: String = row.get(9).unwrap_or_default();

        Ok(MemoryEntry {
            id,
            agent_id,
            content,
            timestamp,
            tags,
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::parse(&layer_str),
            importance,
            access_count,
            last_accessed,
            source_event,
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
        let last_accessed_str = entry.last_accessed.map(|t| t.to_rfc3339());

        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp, tags, layer, importance, access_count, last_accessed, source_event)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.id, agent_id, entry.content, timestamp_str, tags_json,
                entry.layer.as_str(), entry.importance, entry.access_count,
                last_accessed_str, entry.source_event
            ],
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

        // Sanitize FTS5 query: strip ALL special characters and operators (BE-H2).
        // Then wrap as a phrase query to prevent boolean operators (AND/OR/NOT/NEAR).
        let cleaned: String = query
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ':' | '^' | '{' | '}' | '*' | '(' | ')'))
            .take(500)
            .collect();
        let sanitized_query = format!("\"{}\"", cleaned.replace('"', ""));
        if cleaned.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Use FTS5 MATCH to find relevant memory ids, then join back for full rows.
        // Retrieve more candidates than needed for post-retrieval re-ranking by importance.
        let fetch_limit = (limit * 4).max(20);
        let sql = format!(
            "SELECT {cols}
             FROM memories_fts AS f
             JOIN memories AS m ON m.id = f.memory_id
             WHERE f.memories_fts MATCH ?1
               AND f.agent_id = ?2
             ORDER BY rank
             LIMIT ?3",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, agent_id, fetch_limit as i64], Self::row_to_entry)
            .map_err(|e| {
                // Don't leak schema details — return generic error (BE-H2)
                tracing::warn!("FTS5 search error: {e}");
                DuDuClawError::Memory("Search query error — please simplify your query".to_string())
            })?;

        let mut candidates = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            candidates.push(entry);
        }

        // Post-retrieval re-ranking by recency + importance + FTS position.
        // Generative Agents (arXiv 2304.03442) three-dimensional weighting.
        let now = Utc::now();
        let w = &self.retrieval_weights;
        let mut scored: Vec<(f64, MemoryEntry)> = candidates
            .into_iter()
            .enumerate()
            .map(|(rank_pos, entry)| {
                let hours_ago = now
                    .signed_duration_since(
                        entry.last_accessed.unwrap_or(entry.timestamp)
                    )
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
        let results: Vec<MemoryEntry> = scored.into_iter().take(limit).map(|(_, e)| e).collect();

        // Update access_count for returned results (within same lock, but after stmt is dropped)
        let result_ids: Vec<String> = results.iter().map(|e| e.id.clone()).collect();
        let now_str = now.to_rfc3339();
        // stmt is already dropped here (it went out of scope after query_map)
        for id in &result_ids {
            let _ = conn.execute(
                "UPDATE memories SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
                params![now_str, id],
            );
        }

        Ok(results)
    }

    async fn summarize(&self, agent_id: &str, window: TimeWindow) -> Result<String> {
        // --- Phase 1: fetch entries while holding the lock ---
        let (raw, header) = {
            let conn = self.conn.lock().await;

            let start_str = window.start.to_rfc3339();
            let end_str = window.end.to_rfc3339();

            let sql = format!(
                "SELECT {} FROM memories AS m WHERE m.agent_id = ?1 AND m.timestamp >= ?2 AND m.timestamp <= ?3 ORDER BY m.timestamp ASC",
                Self::SELECT_COLS
            );
            let mut stmt = conn
                .prepare(&sql)
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

            let raw = entries
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

            let header = format!(
                "Summary for agent '{agent_id}' ({} memories, {} to {}):\n",
                entries.len(),
                window.start.format("%Y-%m-%d"),
                window.end.format("%Y-%m-%d"),
            );

            (raw, header)
            // lock released here — `conn`, `stmt`, and `entries` are all dropped
        };

        // --- Phase 2: call Claude without holding the lock ---
        let claude_summary = call_claude_summarize(&raw).await;
        if !claude_summary.is_empty() {
            return Ok(format!("{header}{claude_summary}"));
        }

        Ok(format!("{header}{raw}"))
    }
}

// ── Claude helper ────────────────────────────────────────────

/// Call the `claude` CLI to produce a narrative summary of raw memory entries.
/// Returns an empty string on any failure so callers can fall back to raw text.
async fn call_claude_summarize(raw_memories: &str) -> String {
    let api_key = match std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => return String::new(),
    };

    let claude = match duduclaw_core::which_claude() {
        Some(p) => p,
        None => return String::new(),
    };

    // Escape XML closing tags in memory content to prevent prompt injection
    // via crafted memory entries that could break out of the XML delimiters.
    let escaped_memories = raw_memories
        .replace("</memory_entries>", "&lt;/memory_entries&gt;")
        .replace("<memory_entries>", "&lt;memory_entries&gt;");

    let prompt = format!(
        "Summarize the following agent memory entries into a concise narrative (max 300 words). \
         Focus on patterns, key decisions, and recurring themes.\n\n\
         <memory_entries>\n{escaped_memories}\n</memory_entries>"
    );

    let mut cmd = duduclaw_core::platform::async_command_for(&claude);
    cmd.args(["-p", &prompt, "--model", "claude-haiku-4-5", "--output-format", "text"]);
    cmd.env("ANTHROPIC_API_KEY", &api_key);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    cmd.kill_on_drop(true);

    match tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}


// ── Key-Fact Accumulator (P2) ──────────────────────────────────

impl SqliteMemoryEngine {
    /// Store a key fact extracted from a conversation turn.
    pub async fn store_fact(
        &self,
        agent_id: &str,
        fact: &str,
        channel: &str,
        chat_id: &str,
        source_session: &str,
    ) -> Result<String> {
        let conn = self.conn.lock().await;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO key_facts (id, agent_id, fact, channel, chat_id, source_session, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, agent_id, fact, channel, chat_id, source_session, now],
        )
        .map_err(|e| DuDuClawError::Memory(format!("store_fact: {e}")))?;

        // Sync FTS5 index
        let _ = conn.execute(
            "INSERT INTO key_facts_fts (rowid, fact) VALUES (last_insert_rowid(), ?1)",
            params![fact],
        );

        Ok(id)
    }

    /// Get the most recent key facts for an agent.
    pub async fn get_recent_facts(&self, agent_id: &str, limit: u32) -> Result<Vec<KeyFact>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, fact, channel, chat_id, source_session, timestamp, access_count
                 FROM key_facts WHERE agent_id = ?1 ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![agent_id, limit], |row| {
                Ok(KeyFact {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    fact: row.get(2)?,
                    channel: row.get(3)?,
                    chat_id: row.get(4)?,
                    source_session: row.get(5)?,
                    timestamp: row.get(6)?,
                    access_count: row.get(7)?,
                })
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut facts = Vec::new();
        for row in rows {
            facts.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(facts)
    }

    /// Search key facts by query using FTS5 full-text search.
    pub async fn search_facts(&self, agent_id: &str, query: &str, limit: u32) -> Result<Vec<KeyFact>> {
        let conn = self.conn.lock().await;

        // FTS5 search with agent_id filter via JOIN
        let mut stmt = conn
            .prepare(
                "SELECT k.id, k.agent_id, k.fact, k.channel, k.chat_id, k.source_session, k.timestamp, k.access_count
                 FROM key_facts k
                 JOIN key_facts_fts f ON f.rowid = k.rowid
                 WHERE k.agent_id = ?1 AND key_facts_fts MATCH ?2
                 ORDER BY rank
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Memory(format!("search_facts prepare: {e}")))?;

        let rows = stmt
            .query_map(params![agent_id, query, limit], |row| {
                Ok(KeyFact {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    fact: row.get(2)?,
                    channel: row.get(3)?,
                    chat_id: row.get(4)?,
                    source_session: row.get(5)?,
                    timestamp: row.get(6)?,
                    access_count: row.get(7)?,
                })
            })
            .map_err(|e| DuDuClawError::Memory(format!("search_facts query: {e}")))?;

        let mut facts = Vec::new();
        for row in rows {
            facts.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }

        // Fallback: if FTS5 returns nothing (query too short, no match), use recency
        if facts.is_empty() {
            drop(stmt);
            return self.get_recent_facts(agent_id, limit).await;
        }

        Ok(facts)
    }

    /// Count total key facts for an agent.
    pub async fn count_facts(&self, agent_id: &str) -> Result<u64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM key_facts WHERE agent_id = ?1",
                params![agent_id],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(count as u64)
    }

    /// Bump access count for a fact (used during deduplication).
    pub async fn bump_fact_access(&self, fact_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE key_facts SET access_count = access_count + 1 WHERE id = ?1",
            params![fact_id],
        )
        .map_err(|e| DuDuClawError::Memory(format!("bump_fact_access: {e}")))?;
        Ok(())
    }

    /// Purge stale facts older than `max_age_days` with `access_count < min_access`.
    pub async fn purge_stale_facts(&self, agent_id: &str, max_age_days: u32, min_access: u32) -> Result<u64> {
        let conn = self.conn.lock().await;
        let cutoff = (Utc::now() - chrono::Duration::days(max_age_days as i64)).to_rfc3339();
        let count = conn
            .execute(
                "DELETE FROM key_facts WHERE agent_id = ?1 AND timestamp < ?2 AND access_count < ?3",
                params![agent_id, cutoff, min_access],
            )
            .map_err(|e| DuDuClawError::Memory(format!("purge_stale_facts: {e}")))?;
        Ok(count as u64)
    }
}

/// Word-level Jaccard similarity for fact deduplication.
///
/// Returns 0.0–1.0. Used to detect near-duplicate facts before storing.
pub fn word_jaccard(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
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
            layer: Default::default(),
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: String::new(),
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

    // ── get_by_id tests ────────────────────────────────────────────────────────

    /// Storing an entry and retrieving it by ID returns the correct content.
    #[tokio::test]
    async fn get_by_id_returns_stored_entry() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-a";
        let entry = make_entry(agent, "unique memory content", vec!["tag1".to_string()]);
        let stored_id = entry.id.clone();

        engine.store(agent, entry).await.unwrap();

        let result = engine.get_by_id(agent, &stored_id).await.unwrap();
        let found = result.expect("entry should be found by ID");
        assert_eq!(found.id, stored_id);
        assert_eq!(found.content, "unique memory content");
        assert_eq!(found.agent_id, agent);
    }

    /// Unknown ID returns None (not an error).
    #[tokio::test]
    async fn get_by_id_unknown_id_returns_none() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let result = engine.get_by_id("agent-a", "nonexistent-uuid").await.unwrap();
        assert!(result.is_none(), "unknown ID should return None");
    }

    /// Cross-agent lookup: agent-b cannot read agent-a's entry by ID.
    #[tokio::test]
    async fn get_by_id_cross_agent_returns_none() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let entry = make_entry("agent-a", "secret of agent-a", vec![]);
        let stored_id = entry.id.clone();
        engine.store("agent-a", entry).await.unwrap();

        // agent-b queries the same ID — must not see agent-a's data.
        let result = engine.get_by_id("agent-b", &stored_id).await.unwrap();
        assert!(
            result.is_none(),
            "cross-agent get_by_id must return None (ownership enforcement)"
        );
    }
}
