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

/// Same model, same destination, still usable.
fn same_embedder(
    a: &dyn duduclaw_knowledge::embed::Embedder,
    b: &dyn duduclaw_knowledge::embed::Embedder,
) -> bool {
    is_available(a) && a.model_id() == b.model_id() && a.egress() == b.egress()
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
    let runtime = tokio::runtime::Handle::current();
    let superseded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let superseded_flag = superseded.clone();
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
                // The provider, endpoint, model, acknowledgement or redaction
                // may change while a long job runs: re-resolve before every
                // batch and stop when it is no longer the same embedder.
                let now = runtime.block_on(super::embedding::resolve(&h));
                if !same_embedder(now.as_ref(), embedder.as_ref()) {
                    superseded_flag.store(true, std::sync::atomic::Ordering::SeqCst);
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
            EmbedJobError::Cancelled if superseded.load(std::sync::atomic::Ordering::SeqCst) => {
                tracing::info!(source = %job.source_id, "knowledge: embedding stopped (embedding settings changed)");
                finish(
                    home,
                    &job,
                    "keyword_only",
                    None,
                    Some((
                        "embedding_superseded",
                        "向量設定已變更，改由新的設定重新計算",
                    )),
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::embed::{Egress, Embedder, UnavailableEmbedder};

    struct E(&'static str, Egress);
    impl Embedder for E {
        fn model_id(&self) -> &str {
            self.0
        }
        fn dim(&self) -> usize {
            2
        }
        fn max_input_tokens(&self) -> usize {
            8
        }
        fn embed_passages(&self, t: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(t.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbedError> {
            Ok(vec![1.0, 0.0])
        }
        fn egress(&self) -> Egress {
            self.1.clone()
        }
    }

    #[test]
    fn a_changed_or_revoked_embedder_is_not_the_same() {
        let cloud = |host: &str| Egress::Cloud {
            provider: "openai".into(),
            host: host.into(),
        };
        let a = E("openai/m@1536", cloud("api.openai.com"));
        assert!(same_embedder(
            &E("openai/m@1536", cloud("api.openai.com")),
            &a
        ));
        assert!(!same_embedder(
            &E("openai/m@1536", cloud("proxy.example.com")),
            &a
        ));
        assert!(!same_embedder(
            &E("gemini/m@768", cloud("api.openai.com")),
            &a
        ));
        assert!(!same_embedder(&UnavailableEmbedder::new("revoked"), &a));
    }
}
