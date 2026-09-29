//! WP2.5 — the background sweep that closes due AEE settlements.
//!
//! Extracted from the removed `gvu::observation_finalizer` (S11, 2026-09-29).
//! That module carried two unrelated queues in one struct: the legacy SOUL.md
//! observation window (24 h, confirm / roll back a whole persona file) and
//! this one. Only the legacy half was removed; this half is AEE's own
//! entry-level accept/rollback clock and is load-bearing for the default
//! evolution path, so it moved here rather than disappearing with it.
//!
//! Best-effort throughout, and deliberately so: a settlement that cannot be
//! judged (no eval score reachable) leaves its entries exactly as they are and
//! says so in the log — "we could not check" is never rendered as "checked and
//! fine".

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use tracing::info;

use super::{EvalMeasureScorer, PendingSettlementStore, settle_pending};

/// Run one sweep over `<home>/evolution.db`, settling every AEE round whose
/// observation window has elapsed. Returns how many settled.
pub async fn sweep_due_settlements(home_dir: &Path) -> usize {
    let db_path = home_dir.join("evolution.db");
    let due = PendingSettlementStore::new(&db_path).due(Utc::now());
    if due.is_empty() {
        return 0;
    }
    let scorer = EvalMeasureScorer::from_home(home_dir);
    info!(
        target: "aee_sweeper",
        count = due.len(),
        "Sweeping due AEE playbook settlements"
    );
    let mut settled = 0usize;
    for pending in due {
        if settle_pending(&pending, &db_path, home_dir, &scorer)
            .await
            .is_some()
        {
            settled += 1;
        }
    }
    settled
}

/// Long-running task: sweep on a fixed interval until cancelled.
///
/// The first tick fires immediately so a settlement left pending across a
/// gateway restart is not held back for another full interval.
pub async fn run_settlement_sweeper(home_dir: PathBuf, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let _ = sweep_due_settlements(&home_dir).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (S11): the sweeper must survive a home directory with no
    /// `evolution.db` at all — the legacy finaliser used to own the only
    /// construction site that guaranteed the file existed.
    #[tokio::test]
    async fn sweep_on_empty_home_is_a_noop() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(sweep_due_settlements(home.path()).await, 0);
    }
}
