//! Semantic vector retrieval layer — the third memory re-rank signal
//! (`w_vec`) alongside `w_fts` (keyword) and `w_graph` (Personalized PageRank).
//!
//! ## Design (per the 2026-07 scoping decision, Option C)
//!
//! - **Pluggable embedder** via the [`EmbeddingProvider`] trait. The shipped
//!   default is [`NgramHashEmbedder`] — a zero-dependency, fully-local,
//!   deterministic char-n-gram feature-hashing embedder. It is CJK-strong
//!   (n-grams over `chars()`, never bytes) and needs no model download, so the
//!   whole `w_vec` path is live-verifiable today. A real dense model
//!   (EmbeddingGemma via `duduclaw-inference`) is a drop-in future provider.
//!
//! - **Storage** = two additive columns on `memories` (`embedding BLOB`,
//!   `embedding_model TEXT`), following the v1.19.0 idempotent-migration
//!   convention. Old rows keep `NULL` and simply skip the vector signal.
//!
//! - **KNN** = brute-force cosine over the agent's currently-valid rows (free
//!   functions on `&Connection`, mirroring `graph_rank.rs`). Personal-scale
//!   memory DBs are microseconds here. `sqlite-vec`'s `vec0` ANN index is the
//!   documented scale-up backend (trigger: >10k embedded rows or P95 KNN
//!   latency budget exceeded) — deliberately NOT built now, matching the
//!   research doc's own "measure before you partition" philosophy.
//!
//! ## Hard invariants
//!
//! 1. **No signal ⇒ byte-identical.** No embedder, no embedded rows, or an
//!    empty KNN result all leave ranking exactly as the FTS/graph path produced
//!    it.
//! 2. **Embedder identity is bound to every vector.** A row stores the
//!    `embedding_model` id that produced its vector; cosine is computed ONLY
//!    between vectors from the *same* model id (and same dimension). Switching
//!    providers never mixes incompatible spaces — mismatched rows are skipped
//!    until re-embedded.
//! 3. **KNN respects agent isolation + temporal validity** in SQL, identical to
//!    the FTS/graph queries — no cross-agent or superseded-fact leakage.

use rusqlite::{params, Connection};

use crate::embedding::cosine_similarity;
use duduclaw_core::error::{DuDuClawError, Result};

/// Default embedding dimension for [`NgramHashEmbedder`].
pub const NGRAM_DIM: usize = 256;
/// Stable id for the default embedder (persisted next to each vector so a
/// future provider swap never cross-computes cosine on a different space).
pub const NGRAM_MODEL_ID: &str = "ngram-hash-v1";
/// Upper bound on rows scanned in one brute-force KNN pass; if the agent has
/// more embedded rows than this the scan is capped (and the cap is logged —
/// never silently truncated).
pub const MAX_KNN_SCAN: usize = 20_000;

/// Produces a fixed-dimension embedding for a piece of text.
///
/// Implementors MUST be deterministic across process restarts and versions for
/// a given [`id`](EmbeddingProvider::id): stored vectors are compared later, so
/// a non-stable hash would silently corrupt similarity after a restart.
pub trait EmbeddingProvider: Send + Sync {
    /// Stable identifier for the embedding space (persisted with each vector).
    fn id(&self) -> &str;
    /// Output dimension.
    fn dim(&self) -> usize;
    /// Embed `text` into a `dim`-length vector (ideally L2-normalized).
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// FNV-1a 64-bit — a *stable* hash (unlike `DefaultHasher`, whose output may
/// change across std versions). Stability matters because embeddings are
/// persisted and compared across restarts.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Zero-dependency deterministic embedder: signed feature-hashing of character
/// bi-grams and tri-grams into a fixed-dimension L2-normalized vector.
///
/// Character n-grams (over `chars()`, so multi-byte / CJK safe) make this a
/// strong lexical-fuzzy signal: it catches morphological / substring overlap
/// that exact FTS token matching misses, and works well for CJK where word
/// segmentation is hard. Not a semantic dense model — that's the future
/// EmbeddingGemma provider — but real, useful, and fully verifiable offline.
pub struct NgramHashEmbedder {
    dim: usize,
}

impl Default for NgramHashEmbedder {
    fn default() -> Self {
        Self { dim: NGRAM_DIM }
    }
}

impl NgramHashEmbedder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_dim(dim: usize) -> Self {
        Self { dim: dim.max(8) }
    }

    /// Accumulate one n-gram into the signed feature-hash accumulator.
    fn add_ngram(&self, acc: &mut [f32], ngram: &str) {
        let h = fnv1a(ngram.as_bytes());
        let bucket = (h % self.dim as u64) as usize;
        // Independent sign bit from the high half of the hash → signed hashing,
        // which halves the collision bias of unsigned feature hashing.
        let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
        acc[bucket] += sign;
    }
}

impl EmbeddingProvider for NgramHashEmbedder {
    fn id(&self) -> &str {
        NGRAM_MODEL_ID
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut acc = vec![0.0f32; self.dim];
        // CJK-safe: iterate Unicode scalar values, never bytes.
        let chars: Vec<char> = text.trim().to_lowercase().chars().collect();
        if chars.is_empty() {
            return Ok(acc); // zero vector; cosine against it is 0 → no signal
        }
        // Unigrams give a floor of signal for very short inputs; bi/tri-grams
        // carry the discriminative weight.
        for c in &chars {
            let mut buf = [0u8; 4];
            self.add_ngram(&mut acc, c.encode_utf8(&mut buf));
        }
        for w in chars.windows(2) {
            let g: String = w.iter().collect();
            self.add_ngram(&mut acc, &g);
        }
        for w in chars.windows(3) {
            let g: String = w.iter().collect();
            self.add_ngram(&mut acc, &g);
        }
        // L2 normalize so cosine == dot product and magnitudes don't bias.
        let norm: f32 = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in acc.iter_mut() {
                *x /= norm;
            }
        }
        Ok(acc)
    }
}

/// Encode an `f32` vector as little-endian bytes for BLOB storage.
pub fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian `f32` BLOB. Returns `None` on a malformed (non-4-byte
/// multiple) blob rather than panicking.
pub fn decode_vec(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(out)
}

/// Persist (upsert) a memory row's embedding + the model id that produced it.
/// Agent isolation is enforced in the `WHERE` clause.
pub fn store_embedding(
    conn: &Connection,
    agent_id: &str,
    memory_id: &str,
    model_id: &str,
    vector: &[f32],
) -> Result<()> {
    let blob = encode_vec(vector);
    conn.execute(
        "UPDATE memories SET embedding = ?1, embedding_model = ?2
         WHERE id = ?3 AND agent_id = ?4",
        params![blob, model_id, memory_id, agent_id],
    )
    .map_err(|e| DuDuClawError::Memory(format!("store embedding: {e}")))?;
    Ok(())
}

/// Cheap gate: number of currently-valid rows for `agent_id` that carry an
/// embedding from `model_id`.
pub fn count_embedded(
    conn: &Connection,
    agent_id: &str,
    model_id: &str,
    now_rfc: &str,
) -> Result<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM memories
         WHERE agent_id = ?1 AND embedding IS NOT NULL AND embedding_model = ?2
           AND (valid_until IS NULL OR valid_until > ?3)",
        params![agent_id, model_id, now_rfc],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
    .map_err(|e| DuDuClawError::Memory(format!("count embedded: {e}")))
}

/// Brute-force cosine KNN over the agent's currently-valid, same-model embedded
/// rows. Returns `(memory_id, cosine)` sorted descending, top `k`.
///
/// Returns `Ok(None)` — "skip the vector signal, leave ranking byte-identical"
/// — when there are no matching embedded rows or the query vector is empty.
/// Only rows whose `embedding_model` equals `model_id` AND whose decoded
/// dimension equals the query's are scored; any mismatch is skipped (never
/// cross-space compared).
pub fn vector_knn(
    conn: &Connection,
    agent_id: &str,
    query_vec: &[f32],
    model_id: &str,
    now_rfc: &str,
    k: usize,
) -> Result<Option<Vec<(String, f32)>>> {
    if query_vec.is_empty() || query_vec.iter().all(|x| *x == 0.0) {
        return Ok(None);
    }
    if count_embedded(conn, agent_id, model_id, now_rfc)? == 0 {
        return Ok(None);
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, embedding FROM memories
             WHERE agent_id = ?1 AND embedding IS NOT NULL AND embedding_model = ?2
               AND (valid_until IS NULL OR valid_until > ?3)
             LIMIT ?4",
        )
        .map_err(|e| DuDuClawError::Memory(format!("knn prepare: {e}")))?;

    let rows = stmt
        .query_map(
            params![agent_id, model_id, now_rfc, MAX_KNN_SCAN as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(|e| DuDuClawError::Memory(format!("knn query: {e}")))?;

    let mut scored: Vec<(String, f32)> = Vec::new();
    let mut scanned = 0usize;
    for row in rows {
        let (id, blob) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        scanned += 1;
        let v = match decode_vec(&blob) {
            Some(v) if v.len() == query_vec.len() => v,
            _ => continue, // malformed or different-dimension space: skip
        };
        let sim = cosine_similarity(query_vec, &v);
        if sim > 0.0 {
            scored.push((id, sim));
        }
    }
    if scanned >= MAX_KNN_SCAN {
        tracing::warn!(
            agent_id,
            model_id,
            cap = MAX_KNN_SCAN,
            "vector_knn hit scan cap — consider the sqlite-vec vec0 backend"
        );
    }
    if scored.is_empty() {
        return Ok(None);
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0)) // deterministic tie-break by id
    });
    scored.truncate(k.max(1));
    Ok(Some(scored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngram_embedder_is_deterministic_and_normalized() {
        let e = NgramHashEmbedder::new();
        let a = e.embed("hello world").unwrap();
        let b = e.embed("hello world").unwrap();
        assert_eq!(a, b, "same input → identical vector (stable hash)");
        assert_eq!(a.len(), NGRAM_DIM);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2 normalized");
    }

    #[test]
    fn similar_text_scores_higher_than_unrelated() {
        let e = NgramHashEmbedder::new();
        let q = e.embed("database migration schema").unwrap();
        let near = e.embed("schema migration for the database").unwrap();
        let far = e.embed("the weather is sunny today").unwrap();
        let s_near = cosine_similarity(&q, &near);
        let s_far = cosine_similarity(&q, &far);
        assert!(s_near > s_far, "related text closer: {s_near} vs {s_far}");
    }

    #[test]
    fn cjk_similarity_works() {
        let e = NgramHashEmbedder::new();
        let q = e.embed("記憶體向量檢索").unwrap();
        let near = e.embed("向量檢索記憶體實作").unwrap();
        let far = e.embed("今天天氣很好").unwrap();
        assert!(
            cosine_similarity(&q, &near) > cosine_similarity(&q, &far),
            "CJK n-gram overlap must score higher"
        );
    }

    #[test]
    fn empty_text_yields_zero_vector() {
        let e = NgramHashEmbedder::new();
        let v = e.embed("   ").unwrap();
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![1.0f32, -0.5, 0.25, 0.0];
        let enc = encode_vec(&v);
        assert_eq!(decode_vec(&enc), Some(v));
        assert_eq!(decode_vec(&[1, 2, 3]), None, "non-4-multiple → None");
        assert_eq!(decode_vec(&[]), None);
    }
}
