use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock as StdRwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
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
    /// Weight for FTS relevance dimension (default 0.35 — lowered from 0.40
    /// when `w_trust` was introduced so total weight "feel" stays constant: the
    /// 0.05 given up here funds the new `w_trust` trust dimension, D2).
    pub w_fts: f64,
    /// Weight for the HippoRAG-lite graph dimension (normalized Personalized
    /// PageRank mass over the SPO triple graph, arXiv:2405.14831; default 0.15).
    /// Only applied when the query seeds at least one graph entity.
    pub w_graph: f64,
    /// Weight for the semantic vector dimension (cosine similarity of the
    /// query embedding to each candidate's stored embedding; default 0.15).
    /// Only applied when an embedder is attached AND matching embedded rows
    /// exist — otherwise this dimension contributes nothing and ranking is
    /// byte-identical to the FTS/graph path.
    pub w_vec: f64,
    /// Weight for the origin-trust dimension (D2, PoisonedRAG 2402.07867).
    /// Each candidate's final score is multiplied by
    /// `(1 - w_trust) + w_trust * origin_trust`, so a fully-trusted fact
    /// (`origin_trust = 1.0`, the default for existing rows) is unchanged while
    /// low-trust channel-distilled facts are gently pushed down. Default 0.10.
    /// At `w_trust = 0.0` ranking is byte-identical to the pre-D2 path.
    pub w_trust: f64,
    /// D3.4 — enable embedding-based graph seeding: when `true` AND an embedder
    /// is attached, PPR seeds are the union of whole-word FTS entity matches and
    /// the query embedding's nearest entity vectors. Default `false` — with it
    /// off (or no embedder) seeding is byte-identical to the whole-word path.
    pub graph_embed_seed: bool,
    /// D3.4 — number of embedding-nearest entities to add as seeds when
    /// `graph_embed_seed` is on. Default 5.
    pub graph_embed_seed_top_k: usize,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            base_stability_days: 14.0,
            reinforce_k: 0.6,
            max_stability_days: 365.0,
            w_recency: 0.25,
            w_importance: 0.35,
            w_fts: 0.35,
            w_graph: 0.15,
            w_vec: 0.15,
            w_trust: 0.10,
            graph_embed_seed: false,
            graph_embed_seed_top_k: 5,
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
    /// P2-1 provenance: where this fact originated ("channel"/"user"/"distill"…).
    pub origin: Option<String>,
    /// P2-1: 0.0–1.0 trust of the origin. Defaults to 1.0 (self/authoritative)
    /// when `None`. Clamped to `[0,1]` and to ≤ min(source trusts) on store.
    pub origin_trust: Option<f64>,
    /// P2-1: source memory ids this fact was derived from. When set, the stored
    /// `origin_trust` is clamped to ≤ the minimum trust across these sources
    /// (I8: trust is non-malleable — re-derivation/summarization can never
    /// launder a low-trust fact into a higher-trust one).
    pub derived_from: Option<Vec<String>>,
    /// D1 build-time provenance: the `source_event` id that produced this write.
    /// On supersession it is stamped onto the superseded row's
    /// `invalidated_by_event`, and on reaffirm it is appended to the surviving
    /// row's `reaffirmed_by` list. Defaults to `entry.source_event` when `None`.
    pub source_event: Option<String>,
    /// D2 write-side poison quarantine: when `true` the fact is inserted with
    /// `quarantined = 1` — excluded from every retrieval/ranking read path AND
    /// held inert (it never supersedes or reaffirms an existing fact) until a
    /// human releases it via the ApprovalBroker. Default `false`.
    pub quarantined: bool,
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
    /// D1 transaction-time axis: when the system learned this fact. Reads fall
    /// back to `created_at` / `timestamp` for rows written before v1.38.
    #[serde(default)]
    pub ingested_at: Option<String>,
    /// D1 build-time provenance: the `source_event` that closed this row out
    /// (supersession) or `"origin_purge"` (rollback). `None` while still valid.
    #[serde(default)]
    pub invalidated_by_event: Option<String>,
    /// D1: when the row was invalidated. `None` while still valid.
    #[serde(default)]
    pub invalidated_at: Option<String>,
    /// D1: `source_event` ids that reaffirmed this fact (same triple + object +
    /// content re-observed instead of superseded). Capped at 20 most-recent.
    #[serde(default)]
    pub reaffirmed_by: Vec<String>,
}

/// SQLite-backed memory engine with FTS5 full-text search.
///
/// Note: `list_recent()` is an inherent method (not on the `MemoryEngine` trait)
/// that returns entries ordered by recency without requiring an FTS query.
/// D3.1 — minimum currently-valid triple count before the persistent graph
/// cache kicks in. Below this the graph is cheap to rebuild per query, so the
/// cache is skipped and behaviour is identical to the pre-cache path.
pub const GRAPH_CACHE_MIN_TRIPLES: usize = 500;

/// A per-agent cached SPO graph plus the generation it was built at (D3.1).
/// Any triple-mutating write bumps the agent's generation; a query whose stored
/// generation no longer matches rebuilds. `TripleGraph`'s ranking methods are
/// `&self`-only, so the cached graph is shared read-only across queries.
struct CachedGraph {
    graph: crate::graph_rank::TripleGraph,
    generation: u64,
}

/// One entity node in a [`GraphExport`] (D3.3): a canonical entity string and
/// its degree (number of incident valid edges). For the D6 curation UI.
#[derive(Debug, Clone, Serialize)]
pub struct GraphExportNode {
    pub entity: String,
    pub degree: usize,
}

/// One labelled edge in a [`GraphExport`] (D3.3): the SPO triple plus its
/// provenance (`origin_trust`) and whether it is currently quarantined.
#[derive(Debug, Clone, Serialize)]
pub struct GraphExportEdge {
    pub subject: String,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub memory_id: String,
    pub origin_trust: f64,
    pub quarantined: bool,
}

/// Serializable snapshot of an agent's SPO knowledge graph for the D6 curation
/// UI (D3.3). Includes quarantined-but-pending edges (flagged) so a reviewer can
/// see what is held for approval. `truncated` is `true` when the edge set hit
/// `limit` and was cut (newest-first by `created_at`).
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphExport {
    pub nodes: Vec<GraphExportNode>,
    pub edges: Vec<GraphExportEdge>,
    #[serde(default)]
    pub truncated: bool,
}

pub struct SqliteMemoryEngine {
    conn: Mutex<Connection>,
    /// Configurable retrieval weights for search re-ranking.
    pub retrieval_weights: RetrievalWeights,
    /// D3.1 per-agent graph generation counter. Bumped by every triple-mutating
    /// write path; compared against a cached graph's stored generation to detect
    /// staleness. Absent agent ⇒ generation 0.
    graph_generations: StdRwLock<HashMap<String, u64>>,
    /// D3.1 per-agent persistent SPO graph cache (only populated for agents
    /// above [`GRAPH_CACHE_MIN_TRIPLES`]).
    graph_cache: StdRwLock<HashMap<String, CachedGraph>>,
    /// Optional semantic embedder. When `None` (default) the vector signal
    /// (`w_vec`) is skipped entirely and ranking is byte-identical to the
    /// FTS/graph-only path. When set, each stored memory is embedded lazily on
    /// write and `search()` blends a cosine similarity signal + appends
    /// vector-only recall hits.
    embedder: Option<std::sync::Arc<dyn crate::vector::EmbeddingProvider>>,
    /// M1 moat-gate: Cloud paid-tier memory storage quota in bytes.
    ///
    /// `0` means **unlimited** — the default for every fresh engine, so
    /// opensource / self-host deployments are entirely unaffected (the
    /// enforcement path early-returns before touching the database). Only when a
    /// caller (the gateway, resolving the active license tier) opts in via
    /// [`set_memory_quota_gb`](Self::set_memory_quota_gb) does the write path
    /// begin comparing DB size against this cap. Kept as a plain byte budget so
    /// the crate never depends on `duduclaw-license`: the quota is a parameter,
    /// not a license lookup.
    memory_quota_bytes: u64,
}

/// Bytes per gigabyte (binary GiB, matching SQLite page-size arithmetic).
const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

/// Pure quota predicate: is `usage_bytes` at or over the `quota_bytes` cap?
///
/// `quota_bytes == 0` is the **unlimited** sentinel and always returns `false`
/// (never blocks) — this is the load-bearing non-breaking guarantee for
/// opensource / self-host tiers. Otherwise the check is `usage >= quota`
/// (fail-closed at the boundary).
pub fn quota_exceeded_bytes(usage_bytes: u64, quota_bytes: u64) -> bool {
    quota_bytes != 0 && usage_bytes >= quota_bytes
}

/// A currently-active row for a `(subject, predicate)` triple, loaded during
/// D1 conflict resolution in [`SqliteMemoryEngine::store_temporal`].
struct ActiveTriple {
    id: String,
    object: Option<String>,
    content: String,
    metadata: String,
    #[allow(dead_code)]
    access_count: i64,
    /// `COALESCE(valid_from, timestamp)` as stored (rfc3339 for temporal writes).
    valid_from: Option<String>,
}

/// Parse an rfc3339 timestamp into a UTC `DateTime`, ignoring malformed input.
fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Return the earlier of two rfc3339 timestamps by parsed instant (D1 historical
/// segment bounding). Falls back to lexical order for unparseable input.
fn min_rfc3339(a: String, b: String) -> String {
    match (parse_rfc3339(&a), parse_rfc3339(&b)) {
        (Some(da), Some(db)) => {
            if da <= db {
                a
            } else {
                b
            }
        }
        _ => {
            if a <= b {
                a
            } else {
                b
            }
        }
    }
}

/// Trimmed `Option<String>` equality used for D1 reaffirm object matching.
/// `None == None`; a present value never equals an absent one.
fn object_opt_eq(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x.trim() == y.trim(),
        (None, None) => true,
        _ => false,
    }
}

/// Append a `source_event` to the `reaffirmed_by` array inside a metadata JSON
/// blob (D1 reaffirm), keeping at most the 20 most-recent unique ids. Empty
/// event ids and duplicates are no-ops. Malformed metadata is reset to `{}`.
fn append_reaffirmed_by(metadata_json: &str, source_event: &str) -> String {
    let mut v: serde_json::Value =
        serde_json::from_str(metadata_json).unwrap_or_else(|_| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    // Safe: v is guaranteed to be an object here.
    let obj = v.as_object_mut().expect("metadata is an object");
    let arr = obj
        .entry("reaffirmed_by")
        .or_insert_with(|| serde_json::json!([]));
    if !arr.is_array() {
        *arr = serde_json::json!([]);
    }
    let list = arr.as_array_mut().expect("reaffirmed_by is an array");
    if !source_event.is_empty() && !list.iter().any(|x| x.as_str() == Some(source_event)) {
        list.push(serde_json::Value::String(source_event.to_string()));
        let len = list.len();
        if len > 20 {
            list.drain(0..len - 20);
        }
    }
    v.to_string()
}

/// Extract the `reaffirmed_by` string array from a metadata JSON blob (D1).
/// Returns an empty vec for absent / malformed data.
fn reaffirmed_by_from_metadata(metadata_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(metadata_json)
        .ok()
        .and_then(|v| {
            v.get("reaffirmed_by")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// Map a `get_history` / `get_at` row into a [`TemporalRecord`] (D1). Column
/// order: id, content, valid_from, valid_until, superseded_by, supersedes,
/// confidence, ingested_at (COALESCE'd), invalidated_by_event, invalidated_at,
/// metadata (source of `reaffirmed_by`).
fn map_temporal_record(r: &rusqlite::Row) -> rusqlite::Result<TemporalRecord> {
    let metadata: String = r.get::<_, Option<String>>(10)?.unwrap_or_else(|| "{}".to_string());
    Ok(TemporalRecord {
        id: r.get(0)?,
        content: r.get(1)?,
        valid_from: r.get(2)?,
        valid_until: r.get(3)?,
        superseded_by: r.get(4)?,
        supersedes: r.get(5)?,
        confidence: r.get::<_, Option<f64>>(6)?.unwrap_or(1.0),
        ingested_at: r.get(7)?,
        invalidated_by_event: r.get(8)?,
        invalidated_at: r.get(9)?,
        reaffirmed_by: reaffirmed_by_from_metadata(&metadata),
    })
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
            graph_generations: StdRwLock::new(HashMap::new()),
            graph_cache: StdRwLock::new(HashMap::new()),
            embedder: None,
            memory_quota_bytes: 0,
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
            graph_generations: StdRwLock::new(HashMap::new()),
            graph_cache: StdRwLock::new(HashMap::new()),
            embedder: None,
            memory_quota_bytes: 0,
        })
    }

    /// Set the memory storage quota in gigabytes (`0` = unlimited). Builder form.
    ///
    /// The gateway resolves the active license tier's
    /// `effective_memory_quota_gb` and passes it here; the crate itself never
    /// consults a license.
    pub fn with_memory_quota_gb(mut self, gb: usize) -> Self {
        self.set_memory_quota_gb(gb);
        self
    }

    /// Set the memory storage quota in gigabytes (`0` = unlimited). Mutating form
    /// for the common case where the engine is constructed then configured.
    pub fn set_memory_quota_gb(&mut self, gb: usize) {
        self.memory_quota_bytes = (gb as u64).saturating_mul(BYTES_PER_GB);
    }

    /// Set the memory storage quota in raw bytes (`0` = unlimited). Exposes
    /// sub-GB granularity for tests and precise-budget callers.
    pub fn set_memory_quota_bytes(&mut self, bytes: u64) {
        self.memory_quota_bytes = bytes;
    }

    /// Current on-disk (or in-memory) database size in bytes, used as the quota
    /// usage estimate. `page_count * page_size` covers every agent sharing this
    /// DB file — the cheapest signal already maintained by SQLite, and it works
    /// identically for `:memory:` databases (so the enforcement is testable).
    pub async fn db_usage_bytes(&self) -> u64 {
        let conn = self.conn.lock().await;
        Self::db_size_bytes(&conn)
    }

    /// Synchronous DB-size estimate against an already-held connection.
    fn db_size_bytes(conn: &Connection) -> u64 {
        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap_or(0);
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap_or(0);
        (page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64)
    }

    /// Fail-closed-but-graceful quota gate for the write path. Called with the
    /// connection already locked (so it shares the store's single lock and never
    /// deadlocks).
    ///
    /// - Quota `0` (unlimited): returns immediately **before** any DB query —
    ///   zero overhead and zero behaviour change for free / self-host tiers.
    /// - Over quota: returns a clear `Err` (no INSERT runs → no data loss, no
    ///   panic). The rejected entry is simply not written.
    fn enforce_quota(&self, conn: &Connection) -> Result<()> {
        if self.memory_quota_bytes == 0 {
            return Ok(());
        }
        let usage = Self::db_size_bytes(conn);
        if quota_exceeded_bytes(usage, self.memory_quota_bytes) {
            warn!(
                usage_bytes = usage,
                quota_bytes = self.memory_quota_bytes,
                "memory quota exceeded — rejecting write (no data lost)"
            );
            return Err(DuDuClawError::Memory(format!(
                "memory quota exceeded: {usage} bytes used ≥ {} byte cap ({} GB tier limit); \
                 entry rejected, existing data preserved",
                self.memory_quota_bytes,
                self.memory_quota_bytes / BYTES_PER_GB
            )));
        }
        Ok(())
    }

    /// Attach a semantic embedder, enabling the `w_vec` retrieval signal.
    ///
    /// Builder-style; `None` (the default) keeps ranking byte-identical to the
    /// FTS/graph-only path. New memories are embedded lazily on write; existing
    /// rows stay `NULL` (and skip the vector signal) until re-embedded via
    /// [`backfill_embeddings`](Self::backfill_embeddings).
    pub fn with_embedder(
        mut self,
        embedder: std::sync::Arc<dyn crate::vector::EmbeddingProvider>,
    ) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Whether a semantic embedder is attached (the `w_vec` signal is active).
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }

    /// Embed any currently-valid rows for `agent_id` that lack an embedding
    /// from the attached embedder's model (lazy backfill). Returns the number
    /// of rows embedded. No-op when no embedder is attached.
    pub async fn backfill_embeddings(&self, agent_id: &str) -> Result<usize> {
        let embedder = match &self.embedder {
            Some(e) => e.clone(),
            None => return Ok(0),
        };
        let conn = self.conn.lock().await;
        let now_rfc = Utc::now().to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT id, content FROM memories
                 WHERE agent_id = ?1
                   AND (embedding IS NULL OR embedding_model IS NULL OR embedding_model != ?2)
                   AND (valid_until IS NULL OR valid_until > ?3)",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![agent_id, embedder.id(), now_rfc], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut count = 0;
        for (id, content) in rows {
            if let Ok(vec) = embedder.embed(&content) {
                crate::vector::store_embedding(&conn, agent_id, &id, embedder.id(), &vec)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Acquire the database connection for maintenance tasks (e.g., decay/archival).
    ///
    /// Callers hold the lock for the duration of their work, preventing concurrent
    /// writes during multi-statement maintenance operations.
    pub async fn conn_for_maintenance(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        self.conn.lock().await
    }

    // ── D3.1 persistent graph cache: generation counter ─────────────────────
    //
    // Every triple-mutating write path bumps the agent's generation; a query
    // whose cached graph carries a different generation rebuilds. Reads and
    // writes are serialized by the `conn` mutex (both `search` and every write
    // path hold it), so the generation captured at cache-build time always
    // reflects the exact DB snapshot the graph was built from — no TOCTOU.

    /// Current graph generation for `agent_id` (0 when never bumped).
    fn graph_generation(&self, agent_id: &str) -> u64 {
        self.graph_generations
            .read()
            .map(|g| g.get(agent_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// Bump one agent's graph generation, invalidating its cached graph (D3.1).
    /// Called by every triple-mutating write path.
    pub(crate) fn bump_graph_generation(&self, agent_id: &str) {
        if let Ok(mut g) = self.graph_generations.write() {
            *g.entry(agent_id.to_string()).or_insert(0) += 1;
        }
    }

    /// Invalidate every cached agent graph (D3.1) — for bulk maintenance
    /// (decay archival, cross-agent reassignment) that mutates many agents' rows
    /// through the raw maintenance connection. Bumps the generation of each
    /// currently-cached agent so the next query rebuilds.
    pub(crate) fn invalidate_all_graph_caches(&self) {
        let cached: Vec<String> = self
            .graph_cache
            .read()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default();
        if let Ok(mut g) = self.graph_generations.write() {
            for a in cached {
                *g.entry(a).or_insert(0) += 1;
            }
        }
    }

    /// D3.1 — compute PPR scores for `query`, using the persistent graph cache
    /// when the agent is large enough (> [`GRAPH_CACHE_MIN_TRIPLES`]). Below the
    /// threshold the graph is rebuilt per query (cheap) exactly as before. The
    /// returned scores are byte-identical whether served from cache or freshly
    /// built — the cache only skips the rebuild, never changes the math.
    ///
    /// Returns `Ok(None)` (FTS-only, byte-identical) when the agent has zero
    /// triples or `query` seeds no graph entity — same fail-safe as before.
    fn graph_rank_cached(
        &self,
        conn: &Connection,
        agent_id: &str,
        query: &str,
        now_rfc: &str,
    ) -> Result<Option<Vec<(String, f64)>>> {
        let cur_gen = self.graph_generation(agent_id);

        // Fast path: a cache entry whose generation still matches.
        if let Ok(cache) = self.graph_cache.read() {
            if let Some(c) = cache.get(agent_id) {
                if c.generation == cur_gen {
                    return self.rank_from_graph(conn, &c.graph, agent_id, query, now_rfc);
                }
            }
        }

        // Miss / stale: cheap COUNT gate, then load + build.
        let count = crate::graph_rank::count_agent_triples(conn, agent_id, now_rfc)?;
        if count == 0 {
            return Ok(None);
        }
        let triples = crate::graph_rank::load_agent_graph_triples(conn, agent_id, now_rfc)?;
        let aliases = crate::graph_rank::load_alias_map(conn, agent_id)?;
        let graph = crate::graph_rank::TripleGraph::from_graph_triples(&triples, &aliases);

        // D3.4: lazily ensure entity vectors exist before this graph is used /
        // cached (opt-in + embedder attached). Failure-tolerant (warn + skip).
        if self.retrieval_weights.graph_embed_seed {
            if let Some(embedder) = self.embedder.clone() {
                self.ensure_entity_embeddings(conn, &graph, agent_id, embedder.as_ref());
            }
        }

        let result = self.rank_from_graph(conn, &graph, agent_id, query, now_rfc)?;

        if count as usize > GRAPH_CACHE_MIN_TRIPLES {
            // Ensure a generation entry exists so bulk invalidation catches this
            // agent, then cache the graph at the generation it was built from.
            if let Ok(mut g) = self.graph_generations.write() {
                g.entry(agent_id.to_string()).or_insert(cur_gen);
            }
            if let Ok(mut cache) = self.graph_cache.write() {
                cache.insert(
                    agent_id.to_string(),
                    CachedGraph {
                        graph,
                        generation: cur_gen,
                    },
                );
            }
        }
        Ok(result)
    }

    /// Run seeding + PPR + ranking over an already-built graph (D3.1 shared by
    /// the cache-hit and fresh-build paths — same code, so results match).
    fn rank_from_graph(
        &self,
        conn: &Connection,
        graph: &crate::graph_rank::TripleGraph,
        agent_id: &str,
        query: &str,
        now_rfc: &str,
    ) -> Result<Option<Vec<(String, f64)>>> {
        if graph.is_empty() {
            return Ok(None);
        }
        let mut seeds = graph.seed_nodes(query);

        // D3.4 embedding seeding (opt-in + embedder attached). Union of the
        // whole-word FTS seeds and the query embedding's nearest entity vectors.
        if self.retrieval_weights.graph_embed_seed {
            if let Some(embedder) = &self.embedder {
                if let Ok(qvec) = embedder.embed(query) {
                    let extra = self
                        .embed_seed_entities(conn, graph, agent_id, embedder.id(), &qvec, now_rfc)
                        .unwrap_or_default();
                    seeds.extend(extra);
                    seeds.sort_unstable();
                    seeds.dedup();
                }
            }
        }

        if seeds.is_empty() {
            return Ok(None);
        }
        let mass = graph.personalized_pagerank(&seeds);
        Ok(Some(graph.ranked_memories(&mass)))
    }

    /// D3.4 — ensure a same-model embedding exists for every entity in `graph`
    /// (lazy, batched, failure-tolerant). An embed error only warns and skips
    /// that entity, falling back to FTS-only seeding for it. No-op if all
    /// entities are already embedded.
    fn ensure_entity_embeddings(
        &self,
        conn: &Connection,
        graph: &crate::graph_rank::TripleGraph,
        agent_id: &str,
        embedder: &dyn crate::vector::EmbeddingProvider,
    ) {
        let model = embedder.id();
        for name in graph.entity_names() {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM entity_embedding
                     WHERE agent_id = ?1 AND entity = ?2 AND model = ?3 LIMIT 1",
                    params![agent_id, name, model],
                    |_| Ok(true),
                )
                .optional()
                .unwrap_or(None)
                .unwrap_or(false);
            if exists {
                continue;
            }
            match embedder.embed(name) {
                Ok(vec) if !vec.iter().all(|x| *x == 0.0) => {
                    let blob = crate::vector::encode_vec(&vec);
                    if let Err(e) = conn.execute(
                        "INSERT OR REPLACE INTO entity_embedding
                            (agent_id, entity, model, vec) VALUES (?1, ?2, ?3, ?4)",
                        params![agent_id, name, model, blob],
                    ) {
                        warn!(agent_id, entity = %name, "entity embed store failed: {e}");
                    }
                }
                Ok(_) => {} // empty/zero vector — nothing useful to store
                Err(e) => {
                    warn!(agent_id, entity = %name, "entity embed failed: {e}");
                }
            }
        }
    }

    /// D3.4 — entity node indices whose stored embedding is nearest the query
    /// embedding (cosine, same model only), top-k. Entities without a matching
    /// vector are skipped. Returns node indices in the supplied `graph`.
    fn embed_seed_entities(
        &self,
        conn: &Connection,
        graph: &crate::graph_rank::TripleGraph,
        agent_id: &str,
        model: &str,
        query_vec: &[f32],
        _now_rfc: &str,
    ) -> Result<Vec<usize>> {
        let top_k = self.retrieval_weights.graph_embed_seed_top_k.max(1);
        let mut stmt = conn
            .prepare(
                "SELECT entity, vec FROM entity_embedding
                 WHERE agent_id = ?1 AND model = ?2",
            )
            .map_err(|e| DuDuClawError::Memory(format!("entity embed load: {e}")))?;
        let rows = stmt
            .query_map(params![agent_id, model], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| DuDuClawError::Memory(format!("entity embed query: {e}")))?;

        let mut scored: Vec<(usize, f32)> = Vec::new();
        for row in rows {
            let (entity, blob) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let v = match crate::vector::decode_vec(&blob) {
                Some(v) if v.len() == query_vec.len() => v,
                _ => continue, // malformed / different-dimension: skip
            };
            let sim = crate::embedding::cosine_similarity(query_vec, &v);
            if sim <= 0.0 {
                continue;
            }
            if let Some(idx) = graph.entity_node(&entity) {
                scored.push((idx, sim));
            }
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(top_k);
        Ok(scored.into_iter().map(|(idx, _)| idx).collect())
    }

    // ── D3.2 entity alias API ───────────────────────────────────────────────

    /// Add (or update) an entity alias for `agent_id` (D3.2). Both `alias` and
    /// `canonical` are normalized (trim + lowercase). Alias chains are flattened
    /// on store: if `canonical` is itself an alias of something else, the deeper
    /// canonical is used, so a lookup never has to chase a chain. A self-alias
    /// (alias == resolved canonical) or an empty side is rejected. Bumps the
    /// agent's graph generation so the cache rebuilds with the new mapping.
    pub async fn add_entity_alias(
        &self,
        agent_id: &str,
        canonical: &str,
        alias: &str,
    ) -> Result<()> {
        let alias_n = alias.trim().to_lowercase();
        let mut canonical_n = canonical.trim().to_lowercase();
        if alias_n.is_empty() || canonical_n.is_empty() {
            return Err(DuDuClawError::Memory(
                "add_entity_alias: alias and canonical must be non-empty".to_string(),
            ));
        }
        let conn = self.conn.lock().await;
        // Flatten: resolve canonical's own canonical (one level is enough since
        // the table is always kept flat by this same rule).
        if let Some(deeper) = conn
            .query_row(
                "SELECT canonical FROM entity_alias WHERE agent_id = ?1 AND alias = ?2 LIMIT 1",
                params![agent_id, canonical_n],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?
        {
            canonical_n = deeper;
        }
        if alias_n == canonical_n {
            return Err(DuDuClawError::Memory(
                "add_entity_alias: alias resolves to itself".to_string(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO entity_alias (agent_id, canonical, alias, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id, alias) DO UPDATE SET canonical = excluded.canonical",
            params![agent_id, canonical_n, alias_n, now],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        drop(conn);
        self.bump_graph_generation(agent_id);
        Ok(())
    }

    /// Remove an entity alias for `agent_id` (D3.2). Returns whether a row was
    /// removed. Bumps the graph generation when something changed.
    pub async fn remove_entity_alias(&self, agent_id: &str, alias: &str) -> Result<bool> {
        let alias_n = alias.trim().to_lowercase();
        if alias_n.is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM entity_alias WHERE agent_id = ?1 AND alias = ?2",
                params![agent_id, alias_n],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        drop(conn);
        if n > 0 {
            self.bump_graph_generation(agent_id);
        }
        Ok(n > 0)
    }

    /// List an agent's entity aliases as `(canonical, alias)` pairs, sorted by
    /// canonical then alias for deterministic output (D3.2).
    pub async fn list_entity_aliases(&self, agent_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT canonical, alias FROM entity_alias WHERE agent_id = ?1
                 ORDER BY canonical, alias",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(out)
    }

    /// D3.3 — export the agent's SPO knowledge graph for the D6 curation UI.
    /// Edges are the newest `limit` currently-referenced triples (by memory
    /// `created_at`, descending); quarantined-but-pending facts are included and
    /// flagged so a reviewer can see what awaits approval. Node degree counts
    /// incident edges within the exported set. Entity names are alias-resolved
    /// so the exported graph matches the ranking graph's node identities.
    pub async fn export_graph(&self, agent_id: &str, limit: usize) -> Result<GraphExport> {
        let limit = limit.max(1);
        let conn = self.conn.lock().await;
        let now_rfc = Utc::now().to_rfc3339();
        let aliases = crate::graph_rank::load_alias_map(&conn, agent_id)?;

        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, origin_trust, quarantined
                 FROM memories
                 WHERE agent_id = ?1 AND subject IS NOT NULL
                   AND (valid_until IS NULL OR valid_until > ?2)
                 ORDER BY COALESCE(ingested_at, created_at, timestamp) DESC, id DESC
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id, now_rfc, (limit + 1) as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<f64>>(4)?.unwrap_or(1.0),
                    r.get::<_, i64>(5)? != 0,
                ))
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let resolve = |raw: &str| -> String {
            let n = raw.trim().to_lowercase();
            aliases.get(&n).cloned().unwrap_or(n)
        };

        let mut edges: Vec<GraphExportEdge> = Vec::new();
        let mut degree: HashMap<String, usize> = HashMap::new();
        for row in rows {
            let (memory_id, subject, predicate, object, origin_trust, quarantined) =
                row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let subject_c = resolve(&subject);
            if subject_c.is_empty() {
                continue;
            }
            let object_c = object
                .as_deref()
                .map(|o| resolve(o))
                .filter(|o| !o.is_empty());
            if edges.len() >= limit {
                // We fetched one extra row to detect truncation; stop here.
                return Ok(Self::finalize_graph_export(edges, degree, true));
            }
            *degree.entry(subject_c.clone()).or_insert(0) += 1;
            if let Some(o) = &object_c {
                *degree.entry(o.clone()).or_insert(0) += 1;
            }
            edges.push(GraphExportEdge {
                subject: subject_c,
                predicate,
                object: object_c,
                memory_id,
                origin_trust,
                quarantined,
            });
        }
        Ok(Self::finalize_graph_export(edges, degree, false))
    }

    /// Assemble a [`GraphExport`] from collected edges + degree map (D3.3),
    /// nodes sorted by degree descending then name for determinism.
    fn finalize_graph_export(
        edges: Vec<GraphExportEdge>,
        degree: HashMap<String, usize>,
        truncated: bool,
    ) -> GraphExport {
        let mut nodes: Vec<GraphExportNode> = degree
            .into_iter()
            .map(|(entity, degree)| GraphExportNode { entity, degree })
            .collect();
        nodes.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.entity.cmp(&b.entity)));
        GraphExport {
            nodes,
            edges,
            truncated,
        }
    }

    /// Select clause for all memory columns (qualified with table alias `m.`).
    const SELECT_COLS: &str = "m.id, m.agent_id, m.content, m.timestamp, m.tags, m.layer, m.importance, m.access_count, m.last_accessed, m.source_event";


    /// Return up to `limit` most-recent memory entries for `agent_id`, newest first.
    pub async fn list_recent(&self, agent_id: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {} FROM memories AS m WHERE m.agent_id = ?1 AND m.quarantined = 0 \
             ORDER BY m.timestamp DESC LIMIT ?2",
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
               AND m.quarantined = 0
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
               AND m.quarantined = 0
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
        // M1 moat-gate: reject once the Cloud paid-tier quota is hit. No-op when
        // unlimited (quota 0). Runs before any write so a rejection loses nothing.
        self.enforce_quota(&conn)?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let valid_from = meta.valid_from.unwrap_or(now).to_rfc3339();
        let mut valid_until = meta.valid_until.map(|t| t.to_rfc3339());
        let confidence = meta.confidence.unwrap_or(1.0);
        // D1: the source_event that drives build-time provenance for this write.
        // Prefer an explicit `meta.source_event`, else fall back to the entry's.
        let source_event = meta
            .source_event
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| entry.source_event.clone());
        let metadata = meta
            .metadata
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());

        // ── P2-1: origin-bound trust (I8, non-malleable) ─────────────────────
        // Start from the caller's declared trust, clamped to [0,1]. If the fact
        // is derived from source memories, clamp further to ≤ the MINIMUM trust
        // across those sources — a derived/summarized fact can never be more
        // trusted than its least-trusted input.
        let mut origin_trust = meta.origin_trust.unwrap_or(1.0).clamp(0.0, 1.0);
        let derived_from_json = match &meta.derived_from {
            Some(sources) if !sources.is_empty() => {
                let mut min_source: f64 = 1.0;
                for sid in sources {
                    let t: Option<f64> = conn
                        .query_row(
                            "SELECT origin_trust FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
                            params![sid, agent_id],
                            |r| r.get::<_, f64>(0),
                        )
                        .optional()
                        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                    // An unknown source id contributes trust 0.0 (fail-closed):
                    // we cannot vouch for a source we cannot find.
                    min_source = min_source.min(t.unwrap_or(0.0));
                }
                origin_trust = origin_trust.min(min_source);
                Some(
                    serde_json::to_string(sources)
                        .map_err(|e| DuDuClawError::Memory(e.to_string()))?,
                )
            }
            _ => None,
        };

        // ── Conflict resolution: only when a full triple is supplied ──────────
        // D2: a quarantined fact is INERT — it must never expire, supersede, or
        // reaffirm a currently-valid fact (that is exactly the PoisonedRAG
        // primitive we are defending against: unverified input silently
        // overwriting curated knowledge). Skip the whole conflict-resolution
        // block; the row is inserted with `quarantined = 1` and stays isolated
        // until a human releases it.
        let mut supersedes: Option<String> = None;
        if !meta.quarantined {
        if let (Some(subj), Some(pred)) = (meta.subject.as_ref(), meta.predicate.as_ref()) {
            // Load currently-active rows (id + object + content + metadata +
            // access_count + world-time start), newest world-time first. We need
            // object/content to decide reaffirm-vs-supersede and valid_from to
            // decide the out-of-order (historical) insert.
            let active: Vec<ActiveTriple> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, object, content, metadata, access_count,
                                COALESCE(valid_from, timestamp)
                         FROM memories
                         WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                           AND valid_until IS NULL
                         ORDER BY COALESCE(valid_from, timestamp) DESC, created_at DESC, id DESC",
                    )
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                let rows = stmt
                    .query_map(params![agent_id, subj, pred], |r| {
                        Ok(ActiveTriple {
                            id: r.get(0)?,
                            object: r.get(1)?,
                            content: r.get(2)?,
                            metadata: r
                                .get::<_, Option<String>>(3)?
                                .unwrap_or_else(|| "{}".to_string()),
                            access_count: r.get(4)?,
                            valid_from: r.get(5)?,
                        })
                    })
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
                }
                v
            };

            // ── Reaffirm: same (subject, predicate, object) + content re-observed.
            // Don't create a new row — append this write's source_event to the
            // surviving row's `reaffirmed_by` list (cap 20) and bump access_count.
            if let Some(row) = active.iter().find(|r| {
                object_opt_eq(&meta.object, &r.object) && r.content.trim() == entry.content.trim()
            }) {
                let new_meta = append_reaffirmed_by(&row.metadata, &source_event);
                conn.execute(
                    "UPDATE memories SET metadata = ?1, access_count = access_count + 1
                     WHERE id = ?2 AND agent_id = ?3",
                    params![new_meta, row.id, agent_id],
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                info!(agent_id, entry_id = %row.id, "temporal memory reaffirmed");
                return Ok(row.id.clone());
            }

            if active.len() > 1 {
                warn!(
                    agent_id,
                    subject = %subj,
                    predicate = %pred,
                    count = active.len(),
                    "multiple active memories for the same triple"
                );
            }

            // ── Out-of-order resilience: only when the new fact carries a
            // world-time start. If it predates the reigning active fact, it
            // belongs to the PAST — insert it as a bounded historical segment
            // (valid_until = the next-known fact's valid_from) WITHOUT disturbing
            // the current fact or forming a supersession chain (it was never
            // current). Facts with no valid_from keep the legacy ingestion-order
            // behavior byte-for-byte.
            let mut treat_as_history = false;
            if let Some(new_vf) = meta.valid_from {
                if let Some(reigning) = active
                    .first()
                    .and_then(|r| r.valid_from.as_deref())
                    .and_then(parse_rfc3339)
                {
                    if new_vf < reigning {
                        treat_as_history = true;
                        // Tightest bound: the smallest active valid_from strictly
                        // after the new fact's start.
                        let bound = active
                            .iter()
                            .filter_map(|r| r.valid_from.as_deref().and_then(parse_rfc3339))
                            .filter(|dt| *dt > new_vf)
                            .min()
                            .map(|dt| dt.to_rfc3339());
                        valid_until = match (valid_until.take(), bound) {
                            (Some(a), Some(b)) => Some(min_rfc3339(a, b)),
                            (a, b) => a.or(b),
                        };
                    }
                }
            }

            if !treat_as_history {
                // The new fact wins — expire all active rows, stamp build-time
                // provenance, and chain the back-pointer to the most recent one.
                for row in &active {
                    conn.execute(
                        "UPDATE memories
                         SET valid_until = ?1, superseded_by = ?2,
                             invalidated_by_event = ?3, invalidated_at = ?4
                         WHERE id = ?5",
                        params![now_str, entry.id, source_event, now_str, row.id],
                    )
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                }
                supersedes = active.first().map(|r| r.id.clone());
            }
        }
        } // end `if !meta.quarantined`

        let tags_json = serde_json::to_string(&entry.tags)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let timestamp_str = entry.timestamp.to_rfc3339();
        let last_accessed_str = entry.last_accessed.map(|t| t.to_rfc3339());

        conn.execute(
            "INSERT INTO memories
                (id, agent_id, content, timestamp, tags, layer, importance, access_count,
                 last_accessed, source_event,
                 valid_from, valid_until, superseded_by, supersedes,
                 subject, predicate, object, confidence, metadata,
                 origin, origin_trust, derived_from, ingested_at, quarantined)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
            params![
                entry.id, agent_id, entry.content, timestamp_str, tags_json,
                entry.layer.as_str(), entry.importance, entry.access_count,
                last_accessed_str, entry.source_event,
                valid_from, valid_until, Option::<String>::None, supersedes,
                meta.subject, meta.predicate, meta.object, confidence, metadata,
                meta.origin, origin_trust, derived_from_json, now_str,
                if meta.quarantined { 1i64 } else { 0i64 }
            ],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        conn.execute(
            "INSERT INTO memories_fts (content, agent_id, memory_id) VALUES (?1, ?2, ?3)",
            params![entry.content, agent_id, entry.id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        self.embed_on_write(&conn, agent_id, &entry.id, &entry.content);

        // D3.1: a new triple (and any supersession expiry above) changed the
        // currently-valid triple set — invalidate this agent's cached graph.
        self.bump_graph_generation(agent_id);

        info!(agent_id, entry_id = %entry.id, "temporal memory stored");
        Ok(entry.id)
    }

    /// Compute + persist an embedding for a just-written row when an embedder is
    /// attached. Best-effort: an embed failure is logged and skipped (the row
    /// simply won't carry the vector signal), never failing the store.
    fn embed_on_write(&self, conn: &Connection, agent_id: &str, memory_id: &str, content: &str) {
        if let Some(embedder) = &self.embedder {
            match embedder.embed(content) {
                Ok(vec) => {
                    if let Err(e) =
                        crate::vector::store_embedding(conn, agent_id, memory_id, embedder.id(), &vec)
                    {
                        tracing::warn!(agent_id, memory_id, "embed store failed: {e}");
                    }
                }
                Err(e) => tracing::warn!(agent_id, memory_id, "embed failed: {e}"),
            }
        }
    }

    /// Whether a memory id is currently quarantined (D2). `None` when the id is
    /// not found for this agent. Used by the approval flow / tests.
    pub async fn is_quarantined(&self, agent_id: &str, memory_id: &str) -> Result<Option<bool>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT quarantined FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
            params![memory_id, agent_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map(|opt| opt.map(|n| n != 0))
        .map_err(|e| DuDuClawError::Memory(e.to_string()))
    }

    /// D2 approval — RELEASE a quarantined batch. Clears `quarantined` on the
    /// given ids owned by `agent_id`, making them visible to retrieval again.
    /// Only rows currently quarantined are touched (idempotent). Returns the
    /// number of rows released. `ids` is capped at 100 per call.
    pub async fn release_quarantine(
        &self,
        agent_id: &str,
        ids: &[String],
    ) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > 100 {
            return Err(DuDuClawError::Memory(
                "release_quarantine limited to 100 ids per call".to_string(),
            ));
        }
        let conn = self.conn.lock().await;
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE memories SET quarantined = 0
             WHERE agent_id = ? AND quarantined = 1 AND id IN ({placeholders})"
        );
        let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        bind.push(&agent_id);
        for id in ids {
            bind.push(id);
        }
        let n = conn
            .execute(&sql, bind.as_slice())
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        drop(conn);
        if n > 0 {
            // D3.1: released rows re-enter the valid-triple set.
            self.bump_graph_generation(agent_id);
        }
        Ok(n)
    }

    /// D2 approval — REJECT a quarantined batch. Expires the given ids
    /// (`valid_until = now`, `invalidated_by_event = event`,
    /// `invalidated_at = now`) and downgrades their `origin_trust` to
    /// `min(current, 0.1)` (no separate origin-trust store exists, so the
    /// downgrade is applied to the rejected rows themselves). Rows stay
    /// `quarantined = 1` so they can never resurface via a read path. Returns
    /// the number of rows rejected. `ids` is capped at 100 per call.
    pub async fn reject_quarantine(
        &self,
        agent_id: &str,
        ids: &[String],
        event: &str,
    ) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > 100 {
            return Err(DuDuClawError::Memory(
                "reject_quarantine limited to 100 ids per call".to_string(),
            ));
        }
        let now_str = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE memories
             SET valid_until = ?1, invalidated_by_event = ?2, invalidated_at = ?1,
                 origin_trust = MIN(origin_trust, 0.1)
             WHERE agent_id = ? AND quarantined = 1 AND id IN ({placeholders})"
        );
        let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 3);
        bind.push(&now_str);
        bind.push(&event);
        bind.push(&agent_id);
        for id in ids {
            bind.push(id);
        }
        let n = conn
            .execute(&sql, bind.as_slice())
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        drop(conn);
        if n > 0 {
            // D3.1: rejected rows were expired (dropped from the valid set).
            self.bump_graph_generation(agent_id);
        }
        Ok(n)
    }

    /// Read back the stored `origin_trust` for a memory id (P2-1). Returns
    /// `None` when the id is not found for this agent.
    pub async fn get_origin_trust(&self, agent_id: &str, memory_id: &str) -> Result<Option<f64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT origin_trust FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
            params![memory_id, agent_id],
            |r| r.get::<_, f64>(0),
        )
        .optional()
        .map_err(|e| DuDuClawError::Memory(e.to_string()))
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
                "SELECT id, content, valid_from, valid_until, superseded_by, supersedes, confidence,
                        COALESCE(ingested_at, created_at, timestamp),
                        invalidated_by_event, invalidated_at, metadata
                 FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                 ORDER BY COALESCE(valid_from, timestamp) ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id, subject, predicate], map_temporal_record)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(out)
    }

    /// Point-in-time lookup for a triple: the memory valid at instant `at`
    /// (`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`) (F1).
    /// Exposed over MCP as `memory_get_at` since D1 (2026-07).
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
                "SELECT id, content, valid_from, valid_until, superseded_by, supersedes, confidence,
                        COALESCE(ingested_at, created_at, timestamp),
                        invalidated_by_event, invalidated_at, metadata
                 FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
                   AND COALESCE(valid_from, timestamp) <= ?4
                   AND (valid_until IS NULL OR valid_until > ?4)
                 ORDER BY COALESCE(valid_from, timestamp) DESC
                 LIMIT 1",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![agent_id, subject, predicate, at_str], map_temporal_record)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// D1 rollback primitive: expire (never delete) every currently-valid fact
    /// for `agent_id` that originated from exactly `origin`, optionally limited
    /// to rows learned at/after `since` (transaction-time, `ingested_at`). The
    /// `origin` match is an **exact equality** — never a substring — so
    /// `channel-a` can be purged without touching `channel-abc` (project rule:
    /// no unanchored `contains` for security/routing decisions).
    ///
    /// Expired rows get `valid_until = now`, `invalidated_by_event = "origin_purge"`,
    /// `invalidated_at = now`, so `search()` stops returning them while
    /// `get_history()` keeps the full chain. Any row whose `derived_from` cites a
    /// purged id has its `origin_trust` lowered to `min(current, 0.1)` (a derived
    /// fact of a poisoned source can't stay trusted) — but is **not** expired.
    ///
    /// Returns the number of rows expired.
    pub async fn invalidate_by_origin(
        &self,
        agent_id: &str,
        origin: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let since_str = since.map(|t| t.to_rfc3339());

        // ── 1. Collect the currently-valid rows to expire (exact origin match). ──
        // `datetime()` normalizes both rfc3339 (ingested_at/timestamp) and the
        // SQLite "YYYY-MM-DD HH:MM:SS" default of `created_at` so the `since`
        // comparison is representation-agnostic.
        let target_ids: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM memories
                     WHERE agent_id = ?1 AND origin = ?2 AND valid_until IS NULL
                       AND (?3 IS NULL
                            OR datetime(COALESCE(ingested_at, created_at, timestamp))
                               >= datetime(?3))",
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let rows = stmt
                .query_map(params![agent_id, origin, since_str], |r| r.get::<_, String>(0))
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
            }
            v
        };

        if target_ids.is_empty() {
            return Ok(0);
        }

        // ── 2. Expire them (single UPDATE mirroring the SELECT predicate). ──
        conn.execute(
            "UPDATE memories
             SET valid_until = ?1, invalidated_by_event = 'origin_purge', invalidated_at = ?1
             WHERE agent_id = ?2 AND origin = ?3 AND valid_until IS NULL
               AND (?4 IS NULL
                    OR datetime(COALESCE(ingested_at, created_at, timestamp))
                       >= datetime(?4))",
            params![now_str, agent_id, origin, since_str],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        // ── 3. Cascade: any row deriving from a purged id gets trust ≤ 0.1. ──
        let purged: std::collections::HashSet<&str> =
            target_ids.iter().map(|s| s.as_str()).collect();
        let candidates: Vec<(String, f64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, origin_trust, derived_from FROM memories
                     WHERE agent_id = ?1 AND derived_from IS NOT NULL",
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let rows = stmt
                .query_map(params![agent_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<f64>>(1)?.unwrap_or(1.0),
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                })
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
            }
            v
        };
        for (id, trust, derived_json) in candidates {
            if trust <= 0.1 {
                continue; // already at/below the floor — nothing to lower
            }
            let cites_purged = serde_json::from_str::<Vec<String>>(&derived_json)
                .map(|ids| ids.iter().any(|s| purged.contains(s.as_str())))
                .unwrap_or(false);
            if cites_purged {
                conn.execute(
                    "UPDATE memories SET origin_trust = ?1 WHERE id = ?2 AND agent_id = ?3",
                    params![trust.min(0.1), id, agent_id],
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            }
        }

        drop(conn);
        // D3.1: expired rows + lowered-trust derivatives changed the graph.
        self.bump_graph_generation(agent_id);

        info!(
            agent_id,
            origin,
            expired = target_ids.len(),
            "invalidate_by_origin"
        );
        Ok(target_ids.len() as u64)
    }

    /// Resolve the `(subject, predicate)` triple keys for a single memory id
    /// (agent-isolated). Returns `None` when the row is absent, belongs to
    /// another agent, or has NULL subject/predicate (i.e. it's not a temporal
    /// triple). Used by the dashboard `memory.history` RPC so the operator can
    /// look up a supersession chain starting from a memory id (F1).
    pub async fn triple_for_id(
        &self,
        agent_id: &str,
        memory_id: &str,
    ) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT subject, predicate FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
            params![memory_id, agent_id],
            |r| {
                let subject: Option<String> = r.get(0)?;
                let predicate: Option<String> = r.get(1)?;
                Ok((subject, predicate))
            },
        )
        .optional()
        .map_err(|e| DuDuClawError::Memory(e.to_string()))
        .map(|opt| match opt {
            Some((Some(s), Some(p))) if !s.is_empty() && !p.is_empty() => Some((s, p)),
            _ => None,
        })
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
        drop(conn);
        self.bump_graph_generation(agent_id); // D3.1: decision triples expired
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
        drop(conn);
        if n > 0 {
            self.bump_graph_generation(agent_id); // D3.1: stale decisions expired
        }
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
        drop(conn);
        if n > 0 {
            self.bump_graph_generation(agent_id); // D3.1: decision triples closed
        }
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
                ..Default::default()
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
               AND m.quarantined = 0
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
               AND m.quarantined = 0
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
            // ── P2-1 origin-bound provenance (I8: trust non-malleable) ──
            // `origin` = where the fact came from ("channel"/"user"/"distill"/…);
            // `origin_trust` = 0..1 trust of that origin (default 1.0 = self /
            // authoritative); `derived_from` = JSON array of source memory ids.
            // A derived fact's `origin_trust` is clamped to ≤ min(source trusts)
            // in `store_temporal`, so trust cannot be laundered upward.
            "ALTER TABLE memories ADD COLUMN origin TEXT",
            "ALTER TABLE memories ADD COLUMN origin_trust REAL NOT NULL DEFAULT 1.0",
            "ALTER TABLE memories ADD COLUMN derived_from TEXT",
            // ── D1 Bi-temporal + build-time provenance (2026-07) ──
            // `ingested_at` = transaction-time axis (when the system LEARNED the
            // fact), distinct from `valid_from` (world-time the fact became true).
            // New writes set it to now; old rows read via
            // COALESCE(ingested_at, created_at, timestamp), so no backfill UPDATE
            // is needed. `invalidated_by_event` / `invalidated_at` record which
            // source_event closed a row out during supersession/purge and when —
            // the minimal build-time provenance Graphiti calls for.
            "ALTER TABLE memories ADD COLUMN ingested_at TEXT",
            "ALTER TABLE memories ADD COLUMN invalidated_by_event TEXT",
            "ALTER TABLE memories ADD COLUMN invalidated_at TEXT",
            // ── Semantic vector layer (w_vec signal) ──
            // `embedding` = little-endian f32 BLOB; `embedding_model` = the
            // embedder id that produced it (never cross-space compared). Both
            // NULL on old rows ⇒ they skip the vector signal until re-embedded.
            "ALTER TABLE memories ADD COLUMN embedding BLOB",
            "ALTER TABLE memories ADD COLUMN embedding_model TEXT",
            // ── D2 write-side poison quarantine (2026-07) ──
            // `quarantined = 1` marks a fact held for human review (write-side
            // injection hit or same-origin burst). Every retrieval read path
            // filters `quarantined = 0`; the row stays inert until an approval
            // releases it (→ 0) or rejects it (→ expired). Default 0 so every
            // existing row is treated as clean with no backfill.
            "ALTER TABLE memories ADD COLUMN quarantined INTEGER NOT NULL DEFAULT 0",
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

        // ── D3.2 entity alias table ──────────────────────────────────────────
        // Maps a surface form (`alias`) to a canonical entity string per agent,
        // used to collapse "老闆/李老闆/zhixu" into one graph node. Both sides are
        // stored pre-normalized (trim + lowercase). Absent ⇒ alias-free graph.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entity_alias (
                agent_id TEXT NOT NULL,
                canonical TEXT NOT NULL,
                alias TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (agent_id, alias)
            );
            CREATE INDEX IF NOT EXISTS idx_entity_alias_agent
                ON entity_alias(agent_id);",
        )
        .map_err(|e| DuDuClawError::Memory(format!("entity_alias table: {e}")))?;

        // ── D3.4 entity embedding table (opt-in graph seeding) ───────────────
        // Caches one embedding per (agent, canonical entity, model). Populated
        // lazily when embedding-seeding is enabled AND an embedder is attached;
        // never touched otherwise (byte-identical default-off path).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entity_embedding (
                agent_id TEXT NOT NULL,
                entity TEXT NOT NULL,
                model TEXT NOT NULL,
                vec BLOB NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (agent_id, entity, model)
            );",
        )
        .map_err(|e| DuDuClawError::Memory(format!("entity_embedding table: {e}")))?;

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
        // M1 moat-gate: reject once the Cloud paid-tier quota is hit. No-op when
        // unlimited (quota 0). Runs before any write so a rejection loses nothing.
        self.enforce_quota(&conn)?;

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

        self.embed_on_write(&conn, agent_id, &entry.id, &entry.content);

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
               AND m.quarantined = 0
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
        let graph_ranked = match self.graph_rank_cached(&conn, agent_id, query, &now_rfc) {
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

        // ── Semantic vector signal (w_vec) ──
        // Fail-safe identical to the graph path: no embedder attached, no
        // matching embedded rows, or an empty query embedding all yield `None`,
        // leaving the FTS/graph ranking byte-identical. Cosine is computed ONLY
        // against same-model vectors (vector_knn enforces `embedding_model`), so
        // switching embedders never mixes incompatible spaces.
        if let Some(embedder) = &self.embedder {
            if let Ok(qvec) = embedder.embed(query) {
                let knn = crate::vector::vector_knn(
                    &conn,
                    agent_id,
                    &qvec,
                    embedder.id(),
                    &now_rfc,
                    fetch_limit,
                )
                .unwrap_or(None);
                if let Some(hits) = knn {
                    let vmap: std::collections::HashMap<&str, f32> =
                        hits.iter().map(|(id, s)| (id.as_str(), *s)).collect();
                    let present: std::collections::HashSet<String> =
                        scored.iter().map(|(_, e)| e.id.clone()).collect();
                    // Blend into existing candidates.
                    for (score, entry) in scored.iter_mut() {
                        if let Some(sim) = vmap.get(entry.id.as_str()) {
                            *score += w.w_vec * (*sim as f64);
                        }
                    }
                    // Append up to MAX_GRAPH_APPENDS vector-only recall hits FTS
                    // (and graph) missed — same agent/temporal isolation as the
                    // KNN query.
                    let appends = hits
                        .iter()
                        .filter(|(id, _)| !present.contains(id))
                        .take(crate::graph_rank::MAX_GRAPH_APPENDS);
                    for (id, sim) in appends {
                        if let Some(entry) = Self::fetch_valid_entry(&conn, agent_id, id, &now_rfc)? {
                            let score = Self::base_relevance_score(&entry, now, w)
                                + w.w_vec * (*sim as f64);
                            scored.push((score, entry));
                        }
                    }
                }
            }
        }

        // P2-1 / D2: let origin-trust participate in the ranking (I8,
        // PoisonedRAG 2402.07867). Each score is multiplied by
        // `(1 - w_trust) + w_trust * origin_trust`, a soft weighting rather than
        // the old full multiply — a fully-trusted fact (`origin_trust = 1.0`,
        // the default for every existing row) is unchanged, so behaviour
        // degrades to the pre-D2 path for legacy data; only distilled/channel
        // facts with trust < 1.0 are gently pushed down. A multiplier, not a
        // filter, so nothing is silently hidden — a low-trust fact can still
        // surface if little else competes.
        let w_trust = w.w_trust.clamp(0.0, 1.0);
        for (score, entry) in scored.iter_mut() {
            let trust: f64 = conn
                .query_row(
                    "SELECT origin_trust FROM memories WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
                    params![entry.id, agent_id],
                    |r| r.get::<_, f64>(0),
                )
                .optional()
                .ok()
                .flatten()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            *score *= (1.0 - w_trust) + w_trust * trust;
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
                "SELECT {} FROM memories AS m WHERE m.agent_id = ?1 AND m.quarantined = 0 \
                 AND m.timestamp >= ?2 AND m.timestamp <= ?3 ORDER BY m.timestamp ASC",
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

    // ── M1 moat-gate: memory_quota_gb enforcement ────────────────────────────

    #[test]
    fn quota_predicate_zero_is_unlimited() {
        // The load-bearing non-breaking guarantee: quota 0 never blocks,
        // regardless of usage.
        assert!(!quota_exceeded_bytes(0, 0));
        assert!(!quota_exceeded_bytes(u64::MAX, 0));
        // Below the cap → allowed; at/over the cap → blocked (fail-closed).
        assert!(!quota_exceeded_bytes(999, 1000));
        assert!(quota_exceeded_bytes(1000, 1000));
        assert!(quota_exceeded_bytes(1001, 1000));
    }

    #[tokio::test]
    async fn quota_zero_never_blocks() {
        // Default engine = unlimited: many writes all succeed, behaviour
        // byte-identical to the un-gated path.
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "unlimited-agent";
        for i in 0..50 {
            engine
                .store(agent, make_entry(agent, &format!("entry {i}"), vec![]))
                .await
                .expect("quota 0 must never reject a write");
        }
        assert_eq!(engine.list_recent(agent, 100).await.unwrap().len(), 50);
    }

    #[tokio::test]
    async fn quota_under_limit_allows_write() {
        // A quota comfortably above current DB size does not block.
        let mut engine = SqliteMemoryEngine::in_memory().unwrap();
        engine.set_memory_quota_gb(10); // 10 GB — far above a fresh in-memory DB
        let agent = "under-agent";
        engine
            .store(agent, make_entry(agent, "well within budget", vec![]))
            .await
            .expect("write under quota must succeed");
        assert_eq!(engine.list_recent(agent, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn quota_over_limit_rejects_gracefully_without_data_loss() {
        let mut engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "capped-agent";

        // Seed some data while unlimited, then measure real usage.
        for i in 0..5 {
            engine
                .store(agent, make_entry(agent, &format!("seed {i}"), vec![]))
                .await
                .unwrap();
        }
        let before = engine.list_recent(agent, 100).await.unwrap().len();
        let usage = engine.db_usage_bytes().await;
        assert!(usage > 0, "DB usage must be measurable for the test to bite");

        // Set the quota BELOW current usage → the next write must be rejected.
        engine.set_memory_quota_bytes(usage / 2);

        let err = engine
            .store(agent, make_entry(agent, "over the cap", vec![]))
            .await
            .expect_err("write over quota must be rejected");
        match err {
            DuDuClawError::Memory(msg) => assert!(
                msg.contains("quota exceeded"),
                "graceful, explicit error; got: {msg}"
            ),
            other => panic!("expected Memory error, got {other:?}"),
        }

        // Fail-closed but graceful: nothing was lost, nothing was partially
        // written, and no panic occurred.
        assert_eq!(
            engine.list_recent(agent, 100).await.unwrap().len(),
            before,
            "existing data preserved; rejected entry not written"
        );

        // store_temporal is gated on the same predicate.
        let temporal_err = engine
            .store_temporal(agent, make_entry(agent, "temporal over cap", vec![]), TemporalMeta::default())
            .await
            .expect_err("store_temporal over quota must also be rejected");
        assert!(matches!(temporal_err, DuDuClawError::Memory(_)));

        // Lifting the quota back to unlimited restores writes.
        engine.set_memory_quota_gb(0);
        engine
            .store(agent, make_entry(agent, "after lift", vec![]))
            .await
            .expect("unlimited again after quota cleared");
        assert_eq!(engine.list_recent(agent, 100).await.unwrap().len(), before + 1);
    }

    /// Live verification of the `w_vec` signal: the vector layer recalls a
    /// memory that exact FTS token matching misses (a CJK sub-word), and the
    /// no-embedder baseline stays empty (byte-identical fail-safe).
    #[tokio::test]
    async fn vector_recall_surfaces_what_fts_misses() {
        use std::sync::Arc;
        let agent = "vec-agent";

        // Baseline WITHOUT embedder: FTS5 unicode61 tokenizes a contiguous CJK
        // run as ONE token, so a sub-word query does not match → empty result.
        let plain = SqliteMemoryEngine::in_memory().unwrap();
        plain
            .store(agent, make_entry(agent, "資料庫遷移工作排程", vec![]))
            .await
            .unwrap();
        plain
            .store(agent, make_entry(agent, "schema 設計文件", vec![]))
            .await
            .unwrap();
        let base = plain.search(agent, "遷移", 5).await.unwrap();
        assert!(
            base.is_empty(),
            "FTS-only baseline misses the CJK sub-word (byte-identical fail-safe)"
        );

        // WITH the char-ngram embedder: the vector signal recalls it.
        let vec_engine = SqliteMemoryEngine::in_memory()
            .unwrap()
            .with_embedder(Arc::new(crate::vector::NgramHashEmbedder::new()));
        vec_engine
            .store(agent, make_entry(agent, "資料庫遷移工作排程", vec![]))
            .await
            .unwrap();
        vec_engine
            .store(agent, make_entry(agent, "schema 設計文件", vec![]))
            .await
            .unwrap();
        let hits = vec_engine.search(agent, "遷移", 5).await.unwrap();
        assert!(
            !hits.is_empty(),
            "vector recall must surface the 遷移 memory FTS missed"
        );
        assert!(hits[0].content.contains("遷移"));

        // Agent isolation holds through the vector path.
        let other = vec_engine.search("different-agent", "遷移", 5).await.unwrap();
        assert!(other.is_empty(), "KNN must not leak across agents");
    }

    /// Rows stored before an embedder was attached carry no embedding; a
    /// `backfill_embeddings` pass embeds them and they then become recallable.
    #[tokio::test]
    async fn backfill_embeds_preexisting_rows() {
        use std::sync::Arc;
        let agent = "bf-agent";
        // Store WITHOUT embedder (rows land with NULL embedding).
        let mut engine = SqliteMemoryEngine::in_memory().unwrap();
        engine
            .store(agent, make_entry(agent, "資料庫遷移工作排程", vec![]))
            .await
            .unwrap();
        // Attach embedder now; existing row is still un-embedded.
        engine = engine.with_embedder(Arc::new(crate::vector::NgramHashEmbedder::new()));
        assert!(
            engine.search(agent, "遷移", 5).await.unwrap().is_empty(),
            "pre-existing row not yet embedded → no vector recall"
        );
        let n = engine.backfill_embeddings(agent).await.unwrap();
        assert_eq!(n, 1, "one row backfilled");
        assert!(
            !engine.search(agent, "遷移", 5).await.unwrap().is_empty(),
            "after backfill the row is recallable via w_vec"
        );
        // Idempotent: a second pass re-embeds nothing (same model id).
        assert_eq!(engine.backfill_embeddings(agent).await.unwrap(), 0);
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

    /// `triple_for_id` resolves a memory id back to its (subject, predicate)
    /// keys, enforces agent isolation, and returns `None` for non-triple rows.
    #[tokio::test]
    async fn triple_for_id_resolves_and_isolates() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "triple-agent";

        let e = make_entry(agent, "python era", vec![]);
        let id = engine
            .store_temporal(agent, e, triple_meta("user", "lang", "python"))
            .await
            .unwrap();

        // Owner resolves the triple.
        let got = engine.triple_for_id(agent, &id).await.unwrap();
        assert_eq!(got, Some(("user".to_string(), "lang".to_string())));

        // Cross-agent lookup is isolated → None (no existence leak).
        let other = engine.triple_for_id("someone-else", &id).await.unwrap();
        assert_eq!(other, None);

        // A plain (non-triple) row → None.
        let plain = make_entry(agent, "no triple here", vec![]);
        let plain_id = engine
            .store_temporal(agent, plain, TemporalMeta::default())
            .await
            .unwrap();
        assert_eq!(engine.triple_for_id(agent, &plain_id).await.unwrap(), None);

        // The resolved triple drives get_history end-to-end.
        let (s, p) = got.unwrap();
        let history = engine.get_history(agent, &s, &p).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "python era");
        assert!(history[0].valid_until.is_none(), "sole version is current");
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

    // ── P2-1: origin-bound trust (I8, non-malleable) ────────────────────────────

    /// A plain store defaults origin_trust to 1.0 (backward compatible).
    #[tokio::test]
    async fn origin_trust_defaults_to_one() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "trust-default";
        let e = make_entry(agent, "x", vec![]);
        let id = engine.store_temporal(agent, e, TemporalMeta::default()).await.unwrap();
        assert_eq!(engine.get_origin_trust(agent, &id).await.unwrap(), Some(1.0));
    }

    /// A declared low origin_trust is stored and clamped to [0,1].
    #[tokio::test]
    async fn origin_trust_stored_and_clamped() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "trust-store";
        let e = make_entry(agent, "channel fact", vec![]);
        let meta = TemporalMeta {
            origin: Some("channel".into()),
            origin_trust: Some(0.3),
            ..Default::default()
        };
        let id = engine.store_temporal(agent, e, meta).await.unwrap();
        assert_eq!(engine.get_origin_trust(agent, &id).await.unwrap(), Some(0.3));

        // Above-range trust is clamped to 1.0.
        let e2 = make_entry(agent, "y", vec![]);
        let id2 = engine
            .store_temporal(agent, e2, TemporalMeta { origin_trust: Some(5.0), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(engine.get_origin_trust(agent, &id2).await.unwrap(), Some(1.0));
    }

    /// A derived fact can never be more trusted than its least-trusted source.
    #[tokio::test]
    async fn derived_fact_inherits_min_source_trust() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "trust-derive";

        // Two sources: trust 0.8 and 0.2.
        let hi = engine
            .store_temporal(
                agent,
                make_entry(agent, "hi src", vec![]),
                TemporalMeta { origin_trust: Some(0.8), ..Default::default() },
            )
            .await
            .unwrap();
        let lo = engine
            .store_temporal(
                agent,
                make_entry(agent, "lo src", vec![]),
                TemporalMeta { origin_trust: Some(0.2), ..Default::default() },
            )
            .await
            .unwrap();

        // Derived fact declares trust 0.9 but must be clamped to min(sources)=0.2.
        let derived = engine
            .store_temporal(
                agent,
                make_entry(agent, "derived", vec![]),
                TemporalMeta {
                    origin_trust: Some(0.9),
                    derived_from: Some(vec![hi.clone(), lo.clone()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(engine.get_origin_trust(agent, &derived).await.unwrap(), Some(0.2));
    }

    /// An unknown source id contributes trust 0.0 (fail-closed): can't vouch for it.
    #[tokio::test]
    async fn derived_from_unknown_source_is_fail_closed() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "trust-unknown";
        let derived = engine
            .store_temporal(
                agent,
                make_entry(agent, "derived from ghost", vec![]),
                TemporalMeta {
                    origin_trust: Some(1.0),
                    derived_from: Some(vec!["does-not-exist".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(engine.get_origin_trust(agent, &derived).await.unwrap(), Some(0.0));
    }

    // ── D1: bi-temporal out-of-order resilience + provenance ────────────────────

    /// Build a spouse triple meta with a world-time `valid_from`.
    fn spouse_meta(object: &str, valid_from: DateTime<Utc>) -> TemporalMeta {
        TemporalMeta {
            subject: Some("person:me".into()),
            predicate: Some("spouse".into()),
            object: Some(object.into()),
            valid_from: Some(valid_from),
            source_event: Some(format!("ev-{object}")),
            ..Default::default()
        }
    }

    /// The Graphiti marriage→divorce→remarriage example ingested OUT OF ORDER:
    /// divorce first, then the earlier marriage, then the later one. `get_at`
    /// must still resolve the correct spouse at every point in time, and the
    /// supersession chain must stay intact.
    #[tokio::test]
    async fn out_of_order_marriage_resolves_by_world_time() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "bitemporal";
        let now = Utc::now();
        let t_early = now - Duration::days(3000); // marry Alice
        let t_mid = now - Duration::days(2000); // divorce
        let t_late = now - Duration::days(1000); // marry Bob

        // Ingest order deliberately scrambled vs. world-time order.
        let divorce_id = engine
            .store_temporal(agent, make_entry(agent, "divorced", vec![]), spouse_meta("none", t_mid))
            .await
            .unwrap();
        engine
            .store_temporal(agent, make_entry(agent, "married Alice", vec![]), spouse_meta("Alice", t_early))
            .await
            .unwrap();
        let bob_id = engine
            .store_temporal(agent, make_entry(agent, "married Bob", vec![]), spouse_meta("Bob", t_late))
            .await
            .unwrap();

        let at = |r: Option<TemporalRecord>| r.map(|x| x.content);
        assert_eq!(
            at(engine.get_at(agent, "person:me", "spouse", t_early + Duration::days(500)).await.unwrap()).as_deref(),
            Some("married Alice")
        );
        assert_eq!(
            at(engine.get_at(agent, "person:me", "spouse", t_mid + Duration::days(500)).await.unwrap()).as_deref(),
            Some("divorced")
        );
        assert_eq!(
            at(engine.get_at(agent, "person:me", "spouse", t_late + Duration::days(500)).await.unwrap()).as_deref(),
            Some("married Bob")
        );

        // Full chain preserved; the reigning supersession links divorce → Bob.
        let hist = engine.get_history(agent, "person:me", "spouse").await.unwrap();
        assert_eq!(hist.len(), 3, "all three world-time segments retained");
        let bob = hist.iter().find(|r| r.id == bob_id).unwrap();
        assert_eq!(bob.supersedes.as_deref(), Some(divorce_id.as_str()));
        let divorce = hist.iter().find(|r| r.id == divorce_id).unwrap();
        assert_eq!(divorce.superseded_by.as_deref(), Some(bob_id.as_str()));

        // Only the currently-valid (Bob) fact surfaces in ordinary search.
        let hits = engine.search(agent, "married", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, bob_id);
    }

    /// A historical-segment insert (earlier valid_from) does NOT disturb the
    /// reigning fact and is bounded by the next-known fact's start.
    #[tokio::test]
    async fn historical_segment_does_not_touch_current() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "hist-seg";
        let now = Utc::now();
        let current_id = engine
            .store_temporal(
                agent,
                make_entry(agent, "current spouse", vec![]),
                spouse_meta("Current", now - Duration::days(100)),
            )
            .await
            .unwrap();
        // Insert an older fact — it must NOT expire the current one.
        engine
            .store_temporal(
                agent,
                make_entry(agent, "old spouse", vec![]),
                spouse_meta("Old", now - Duration::days(500)),
            )
            .await
            .unwrap();

        // Current fact is still the active one.
        let at_now = engine.get_at(agent, "person:me", "spouse", now).await.unwrap();
        assert_eq!(at_now.unwrap().id, current_id);
        // The old fact is bounded and excluded from search.
        let hits = engine.search(agent, "spouse", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "only the current fact is currently-valid");
        assert_eq!(hits[0].id, current_id);
    }

    /// Re-observing the same (subject, predicate, object) + content reaffirms the
    /// existing row instead of inserting a new one: no chain growth, the new
    /// source_event lands in `reaffirmed_by`, and access_count is bumped.
    #[tokio::test]
    async fn reaffirm_appends_event_and_bumps_access() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "reaffirm";
        let meta1 = TemporalMeta {
            subject: Some("user".into()),
            predicate: Some("lang".into()),
            object: Some("python".into()),
            source_event: Some("ev1".into()),
            ..Default::default()
        };
        let id1 = engine
            .store_temporal(agent, make_entry(agent, "likes python", vec![]), meta1)
            .await
            .unwrap();

        let meta2 = TemporalMeta {
            subject: Some("user".into()),
            predicate: Some("lang".into()),
            object: Some("python".into()),
            source_event: Some("ev2".into()),
            ..Default::default()
        };
        let id2 = engine
            .store_temporal(agent, make_entry(agent, "likes python", vec![]), meta2)
            .await
            .unwrap();

        assert_eq!(id2, id1, "reaffirm returns the surviving row id");
        let hist = engine.get_history(agent, "user", "lang").await.unwrap();
        assert_eq!(hist.len(), 1, "no new row inserted on reaffirm");
        assert!(hist[0].reaffirmed_by.contains(&"ev2".to_string()));
        assert!(!hist[0].reaffirmed_by.contains(&"ev1".to_string()), "the original write is not a reaffirm");

        let entries = engine.get_by_ids(agent, &[id1.clone()]).await.unwrap();
        assert_eq!(entries[0].access_count, 1, "access_count bumped on reaffirm");
    }

    /// A changed object supersedes (not reaffirms) — reaffirm must be object-exact.
    #[tokio::test]
    async fn changed_object_supersedes_not_reaffirms() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "reaffirm-neg";
        engine
            .store_temporal(agent, make_entry(agent, "old", vec![]), triple_meta("s", "p", "v1"))
            .await
            .unwrap();
        engine
            .store_temporal(agent, make_entry(agent, "new", vec![]), triple_meta("s", "p", "v2"))
            .await
            .unwrap();
        let hist = engine.get_history(agent, "s", "p").await.unwrap();
        assert_eq!(hist.len(), 2, "different object → real supersession, two rows");
    }

    /// `ingested_at` is populated on every temporal write (transaction-time axis).
    #[tokio::test]
    async fn ingested_at_is_populated() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "ingested";
        engine
            .store_temporal(agent, make_entry(agent, "x", vec![]), triple_meta("s", "p", "o"))
            .await
            .unwrap();
        let hist = engine.get_history(agent, "s", "p").await.unwrap();
        assert!(hist[0].ingested_at.is_some(), "ingested_at recorded");
    }

    // ── D1: invalidate_by_origin (source rollback) ──────────────────────────────

    /// Purging an origin expires its currently-valid facts (search no longer
    /// returns them), preserves the full history, stamps `origin_purge`
    /// provenance, and cascades a trust downgrade to derived facts.
    #[tokio::test]
    async fn invalidate_by_origin_expires_preserves_and_cascades() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "purge";

        // Poisoned fact from a bad channel (has a triple for history lookup).
        let f1 = engine
            .store_temporal(
                agent,
                make_entry(agent, "poison findmeee", vec![]),
                TemporalMeta {
                    subject: Some("topic".into()),
                    predicate: Some("fact".into()),
                    object: Some("a".into()),
                    origin: Some("chan-bad".into()),
                    origin_trust: Some(0.3),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // A fact derived from the poisoned one (trust clamped to 0.3 at store).
        let f2 = engine
            .store_temporal(
                agent,
                make_entry(agent, "derived summary", vec![]),
                TemporalMeta {
                    origin: Some("distill".into()),
                    origin_trust: Some(0.9),
                    derived_from: Some(vec![f1.clone()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(engine.get_origin_trust(agent, &f2).await.unwrap(), Some(0.3));

        // A clean fact from a different origin sharing the search keyword.
        let f3 = engine
            .store_temporal(
                agent,
                make_entry(agent, "clean findmeee", vec![]),
                TemporalMeta { origin: Some("chan-good".into()), ..Default::default() },
            )
            .await
            .unwrap();

        let expired = engine.invalidate_by_origin(agent, "chan-bad", None).await.unwrap();
        assert_eq!(expired, 1, "only the exact bad-origin fact is expired");

        // Search no longer returns the poisoned fact, but the clean one remains.
        let hits = engine.search(agent, "findmeee", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, f3);

        // History is preserved with build-time provenance.
        let hist = engine.get_history(agent, "topic", "fact").await.unwrap();
        let purged = hist.iter().find(|r| r.id == f1).unwrap();
        assert!(purged.valid_until.is_some(), "purged row is expired, not deleted");
        assert_eq!(purged.invalidated_by_event.as_deref(), Some("origin_purge"));

        // Cascade: the derived fact's trust is floored to 0.1.
        assert_eq!(engine.get_origin_trust(agent, &f2).await.unwrap(), Some(0.1));
    }

    /// Origin match is EXACT — `chan` must not purge `chan-extra` (no substring).
    #[tokio::test]
    async fn invalidate_by_origin_is_exact_not_substring() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "purge-exact";
        engine
            .store_temporal(
                agent,
                make_entry(agent, "exact aaa", vec![]),
                TemporalMeta { origin: Some("chan".into()), ..Default::default() },
            )
            .await
            .unwrap();
        let keep = engine
            .store_temporal(
                agent,
                make_entry(agent, "prefix bbb", vec![]),
                TemporalMeta { origin: Some("chan-extra".into()), ..Default::default() },
            )
            .await
            .unwrap();

        let expired = engine.invalidate_by_origin(agent, "chan", None).await.unwrap();
        assert_eq!(expired, 1);
        let hits = engine.search(agent, "bbb", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "the substring-prefixed origin is untouched");
        assert_eq!(hits[0].id, keep);
    }

    /// The `since` transaction-time filter bounds what gets purged.
    #[tokio::test]
    async fn invalidate_by_origin_honours_since() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "purge-since";
        engine
            .store_temporal(
                agent,
                make_entry(agent, "sinceable ccc", vec![]),
                TemporalMeta { origin: Some("chan".into()), ..Default::default() },
            )
            .await
            .unwrap();

        // A cutoff in the future matches nothing learned before it.
        let future = Utc::now() + Duration::days(1);
        assert_eq!(engine.invalidate_by_origin(agent, "chan", Some(future)).await.unwrap(), 0);

        // A cutoff in the past matches the row.
        let past = Utc::now() - Duration::days(1);
        assert_eq!(engine.invalidate_by_origin(agent, "chan", Some(past)).await.unwrap(), 1);
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

    // ── D2 quarantine ─────────────────────────────────────────────────────

    fn quarantined_meta(subject: &str, predicate: &str, object: &str) -> TemporalMeta {
        TemporalMeta {
            subject: Some(subject.to_string()),
            predicate: Some(predicate.to_string()),
            object: Some(object.to_string()),
            quarantined: true,
            ..Default::default()
        }
    }

    /// A quarantined fact is invisible to `search` / `search_layer` until it is
    /// released, then it surfaces.
    #[tokio::test]
    async fn quarantined_fact_excluded_from_search_until_released() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "q-agent";

        let e = make_entry(agent, "the vault password is hunter2", vec![]);
        let id = engine
            .store_temporal(agent, e, quarantined_meta("vault", "password_is", "hunter2"))
            .await
            .unwrap();
        assert_eq!(engine.is_quarantined(agent, &id).await.unwrap(), Some(true));

        // Excluded from both search entry points.
        assert!(engine.search(agent, "vault password", 10).await.unwrap().is_empty());
        assert!(engine
            .search_layer(agent, "vault password", &duduclaw_core::types::MemoryLayer::Semantic, 10)
            .await
            .unwrap()
            .is_empty());

        // Release → now visible.
        assert_eq!(engine.release_quarantine(agent, &[id.clone()]).await.unwrap(), 1);
        assert_eq!(engine.is_quarantined(agent, &id).await.unwrap(), Some(false));
        let hits = engine.search(agent, "vault password", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    /// A quarantined triple must NOT supersede a currently-valid clean fact —
    /// the core PoisonedRAG defense.
    #[tokio::test]
    async fn quarantined_fact_does_not_supersede_clean_fact() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "q-agent2";

        let clean = make_entry(agent, "capital of france is paris", vec![]);
        let clean_id = engine
            .store_temporal(agent, clean, triple_meta("france", "capital_is", "paris"))
            .await
            .unwrap();

        // Poison: same (subject, predicate), quarantined.
        let poison = make_entry(agent, "capital of france is berlin", vec![]);
        let poison_id = engine
            .store_temporal(agent, poison, quarantined_meta("france", "capital_is", "berlin"))
            .await
            .unwrap();

        // The clean fact is untouched (still valid, not superseded).
        let clean_row = engine.get_by_id(agent, &clean_id).await.unwrap().unwrap();
        assert_eq!(clean_row.id, clean_id);
        let hits = engine.search(agent, "capital of france", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "only the clean fact is retrievable");
        assert!(hits[0].content.contains("paris"));
        // Poison stays isolated.
        assert_eq!(engine.is_quarantined(agent, &poison_id).await.unwrap(), Some(true));
    }

    /// Rejecting a quarantined batch expires the rows and downgrades trust; the
    /// rows never resurface.
    #[tokio::test]
    async fn reject_quarantine_expires_and_downgrades_trust() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "q-agent3";

        let e = make_entry(agent, "poisoned claim about acme corp", vec![]);
        let mut meta = quarantined_meta("acme", "status_is", "bankrupt");
        meta.origin = Some("channel".to_string());
        meta.origin_trust = Some(0.3);
        let id = engine.store_temporal(agent, e, meta).await.unwrap();

        let n = engine
            .reject_quarantine(agent, &[id.clone()], "quarantine_reject")
            .await
            .unwrap();
        assert_eq!(n, 1);

        // Trust downgraded to ≤ 0.1.
        let trust = engine.get_origin_trust(agent, &id).await.unwrap().unwrap();
        assert!(trust <= 0.1 + f64::EPSILON, "trust must be downgraded: {trust}");
        // Still not retrievable (expired + still quarantined).
        assert!(engine.search(agent, "acme corp", 10).await.unwrap().is_empty());
        // The chain records the rejection event.
        let hist = engine.get_history(agent, "acme", "status_is").await.unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].invalidated_by_event.as_deref(), Some("quarantine_reject"));
    }

    // ── D2 w_trust ranking ────────────────────────────────────────────────

    /// With a positive `w_trust`, a low-trust fact ranks below a high-trust
    /// fact of otherwise-identical relevance; with `w_trust = 0` the trust
    /// dimension is inert and both orderings agree on the non-trust score.
    #[tokio::test]
    async fn w_trust_pushes_low_trust_fact_down() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "trust-rank";

        // Two equally-relevant matches, same age/importance, differing only in
        // origin_trust. The high-trust one must rank first under default weights.
        let high = make_entry(agent, "widget price is 100 dollars", vec![]);
        let high_meta = TemporalMeta { origin_trust: Some(1.0), ..Default::default() };
        engine.store_temporal(agent, high, high_meta).await.unwrap();

        let low = make_entry(agent, "widget price is 999 dollars", vec![]);
        let low_meta = TemporalMeta {
            origin: Some("channel".to_string()),
            origin_trust: Some(0.3),
            ..Default::default()
        };
        engine.store_temporal(agent, low, low_meta).await.unwrap();

        let results = engine.search(agent, "widget price", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results[0].content.contains("100"),
            "high-trust fact must rank first, got: {}",
            results[0].content
        );
    }

    /// The default `RetrievalWeights` keeps the same total "budget" feel: the
    /// 0.05 taken from `w_fts` funds `w_trust`.
    #[test]
    fn default_weights_shift_from_fts_to_trust() {
        let w = RetrievalWeights::default();
        assert!((w.w_fts - 0.35).abs() < 1e-12);
        assert!((w.w_trust - 0.10).abs() < 1e-12);
    }

    // ── D3.1 persistent graph cache ───────────────────────────────────────

    /// Store `n` distinct triples all sharing an "hub" object so a query on
    /// "hub" seeds a connected component, guaranteeing a non-None graph rank.
    async fn seed_hub_triples(engine: &SqliteMemoryEngine, agent: &str, n: usize) {
        for i in 0..n {
            let e = make_entry(agent, &format!("person {i} knows the hub"), vec![]);
            engine
                .store_temporal(agent, e, triple_meta(&format!("person{i}"), "knows", "hub"))
                .await
                .unwrap();
        }
    }

    fn bits(v: &Option<Vec<(String, f64)>>) -> Vec<(String, u64)> {
        v.as_ref()
            .map(|r| r.iter().map(|(id, s)| (id.clone(), s.to_bits())).collect())
            .unwrap_or_default()
    }

    /// D3.1 byte-identical guarantee: for a >500-triple agent the cache-hit
    /// result equals the fresh-build result bit-for-bit.
    #[tokio::test]
    async fn graph_cache_hit_is_byte_identical() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "cache-agent";
        seed_hub_triples(&engine, agent, GRAPH_CACHE_MIN_TRIPLES + 1).await;

        let now = Utc::now().to_rfc3339();
        let conn = engine.conn_for_maintenance().await;
        // First call: cache miss → fresh build (also populates the cache).
        let fresh = engine
            .graph_rank_cached(&conn, agent, "hub", &now)
            .unwrap();
        // Second call: cache hit.
        let cached = engine
            .graph_rank_cached(&conn, agent, "hub", &now)
            .unwrap();
        assert!(fresh.is_some(), "hub query must seed the connected component");
        assert_eq!(bits(&fresh), bits(&cached), "cache hit must be byte-identical to fresh build");

        // Cache is actually populated for a large agent.
        assert!(engine.graph_cache.read().unwrap().contains_key(agent));
    }

    /// D3.1: below the threshold the cache is never populated (behaviour is the
    /// per-query fresh build), yet the result is still correct.
    #[tokio::test]
    async fn small_graph_is_not_cached() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "small-agent";
        seed_hub_triples(&engine, agent, 3).await;

        let now = Utc::now().to_rfc3339();
        let conn = engine.conn_for_maintenance().await;
        let r = engine.graph_rank_cached(&conn, agent, "hub", &now).unwrap();
        assert!(r.is_some());
        assert!(
            !engine.graph_cache.read().unwrap().contains_key(agent),
            "sub-threshold agent must not be cached"
        );
    }

    /// D3.1: a triple-mutating write bumps the generation so a stale cache is
    /// rebuilt — the newly-stored fact appears on the next query.
    #[tokio::test]
    async fn write_bumps_generation_and_rebuilds_cache() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "gen-agent";
        seed_hub_triples(&engine, agent, GRAPH_CACHE_MIN_TRIPLES + 1).await;

        let now = Utc::now().to_rfc3339();
        {
            let conn = engine.conn_for_maintenance().await;
            let _ = engine.graph_rank_cached(&conn, agent, "hub", &now).unwrap();
        }
        let gen_before = engine.graph_generation(agent);

        // A new triple on a fresh subject connected to "hub".
        let e = make_entry(agent, "newcomer knows the hub", vec![]);
        engine
            .store_temporal(agent, e, triple_meta("newcomer", "knows", "hub"))
            .await
            .unwrap();
        assert!(engine.graph_generation(agent) > gen_before, "store must bump generation");

        // Next query rebuilds; "newcomer" now seeds and is retrievable.
        let now2 = Utc::now().to_rfc3339();
        let conn = engine.conn_for_maintenance().await;
        let r = engine
            .graph_rank_cached(&conn, agent, "newcomer", &now2)
            .unwrap()
            .unwrap();
        assert!(!r.is_empty(), "rebuilt graph must include the new triple");
    }

    // ── D3.2 entity aliases ───────────────────────────────────────────────

    #[tokio::test]
    async fn alias_add_list_remove_roundtrip() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "alias-agent";
        engine.add_entity_alias(agent, "李老闆", "老闆").await.unwrap();
        engine.add_entity_alias(agent, "李老闆", "zhixu").await.unwrap();
        let list = engine.list_entity_aliases(agent).await.unwrap();
        // Both aliases stored under the canonical, normalized (lowercased).
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|(c, _)| c == "李老闆"));
        assert!(engine.remove_entity_alias(agent, "老闆").await.unwrap());
        assert_eq!(engine.list_entity_aliases(agent).await.unwrap().len(), 1);
        // Removing a non-existent alias is a no-op false.
        assert!(!engine.remove_entity_alias(agent, "nope").await.unwrap());
    }

    /// Alias chains are flattened on store: `a → b` where `b → c` stores `a → c`.
    #[tokio::test]
    async fn alias_chain_is_flattened() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "alias-chain";
        engine.add_entity_alias(agent, "c", "b").await.unwrap();
        engine.add_entity_alias(agent, "b", "a").await.unwrap();
        let list = engine.list_entity_aliases(agent).await.unwrap();
        // "a" must resolve directly to "c" (flattened), not to "b".
        assert!(
            list.iter().any(|(canon, al)| canon == "c" && al == "a"),
            "chain must flatten a→c, got {list:?}"
        );
    }

    /// A search querying an alias surfaces the fact stored under the canonical
    /// entity — the seeding-hit-rate win D3.2 is about.
    #[tokio::test]
    async fn alias_makes_canonical_fact_retrievable_by_alias() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "alias-search";
        // A triple keyed on the canonical entity; the memory content deliberately
        // does NOT contain the alias, so only graph seeding can bridge it.
        let e = make_entry(agent, "李老闆 prefers oolong", vec![]);
        engine
            .store_temporal(agent, e, triple_meta("李老闆", "prefers", "oolong"))
            .await
            .unwrap();

        engine.add_entity_alias(agent, "李老闆", "老闆").await.unwrap();
        // Query the alias only.
        let hits = engine.search(agent, "老闆", 5).await.unwrap();
        assert!(
            hits.iter().any(|m| m.content.contains("oolong")),
            "alias query must reach the canonical fact via graph seeding"
        );
    }

    // ── D3.3 graph export ─────────────────────────────────────────────────

    #[tokio::test]
    async fn export_graph_reports_nodes_edges_and_quarantine() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "export-agent";
        engine
            .store_temporal(agent, make_entry(agent, "alice knows bob", vec![]),
                triple_meta("alice", "knows", "bob"))
            .await
            .unwrap();
        // A quarantined triple must still appear in the export, flagged.
        let mut qmeta = triple_meta("mallory", "claims", "admin");
        qmeta.quarantined = true;
        engine
            .store_temporal(agent, make_entry(agent, "mallory claims admin", vec![]), qmeta)
            .await
            .unwrap();

        let export = engine.export_graph(agent, 500).await.unwrap();
        assert_eq!(export.edges.len(), 2);
        assert!(!export.truncated);
        assert!(export.edges.iter().any(|e| e.quarantined && e.subject == "mallory"));
        assert!(export.edges.iter().any(|e| !e.quarantined && e.subject == "alice"));
        // Degrees: alice, bob, mallory, admin all degree 1.
        assert!(export.nodes.iter().any(|n| n.entity == "alice" && n.degree == 1));
        assert_eq!(export.nodes.len(), 4);
    }

    #[tokio::test]
    async fn export_graph_truncates_at_limit() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "export-trunc";
        for i in 0..5 {
            engine
                .store_temporal(agent, make_entry(agent, &format!("f{i}"), vec![]),
                    triple_meta(&format!("s{i}"), "rel", &format!("o{i}")))
                .await
                .unwrap();
        }
        let export = engine.export_graph(agent, 2).await.unwrap();
        assert_eq!(export.edges.len(), 2);
        assert!(export.truncated, "more rows than limit → truncated");
    }

    // ── D3.4 embedding seeding (default off = byte-identical) ──────────────

    /// With an embedder attached but `graph_embed_seed = false` (default), graph
    /// ranking is byte-identical to the no-embed-seed path.
    #[tokio::test]
    async fn embed_seed_off_is_byte_identical() {
        use std::sync::Arc;
        let agent = "embed-off";
        // Baseline: no embedder.
        let plain = SqliteMemoryEngine::in_memory().unwrap();
        seed_hub_triples(&plain, agent, 3).await;
        // With embedder attached but embed seeding OFF.
        let mut withemb = SqliteMemoryEngine::in_memory().unwrap();
        withemb = withemb.with_embedder(Arc::new(crate::vector::NgramHashEmbedder::new()));
        assert!(!withemb.retrieval_weights.graph_embed_seed);
        seed_hub_triples(&withemb, agent, 3).await;

        let now = Utc::now().to_rfc3339();
        let a = {
            let c = plain.conn_for_maintenance().await;
            plain.graph_rank_cached(&c, agent, "hub", &now).unwrap()
        };
        let b = {
            let c = withemb.conn_for_maintenance().await;
            withemb.graph_rank_cached(&c, agent, "hub", &now).unwrap()
        };
        // The two engines carry independent random memory ids, so compare the
        // PPR mass *distribution* (sorted score bits): identical topology ⇒
        // identical scores whether or not an idle embedder is attached.
        let score_bits = |v: &Option<Vec<(String, f64)>>| -> Vec<u64> {
            let mut s: Vec<u64> = v
                .as_ref()
                .map(|r| r.iter().map(|(_, sc)| sc.to_bits()).collect())
                .unwrap_or_default();
            s.sort_unstable();
            s
        };
        assert_eq!(
            score_bits(&a),
            score_bits(&b),
            "embed-seed-off must not change graph ranking"
        );
    }

    /// With `graph_embed_seed = true` + an embedder, entity vectors are lazily
    /// populated and embedding-nearest entities can seed even when the query
    /// shares no whole word with an entity name.
    #[tokio::test]
    async fn embed_seed_on_populates_and_seeds() {
        use std::sync::Arc;
        let agent = "embed-on";
        let mut engine = SqliteMemoryEngine::in_memory().unwrap();
        engine = engine.with_embedder(Arc::new(crate::vector::NgramHashEmbedder::new()));
        engine.retrieval_weights.graph_embed_seed = true;
        seed_hub_triples(&engine, agent, 3).await;

        let now = Utc::now().to_rfc3339();
        let conn = engine.conn_for_maintenance().await;
        // Trigger a build so entity embeddings are lazily populated.
        let _ = engine.graph_rank_cached(&conn, agent, "hub", &now).unwrap();
        let embedded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entity_embedding WHERE agent_id = ?1",
                params![agent],
                |r| r.get(0),
            )
            .unwrap();
        assert!(embedded > 0, "entity vectors must be lazily populated when embed seeding is on");
    }
}
