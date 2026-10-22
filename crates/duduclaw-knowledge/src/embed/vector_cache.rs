//! In-memory copy of one model's vectors for brute-force cosine search.
//!
//! Loaded on first query and reloaded when `kb_meta.generation` (bumped by
//! every chunk or vector change) or the model changes. Rows are filtered by
//! the caller's allowed collections **before** any similarity is computed,
//! so a collection the caller may not read cannot even take a top-k slot.
//!
//! Memory: 384-dim e5 × 50 000 chunks ≈ 77 MB (f32). `[knowledge.embedding]
//! max_vector_rows` stops new vectors past its limit.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::pooling;
use crate::KbResult;
use crate::acl::AllowedSet;

/// A loaded set of vectors.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub model_id: String,
    pub generation: i64,
    pub dim: usize,
    ids: Vec<i64>,
    coll_idx: Vec<u32>,
    collections: Vec<String>,
    data: Vec<f32>,
}

impl Snapshot {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn load(conn: &Connection, model_id: &str, dim: usize, generation: i64) -> KbResult<Self> {
        let mut snap = Snapshot {
            model_id: model_id.to_string(),
            generation,
            dim,
            ..Default::default()
        };
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT e.chunk_id, c.collection_id, e.vec FROM embeddings e JOIN chunks c ON c.id = e.chunk_id
              WHERE e.model_id = ?1 AND e.dim = ?2 ORDER BY e.chunk_id",
        )?;
        let mut rows = stmt.query(params![model_id, dim as i64])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let coll: String = r.get(1)?;
            let blob: Vec<u8> = r.get(2)?;
            let Some(v) = pooling::decode(&blob, dim) else {
                continue; // a malformed row is skipped, never misread
            };
            let next = snap.collections.len() as u32;
            let ci = *index.entry(coll.clone()).or_insert_with(|| {
                snap.collections.push(coll);
                next
            });
            snap.ids.push(id);
            snap.coll_idx.push(ci);
            snap.data.extend_from_slice(&v);
        }
        Ok(snap)
    }

    /// Top `k` `(chunk_id, cosine)` among rows whose collection is in
    /// `allowed` and not in `excluded`, best first, ties by chunk id.
    pub fn knn(
        &self,
        query: &[f32],
        allowed: &AllowedSet,
        excluded: &HashSet<String>,
        k: usize,
    ) -> Vec<(i64, f32)> {
        if query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let permitted: Vec<bool> = self
            .collections
            .iter()
            .map(|c| allowed.contains(c) && !excluded.contains(c))
            .collect();
        let mut scored: Vec<(i64, f32)> = Vec::new();
        for (row, (&id, &ci)) in self.ids.iter().zip(&self.coll_idx).enumerate() {
            if !permitted[ci as usize] {
                continue;
            }
            let v = &self.data[row * self.dim..(row + 1) * self.dim];
            scored.push((id, pooling::dot(query, v)));
        }
        let by_score = |a: &(i64, f32), b: &(i64, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        if scored.len() > k {
            scored.select_nth_unstable_by(k - 1, by_score);
            scored.truncate(k);
        }
        scored.sort_by(by_score);
        scored
    }
}

/// A reloading cache. Cheap to share (`Arc` snapshot inside).
#[derive(Debug, Default)]
pub struct VectorCache {
    current: Mutex<Option<Arc<Snapshot>>>,
}

impl VectorCache {
    /// The snapshot for `model_id` at the database's current generation.
    pub fn snapshot(
        &self,
        conn: &Connection,
        model_id: &str,
        dim: usize,
    ) -> KbResult<Arc<Snapshot>> {
        let generation = crate::schema::generation(conn)?;
        {
            let g = self.current.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(s) = g.as_ref()
                && s.generation == generation
                && s.model_id == model_id
                && s.dim == dim
            {
                return Ok(s.clone());
            }
        }
        let snap = Arc::new(Snapshot::load(conn, model_id, dim, generation)?);
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(snap.clone());
        Ok(snap)
    }

    /// Drop the loaded vectors (model removed or switched).
    pub fn clear(&self) {
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{HumanCtx, Narrowing, effective_collections};
    use crate::embed::vectors::store;
    use crate::index::test_support::indexed_source;
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

    fn chunk_of(conn: &Connection, source: &str) -> i64 {
        conn.query_row(
            "SELECT id FROM chunks WHERE source_id = ?1 LIMIT 1",
            params![source],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn acl_filter_runs_before_ranking() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let mine = create_collection(
            &mut conn,
            &NewCollection {
                name: "mine".into(),
                ..Default::default()
            },
            "me",
            &cfg,
        )
        .unwrap();
        let theirs = create_collection(
            &mut conn,
            &NewCollection {
                name: "theirs".into(),
                ..Default::default()
            },
            "them",
            &cfg,
        )
        .unwrap();
        let s_mine = indexed_source(&mut conn, &mine.id, "a.pdf", &["我的內容。"]);
        let s_theirs = indexed_source(&mut conn, &theirs.id, "b.pdf", &["別人的內容。"]);
        let (cm, ct) = (chunk_of(&conn, &s_mine), chunk_of(&conn, &s_theirs));
        // Their vector is identical to the query; mine is far from it.
        store(
            &mut conn,
            "m",
            &[(ct, vec![1.0, 0.0]), (cm, vec![0.0, 1.0])],
        )
        .unwrap();
        let allowed = effective_collections(
            &conn,
            &HumanCtx::dashboard("me", None, false),
            None,
            &Narrowing::default(),
        )
        .unwrap();
        let cache = VectorCache::default();
        let snap = cache.snapshot(&conn, "m", 2).unwrap();
        assert_eq!(snap.len(), 2);
        // k = 1: post-filtering would return nothing; pre-filtering returns mine.
        let top = snap.knn(&[1.0, 0.0], &allowed, &HashSet::new(), 1);
        assert_eq!(top, vec![(cm, 0.0)]);
        // Excluded collections are skipped the same way.
        let ex: HashSet<String> = [mine.id.clone()].into_iter().collect();
        assert!(snap.knn(&[1.0, 0.0], &allowed, &ex, 5).is_empty());
    }

    #[test]
    fn cache_reloads_on_generation_change_and_orders_deterministically() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let c = create_collection(
            &mut conn,
            &NewCollection {
                name: "c".into(),
                ..Default::default()
            },
            "me",
            &cfg,
        )
        .unwrap();
        let s1 = indexed_source(&mut conn, &c.id, "a.pdf", &["一。"]);
        let s2 = indexed_source(&mut conn, &c.id, "b.pdf", &["二。"]);
        let (c1, c2) = (chunk_of(&conn, &s1), chunk_of(&conn, &s2));
        let cache = VectorCache::default();
        store(&mut conn, "m", &[(c1, vec![0.6, 0.8])]).unwrap();
        assert_eq!(cache.snapshot(&conn, "m", 2).unwrap().len(), 1);
        store(&mut conn, "m", &[(c2, vec![0.6, 0.8])]).unwrap();
        let snap = cache.snapshot(&conn, "m", 2).unwrap();
        assert_eq!(snap.len(), 2, "generation bump reloads");
        let allowed = effective_collections(
            &conn,
            &HumanCtx::dashboard("me", None, false),
            None,
            &Narrowing::default(),
        )
        .unwrap();
        let top = snap.knn(&[0.6, 0.8], &allowed, &HashSet::new(), 5);
        assert_eq!(
            top.iter().map(|t| t.0).collect::<Vec<_>>(),
            vec![c1.min(c2), c1.max(c2)]
        );
        // A different dimension never matches.
        assert!(snap.knn(&[1.0], &allowed, &HashSet::new(), 5).is_empty());
    }
}
