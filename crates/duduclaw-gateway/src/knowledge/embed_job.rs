//! One `embed` job: compute the missing vectors of a source with the
//! embedder in effect. The source stays searchable by keywords throughout;
//! a failure leaves it `ready` with an `embedding_failed` warning (retried by
//! the next backfill), never `failed`.

use std::path::Path;
use std::time::Duration;

use duduclaw_knowledge::embed::config::EmbeddingConfig;
use duduclaw_knowledge::embed::job::{EmbedJobError, embed_source};
use duduclaw_knowledge::embed::{EmbedError, is_available, vectors};
use duduclaw_knowledge::jobs::{self, Job};

fn finish(
    home: &Path,
    job: &Job,
    detail: &str,
    warning: Option<&str>,
    failure: Option<(&str, &str)>,
) {
    let result = duduclaw_knowledge::schema::open_home(home).and_then(|conn| {
        vectors::finish_embed(&conn, &job.source_id, detail, warning)?;
        match failure {
            Some((code, message)) => jobs::fail(&conn, job.id, code, message),
            None => jobs::complete(&conn, job.id),
        }
    });
    if let Err(e) = result {
        tracing::error!(error = %e, source = %job.source_id, "knowledge: could not record embed job outcome");
    }
}

/// Run one claimed `embed` job. Never panics; every outcome is written.
pub async fn process(home: &Path, lease: Duration, job: Job) {
    let embedder = super::embedding::resolve(home).await;
    if !is_available(embedder.as_ref()) {
        let reason = match embedder.embed_query("") {
            Err(EmbedError::Unavailable(r) | EmbedError::Failed(r)) => r,
            Ok(_) => String::new(),
        };
        finish(
            home,
            &job,
            "keyword_only",
            None,
            Some(("embedding_unavailable", &reason)),
        );
        return;
    }
    let cfg = EmbeddingConfig::from_home(home);
    let (h, j) = (home.to_path_buf(), job.clone());
    let started = std::time::Instant::now();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut conn = duduclaw_knowledge::schema::open_home(&h)
            .map_err(|e| Box::new(EmbedJobError::Db(e)))?;
        embed_source(
            &mut conn,
            &j.source_id,
            embedder.as_ref(),
            cfg.batch_size,
            cfg.max_vector_rows,
            |c, done, total| {
                if !jobs::is_running(c, j.id)? {
                    return Ok(false);
                }
                jobs::renew(c, j.id, lease)?;
                jobs::set_progress(c, j.id, done as i64, total as i64)?;
                Ok(true)
            },
        )
        .map_err(Box::new)
    })
    .await;
    match outcome {
        Ok(Ok(o)) => {
            tracing::info!(
                source = %job.source_id, embedded = o.embedded, reused = o.reused, capped = o.capped,
                secs = started.elapsed().as_secs_f64(), "knowledge: source embedded"
            );
            if o.capped {
                finish(home, &job, "vector_limit", Some("vector_limit"), None);
            } else {
                finish(home, &job, "hybrid", None, None);
            }
        }
        Ok(Err(e)) => match *e {
            EmbedJobError::Cancelled => {
                tracing::info!(source = %job.source_id, "knowledge: embedding cancelled (source deleted or job stopped)");
            }
            EmbedJobError::Embed(err) => {
                let msg = err.to_string();
                tracing::warn!(source = %job.source_id, error = %msg, "knowledge: embedding failed; source stays keyword-only");
                finish(
                    home,
                    &job,
                    "keyword_only",
                    Some("embedding_failed"),
                    Some(("embedding_failed", &msg)),
                );
            }
            EmbedJobError::Db(err) => {
                tracing::error!(source = %job.source_id, error = %err, "knowledge: embedding job database error");
                finish(
                    home,
                    &job,
                    "keyword_only",
                    Some("embedding_failed"),
                    Some(("embedding_failed", &err.public_message())),
                );
            }
        },
        Err(e) => {
            tracing::error!(source = %job.source_id, error = %e, "knowledge: embedding task panicked");
            finish(
                home,
                &job,
                "keyword_only",
                Some("embedding_failed"),
                Some(("embedding_failed", "向量計算時發生內部錯誤")),
            );
        }
    }
}
