//! The ingest worker. Runs only in the gateway process that holds
//! `<home>/locks/gateway.lock`, one job at a time (so at most one extraction
//! child exists), woken by [`notify`] when a job is queued and every 30 s
//! otherwise. At start it puts jobs whose lease expired (a previous gateway
//! stopped mid-job) back in the queue. Every 10 minutes it prunes
//! `access_log` past `[knowledge] access_log_retention_days`, removes
//! original files no source references any more (after the blob grace
//! period) and stale upload staging files.
//!
//! `embed` jobs (vectors for a source) run through [`super::embed_job`]; the
//! same 10-minute housekeeping queues embed jobs for sources still missing
//! vectors of the embedder in effect, and every pass unloads a local model
//! idle past `[knowledge.embedding] idle_unload_minutes`.
//!
//! A job that panics is recorded as failed (`extract_crashed`) and the loop
//! goes on; if the loop itself ever ends, a supervisor restarts it with a
//! growing delay (5 s up to 5 min) and writes `knowledge_worker_restarted`
//! to the security audit log.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use duduclaw_knowledge::limits::KnowledgeConfig;
use tokio::sync::Notify;

use super::extract_spawn::ExtractorCmd;
use super::pipeline::{PipelineCtx, process_job};

const IDLE_POLL: Duration = Duration::from_secs(30);
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(600);
const RESTART_MIN: Duration = Duration::from_secs(5);
const RESTART_MAX: Duration = Duration::from_secs(300);
const STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

fn wake() -> &'static Notify {
    static WAKE: OnceLock<Notify> = OnceLock::new();
    WAKE.get_or_init(Notify::new)
}

/// Tell the worker a job is waiting.
pub fn notify() {
    wake().notify_one();
}

fn lease(cfg: &KnowledgeConfig) -> Duration {
    duduclaw_knowledge::jobs::LEASE.max(Duration::from_secs(
        cfg.extract_timeout_secs.saturating_mul(2) + 60,
    ))
}

fn housekeeping(home: &std::path::Path, cfg: &KnowledgeConfig) {
    match duduclaw_knowledge::schema::open_home(home) {
        Ok(mut conn) => {
            match duduclaw_knowledge::audit::prune(&conn, cfg.access_log_retention_days) {
                Ok(n) if n > 0 => tracing::info!(rows = n, "knowledge: pruned access log"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "knowledge: access log prune failed"),
            }
            let store = duduclaw_knowledge::blob::BlobStore::for_home(home);
            match store.gc(&mut conn, duduclaw_knowledge::blob::GC_GRACE) {
                Ok(n) if n > 0 => {
                    tracing::info!(files = n, "knowledge: removed unreferenced originals")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "knowledge: blob sweep failed"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "knowledge: database unavailable for housekeeping"),
    }
    duduclaw_knowledge::blob::BlobStore::for_home(home).sweep_staging(STAGING_MAX_AGE);
}

/// Claim the next job, requeueing expired leases first.
fn next_job(
    home: &std::path::Path,
    cfg: &KnowledgeConfig,
) -> Option<duduclaw_knowledge::jobs::Job> {
    let mut conn = match duduclaw_knowledge::schema::open_home(home) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: database unavailable; worker idle");
            return None;
        }
    };
    if let Err(e) = duduclaw_knowledge::jobs::requeue_expired(&mut conn) {
        tracing::warn!(error = %e, "knowledge: requeue of expired jobs failed");
    }
    match duduclaw_knowledge::jobs::claim_next(&conn, lease(cfg)) {
        Ok(job) => job,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: job claim failed");
            None
        }
    }
}

/// Record a job that panicked, so it is not retried forever.
fn record_panic(home: &std::path::Path, job: &duduclaw_knowledge::jobs::Job) {
    let message = "解析這份文件時 DuDuClaw 發生內部錯誤，已停止。請刪除後重新上傳，或聯絡管理員。";
    let result = duduclaw_knowledge::schema::open_home(home).and_then(|conn| {
        duduclaw_knowledge::sources::record_failure(
            &conn,
            &job.source_id,
            "extract_crashed",
            message,
        )?;
        duduclaw_knowledge::jobs::fail(&conn, job.id, "extract_crashed", message)
    });
    if let Err(e) = result {
        tracing::error!(error = %e, "knowledge: could not record a panicked job");
    }
}

async fn run_loop(home: PathBuf, extractor: ExtractorCmd) {
    let mut last_housekeeping: Option<Instant> = None;
    loop {
        let cfg = KnowledgeConfig::from_home(&home);
        if last_housekeeping.is_none_or(|t| t.elapsed() >= HOUSEKEEPING_EVERY) {
            let (h, c) = (home.clone(), cfg.clone());
            let _ = tokio::task::spawn_blocking(move || housekeeping(&h, &c)).await;
            if cfg.enabled {
                super::embedding::schedule_all(&home).await;
            }
            last_housekeeping = Some(Instant::now());
        }
        super::embedding::unload_idle(
            duduclaw_knowledge::embed::config::EmbeddingConfig::from_home(&home).idle_unload_minutes,
        );
        let job = if cfg.enabled {
            let (h, c) = (home.clone(), cfg.clone());
            tokio::task::spawn_blocking(move || next_job(&h, &c))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        match job {
            Some(job) if job.kind == duduclaw_knowledge::jobs::JobKind::Embed => {
                let (h, j, l) = (home.clone(), job.clone(), lease(&cfg));
                if let Err(e) = tokio::spawn(async move { super::embed_job::process(&h, l, j).await }).await {
                    tracing::error!(error = %e, source = %job.source_id, "knowledge: embed job panicked");
                }
            }
            Some(job) => {
                let ctx = PipelineCtx {
                    home: home.clone(),
                    cfg,
                    extractor: extractor.clone(),
                };
                let j = job.clone();
                // Its own task, so a panic inside one job cannot end the loop.
                if let Err(e) = tokio::spawn(async move { process_job(&ctx, j).await }).await {
                    tracing::error!(error = %e, source = %job.source_id, "knowledge: ingest job panicked");
                    let h = home.clone();
                    let _ = tokio::task::spawn_blocking(move || record_panic(&h, &job)).await;
                }
            }
            None => {
                let _ = tokio::time::timeout(IDLE_POLL, wake().notified()).await;
            }
        }
    }
}

/// Start the worker (call once, in the lock-holding gateway).
pub fn spawn(home: PathBuf) {
    let extractor = match ExtractorCmd::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "knowledge: cannot locate the duduclaw binary; document ingest disabled");
            return;
        }
    };
    tokio::spawn(async move {
        let mut delay = RESTART_MIN;
        loop {
            let outcome = tokio::spawn(run_loop(home.clone(), extractor.clone())).await;
            let reason = match outcome {
                Ok(()) => "loop ended".to_string(),
                Err(e) => e.to_string(),
            };
            tracing::error!(%reason, delay_secs = delay.as_secs(), "knowledge: ingest worker stopped; restarting");
            super::audit_admin(
                &home,
                "knowledge_worker_restarted",
                "system",
                serde_json::json!({"reason": duduclaw_core::truncate_chars(&reason, 300), "delay_secs": delay.as_secs()}),
            );
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(RESTART_MAX);
        }
    });
}
