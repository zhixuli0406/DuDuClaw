//! Vector rows in `embeddings`: what still needs a vector, reuse of an
//! identical passage's vector, writes, coverage.
//!
//! Under a cloud embedder, collections marked `restricted` or
//! `embedding_policy = local_only` are never sent: their chunks are not
//! "pending", get no vectors and stay keyword-only.

use std::collections::HashSet;

use rusqlite::{Connection, params};

use super::pooling;
use crate::KbResult;

/// Collections whose text must not reach a cloud provider.
pub fn cloud_excluded_collections(conn: &Connection) -> KbResult<HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM collections WHERE sensitivity = 'restricted' OR embedding_policy = 'local_only'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn excluded_json(excluded: &HashSet<String>) -> String {
    serde_json::to_string(&excluded.iter().collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
}

/// Give chunks of `source_id` without a `model_id` vector the vector of
/// another chunk with the same `text_sha256` (same text ⇒ same vector).
/// Returns how many were filled.
pub fn reuse_by_sha(
    conn: &Connection,
    source_id: &str,
    model_id: &str,
    excluded: &HashSet<String>,
) -> KbResult<usize> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO embeddings(chunk_id, model_id, dim, vec, created_at)
         SELECT c.id, e.model_id, e.dim, e.vec, ?4
           FROM chunks c
           JOIN embeddings e ON e.model_id = ?2 AND e.chunk_id = (
                SELECT c2.id FROM chunks c2 JOIN embeddings e2 ON e2.chunk_id = c2.id AND e2.model_id = ?2
                 WHERE c2.text_sha256 = c.text_sha256 AND c2.id <> c.id LIMIT 1)
          WHERE c.source_id = ?1
            AND c.collection_id NOT IN (SELECT value FROM json_each(?3))
            AND NOT EXISTS (SELECT 1 FROM embeddings x WHERE x.chunk_id = c.id AND x.model_id = ?2)",
        params![source_id, model_id, excluded_json(excluded), crate::now_rfc3339()],
    )?;
    if n > 0 {
        crate::schema::bump_generation(conn)?;
    }
    Ok(n)
}

/// Up to `limit` chunks of `source_id` still without a `model_id` vector,
/// as `(chunk_id, text)`, in document order.
pub fn pending(
    conn: &Connection,
    source_id: &str,
    model_id: &str,
    excluded: &HashSet<String>,
    limit: usize,
) -> KbResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.id, c.text FROM chunks c
          WHERE c.source_id = ?1
            AND c.collection_id NOT IN (SELECT value FROM json_each(?3))
            AND NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.chunk_id = c.id AND e.model_id = ?2)
          ORDER BY c.ord LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![source_id, model_id, excluded_json(excluded), limit as i64],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Count of chunks of `source_id` still pending.
pub fn pending_count(
    conn: &Connection,
    source_id: &str,
    model_id: &str,
    excluded: &HashSet<String>,
) -> KbResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM chunks c
          WHERE c.source_id = ?1
            AND c.collection_id NOT IN (SELECT value FROM json_each(?3))
            AND NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.chunk_id = c.id AND e.model_id = ?2)",
        params![source_id, model_id, excluded_json(excluded)],
        |r| r.get(0),
    )?)
}

/// Write vectors (replacing any of the same model) and bump the generation.
/// Rows whose chunk no longer exists are skipped (deleted meanwhile).
pub fn store(conn: &mut Connection, model_id: &str, rows: &[(i64, Vec<f32>)]) -> KbResult<usize> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = crate::now_rfc3339();
    let mut n = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO embeddings(chunk_id, model_id, dim, vec, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM chunks WHERE id = ?1)",
        )?;
        for (id, v) in rows {
            n += stmt.execute(params![
                id,
                model_id,
                v.len() as i64,
                pooling::encode(v),
                now
            ])?;
        }
    }
    crate::schema::bump_generation(&tx)?;
    tx.commit()?;
    Ok(n)
}

/// Vector rows of `model_id`.
pub fn vector_rows(conn: &Connection, model_id: &str) -> KbResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE model_id = ?1",
        params![model_id],
        |r| r.get(0),
    )?)
}

/// `(with a vector, eligible)` chunk counts over `collections` (all when
/// `None`), excluding `excluded` collections from both.
pub fn coverage(
    conn: &Connection,
    model_id: &str,
    collections: Option<&[String]>,
    excluded: &HashSet<String>,
) -> KbResult<(i64, i64)> {
    let only = collections.map(|c| serde_json::to_string(c).unwrap_or_else(|_| "[]".into()));
    Ok(conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM chunks c JOIN embeddings e ON e.chunk_id = c.id AND e.model_id = ?1
             WHERE (?2 IS NULL OR c.collection_id IN (SELECT value FROM json_each(?2)))
               AND c.collection_id NOT IN (SELECT value FROM json_each(?3))),
           (SELECT COUNT(*) FROM chunks c
             WHERE (?2 IS NULL OR c.collection_id IN (SELECT value FROM json_each(?2)))
               AND c.collection_id NOT IN (SELECT value FROM json_each(?3)))",
        params![model_id, only, excluded_json(excluded)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Ready sources that still miss `model_id` vectors and have no embed job
/// queued or running.
pub fn sources_needing(
    conn: &Connection,
    model_id: &str,
    excluded: &HashSet<String>,
) -> KbResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT s.id FROM sources s
          WHERE s.status IN ('ready','embedding')
            AND s.collection_id NOT IN (SELECT value FROM json_each(?2))
            AND EXISTS (SELECT 1 FROM chunks c WHERE c.source_id = s.id
                         AND NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.chunk_id = c.id AND e.model_id = ?1))
            AND NOT EXISTS (SELECT 1 FROM ingest_jobs j WHERE j.source_id = s.id
                             AND j.state IN ('queued','running'))
          ORDER BY s.created_at, s.id",
    )?;
    let rows = stmt.query_map(params![model_id, excluded_json(excluded)], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Queue an `embed` job for `source_id` and mark it `embedding` (still
/// searchable by keywords meanwhile). No-op when a job for the source is
/// already queued or running, or the source is not `ready`/`embedding`.
/// Returns whether a job was queued.
pub fn queue_embed(conn: &mut Connection, source_id: &str) -> KbResult<bool> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let busy: i64 = tx.query_row(
        "SELECT COUNT(*) FROM ingest_jobs WHERE source_id = ?1 AND state IN ('queued','running')",
        params![source_id],
        |r| r.get(0),
    )?;
    let status: Option<String> = rusqlite::OptionalExtension::optional(tx.query_row(
        "SELECT status FROM sources WHERE id = ?1",
        params![source_id],
        |r| r.get(0),
    ))?;
    if busy > 0 || !matches!(status.as_deref(), Some("ready" | "embedding")) {
        return Ok(false);
    }
    crate::jobs::enqueue_tx(&tx, source_id, crate::jobs::JobKind::Embed)?;
    tx.execute(
        "UPDATE sources SET status = 'embedding', status_detail = 'embedding', updated_at = ?2 WHERE id = ?1",
        params![source_id, crate::now_rfc3339()],
    )?;
    tx.commit()?;
    Ok(true)
}

/// End of an embed job: back to `ready` with `detail` (`hybrid`,
/// `keyword_only`, `vector_limit`); `warning` (a code such as
/// `embedding_failed`) is added to the source's warnings once.
pub fn finish_embed(
    conn: &Connection,
    source_id: &str,
    detail: &str,
    warning: Option<&str>,
) -> KbResult<()> {
    let warnings: Option<String> = rusqlite::OptionalExtension::optional(conn.query_row(
        "SELECT warnings_json FROM sources WHERE id = ?1",
        params![source_id],
        |r| r.get(0),
    ))?;
    let Some(warnings) = warnings else {
        return Ok(());
    };
    let mut list: Vec<serde_json::Value> = serde_json::from_str(&warnings).unwrap_or_default();
    list.retain(|w| {
        !matches!(
            w.get("code").and_then(|c| c.as_str()),
            Some("embedding_failed" | "vector_limit")
        )
    });
    if let Some(code) = warning {
        list.push(serde_json::json!({ "code": code }));
    }
    conn.execute(
        "UPDATE sources SET status = 'ready', status_detail = ?2, warnings_json = ?3, updated_at = ?4
          WHERE id = ?1 AND status IN ('embedding','ready')",
        params![source_id, detail, serde_json::Value::Array(list).to_string(), crate::now_rfc3339()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::indexed_source;
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

    fn coll(conn: &mut Connection, name: &str) -> String {
        create_collection(
            conn,
            &NewCollection {
                name: name.into(),
                ..Default::default()
            },
            "u",
            &KnowledgeConfig::default(),
        )
        .unwrap()
        .id
    }

    #[test]
    fn identical_text_reuses_a_vector_and_coverage_counts() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let a = coll(&mut conn, "A");
        let b = coll(&mut conn, "B");
        let s1 = indexed_source(&mut conn, &a, "一.pdf", &["新進員工第一年特休假為七日。"]);
        let s2 = indexed_source(&mut conn, &b, "二.pdf", &["新進員工第一年特休假為七日。"]);
        let none = HashSet::new();
        let p = pending(&conn, &s1, "m", &none, 10).unwrap();
        assert_eq!(p.len(), 1);
        store(&mut conn, "m", &[(p[0].0, vec![0.6, 0.8])]).unwrap();
        assert_eq!(pending_count(&conn, &s2, "m", &none).unwrap(), 1);
        assert_eq!(reuse_by_sha(&conn, &s2, "m", &none).unwrap(), 1);
        assert_eq!(pending_count(&conn, &s2, "m", &none).unwrap(), 0);
        assert_eq!(coverage(&conn, "m", None, &none).unwrap(), (2, 2));
        assert_eq!(coverage(&conn, "other", None, &none).unwrap(), (0, 2));
        assert_eq!(
            coverage(&conn, "m", Some(std::slice::from_ref(&a)), &none).unwrap(),
            (1, 1)
        );
        assert!(sources_needing(&conn, "m", &none).unwrap().is_empty());
        // Both sources need the other model (their ingest jobs are still
        // queued in this fixture, so finish them first).
        conn.execute("UPDATE ingest_jobs SET state = 'done'", [])
            .unwrap();
        assert_eq!(sources_needing(&conn, "other", &none).unwrap().len(), 2);
    }

    #[test]
    fn local_only_and_restricted_collections_are_never_pending_for_cloud() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let open = coll(&mut conn, "open");
        let local = coll(&mut conn, "local");
        let secret = coll(&mut conn, "secret");
        conn.execute(
            "UPDATE collections SET embedding_policy = 'local_only' WHERE id = ?1",
            params![local],
        )
        .unwrap();
        conn.execute(
            "UPDATE collections SET sensitivity = 'restricted' WHERE id = ?1",
            params![secret],
        )
        .unwrap();
        let excluded = cloud_excluded_collections(&conn).unwrap();
        assert_eq!(excluded.len(), 2);
        assert!(!excluded.contains(&open));
        let sl = indexed_source(&mut conn, &local, "l.pdf", &["本機才能算的內容。"]);
        let so = indexed_source(&mut conn, &open, "o.pdf", &["可以送出的內容。"]);
        assert!(
            pending(&conn, &sl, "cloud", &excluded, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            pending(&conn, &so, "cloud", &excluded, 10).unwrap().len(),
            1
        );
        assert_eq!(coverage(&conn, "cloud", None, &excluded).unwrap(), (0, 1));
    }

    #[test]
    fn queue_and_finish_embed() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let a = coll(&mut conn, "A");
        let s = indexed_source(&mut conn, &a, "x.pdf", &["內容。"]);
        assert!(
            !queue_embed(&mut conn, &s).unwrap(),
            "ingest job still queued"
        );
        conn.execute("UPDATE ingest_jobs SET state = 'done'", [])
            .unwrap();
        assert!(queue_embed(&mut conn, &s).unwrap());
        assert!(!queue_embed(&mut conn, &s).unwrap(), "one job at a time");
        let src = crate::sources::get_source(&conn, &s).unwrap().unwrap();
        assert_eq!(src.status, "embedding");
        finish_embed(&conn, &s, "keyword_only", Some("embedding_failed")).unwrap();
        finish_embed(&conn, &s, "keyword_only", Some("embedding_failed")).unwrap();
        let src = crate::sources::get_source(&conn, &s).unwrap().unwrap();
        assert_eq!(src.status, "ready");
        assert_eq!(
            src.warnings.to_string().matches("embedding_failed").count(),
            1
        );
        finish_embed(&conn, &s, "hybrid", None).unwrap();
        let src = crate::sources::get_source(&conn, &s).unwrap().unwrap();
        assert_eq!(src.status_detail.as_deref(), Some("hybrid"));
        assert!(!src.warnings.to_string().contains("embedding_failed"));
    }

    #[test]
    fn store_skips_deleted_chunks() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let a = coll(&mut conn, "A");
        indexed_source(&mut conn, &a, "x.pdf", &["內容。"]);
        assert_eq!(store(&mut conn, "m", &[(999_999, vec![1.0])]).unwrap(), 0);
        assert_eq!(vector_rows(&conn, "m").unwrap(), 0);
    }
}
