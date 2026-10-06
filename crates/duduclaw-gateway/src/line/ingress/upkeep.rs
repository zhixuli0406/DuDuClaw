//! Background upkeep of the LINE inbox: the snapshot lane and the
//! maintenance task.
//!
//! - Snapshot lane (review N1): a freshly accepted event is stored with a
//!   pending marker and cannot be claimed until its route/authority snapshot
//!   is stored. [`snapshot_pass`] takes it right after the commit
//!   ([`spawn_snapshots`]) and again on every tick of [`snapshot_loop`]: an
//!   unreadable configuration backs off and, at the limit, quarantines the
//!   event as `snapshot_unavailable` (retryable: it never ran). An event
//!   that waited longer than `SNAPSHOT_MAX_AGE_SECS` (for example the
//!   gateway was down) is quarantined the same way rather than adopting a
//!   later configuration silently. After an operator's `retry` the snapshot
//!   is taken from the configuration current at that time.
//! - Maintenance: recovery, payload expiry, retention, "stuck" / capacity
//!   alerts, and flushing the alert queue into summarized Activity rows.

use super::super::*;
use super::revision::{RevisionError, line_revision};
use crate::channel_ingress::alerts::{self, AlertKind};
use crate::channel_ingress::config::IngressConfig;
use crate::channel_ingress::{Deferred, IngressRow, IngressStore, PENDING_REVISION};

/// Rows looked at per snapshot pass.
const SNAPSHOT_BATCH: i64 = 64;

pub(super) async fn notify_agent(state: &LineState) -> Option<String> {
    let reg = state.ctx.registry.read().await;
    reg.main_agent().and_then(|a| {
        a.dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    })
}

fn pending_event(row: &IngressRow) -> Option<LineEvent> {
    row.payload
        .as_deref()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .and_then(|p| serde_json::from_value::<LineEvent>(p["event"].clone()).ok())
}

/// One snapshot attempt for every pending event that is due.
pub(super) async fn snapshot_pass(state: &LineState, store: &IngressStore) {
    let now = chrono::Utc::now().timestamp();
    let Ok(rows) = store.pending_due(now, SNAPSHOT_BATCH).await else {
        error!("LINE ingress snapshot lane unavailable");
        return;
    };
    for row in rows {
        snapshot_one(state, store, &row, now).await;
    }
}

async fn quarantine_pending(store: &IngressStore, row: &IngressRow, reason: &str, cause: &str) {
    if store.quarantine_pending(row, reason).await == Ok(true) {
        alerts::record(store, AlertKind::Quarantined, &row.id, cause).await;
    }
}

async fn snapshot_one(state: &LineState, store: &IngressStore, row: &IngressRow, now: i64) {
    use crate::channel_ingress::snapshot::SNAPSHOT_UNAVAILABLE;
    // An operator-approved retry adopts the configuration of now.
    let operator_retry = row.run_authorization_id.is_some();
    if !operator_retry && IngressStore::snapshot_too_old(row, now) {
        quarantine_pending(store, row, SNAPSHOT_UNAVAILABLE, "snapshot_too_old").await;
        return;
    }
    let Some(event) = pending_event(row) else {
        quarantine_pending(store, row, "payload_unavailable", "payload_unavailable").await;
        return;
    };
    let expected = if operator_retry {
        None
    } else {
        row.authorization_revision
            .strip_prefix(&format!("{PENDING_REVISION}:"))
    };
    match line_revision(state, &event, expected).await {
        Ok(rev) => {
            let stored = store
                .store_snapshot(
                    &row.id,
                    &row.authorization_revision,
                    &rev.route,
                    &rev.authority,
                )
                .await;
            if stored == Ok(true) {
                store.work_signal().notify_waiters();
            }
        }
        Err(RevisionError::Unavailable(reason)) => {
            if store.defer_snapshot(row, reason, now).await == Ok(Deferred::Quarantined) {
                alerts::record(store, AlertKind::Quarantined, &row.id, SNAPSHOT_UNAVAILABLE).await;
            }
        }
        Err(RevisionError::Changed(cause)) => {
            quarantine_pending(store, row, "account_route_authorization_changed", cause).await;
        }
    }
}

/// Snapshot the events of a webhook right after its commit.
pub(crate) fn spawn_snapshots(state: LineState, _ids: Vec<String>) {
    let Some(store) = state.ingress.clone() else {
        return;
    };
    tokio::spawn(async move { snapshot_pass(&state, &store).await });
}

/// The snapshot lane: one pass per second, or sooner when work arrives.
pub(super) async fn snapshot_loop(state: LineState) {
    let Some(store) = state.ingress.clone() else {
        return;
    };
    loop {
        let woken = store.work_signal().notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        snapshot_pass(&state, &store).await;
        tokio::select! {
            _ = &mut woken => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
    }
}

/// Recovery, payload expiry, retention, stuck and capacity alerts, and the
/// summarized alert flush.
pub(super) async fn maintenance_loop(state: LineState) {
    let Some(store) = state.ingress.clone() else {
        return;
    };
    loop {
        let cfg = IngressConfig::load(&state.home_dir).await;
        let now = chrono::Utc::now().timestamp();
        maintenance_pass(&state, &store, &cfg, now).await;
        let agent = notify_agent(&state).await;
        alerts::flush(&store, &state.home_dir, agent.as_deref(), now).await;
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

async fn maintenance_pass(state: &LineState, store: &IngressStore, cfg: &IngressConfig, now: i64) {
    // Held when the store opened after a device restore.
    for id in store.take_restored() {
        alerts::record(
            store,
            AlertKind::RestoredFromBackup,
            &id,
            crate::channel_ingress::RESTORED_REASON,
        )
        .await;
    }
    match store.recover_and_purge(now).await {
        Ok(report) => {
            for id in report.newly_uncertain {
                alerts::record(store, AlertKind::Uncertain, &id, "dispatch_receipt_missing").await;
            }
            for id in report.newly_quarantined {
                alerts::record(
                    store,
                    AlertKind::Quarantined,
                    &id,
                    "payload_retention_expired",
                )
                .await;
            }
        }
        Err(_) => error!("LINE ingress recovery unavailable"),
    }
    if store.purge_finished(now, cfg.retention_days).await.is_err() {
        error!("LINE ingress retention purge unavailable");
    }
    if let Ok(stuck) = store
        .stuck_conversations(now, cfg.stuck_alert_minutes)
        .await
    {
        for s in stuck {
            let blocker = s.blocker.as_ref().map(|b| b.0.as_str()).unwrap_or("slow");
            let key = format!("stuck:{}:{blocker}", s.conversation_ref);
            if store.claim_alert(&key, now).await == Ok(true) {
                warn!(
                    waiting = s.waiting,
                    oldest_wait_secs = s.oldest_wait_secs,
                    blocker,
                    "LINE conversation stuck"
                );
                let reason = s
                    .blocker
                    .as_ref()
                    .map(|b| b.1.as_str())
                    .unwrap_or("waiting");
                alerts::record(store, AlertKind::Stuck, &s.conversation_ref, reason).await;
            }
        }
    }
    if cfg.capacity_alert_mb > 0 {
        let size = ["channel_ingress.db", "channel_ingress.db-wal"]
            .iter()
            .filter_map(|f| std::fs::metadata(state.home_dir.join(f)).ok())
            .map(|m| m.len())
            .sum::<u64>();
        if size > cfg.capacity_alert_mb.saturating_mul(1024 * 1024) {
            let key = format!("capacity:{}", chrono::Utc::now().format("%Y-%m-%d"));
            if store.claim_alert(&key, now).await == Ok(true) {
                alerts::record(
                    store,
                    AlertKind::Capacity,
                    "channel_ingress.db",
                    "capacity_alert_mb",
                )
                .await;
            }
        }
    }
}
