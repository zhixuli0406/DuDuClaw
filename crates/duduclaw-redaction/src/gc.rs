//! Vault garbage-collection task.
//!
//! Two operations on a tokio interval:
//!
//! - **mark_expired** — sets `expired_marker = 1` on TTL-expired rows (cheap UPDATE)
//! - **purge_expired** — deletes rows that have been expired for `purge_after_expire_days`
//!
//! The task emits a single [`AuditEvent::VaultGc`] per tick summarising
//! the counts. Failures are logged via `tracing::error!` and do NOT crash
//! the task — the next tick retries.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::audit::{AuditEvent, AuditSink};
use crate::vault::VaultStore;

/// Tokio handle for a running GC task. Drop or call [`stop`] to terminate.
pub struct GcTask {
    cancel: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl GcTask {
    /// Signal the task to exit and await its termination.
    pub async fn stop(mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }

    /// Synchronous best-effort cancel (no `await` available, e.g. from `Drop`).
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }

    /// Underlying join handle (mostly for tests).
    pub fn join_handle(&self) -> &JoinHandle<()> {
        &self.join
    }
}

/// Configuration knobs for the GC task.
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// Interval between mark_expired sweeps.
    pub mark_interval: Duration,
    /// Interval between purge_expired sweeps (typically a multiple of mark).
    pub purge_interval: Duration,
    /// Days an entry must have been expired before purge erases it.
    pub purge_after_expire_days: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            mark_interval: Duration::from_secs(6 * 3600),  // 6 hours
            purge_interval: Duration::from_secs(24 * 3600), // 24 hours
            purge_after_expire_days: 30,
        }
    }
}

/// Spawn the GC task. Runs `mark_expired` on every tick of `mark_interval`,
/// and `purge_expired` every `purge_interval`.
pub fn spawn_gc(
    vault: Arc<VaultStore>,
    audit: Arc<dyn AuditSink>,
    config: GcConfig,
) -> GcTask {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

    let join = tokio::spawn(async move {
        let mut mark_tick = tokio::time::interval(config.mark_interval);
        // Skip the immediate first tick — give the system a moment to settle.
        mark_tick.tick().await;

        let mut purge_tick = tokio::time::interval(config.purge_interval);
        purge_tick.tick().await;

        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    tracing::info!(
                        target: "duduclaw_redaction::gc",
                        "GC task received cancel signal, exiting"
                    );
                    break;
                }
                _ = mark_tick.tick() => {
                    let marked = match vault.mark_expired() {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::error!(
                                target: "duduclaw_redaction::gc",
                                error = %e,
                                "vault.mark_expired failed"
                            );
                            0
                        }
                    };
                    audit.emit(AuditEvent::VaultGc {
                        expired_marked: marked,
                        purged: 0,
                    });
                }
                _ = purge_tick.tick() => {
                    let purged = match vault.purge_expired(config.purge_after_expire_days) {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::error!(
                                target: "duduclaw_redaction::gc",
                                error = %e,
                                "vault.purge_expired failed"
                            );
                            0
                        }
                    };
                    audit.emit(AuditEvent::VaultGc {
                        expired_marked: 0,
                        purged,
                    });
                }
            }
        }
    });

    GcTask {
        cancel: Some(cancel_tx),
        join,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::NullAuditSink;
    use crate::rules::RestoreScope;
    use tempfile::TempDir;
    use tokio::time::sleep;

    fn fresh_vault() -> (Arc<VaultStore>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(VaultStore::in_memory(tmp.path().to_path_buf()).unwrap());
        (store, tmp)
    }

    #[tokio::test]
    async fn gc_marks_expired_then_stops() {
        let (vault, _tmp) = fresh_vault();
        // Insert a TTL-0 (immediately expired) entry.
        vault
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "agnes",
                Some("s1"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                0,
            )
            .unwrap();
        sleep(Duration::from_millis(1100)).await;

        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);
        let cfg = GcConfig {
            mark_interval: Duration::from_millis(50),
            purge_interval: Duration::from_secs(3600),
            purge_after_expire_days: 30,
        };
        let task = spawn_gc(vault.clone(), audit, cfg);

        sleep(Duration::from_millis(200)).await;
        task.stop().await;

        // Lookup an expired entry → returns Some with expired_marker = true.
        let entry = vault
            .lookup_mapping("<REDACT:E:abcdef01>", "agnes", Some("s1"))
            .unwrap()
            .unwrap();
        assert!(entry.expired_marker);
    }

    #[tokio::test]
    async fn gc_cancel_quickly() {
        let (vault, _tmp) = fresh_vault();
        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);
        let task = spawn_gc(vault, audit, GcConfig::default());
        // Stop immediately — should return quickly without waiting 6 hours.
        let start = std::time::Instant::now();
        task.stop().await;
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
