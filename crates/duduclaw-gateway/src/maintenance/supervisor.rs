//! The maintenance supervisor (design M9, §6.2): its own tokio task, started
//! unconditionally at gateway boot. It does not depend on `[dispatch] enabled`
//! or on the dispatch engine's tick (the TTL sweep used to be step 4 of that
//! tick, behind two `?` that could skip it). Each step handles its own errors
//! and a panic in one step is caught, so nothing short-circuits the TTL sweep
//! or the reconciliation.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use tracing::error;

use super::MaintenanceWindow;
use super::lifecycle::{
    MaintEnv, SysdSupport, boot_reconcile_guard, boot_reconcile_guarded, poll_sysd, sweep_expired,
};

/// Start the supervisor. `boot_revoked` are the rows
/// [`super::lifecycle::revoke_on_boot`] closed in the ledger before the
/// dashboard came up; their close actions run here once sysd answers.
/// Acquire the writer lock before returning to the server: the dashboard
/// must not open a fresh window before this task first runs.
pub async fn spawn(
    env: Arc<MaintEnv>,
    boot_revoked: Vec<MaintenanceWindow>,
) -> tokio::task::JoinHandle<()> {
    let guard = boot_reconcile_guard().await;
    tokio::spawn(async move {
        let mut support = guarded(
            "boot reconciliation",
            boot_reconcile_guarded(&env, boot_revoked, guard),
        )
        .await
        .unwrap_or(SysdSupport::Current);
        loop {
            tokio::time::sleep(env.tick_interval).await;
            support = tick(&env, support).await;
        }
    })
}

/// One supervisor pass: TTL sweep and reminder, then the sysd poll (audit
/// mirror, orphan close, OS-side close), skipped once sysd turned out to be
/// too old to know the maintenance verbs. Independent steps.
pub async fn tick(env: &MaintEnv, support: SysdSupport) -> SysdSupport {
    guarded("TTL sweep", sweep_expired(env)).await;
    if support == SysdSupport::Old {
        return support;
    }
    guarded("sysd poll", poll_sysd(env))
        .await
        .unwrap_or(SysdSupport::Current)
}

async fn guarded<T, F: Future<Output = T>>(step: &str, fut: F) -> Option<T> {
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(v) => Some(v),
        Err(_) => {
            error!(
                step,
                "maintenance supervisor: step panicked; continuing with the next step"
            );
            None
        }
    }
}
