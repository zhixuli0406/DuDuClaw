//! The ingest worker. Runs only in the gateway process that holds
//! `<home>/locks/gateway.lock`, one job at a time (so at most one extraction
//! child exists), woken by [`notify`] when a job is queued and every 30 s
//! otherwise. At start it puts jobs whose lease expired (a previous gateway
//! stopped mid-job) back in the queue. Hourly it prunes `access_log` past
//! `[knowledge] access_log_retention_days` and stale upload staging files.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use duduclaw_knowledge::limits::KnowledgeConfig;
use tokio::sync::Notify;

use super::extract_spawn::ExtractorCmd;
use super::pipeline::{PipelineCtx, process_job};

const IDLE_POLL: Duration = Duration::from_secs(30);
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(3600);
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
    duduclaw_knowledge::jobs::LEASE.max(Duration::from_secs(cfg.extract_timeout_secs.saturating_mul(2) + 60))
}

fn housekeeping(home: &std::path::Path, cfg: &KnowledgeConfig) {
    match duduclaw_knowledge::schema::open_home(home) {
        Ok(conn) => match duduclaw_knowledge::audit::prune(&conn, cfg.access_log_retention_days) {
            Ok(n) if n > 0 => tracing::info!(rows = n, "knowledge: pruned access log"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "knowledge: access log prune failed"),
        },
        Err(e) => tracing::warn!(error = %e, "knowledge: database unavailable for housekeeping"),
    }
    duduclaw_knowledge::blob::BlobStore::for_home(home).sweep_staging(STAGING_MAX_AGE);
}

/// Claim the next job, requeueing expired leases first.
fn next_job(home: &std::path::Path, cfg: &KnowledgeConfig) -> Option<duduclaw_knowledge::jobs::Job> {
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
        let mut last_housekeeping: Option<Instant> = None;
        loop {
            let cfg = KnowledgeConfig::from_home(&home);
            if last_housekeeping.is_none_or(|t| t.elapsed() >= HOUSEKEEPING_EVERY) {
                let h = home.clone();
                let c = cfg.clone();
                let _ = tokio::task::spawn_blocking(move || housekeeping(&h, &c)).await;
                last_housekeeping = Some(Instant::now());
            }
            let job = if cfg.enabled {
                let h = home.clone();
                let c = cfg.clone();
                tokio::task::spawn_blocking(move || next_job(&h, &c))
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            match job {
                Some(job) => {
                    let ctx = PipelineCtx {
                        home: home.clone(),
                        cfg,
                        extractor: extractor.clone(),
                    };
                    process_job(&ctx, job).await;
                }
                None => {
                    let _ = tokio::time::timeout(IDLE_POLL, wake().notified()).await;
                }
            }
        }
    });
}
