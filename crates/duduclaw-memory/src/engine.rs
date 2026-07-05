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
///
/// The recency dimension uses Ebbinghaus retrievability `R = exp(-t / S)`
/// (MemoryBank, arXiv:2305.10250) where stability `S` grows with access
/// reinforcement and importance — see [`ebbinghaus_retrievability`].
#[derive(Debug, Clone)]
pub struct RetrievalWeights {
    /// Base stability in days for an importance-5, never-reinforced memory
    /// (default 14.0 — matches the previous ~14-day recency half-life intent).
    pub base_stability_days: f64,
    /// Reinforcement gain: how strongly repeated accesses slow forgetting
    /// (default 0.6; stability multiplier is `1 + k·ln(1 + access_count)`).
    pub reinforce_k: f64,
    /// Upper bound on stability in days (default 365.0).
    pub max_stability_days: f64,
    /// Weight for recency dimension (default 0.25).
    pub w_recency: f64,
    /// Weight for importance dimension (default 0.35).
    pub w_importance: f64,
    /// Weight for FTS relevance dimension (default 0.40).
    pub w_fts: f64,
    /// Weight for the HippoRAG-lite graph dimension (normalized Personalized
    /// PageRank mass over the SPO triple graph, arXiv:2405.14831; default 0.15).
    /// Only applied when the query seeds at least one graph entity.
    pub w_graph: f64,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            base_stability_days: 14.0,
            reinforce_k: 0.6,
            max_stability_days: 365.0,
            w_recency: 0.25,
            w_importance: 0.35,
            w_fts: 0.40,
            w_graph: 0.15,
        }
    }
}

/// Ebbinghaus retrievability `R = exp(-t / S)` (MemoryBank, arXiv:2305.10250).
///
/// `t` is days since the memory was last recalled (falling back to creation
/// time for never-accessed entries). Stability `S` grows logarithmically with
/// reinforcement (`access_count`) and scales with importance, so memories that
/// keep being recalled — or matter more — are forgotten more slowly:
///
/// `S = base · (1 + k·ln(1 + access_count)) · clamp(importance / 5, 0.2, 2.0)`
pub fn ebbinghaus_retrievability(
    days_since_access: f64,
    access_count: u32,
    importance: f64,
    w: &RetrievalWeights,
) -> f64 {
    let stability = (w.base_stability_days
        * (1.0 + w.reinforce_k * (1.0 + access_count as f64).ln())
        * (importance / 5.0).clamp(0.2, 2.0))
    .min(w.max_stability_days)
    .max(f64::EPSILON);
    (-days_since_access.max(0.0) / stability).exp()
}

/// Optional temporal / knowledge-graph metadata for a memory write (F1, v1.19.0).
///
/// When both `subject` and `predicate` are set, [`SqliteMemoryEngine::store_temporal`]
/// performs automatic conflict resolution: any currently-valid memory with the
/// same `(agent_id, subject, predicate)` is closed out (`valid_until = now`,
/// `superseded_by` pointed at the new row) before the new row is inserted,
/// forming a supersession chain. Without a full triple it behaves like a plain
/// insert that also populates the extra columns.
#[derive(Debug, Clone, Default)]
pub struct TemporalMeta {
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    /// World-time the fact becomes valid. Defaults to "now" when `None`.
    pub valid_from: Option<DateTime<Utc>>,
    /// World-time the fact expires. `None` = still valid.
    pub valid_until: Option<DateTime<Utc>>,
    /// 0.0–1.0 confidence; defaults to 1.0 when `None`.
    pub confidence: Option<f64>,
    /// JSON metadata blob (e.g. source mistake ids for reflexion consolidation).
    pub metadata: Option<serde_json::Value>,
}

/// A pending/known decision surfaced from the temporal store (RFC-24).
///
/// Reconstructed from the `(decision:<id>, question|option:<key>|status)` triples
/// written by the decision-capture layer. `options` is sorted by key for stable
/// rendering; `id` is the short id without the `decision:` prefix.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionView {
    pub id: String,
    pub question: String,
    pub options: Vec<(String, String)>,
    pub created_at: Option<String>,
}

/// Outcome of [`SqliteMemoryEngine::resolve_decision`] (RFC-24 §4.4). Fail-closed:
/// every non-`Resolved` variant means nothing was written.
#[derive(Debug, Clone, Serialize)]
pub enum DecisionResolveOutcome {
    /// Decision resolved to the chosen option.
    Resolved {
        chosen_key: String,
        chosen_content: String,
        question: String,
    },
    /// No decision with that id exists for this agent.
    NotFound,
    /// The decision was already resolved/expired (carries the current status).
    AlreadyResolved(String),
    /// The chosen key is not among the decision's options.
    UnknownKey { available: Vec<String> },
}

/// One node in a temporal supersession chain, returned by
/// [`SqliteMemoryEngine::get_history`] (F1).
#[derive(Debug, Clone, Serialize)]
pub struct TemporalRecord {
    pub id: String,
    pub content: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub superseded_by: Option<String>,
    pub supersedes: Option<String>,
    pub confidence: f64,
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

    /// Fetch one row by id with the same agent-isolation + temporal-validity
    /// filters as `search()`. Used to materialize graph-only candidates
    /// (HippoRAG-lite) while the caller already holds the connection lock.
    fn fetch_valid_entry(
        conn: &Connection,
        agent_id: &str,
        memory_id: &str,
        now_rfc: &str,
    ) -> Result<Option<MemoryEntry>> {
        let sql = format!(
            "SELECT {cols} FROM memories AS m
             WHERE m.id = ?1 AND m.agent_id = ?2
               AND (m.valid_until IS NULL OR m.valid_until > ?3)
             LIMIT 1",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![memory_id, agent_id, now_rfc], Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Recency + importance portion of the Generative-Agents score (shared by
    /// the FTS path and the graph-only append path so both use identical math).
    fn base_relevance_score(entry: &MemoryEntry, now: DateTime<Utc>, w: &RetrievalWeights) -> f64 {
        let days_since_access = now
            .signed_duration_since(entry.last_accessed.unwrap_or(entry.timestamp))
            .num_seconds()
            .max(0) as f64
            / 86_400.0;
        let recency =
            ebbinghaus_retrievability(days_since_access, entry.access_count, entry.importance, w);
        let importance = entry.importance / 10.0;
        w.w_recency * recency + w.w_importance * importance
    }

    /// Fetch multiple memory entries by ID for a single agent in one query (F3).
    ///
    /// Ownership is enforced identically to [`get_by_id`]: only entries owned by
    /// `agent_id` are returned. IDs that don't exist **or** belong to another
    /// agent are simply absent from the result — callers diff against the
    /// requested IDs to find misses (no existence leak between the two cases).
    /// `ids` is capped at 100 per call.
    pub async fn get_by_ids(
        &self,
        agent_id: &str,
        ids: &[String],
    ) -> Result<Vec<MemoryEntry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > 100 {
            return Err(DuDuClawError::Memory(
                "batch fetch limited to 100 ids per request".to_string(),
            ));
        }
        let conn = self.conn.lock().await;
        // Build a "?,?,?" placeholder list for the IN clause (one per id).
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {cols} FROM memories AS m WHERE m.agent_id = ? AND m.id IN ({ph})",
            cols = Self::SELECT_COLS,
            ph = placeholders
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        // Bind agent_id first (the leading `?`), then every requested id.
        let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        bind.push(&agent_id);
        for id in ids {
            bind.push(id);
        }

        let rows = stmt
            .query_map(bind.as_slice(), Self::row_to_entry)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(entries)
    }

    /// Read the metadata JSON blob for a single entry (ownership enforced).
    ///
    /// Returns `Ok(None)` when the id does not exist or belongs to another
    /// agent. A malformed stored blob degrades to `{}` rather than erroring so
    /// one corrupt row cannot wedge callers that iterate many entries.
    pub async fn get_metadata(
        &self,
        agent_id: &str,
        memory_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT metadata FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1")
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![memory_id, agent_id], |r| r.get::<_, String>(0))
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        match rows.next() {
            Some(row) => {
                let raw = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                Ok(Some(
                    serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
                ))
            }
            None => Ok(None),
        }
    }

    /// Overwrite the metadata JSON blob for a single entry (ownership enforced).
    ///
    /// Returns `Ok(true)` when a row was updated, `Ok(false)` when the id does
    /// not exist or belongs to another agent.
    pub async fn update_metadata(
        &self,
        agent_id: &str,
        memory_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE memories SET metadata = ?1 WHERE id = ?2 AND agent_id = ?3",
                params![metadata.to_string(), memory_id, agent_id],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(n > 0)
    }

    /// List currently-valid entries with a given `source_event`, newest first,
    /// each paired with its metadata JSON blob.
    ///
    /// Used by the rule-lifecycle layer to enumerate consolidated reflexion
    /// rules together with their `rule_stats` counters in one query.
    pub async fn list_valid_by_source_event(
        &self,
        agent_id: &str,
        source_event: &str,
        limit: usize,
    ) -> Result<Vec<(MemoryEntry, serde_json::Value)>> {
        let conn = self.conn.lock().await;
        let now_rfc = Utc::now().to_rfc3339();
        let sql = format!(
            "SELECT {cols}, m.metadata FROM memories AS m
             WHERE m.agent_id = ?1 AND m.source_event = ?2
               AND (m.valid_until IS NULL OR m.valid_until > ?3)
             ORDER BY m.timestamp DESC LIMIT ?4",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![agent_id, source_event, now_rfc, limit as i64],
                |row| {
                    let entry = Self::row_to_entry(row)?;
                    let raw: String = row.get(10)?;
                    Ok((entry, raw))
                },
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            let (entry, raw) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let meta = serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
            out.push((entry, meta));
        }
        Ok(out)
    }

    /// Set an entry's importance and append `tag` to its tags in one update
    /// (ownership enforced, idempotent — the tag is not duplicated).
    ///
    /// Returns `Ok(true)` when a row was updated.
    pub async fn set_importance_and_add_tag(
        &self,
        agent_id: &str,
        memory_id: &str,
        importance: f64,
        tag: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let existing: Option<String> = {
            let mut stmt = conn
                .prepare("SELECT tags FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1")
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let mut rows = stmt
                .query_map(params![memory_id, agent_id], |r| r.get::<_, String>(0))
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            match rows.next() {
                Some(row) => Some(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?),
                None => None,
            }
        };
        let Some(tags_json) = existing else {
            return Ok(false);
        };
        let mut tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
        let new_tags =
            serde_json::to_string(&tags).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let n = conn
            .execute(
                "UPDATE memories SET importance = ?1, tags = ?2 WHERE id = ?3 AND agent_id = ?4",
                params![importance, new_tags, memory_id, agent_id],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(n > 0)
    }

    /// Store a memory with temporal / knowledge-graph metadata and automatic
    /// conflict resolution (F1, v1.19.0).
    ///
    /// If `meta` carries both `subject` and `predicate`, any currently-valid
    /// memory with the same triple is superseded (its `valid_until` is set to
    /// now and `superseded_by` points at this new row). Without a full triple
    /// this is a plain insert that also populates the temporal columns.
    /// Returns the stored entry id.
    pub async fn store_temporal(
        &self,
        agent_id: &str,
        entry: MemoryEntry,
        meta: TemporalMeta,
    ) -> Result<String> {
        let conn = self.conn.lock().await;

        let now = Utc::now();
        let valid_from = meta.valid_from.unwrap_or(now).to_rfc3339();
        let valid_until = meta.valid_until.map(|t| t.to_rfc3339());
        let confidence = meta.confidence.unwrap_or(1.0);
        let metadata = meta
            .metadata
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());

        // ── Conflict resolution: only when a full triple is supplied ──────────
        let mut supersedes: Option<String> = None;
        if let (Some(subj), Some(pred)) = (meta.subject.as_ref(), meta.predicate.as_ref()) {
            let existing: Vec<String> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM memories
                         WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                           AND valid_until IS NULL
                         ORDER BY COALESCE(valid_from, timestamp) DESC, created_at DESC, id DESC",
                    )
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                let rows = stmt
                    .query_map(params![agent_id, subj, pred], |r| r.get::<_, String>(0))
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
                }
                v
            };
            if existing.len() > 1 {
                warn!(
                    agent_id,
                    subject = %subj,
                    predicate = %pred,
                    count = existing.len(),
                    "multiple active memories for the same triple — superseding all"
                );
            }
            let now_str = now.to_rfc3339();
            for old_id in &existing {
                conn.execute(
                    "UPDATE memories SET valid_until = ?1, superseded_by = ?2 WHERE id = ?3",
                    params![now_str, entry.id, old_id],
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            }
            // Record the most-recent superseded id for the new row's back-pointer.
            supersedes = existing.into_iter().next();
        }

        let tags_json = serde_json::to_string(&entry.tags)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let timestamp_str = entry.timestamp.to_rfc3339();
        let last_accessed_str = entry.last_accessed.map(|t| t.to_rfc3339());

        conn.execute(
            "INSERT INTO memories
                (id, agent_id, content, timestamp, tags, layer, importance, access_count,
                 last_accessed, source_event,
                 valid_from, valid_until, superseded_by, supersedes,
                 subject, predicate, object, confidence, metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                entry.id, agent_id, entry.content, timestamp_str, tags_json,
                entry.layer.as_str(), entry.importance, entry.access_count,
                last_accessed_str, entry.source_event,
                valid_from, valid_until, Option::<String>::None, supersedes,
                meta.subject, meta.predicate, meta.object, confidence, metadata
            ],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        conn.execute(
            "INSERT INTO memories_fts (content, agent_id, memory_id) VALUES (?1, ?2, ?3)",
            params![entry.content, agent_id, entry.id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        info!(agent_id, entry_id = %entry.id, "temporal memory stored");
        Ok(entry.id)
    }

    /// Return the full supersession chain for a `(subject, predicate)` pair,
    /// oldest → newest, including expired/superseded rows (F1).
    pub async fn get_history(
        &self,
        agent_id: &str,
        subject: &str,
        predicate: &str,
    ) -> Result<Vec<TemporalRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, content, valid_from, valid_until, superseded_by, supersedes, confidence
                 FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                 ORDER BY COALESCE(valid_from, timestamp) ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id, subject, predicate], |r| {
                Ok(TemporalRecord {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    valid_from: r.get(2)?,
                    valid_until: r.get(3)?,
                    superseded_by: r.get(4)?,
                    supersedes: r.get(5)?,
                    confidence: r.get::<_, Option<f64>>(6)?.unwrap_or(1.0),
                })
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(out)
    }

    /// Point-in-time lookup for a triple: the memory valid at instant `at`
    /// (`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`) (F1).
    /// Engine-only for v1.19.0 (not exposed over MCP yet).
    pub async fn get_at(
        &self,
        agent_id: &str,
        subject: &str,
        predicate: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<TemporalRecord>> {
        let conn = self.conn.lock().await;
        let at_str = at.to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT id, content, valid_from, valid_until, superseded_by, supersedes, confidence
                 FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                   AND COALESCE(valid_from, timestamp) <= ?4
                   AND (valid_until IS NULL OR valid_until > ?4)
                 ORDER BY COALESCE(valid_from, timestamp) DESC
                 LIMIT 1",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![agent_id, subject, predicate, at_str], |r| {
                Ok(TemporalRecord {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    valid_from: r.get(2)?,
                    valid_until: r.get(3)?,
                    superseded_by: r.get(4)?,
                    supersedes: r.get(5)?,
                    confidence: r.get::<_, Option<f64>>(6)?.unwrap_or(1.0),
                })
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Count currently-valid memories for an agent filtered by tag substring
    /// (F2b helper). Matches against the JSON-encoded `tags` column.
    pub async fn count_active_with_tag(&self, agent_id: &str, tag: &str) -> Result<u32> {
        let conn = self.conn.lock().await;
        let like = format!("%\"{}\"%", tag.replace('"', ""));
        let n: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE agent_id = ?1 AND valid_until IS NULL AND tags LIKE ?2",
                params![agent_id, like],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n)
    }

    // ── Decision Continuity (RFC-24) ────────────────────────────────────────
    //
    // Decisions ride the temporal/knowledge-graph columns rather than FTS:
    // `search_layer()` strips `:` from queries, so `subject = "decision:<id>"`
    // is unreachable via full-text search. These helpers query the structured
    // columns directly and honour the same currently-valid filter
    // (`valid_until IS NULL`) used everywhere else.

    /// List the agent's currently-open decisions, newest first (RFC-24).
    ///
    /// A decision is "open" when its `(decision:<id>, status)` triple has object
    /// `open` and is still valid. For each open decision the question text and all
    /// still-valid option contents are gathered. `limit` caps the number of
    /// decisions returned (the injection layer keeps this small, e.g. 5).
    pub async fn list_open_decisions(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<DecisionView>> {
        let conn = self.conn.lock().await;

        // Subjects with a currently-valid status=open row, newest first.
        let subjects: Vec<(String, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT subject, valid_from FROM memories
                     WHERE agent_id = ?1 AND predicate = 'status' AND object = 'open'
                       AND valid_until IS NULL AND subject LIKE 'decision:%'
                     ORDER BY COALESCE(valid_from, timestamp) DESC, created_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let rows = stmt
                .query_map(params![agent_id, limit as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
            }
            v
        };

        let mut out = Vec::new();
        for (subject, created_at) in subjects {
            if let Some(view) = Self::read_decision_view(&conn, agent_id, &subject, created_at)? {
                out.push(view);
            }
        }
        Ok(out)
    }

    /// Fetch a single decision by its short id (without the `decision:` prefix),
    /// regardless of open/resolved status, gathering still-valid artifacts (RFC-24).
    pub async fn get_decision(
        &self,
        agent_id: &str,
        decision_id: &str,
    ) -> Result<Option<DecisionView>> {
        let conn = self.conn.lock().await;
        let subject = format!("decision:{decision_id}");
        Self::read_decision_view(&conn, agent_id, &subject, None)
    }

    /// Current valid status object for a decision (`open` / `resolved:<key>` /
    /// `expired`), or `None` if the decision is unknown (RFC-24).
    pub async fn decision_status(
        &self,
        agent_id: &str,
        decision_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let subject = format!("decision:{decision_id}");
        let status: Option<String> = conn
            .query_row(
                "SELECT object FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND predicate = 'status'
                   AND valid_until IS NULL
                 ORDER BY COALESCE(valid_from, timestamp) DESC LIMIT 1",
                params![agent_id, subject],
                |r| r.get(0),
            )
            .ok();
        Ok(status)
    }

    /// Expire all still-valid non-status artifacts (question + options) of a
    /// decision so they stop being "active" once the decision is resolved (RFC-24).
    /// The status supersession chain is left intact for history.
    pub async fn expire_decision_artifacts(
        &self,
        agent_id: &str,
        decision_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let subject = format!("decision:{decision_id}");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE memories SET valid_until = ?1
             WHERE agent_id = ?2 AND subject = ?3 AND predicate != 'status'
               AND valid_until IS NULL",
            params![now, agent_id, subject],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    /// Expire this agent's open decisions whose status row is older than
    /// `ttl_days` (RFC-24 §P3.2). All still-valid rows (status + question +
    /// options) of each stale decision are closed (`valid_until = now`), so the
    /// decision drops out of `list_open_decisions` and stops injecting. Returns
    /// the number of rows expired. A non-positive `ttl_days` is a no-op.
    pub async fn expire_stale_decisions(&self, agent_id: &str, ttl_days: i64) -> Result<usize> {
        if ttl_days <= 0 {
            return Ok(0);
        }
        let conn = self.conn.lock().await;
        let now = Utc::now();
        let cutoff = (now - chrono::Duration::days(ttl_days)).to_rfc3339();
        let now_str = now.to_rfc3339();
        let n = conn
            .execute(
                "UPDATE memories SET valid_until = ?1
                 WHERE agent_id = ?2 AND valid_until IS NULL AND subject LIKE 'decision:%'
                   AND subject IN (
                     SELECT subject FROM memories
                     WHERE agent_id = ?2 AND predicate = 'status' AND object = 'open'
                       AND valid_until IS NULL
                       AND COALESCE(valid_from, timestamp) < ?3
                   )",
                params![now_str, agent_id, cutoff],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(n)
    }

    /// Dismiss a decision as a false positive (RFC-24 §P3.3): close ALL its
    /// still-valid rows (status + question + options) so it disappears from
    /// open-decision queries. Returns `true` if the decision existed (any row
    /// was closed), `false` if nothing matched.
    pub async fn dismiss_decision(&self, agent_id: &str, decision_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let subject = format!("decision:{decision_id}");
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE memories SET valid_until = ?1
                 WHERE agent_id = ?2 AND subject = ?3 AND valid_until IS NULL",
                params![now, agent_id, subject],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(n > 0)
    }

    /// Resolve an open decision to a chosen option (RFC-24 §4.4).
    ///
    /// Fail-closed: an unknown decision id, an already-resolved decision, or an
    /// unknown option key returns the corresponding non-`Resolved` outcome and
    /// writes nothing. On success it: (1) supersedes the `status` row to
    /// `resolved:<key>`; (2) expires the question + option artifacts so they stop
    /// surfacing as open; (3) records the choice as a plain long-lived semantic
    /// fact so future `search()` can recall "this decision chose X".
    ///
    /// Orchestrates the other public helpers (no direct lock held here) to avoid
    /// re-entrant locking of the connection mutex.
    pub async fn resolve_decision(
        &self,
        agent_id: &str,
        decision_id: &str,
        chosen_key: &str,
    ) -> Result<DecisionResolveOutcome> {
        // Status gate.
        match self.decision_status(agent_id, decision_id).await? {
            None => return Ok(DecisionResolveOutcome::NotFound),
            Some(s) if s == "open" => {}
            Some(other) => return Ok(DecisionResolveOutcome::AlreadyResolved(other)),
        }
        let Some(view) = self.get_decision(agent_id, decision_id).await? else {
            return Ok(DecisionResolveOutcome::NotFound);
        };
        let Some((_, chosen_content)) =
            view.options.iter().find(|(k, _)| k == chosen_key).cloned()
        else {
            return Ok(DecisionResolveOutcome::UnknownKey {
                available: view.options.iter().map(|(k, _)| k.clone()).collect(),
            });
        };

        let subject = format!("decision:{decision_id}");

        // 1. status → resolved:<key> (supersedes the open status row).
        self.store_temporal(
            agent_id,
            Self::decision_artifact_entry(agent_id, &format!("resolved:{chosen_key}")),
            TemporalMeta {
                subject: Some(subject.clone()),
                predicate: Some("status".to_string()),
                object: Some(format!("resolved:{chosen_key}")),
                valid_from: None,
                valid_until: None,
                confidence: Some(1.0),
                metadata: None,
            },
        )
        .await?;

        // 2. Expire question + option artifacts (status excluded by predicate).
        self.expire_decision_artifacts(agent_id, decision_id).await?;

        // 3. Record the choice as a plain long-lived semantic fact (searchable,
        //    not a decision artifact — so it never tangles with open-decision
        //    queries and is not expired above).
        let fact = format!(
            "已解決的決策：{} → 選擇 {}：{}",
            view.question, chosen_key, chosen_content
        );
        let mut entry = Self::decision_artifact_entry(agent_id, &fact);
        entry.tags = vec![
            "decision".to_string(),
            "resolved".to_string(),
            subject.clone(),
        ];
        self.store_temporal(agent_id, entry, TemporalMeta::default())
            .await?;

        Ok(DecisionResolveOutcome::Resolved {
            chosen_key: chosen_key.to_string(),
            chosen_content,
            question: view.question,
        })
    }

    /// Build a semantic `MemoryEntry` for a decision artifact / fact row (RFC-24).
    fn decision_artifact_entry(agent_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec!["decision".to_string()],
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Semantic,
            importance: 7.0,
            access_count: 0,
            last_accessed: None,
            source_event: "decision_resolve".to_string(),
        }
    }

    /// Assemble a [`DecisionView`] from the still-valid triples of one subject.
    /// Returns `None` when no question text is present (malformed / partial).
    fn read_decision_view(
        conn: &Connection,
        agent_id: &str,
        subject: &str,
        created_at: Option<String>,
    ) -> Result<Option<DecisionView>> {
        let mut question: Option<String> = None;
        let mut created = created_at;
        let mut options: Vec<(String, String)> = Vec::new();

        let mut stmt = conn
            .prepare(
                "SELECT predicate, object, valid_from FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND valid_until IS NULL
                   AND object IS NOT NULL",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id, subject], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        for r in rows {
            let (predicate, object, vf) = r.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            if predicate == "question" {
                question = Some(object);
                if created.is_none() {
                    created = vf;
                }
            } else if let Some(key) = predicate.strip_prefix("option:") {
                options.push((key.to_string(), object));
            }
        }

        let Some(question) = question else {
            return Ok(None);
        };
        // Stable ordering by option key (A, B, C / 1, 2, 3).
        options.sort_by(|a, b| a.0.cmp(&b.0));
        let id = subject
            .strip_prefix("decision:")
            .unwrap_or(subject)
            .to_string();
        Ok(Some(DecisionView {
            id,
            question,
            options,
            created_at: created,
        }))
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
        // RFC3339 "now" bound as a param: comparing against datetime('now')
        // (space-separated, no offset) would mis-sort vs our RFC3339 strings.
        let now_rfc = Utc::now().to_rfc3339();

        let sql = format!(
            "SELECT {cols}
             FROM memories_fts AS f
             JOIN memories AS m ON m.id = f.memory_id
             WHERE f.memories_fts MATCH ?1
               AND f.agent_id = ?2
               AND m.layer = ?3
               AND (m.valid_until IS NULL OR m.valid_until > ?4)
             ORDER BY rank
             LIMIT ?5",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, agent_id, layer.as_str(), now_rfc, limit as i64], Self::row_to_entry)
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

        // `timestamp` is stored as RFC3339; comparing it against SQLite's
        // `datetime('now', ...)` format (YYYY-MM-DD HH:MM:SS, no 'T'/offset) is an
        // unreliable string compare. Bind an RFC3339 cutoff computed in Rust instead
        // (mirrors the fix applied in `search()`).
        let cutoff_rfc = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let high_episodic: u32 = conn.query_row(
            "SELECT COUNT(*) FROM memories
             WHERE agent_id = ?1 AND layer = 'episodic' AND importance >= 7.0
             AND timestamp >= ?2",
            params![agent_id, cutoff_rfc],
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
            // ── F1 Temporal Memory columns (v1.19.0) ──
            // All nullable / constant-default so ALTER ADD COLUMN is legal on
            // existing rows. NULL valid_from ⇒ fall back to `timestamp`;
            // NULL valid_until ⇒ still valid.
            "ALTER TABLE memories ADD COLUMN valid_from TEXT",
            "ALTER TABLE memories ADD COLUMN valid_until TEXT",
            "ALTER TABLE memories ADD COLUMN superseded_by TEXT",
            "ALTER TABLE memories ADD COLUMN supersedes TEXT",
            "ALTER TABLE memories ADD COLUMN subject TEXT",
            "ALTER TABLE memories ADD COLUMN predicate TEXT",
            "ALTER TABLE memories ADD COLUMN object TEXT",
            "ALTER TABLE memories ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0",
            "ALTER TABLE memories ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}'",
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

        // Temporal indexes — created AFTER the ALTERs so the columns exist on
        // upgraded databases (F1). `idx_memories_triple` only indexes currently
        // valid rows to keep conflict-resolution lookups cheap.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_triple
                 ON memories(agent_id, subject, predicate) WHERE valid_until IS NULL;
             CREATE INDEX IF NOT EXISTS idx_memories_valid
                 ON memories(agent_id, valid_until);",
        )
        .map_err(|e| DuDuClawError::Memory(format!("temporal index: {e}")))?;

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
        // RFC3339 "now" bound as a param (see search_layer for rationale): default
        // search returns only currently-valid memories (F1 temporal filter).
        let now_rfc = Utc::now().to_rfc3339();
        let sql = format!(
            "SELECT {cols}
             FROM memories_fts AS f
             JOIN memories AS m ON m.id = f.memory_id
             WHERE f.memories_fts MATCH ?1
               AND f.agent_id = ?2
               AND (m.valid_until IS NULL OR m.valid_until > ?3)
             ORDER BY rank
             LIMIT ?4",
            cols = Self::SELECT_COLS
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![sanitized_query, agent_id, now_rfc, fetch_limit as i64], Self::row_to_entry)
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

        // ── HippoRAG-lite graph ranking (arXiv:2405.14831, zero LLM cost) ──
        // Fail-safe: zero triples (cheap COUNT gate), a query seeding no graph
        // entity, or any error all yield `None`, leaving the FTS-only result
        // byte-identical to before.
        let graph_ranked = match crate::graph_rank::graph_rank_scores(
            &conn, agent_id, query, &now_rfc,
        ) {
            Ok(ranked) => ranked,
            Err(e) => {
                tracing::warn!("graph ranking skipped: {e}");
                None
            }
        };

        // Post-retrieval re-ranking by recency + importance + FTS position.
        // Generative Agents (arXiv 2304.03442) three-dimensional weighting.
        let now = Utc::now();
        let w = &self.retrieval_weights;
        let mut scored: Vec<(f64, MemoryEntry)> = candidates
            .into_iter()
            .enumerate()
            .map(|(rank_pos, entry)| {
                let fts_rank = 1.0 / (1.0 + rank_pos as f64);
                let score = Self::base_relevance_score(&entry, now, w) + w.w_fts * fts_rank;
                (score, entry)
            })
            .collect();

        // Blend graph mass into FTS candidates and append graph-only hits.
        if let Some(ranked) = &graph_ranked {
            let ppr: std::collections::HashMap<&str, f64> =
                ranked.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let fts_ids: std::collections::HashSet<String> =
                scored.iter().map(|(_, e)| e.id.clone()).collect();

            for (score, entry) in scored.iter_mut() {
                if let Some(g) = ppr.get(entry.id.as_str()) {
                    *score += w.w_graph * g;
                }
            }

            // Up to MAX_GRAPH_APPENDS memories FTS missed but PPR ranked in
            // its top TOP_GRAPH_CANDIDATES — fetched with the same agent
            // isolation + temporal validity filters as the FTS query.
            let appends = ranked
                .iter()
                .take(crate::graph_rank::TOP_GRAPH_CANDIDATES)
                .filter(|(id, _)| !fts_ids.contains(id))
                .take(crate::graph_rank::MAX_GRAPH_APPENDS);
            for (id, g) in appends {
                if let Some(entry) = Self::fetch_valid_entry(&conn, agent_id, id, &now_rfc)? {
                    let score = Self::base_relevance_score(&entry, now, w) + w.w_graph * g;
                    scored.push((score, entry));
                }
            }
        }

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
        // Sanitize FTS5 query the same way `search()`/`search_layer()` do: strip ALL
        // special characters and operators, then wrap as a phrase query (HC8). Without
        // this, an unescaped colon — e.g. the `[sender_id: {id}]` prefix injected on
        // authenticated turns — is parsed as an FTS5 column operator and MATCH fails.
        // Done before locking `conn` so the empty-query fallback can re-lock safely.
        let cleaned: String = query
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ':' | '^' | '{' | '}' | '*' | '(' | ')'))
            .take(500)
            .collect();
        if cleaned.trim().is_empty() {
            // Nothing searchable — fall back to recency.
            return self.get_recent_facts(agent_id, limit).await;
        }
        let sanitized_query = format!("\"{}\"", cleaned.replace('"', ""));

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
            .query_map(params![agent_id, sanitized_query, limit], |row| {
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

        // Fallback: if FTS5 returns nothing (query too short, no match), use recency.
        // Release the connection lock first — `get_recent_facts` re-acquires it, so
        // holding it here would deadlock.
        if facts.is_empty() {
            drop(stmt);
            drop(conn);
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

    #[test]
    fn retrievability_reinforcement_slows_forgetting() {
        let w = RetrievalWeights::default();
        // Same age + importance: more recalls → higher retrievability.
        let unreinforced = ebbinghaus_retrievability(30.0, 0, 5.0, &w);
        let reinforced = ebbinghaus_retrievability(30.0, 20, 5.0, &w);
        assert!(reinforced > unreinforced);

        // Higher importance → slower forgetting.
        let low_imp = ebbinghaus_retrievability(30.0, 0, 2.0, &w);
        let high_imp = ebbinghaus_retrievability(30.0, 0, 8.0, &w);
        assert!(high_imp > low_imp);

        // Fresh memory is fully retrievable; bounds hold everywhere.
        assert!((ebbinghaus_retrievability(0.0, 0, 5.0, &w) - 1.0).abs() < 1e-9);
        let r = ebbinghaus_retrievability(10_000.0, 0, 0.0, &w);
        assert!((0.0..=1.0).contains(&r));
    }

    #[tokio::test]
    async fn search_ranks_reinforced_memory_higher() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "rank-agent";
        let old = Utc::now() - Duration::days(40);

        // Two equally old, equally important matches — one recalled often
        // and recently, one never recalled. The reinforced one must rank first.
        let mut stale = make_entry(agent, "project deadline is friday", vec![]);
        stale.timestamp = old;
        let mut reinforced = make_entry(agent, "project deadline moved to monday", vec![]);
        reinforced.timestamp = old;
        reinforced.access_count = 25;
        reinforced.last_accessed = Some(Utc::now() - Duration::hours(2));

        engine.store(agent, stale).await.unwrap();
        engine.store(agent, reinforced).await.unwrap();

        let results = engine.search(agent, "deadline", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].content.contains("monday"));
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

    // ── get_by_ids (F3 batch fetch) tests ──────────────────────────────────────

    /// Batch fetch returns all requested entries; order-independent partial hit.
    #[tokio::test]
    async fn get_by_ids_partial_hit() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "batch-agent";
        let e1 = make_entry(agent, "first", vec![]);
        let e2 = make_entry(agent, "second", vec![]);
        let id1 = e1.id.clone();
        let id2 = e2.id.clone();
        engine.store(agent, e1).await.unwrap();
        engine.store(agent, e2).await.unwrap();

        let ids = vec![id1.clone(), "missing-id".to_string(), id2.clone()];
        let found = engine.get_by_ids(agent, &ids).await.unwrap();
        assert_eq!(found.len(), 2, "two of three ids should be found");
        let found_ids: std::collections::HashSet<_> = found.iter().map(|e| e.id.clone()).collect();
        assert!(found_ids.contains(&id1) && found_ids.contains(&id2));
    }

    /// Empty input returns empty result (not an error).
    #[tokio::test]
    async fn get_by_ids_empty_returns_empty() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let found = engine.get_by_ids("agent", &[]).await.unwrap();
        assert!(found.is_empty());
    }

    /// All-missing returns empty (not an error).
    #[tokio::test]
    async fn get_by_ids_all_missing() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let ids = vec!["nope-1".to_string(), "nope-2".to_string()];
        let found = engine.get_by_ids("agent", &ids).await.unwrap();
        assert!(found.is_empty());
    }

    /// Cross-agent isolation: agent-b cannot batch-fetch agent-a's entries.
    #[tokio::test]
    async fn get_by_ids_cross_agent_isolation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let e = make_entry("agent-a", "secret", vec![]);
        let id = e.id.clone();
        engine.store("agent-a", e).await.unwrap();

        let found = engine.get_by_ids("agent-b", &[id]).await.unwrap();
        assert!(found.is_empty(), "cross-agent batch fetch must return nothing");
    }

    /// Exceeding 100 ids returns an error.
    #[tokio::test]
    async fn get_by_ids_over_limit_errors() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let ids: Vec<String> = (0..101).map(|i| format!("id-{i}")).collect();
        let result = engine.get_by_ids("agent", &ids).await;
        assert!(result.is_err(), "more than 100 ids must error");
    }

    // ── F1 Temporal Memory tests ───────────────────────────────────────────────

    fn triple_meta(subject: &str, predicate: &str, object: &str) -> TemporalMeta {
        TemporalMeta {
            subject: Some(subject.to_string()),
            predicate: Some(predicate.to_string()),
            object: Some(object.to_string()),
            ..Default::default()
        }
    }

    /// Writing the same (subject, predicate) twice supersedes the old row:
    /// old gets valid_until + superseded_by; new gets supersedes back-pointer.
    #[tokio::test]
    async fn store_temporal_supersedes_same_triple() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "temporal-agent";

        let e1 = make_entry(agent, "user prefers python", vec![]);
        let id1 = engine
            .store_temporal(agent, e1, triple_meta("user:main", "prefers_language", "python"))
            .await
            .unwrap();

        let e2 = make_entry(agent, "user prefers typescript", vec![]);
        let id2 = engine
            .store_temporal(agent, e2, triple_meta("user:main", "prefers_language", "typescript"))
            .await
            .unwrap();

        let history = engine
            .get_history(agent, "user:main", "prefers_language")
            .await
            .unwrap();
        assert_eq!(history.len(), 2, "both rows present in history");
        // Oldest first.
        assert_eq!(history[0].id, id1);
        assert_eq!(history[1].id, id2);
        // Old row closed out, pointing at the new one.
        assert!(history[0].valid_until.is_some(), "old row must expire");
        assert_eq!(history[0].superseded_by.as_deref(), Some(id2.as_str()));
        // New row still valid, back-pointer set.
        assert!(history[1].valid_until.is_none(), "new row must be valid");
        assert_eq!(history[1].supersedes.as_deref(), Some(id1.as_str()));
    }

    /// Default search returns only currently-valid memories (superseded excluded).
    #[tokio::test]
    async fn search_excludes_superseded() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "temporal-search";

        let e1 = make_entry(agent, "alpha keyword python rust", vec![]);
        engine
            .store_temporal(agent, e1, triple_meta("s", "p", "old"))
            .await
            .unwrap();
        let e2 = make_entry(agent, "alpha keyword python rust", vec![]);
        let id2 = engine
            .store_temporal(agent, e2, triple_meta("s", "p", "new"))
            .await
            .unwrap();

        let results = engine.search(agent, "alpha", 10).await.unwrap();
        assert_eq!(results.len(), 1, "only the active row is returned");
        assert_eq!(results[0].id, id2);
    }

    /// Explicitly-expired memory (valid_until in the past) is filtered out.
    #[tokio::test]
    async fn search_excludes_expired() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "expiry-agent";
        let e = make_entry(agent, "ephemeral zzzkeyword", vec![]);
        let meta = TemporalMeta {
            valid_until: Some(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        engine.store_temporal(agent, e, meta).await.unwrap();

        let results = engine.search(agent, "zzzkeyword", 10).await.unwrap();
        assert!(results.is_empty(), "expired memory must not be returned");
    }

    /// Point-in-time lookup returns the row valid at that instant.
    #[tokio::test]
    async fn get_at_returns_point_in_time() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "pit-agent";

        let t0 = Utc::now() - Duration::days(10);
        let e1 = make_entry(agent, "python era", vec![]);
        let m1 = TemporalMeta {
            valid_from: Some(t0),
            ..triple_meta("user", "lang", "python")
        };
        engine.store_temporal(agent, e1, m1).await.unwrap();

        // Switch happens "now"; supersession closes the python row at now.
        let e2 = make_entry(agent, "typescript era", vec![]);
        engine
            .store_temporal(agent, e2, triple_meta("user", "lang", "typescript"))
            .await
            .unwrap();

        // 5 days ago → python was valid.
        let mid = Utc::now() - Duration::days(5);
        let at = engine.get_at(agent, "user", "lang", mid).await.unwrap();
        assert!(at.is_some());
        assert_eq!(at.unwrap().content, "python era");
    }

    /// store_temporal without a triple behaves like a plain insert (still searchable).
    #[tokio::test]
    async fn store_temporal_without_triple_is_plain_insert() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "plain-agent";
        let e = make_entry(agent, "plain temporal content findme", vec![]);
        engine
            .store_temporal(agent, e, TemporalMeta::default())
            .await
            .unwrap();
        let results = engine.search(agent, "findme", 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    /// Plain `store()` (no temporal columns) coexists and is searchable — the
    /// temporal filter treats NULL valid_until as valid (backward compatible).
    #[tokio::test]
    async fn plain_store_still_searchable_after_temporal_migration() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "compat-agent";
        engine
            .store(agent, make_entry(agent, "legacy row keyword qwerty", vec![]))
            .await
            .unwrap();
        let results = engine.search(agent, "qwerty", 10).await.unwrap();
        assert_eq!(results.len(), 1, "legacy NULL-valid_until rows remain visible");
    }

    /// count_active_with_tag counts only valid rows whose tags contain the tag.
    #[tokio::test]
    async fn count_active_with_tag_counts_valid_only() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "tag-count";
        for _ in 0..3 {
            engine
                .store(agent, make_entry(agent, "x", vec!["reflexion".to_string()]))
                .await
                .unwrap();
        }
        engine
            .store(agent, make_entry(agent, "y", vec!["other".to_string()]))
            .await
            .unwrap();
        let n = engine.count_active_with_tag(agent, "reflexion").await.unwrap();
        assert_eq!(n, 3);
    }

    // ── HippoRAG-lite graph ranking tests (arXiv:2405.14831) ────────────────────

    /// Two-hop retrieval: FTS on "alice" only matches the alice→bob memory,
    /// but the bob→project-x memory must surface via the graph walk.
    #[tokio::test]
    async fn graph_two_hop_retrieval_surfaces_indirect_memory() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "graph-agent";

        engine
            .store_temporal(
                agent,
                make_entry(agent, "alice reports to bob", vec![]),
                triple_meta("alice", "reports_to", "bob"),
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                agent,
                make_entry(agent, "bob leads the big initiative", vec![]),
                triple_meta("bob", "leads", "project-x"),
            )
            .await
            .unwrap();

        let results = engine.search(agent, "alice", 10).await.unwrap();
        assert!(
            results.iter().any(|e| e.content.contains("big initiative")),
            "two-hop memory must surface via graph even though FTS on 'alice' misses it"
        );
        assert!(
            results.iter().any(|e| e.content.contains("reports to bob")),
            "the direct FTS hit must still be present"
        );
    }

    /// Superseded facts must be excluded from the graph: only the currently
    /// valid replacement may surface, never the superseded original.
    #[tokio::test]
    async fn graph_excludes_superseded_facts() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "graph-supersede";

        engine
            .store_temporal(
                agent,
                make_entry(agent, "alice reports to bob", vec![]),
                triple_meta("alice", "reports_to", "bob"),
            )
            .await
            .unwrap();
        // Old fact, later superseded — its content never FTS-matches "alice",
        // so it could only leak through the graph.
        engine
            .store_temporal(
                agent,
                make_entry(agent, "bob leads codename zeta", vec![]),
                triple_meta("bob", "leads", "project-x"),
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                agent,
                make_entry(agent, "bob leads codename omega", vec![]),
                triple_meta("bob", "leads", "project-y"),
            )
            .await
            .unwrap();

        let results = engine.search(agent, "alice", 10).await.unwrap();
        assert!(
            !results.iter().any(|e| e.content.contains("zeta")),
            "superseded fact must not enter the graph"
        );
        assert!(
            results.iter().any(|e| e.content.contains("omega")),
            "the currently-valid replacement should surface via the graph"
        );
    }

    /// Agent isolation: agent A's triples never leak into agent B's search,
    /// even when agent B has its own (unrelated) triples so the COUNT gate
    /// does not short-circuit.
    #[tokio::test]
    async fn graph_agent_isolation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();

        engine
            .store_temporal(
                "agent-a",
                make_entry("agent-a", "alice reports to bob", vec![]),
                triple_meta("alice", "reports_to", "bob"),
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                "agent-a",
                make_entry("agent-a", "bob leads project-x", vec![]),
                triple_meta("bob", "leads", "project-x"),
            )
            .await
            .unwrap();

        engine
            .store_temporal(
                "agent-b",
                make_entry("agent-b", "carol likes tea", vec![]),
                triple_meta("carol", "likes", "tea"),
            )
            .await
            .unwrap();
        engine
            .store("agent-b", make_entry("agent-b", "alice said hello", vec![]))
            .await
            .unwrap();

        let results = engine.search("agent-b", "alice", 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|e| e.agent_id == "agent-b"),
            "only agent-b memories may be returned"
        );
        assert!(
            !results.iter().any(|e| e.content.contains("project-x")),
            "agent-a's graph must never leak into agent-b's search"
        );
    }

    /// A query that seeds no graph entity must return results identical to an
    /// engine that has no triples at all (fail-safe FTS-only behavior).
    #[tokio::test]
    async fn graph_no_seed_query_identical_to_fts_only() {
        let plain_engine = SqliteMemoryEngine::in_memory().unwrap();
        let graph_engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "graph-noseed";

        // Same plain memories (same ids/timestamps) in both engines.
        let base = Utc::now() - Duration::days(3);
        for (i, content) in [
            "zebra habitat in the savanna",
            "zebra stripes are unique",
            "lions hunt zebra at dawn",
        ]
        .iter()
        .enumerate()
        {
            let mut e = make_entry(agent, content, vec![]);
            e.id = format!("shared-{i}");
            e.timestamp = base + Duration::hours(i as i64);
            plain_engine.store(agent, e.clone()).await.unwrap();
            graph_engine.store(agent, e).await.unwrap();
        }
        // Extra triples in the graph engine whose entities never appear in
        // the query and whose content never FTS-matches it.
        graph_engine
            .store_temporal(
                agent,
                make_entry(agent, "alice reports to bob", vec![]),
                triple_meta("alice", "reports_to", "bob"),
            )
            .await
            .unwrap();

        let expected = plain_engine.search(agent, "zebra habitat", 10).await.unwrap();
        let actual = graph_engine.search(agent, "zebra habitat", 10).await.unwrap();
        let expected_ids: Vec<&str> = expected.iter().map(|e| e.id.as_str()).collect();
        let actual_ids: Vec<&str> = actual.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            actual_ids, expected_ids,
            "no-seed query must be identical to FTS-only ranking"
        );
    }

    // ── HC8: search_facts FTS5 sanitization ─────────────────────────────────────

    /// A query containing a colon (FTS5 column operator) — e.g. the `[sender_id: x]`
    /// prefix injected on authenticated turns — must not break MATCH; sanitization
    /// strips it so the remaining terms still match.
    #[tokio::test]
    async fn search_facts_sanitizes_colon_prefix() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "facts-agent";
        engine
            .store_fact(agent, "the deploy password is hunter2", "tg", "c1", "s1")
            .await
            .unwrap();

        // Without sanitization the colon is parsed as a column filter and MATCH errors
        // (which the old code surfaced as an error / empty result).
        let results = engine
            .search_facts(agent, "[sender_id: 12345] deploy password", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1, "colon-prefixed query should still match");
        assert!(results[0].fact.contains("deploy password"));
    }

    /// Other FTS5 special characters must not break MATCH either.
    #[tokio::test]
    async fn search_facts_sanitizes_special_chars() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "facts-agent-2";
        engine
            .store_fact(agent, "favorite editor is neovim", "tg", "c1", "s1")
            .await
            .unwrap();

        let results = engine
            .search_facts(agent, "(favorite* editor) OR \"neovim\"", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1, "special chars should be stripped, term matched");
    }

    // ── M38: semantic_conflict_count timestamp normalization ────────────────────

    /// High-importance episodic memories stored with an RFC3339 timestamp must be
    /// counted as recent (the comparison cutoff is now an RFC3339 param, not the
    /// mismatched SQLite `datetime('now')` string format).
    #[tokio::test]
    async fn semantic_conflict_counts_recent_rfc3339_episodic() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "conflict-agent";
        for i in 0..4 {
            let mut e = make_entry(agent, &format!("important episodic memory {i}"), vec![]);
            e.importance = 8.0; // >= 7.0 threshold
            engine.store(agent, e).await.unwrap();
        }
        // No semantic memories, 4 high-importance episodic ⇒ count == high_episodic.
        let n = engine.semantic_conflict_count(agent).await;
        assert_eq!(n, 4, "recent RFC3339 episodic memories must be counted");
    }

    // ── M57: store_temporal deterministic supersedes back-pointer ───────────────

    /// When multiple active rows share the same (subject, predicate), the new row's
    /// `supersedes` back-pointer must deterministically point at the most recent of
    /// them (ordered by valid_from/created_at DESC, then id).
    #[tokio::test]
    async fn store_temporal_supersedes_picks_most_recent_deterministically() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "det-agent";

        // Insert two active rows for the same triple directly so both have
        // valid_until = NULL (simulating a pre-existing ambiguous state).
        {
            let conn = engine.conn.lock().await;
            let older = Utc::now() - Duration::hours(2);
            let newer = Utc::now() - Duration::hours(1);
            for (id, vf) in [("old-id", older), ("new-id", newer)] {
                conn.execute(
                    "INSERT INTO memories
                        (id, agent_id, content, timestamp, tags, layer, importance,
                         access_count, source_event, valid_from, subject, predicate, object)
                     VALUES (?1,?2,?3,?4,'[]','semantic',5.0,0,'',?5,?6,?7,?8)",
                    params![id, agent, "x", vf.to_rfc3339(), vf.to_rfc3339(), "s", "p", "o"],
                )
                .unwrap();
            }
        }

        let e = make_entry(agent, "superseding value", vec![]);
        let new_id = engine
            .store_temporal(agent, e, triple_meta("s", "p", "z"))
            .await
            .unwrap();

        let row = engine.get_by_id(agent, &new_id).await.unwrap().unwrap();
        // Most recent active row was "new-id" (valid_from -1h > -2h).
        let history = engine.get_history(agent, "s", "p").await.unwrap();
        let new_row = history.iter().find(|r| r.id == new_id).unwrap();
        assert_eq!(
            new_row.supersedes.as_deref(),
            Some("new-id"),
            "back-pointer must deterministically target the most recent active row"
        );
        // Sanity: the stored row exists.
        assert_eq!(row.id, new_id);
    }
}
