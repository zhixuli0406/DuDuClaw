//! Compute the missing vectors of one source: reuse identical passages'
//! vectors first, then embed the rest in batches. Shared by the gateway's
//! `embed` jobs and `duduclaw knowledge eval`.

use rusqlite::Connection;

use super::{Egress, EmbedError, Embedder, vectors};
use crate::{KbError, KbResult};

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedOutcome {
    pub embedded: usize,
    pub reused: usize,
    /// Stopped at `max_vector_rows`.
    pub capped: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedJobError {
    #[error("{0}")]
    Db(#[from] KbError),
    #[error("{0}")]
    Embed(EmbedError),
    /// `keep_going` said stop (source deleted, job cancelled).
    #[error("cancelled")]
    Cancelled,
}

impl From<rusqlite::Error> for EmbedJobError {
    fn from(e: rusqlite::Error) -> Self {
        EmbedJobError::Db(KbError::Db(e))
    }
}

/// Embed what `source_id` is missing for `embedder`. `keep_going(conn,
/// done, total)` runs before every batch (renew a lease, check
/// cancellation, report progress); returning `false` stops with
/// [`EmbedJobError::Cancelled`].
pub fn embed_source(
    conn: &mut Connection,
    source_id: &str,
    embedder: &dyn Embedder,
    batch: usize,
    max_rows: usize,
    mut keep_going: impl FnMut(&Connection, usize, usize) -> KbResult<bool>,
) -> Result<EmbedOutcome, EmbedJobError> {
    let model = embedder.model_id().to_string();
    let cloud = matches!(embedder.egress(), Egress::Cloud { .. });
    // Re-read before every batch: a collection made `restricted` or
    // `local_only` mid-job stops being sent from the next batch on.
    let excluded_now = |conn: &Connection| -> KbResult<std::collections::HashSet<String>> {
        if cloud {
            vectors::cloud_excluded_collections(conn)
        } else {
            Ok(Default::default())
        }
    };
    let excluded = excluded_now(conn)?;
    let mut out = EmbedOutcome {
        reused: vectors::reuse_by_sha(conn, source_id, &model, &excluded)?,
        ..Default::default()
    };
    let total = vectors::pending_count(conn, source_id, &model, &excluded)? as usize;
    let batch = batch.max(1);
    loop {
        if !keep_going(conn, out.embedded, total)? {
            return Err(EmbedJobError::Cancelled);
        }
        let rows = vectors::vector_rows(conn, &model)? as usize;
        if rows >= max_rows {
            out.capped = true;
            break;
        }
        let take = batch.min(max_rows - rows);
        let excluded = excluded_now(conn)?;
        let pending = vectors::pending(conn, source_id, &model, &excluded, take)?;
        if pending.is_empty() {
            break;
        }
        let texts: Vec<&str> = pending.iter().map(|(_, t)| t.as_str()).collect();
        let vecs = embedder
            .embed_passages(&texts)
            .map_err(EmbedJobError::Embed)?;
        if vecs.len() != pending.len() || vecs.iter().any(|v| v.len() != embedder.dim()) {
            return Err(EmbedJobError::Embed(EmbedError::Failed(
                "向量數量或維度不符".to_string(),
            )));
        }
        if vecs.iter().flatten().any(|x| !x.is_finite()) {
            return Err(EmbedJobError::Embed(EmbedError::Failed(
                "向量含有無效數值".to_string(),
            )));
        }
        let rows: Vec<(i64, Vec<f32>)> = pending.iter().map(|(id, _)| *id).zip(vecs).collect();
        vectors::store(conn, &model, &rows)?;
        out.embedded += rows.len();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::indexed_source;
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

    struct Fixed;
    impl Embedder for Fixed {
        fn model_id(&self) -> &str {
            "fixed"
        }
        fn dim(&self) -> usize {
            2
        }
        fn max_input_tokens(&self) -> usize {
            10
        }
        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbedError> {
            Ok(vec![1.0, 0.0])
        }
        fn egress(&self) -> Egress {
            Egress::Local
        }
    }

    fn source(conn: &mut Connection, title: &str, pages: &[&str]) -> String {
        let c = create_collection(
            conn,
            &NewCollection {
                name: title.into(),
                ..Default::default()
            },
            "u",
            &KnowledgeConfig::default(),
        )
        .unwrap();
        indexed_source(conn, &c.id, title, pages)
    }

    #[test]
    fn embeds_then_reuses_identical_passages() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let long = "段落內容。".repeat(120);
        let a = source(&mut conn, "a", &[&long, &long]);
        let o = embed_source(&mut conn, &a, &Fixed, 2, 1000, |_, _, _| Ok(true)).unwrap();
        assert!(o.embedded >= 1 && o.reused == 0 && !o.capped, "{o:?}");
        let b = source(&mut conn, "b", &[&long, &long]);
        let o2 = embed_source(&mut conn, &b, &Fixed, 2, 1000, |_, _, _| Ok(true)).unwrap();
        assert_eq!(
            o2.embedded, 0,
            "identical text is reused, not re-embedded: {o2:?}"
        );
        assert!(o2.reused >= 1);
    }

    #[test]
    fn a_collection_made_local_only_mid_job_stops_being_sent() {
        struct Cloud;
        impl Embedder for Cloud {
            fn model_id(&self) -> &str {
                "cloud"
            }
            fn dim(&self) -> usize {
                2
            }
            fn max_input_tokens(&self) -> usize {
                10
            }
            fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
                Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
            }
            fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbedError> {
                Ok(vec![1.0, 0.0])
            }
            fn egress(&self) -> Egress {
                Egress::Cloud {
                    provider: "openai".into(),
                    host: "api.openai.com".into(),
                }
            }
        }
        let mut conn = crate::schema::open_in_memory().unwrap();
        let long = "段落內容。".repeat(120);
        let s = source(&mut conn, "a", &[&long, &long, &long]);
        let mut batches = 0;
        let o = embed_source(&mut conn, &s, &Cloud, 1, 1000, |c, _, _| {
            batches += 1;
            if batches == 2 {
                c.execute("UPDATE collections SET embedding_policy = 'local_only'", [])?;
            }
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            o.embedded, 1,
            "only the batch before the change was sent: {o:?}"
        );
    }

    #[test]
    fn non_finite_vectors_are_refused() {
        struct Nan;
        impl Embedder for Nan {
            fn model_id(&self) -> &str {
                "nan"
            }
            fn dim(&self) -> usize {
                2
            }
            fn max_input_tokens(&self) -> usize {
                10
            }
            fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
                Ok(texts.iter().map(|_| vec![f32::NAN, 0.0]).collect())
            }
            fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbedError> {
                Ok(vec![0.0, 0.0])
            }
            fn egress(&self) -> Egress {
                Egress::Local
            }
        }
        let mut conn = crate::schema::open_in_memory().unwrap();
        let s = source(&mut conn, "a", &["一。"]);
        assert!(matches!(
            embed_source(&mut conn, &s, &Nan, 1, 1000, |_, _, _| Ok(true)),
            Err(EmbedJobError::Embed(_))
        ));
    }

    #[test]
    fn cancellation_and_row_cap() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let s = source(&mut conn, "a", &["一。", "二。"]);
        assert!(matches!(
            embed_source(&mut conn, &s, &Fixed, 1, 1000, |_, _, _| Ok(false)),
            Err(EmbedJobError::Cancelled)
        ));
        let o = embed_source(&mut conn, &s, &Fixed, 1, 0, |_, _, _| Ok(true)).unwrap();
        assert!(o.capped && o.embedded == 0);
    }
}
