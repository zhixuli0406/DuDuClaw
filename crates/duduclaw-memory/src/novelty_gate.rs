//! B1 — anti-false-surprise gate (arXiv:2606.29182: 37.5% of the "new
//! discoveries" a self-evolving agent writes into its own memory are
//! near-duplicates of beliefs it already holds).
//!
//! Zero-LLM, fully deterministic: reuses the char-n-gram feature-hashing
//! embedder ([`crate::vector::NgramHashEmbedder`]) already shipped for the
//! `w_vec` retrieval signal — no new dependency, CJK-safe by construction
//! (n-grams are computed over `chars()`, never bytes; see `vector.rs`'s own
//! doc comment for the rationale). This module only adds the *decision*
//! ("is this new text a near-duplicate of something the agent already
//! believes?") on top of the embedding infrastructure that already exists.
//!
//! ## Scope
//!
//! Candidates are restricted to the SAME agent, the SAME memory layer (a
//! semantic belief is only compared against other semantic beliefs — an
//! episodic log line about the same topic is not the same claim), the SAME
//! embedding model (never cross-space cosine), and only currently-valid rows
//! (`valid_until IS NULL OR valid_until > now`) — a fact that has already
//! been superseded can never block a new write.
//!
//! ## What this gate must NOT block
//!
//! F1 temporal supersession (`store_temporal` replacing the object of a
//! known `(subject, predicate)` with a new value) is a legitimate update, not
//! a "new discovery" — callers must not run this gate on that path. See
//! `engine.rs`'s `store_temporal` wiring: the gate only runs on writes that
//! do NOT carry an explicit `(subject, predicate)` triple.
//!
//! ## Hard invariant
//!
//! Matches `vector.rs`'s "no signal ⇒ byte-identical" convention: a disabled
//! config, no embedder attached, or a DB scan error all make this gate a
//! no-op (`Ok(None)` / `None` — proceed with the write). An engine that never
//! opts into the semantic embedder is completely unaffected by this module.

use rusqlite::{params, Connection};

use duduclaw_core::error::{DuDuClawError, Result};

use crate::embedding::cosine_similarity;
use crate::vector::{decode_vec, EmbeddingProvider};

/// Config for the B1 gate.
///
/// `threshold` defaults to 0.92, mirroring
/// `duduclaw_gateway::playbook::dedup::NEAR_DUP_COSINE` — the codebase's
/// existing near-duplicate precedent for the same `NgramHashEmbedder` cosine
/// space. Deliberately high: a false rejection here silently drops a
/// possibly-novel belief, which is worse than tolerating one redundant entry
/// that decay/capacity sweeps will eventually evict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoveltyGateConfig {
    /// Master on/off switch. When `false` the gate never rejects a write,
    /// regardless of embedder attachment.
    pub enabled: bool,
    /// Cosine similarity at/above which a new entry is rejected as a
    /// near-duplicate of an existing one.
    pub threshold: f32,
}

impl Default for NoveltyGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.92,
        }
    }
}

/// Why a write was rejected — carries enough detail for an audit log line
/// and for the caller's own follow-up decision (e.g. reflexion may still
/// mark the source mistakes resolved against the matched id, treating the
/// existing belief as already covering the new observation).
#[derive(Debug, Clone, PartialEq)]
pub struct NoveltyRejection {
    /// The id of the existing memory this write duplicates.
    pub matched_id: String,
    /// Cosine similarity against the matched memory (>= `threshold`).
    pub similarity: f32,
    /// The threshold that was in effect when this rejection was decided.
    pub threshold: f32,
}

impl std::fmt::Display for NoveltyRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "near-duplicate of existing memory {} (cosine {:.3} >= threshold {:.3})",
            self.matched_id, self.similarity, self.threshold
        )
    }
}

/// Fetch currently-valid, same-agent, same-layer, same-embedding-model
/// candidate vectors, capped at `MAX_KNN_SCAN` rows (mirrors
/// `vector::vector_knn`'s scan cap — personal-scale memory DBs never come
/// close to it; a cap protects pathological agents from an unbounded scan).
fn same_layer_candidates(
    conn: &Connection,
    agent_id: &str,
    layer: &str,
    model_id: &str,
    now_rfc: &str,
) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, embedding FROM memories
             WHERE agent_id = ?1 AND layer = ?2 AND embedding IS NOT NULL
               AND embedding_model = ?3
               AND (valid_until IS NULL OR valid_until > ?4)
               AND quarantined = 0
             LIMIT ?5",
        )
        .map_err(|e| DuDuClawError::Memory(format!("novelty gate prepare: {e}")))?;
    let rows = stmt
        .query_map(
            params![agent_id, layer, model_id, now_rfc, crate::vector::MAX_KNN_SCAN as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(|e| DuDuClawError::Memory(format!("novelty gate query: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        let (id, blob) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        if let Some(v) = decode_vec(&blob) {
            out.push((id, v));
        }
    }
    Ok(out)
}

/// The B1 gate. Returns:
/// - `Ok(None)` — novel (or gate disabled, or no candidates, or empty
///   content): caller should proceed with the write.
/// - `Ok(Some(rejection))` — a near-duplicate was found at/above threshold:
///   caller should NOT write, and should treat `rejection` as the audit
///   record.
/// - `Err` — a DB error while scanning candidates. Callers should treat this
///   as fail-open (same as `Ok(None)`) — a scan failure must never block a
///   legitimate write, matching this crate's convention for every other
///   optional retrieval signal (graph rank, vector KNN).
#[allow(clippy::too_many_arguments)]
pub fn check_novelty(
    conn: &Connection,
    agent_id: &str,
    layer: &str,
    content: &str,
    embedder: &dyn EmbeddingProvider,
    config: &NoveltyGateConfig,
    now_rfc: &str,
) -> Result<Option<NoveltyRejection>> {
    if !config.enabled {
        return Ok(None);
    }
    let query_vec = embedder.embed(content)?;
    if query_vec.iter().all(|x| *x == 0.0) {
        // Empty/whitespace content (or an embedder that produced a zero
        // vector) — nothing meaningful to compare, never block.
        return Ok(None);
    }
    let candidates = same_layer_candidates(conn, agent_id, layer, embedder.id(), now_rfc)?;
    if candidates.is_empty() {
        return Ok(None);
    }

    // Only the single closest match is ever reported (L6 simplification:
    // the config used to carry a `top_k` scan-then-truncate knob, but
    // truncating a full descending sort before taking the first element is
    // always equivalent to just taking the max — the field never changed
    // behavior and was removed). A manual fold (rather than `Iterator::
    // max_by`, which returns the LAST max on ties) preserves the original
    // stable-sort-then-take-first tie-breaking: the first candidate row
    // with the highest cosine wins.
    let best = candidates
        .iter()
        .map(|(id, v)| (id.clone(), cosine_similarity(&query_vec, v)))
        .fold(None::<(String, f32)>, |acc, cur| match &acc {
            Some(a) if a.1 >= cur.1 => acc,
            _ => Some(cur),
        });

    match best {
        Some((id, sim)) if sim >= config.threshold => Ok(Some(NoveltyRejection {
            matched_id: id,
            similarity: sim,
            threshold: config.threshold,
        })),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{store_embedding, NgramHashEmbedder};
    use rusqlite::Connection;

    /// Minimal `memories` table for these unit tests — only the columns
    /// `check_novelty` actually touches. The real schema (engine.rs) has
    /// many more columns; this stays deliberately narrow so the test doesn't
    /// silently drift from the gate's real SQL surface.
    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                layer TEXT NOT NULL,
                embedding BLOB,
                embedding_model TEXT,
                valid_until TEXT,
                quarantined INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        conn
    }

    fn insert_row(conn: &Connection, id: &str, agent: &str, layer: &str, content: &str, embedder: &NgramHashEmbedder) {
        conn.execute(
            "INSERT INTO memories (id, agent_id, layer, quarantined) VALUES (?1, ?2, ?3, 0)",
            params![id, agent, layer],
        )
        .unwrap();
        let v = embedder.embed(content).unwrap();
        store_embedding(conn, agent, id, embedder.id(), &v).unwrap();
    }

    #[test]
    fn near_identical_content_is_rejected() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "user prefers dark mode UI",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_some(), "byte-identical content must be rejected");
        let rejection = result.unwrap();
        assert_eq!(rejection.matched_id, "m1");
        assert!(rejection.similarity >= cfg.threshold);
    }

    #[test]
    fn distinct_content_passes() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "the quarterly budget review is due Friday",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "unrelated content must not be rejected");
    }

    #[test]
    fn cjk_near_duplicate_is_rejected_and_paraphrase_passes() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "使用者偏好深色模式介面", &embedder);

        let cfg = NoveltyGateConfig::default();

        // Same claim, same wording (byte-identical restated) → rejected.
        let dup = check_novelty(
            &conn, "agent-a", "semantic", "使用者偏好深色模式介面", &embedder, &cfg, "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(dup.is_some(), "CJK byte-identical restatement must be rejected");

        // Unrelated CJK content → passes.
        let novel = check_novelty(
            &conn, "agent-a", "semantic", "今天天氣很好，適合出門散步", &embedder, &cfg, "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(novel.is_none(), "unrelated CJK content must not be rejected");
    }

    #[test]
    fn different_layer_is_not_a_candidate() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        // Same content, but stored as episodic — must not block a semantic write.
        insert_row(&conn, "m1", "agent-a", "episodic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "user prefers dark mode UI",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "an episodic candidate must not gate a semantic write");
    }

    #[test]
    fn different_agent_is_not_a_candidate() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-other", "semantic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "user prefers dark mode UI",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "another agent's memory must never gate this agent's write");
    }

    #[test]
    fn superseded_row_is_not_a_candidate() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "user prefers dark mode UI", &embedder);
        conn.execute(
            "UPDATE memories SET valid_until = '2020-01-01T00:00:00Z' WHERE id = 'm1'",
            [],
        )
        .unwrap();

        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "user prefers dark mode UI",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "an already-superseded fact must never gate a new write");
    }

    #[test]
    fn disabled_gate_never_rejects() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig { enabled: false, ..NoveltyGateConfig::default() };
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "user prefers dark mode UI",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "disabled config must never reject");
    }

    #[test]
    fn empty_content_never_rejects() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        insert_row(&conn, "m1", "agent-a", "semantic", "user prefers dark mode UI", &embedder);

        let cfg = NoveltyGateConfig::default();
        let result =
            check_novelty(&conn, "agent-a", "semantic", "   ", &embedder, &cfg, "2026-08-06T00:00:00Z").unwrap();
        assert!(result.is_none(), "whitespace-only content must never be rejected");
    }

    #[test]
    fn empty_db_never_rejects() {
        let conn = setup_db();
        let embedder = NgramHashEmbedder::new();
        let cfg = NoveltyGateConfig::default();
        let result = check_novelty(
            &conn,
            "agent-a",
            "semantic",
            "anything at all",
            &embedder,
            &cfg,
            "2026-08-06T00:00:00Z",
        )
        .unwrap();
        assert!(result.is_none(), "no candidates ⇒ nothing to reject against");
    }
}
