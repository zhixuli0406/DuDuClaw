//! One bounded bridge for every migration authority async adapter. Call only from
//! the blocking pool, after the confirmation intent transaction has committed.
use super::{Error, Result};
use std::{future::Future, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::{Duration, Instant}};
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(30);

/// R7d: the first allocation of an employee on a host with sshd answers
/// "pending" while spawnd's one-shot probe proves the live sshd refuses the
/// new account (≤ ~40 s), and spawnd's worker retries an inconclusive probe
/// once after 60 s; 120 s covers one probe plus that one retry. Each attempt
/// is its own [`operation`] (30 s budget); the wait between attempts holds no
/// adapter deadline, no SQLite writer and no lock but the proposal's own.
pub(crate) const PENDING_FIRST_RETRY: Duration = Duration::from_secs(3);
pub(crate) const PENDING_RETRY_EVERY: Duration = Duration::from_secs(5);
pub(crate) const PENDING_WAIT_TOTAL: Duration = Duration::from_secs(120);
/// Granularity at which a sleeping wait notices shutdown.
const PENDING_CANCEL_POLL: Duration = Duration::from_millis(200);
/// Set once when the gateway starts shutting down
/// (`begin_migration_apply_shutdown`); every pending wait stops at its next
/// poll (the item stays `Applying`, nothing is compensated).
///
/// Guarantee (R7d round 3), for the confirmation flow (approve, retry, the
/// retry of a `Failed` item, reject):
/// (a) once this flag is set no new external effect (Allocate/stage,
///     publish, read back, Disable) starts, because every adapter call is
///     immediately preceded by a check of it, made after the proposal lock,
///     the SQLite transaction and the intent commit, with nothing that can
///     block in between (`ConfirmationStore::{apply_next_until, retry_until,
///     reject_until}`, the Employee branch of `retry_failed`). A stopped call
///     keeps the item's state and answers retry-safe.
/// (b) an effect that had already started may still complete. The shutdown
///     wait (`APPLY_SHUTDOWN_WAIT`, best effort, on the apply permits that
///     approve / retry / reject hold) only gives it time to finish cleanly;
///     it is not what enforces (a) and is not bounded by the adapter budget,
///     which is cooperative.
/// (c) if the process exits while an effect is in flight, its committed
///     intent stays in the journal and the next apply / retry / reject
///     repeats the same effect idempotently with the same binding.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
pub(crate) fn begin_shutdown() { SHUTTING_DOWN.store(true, Ordering::SeqCst); }
/// R7d round 2/3: read by the confirmation flow's effect gate (see [`SHUTTING_DOWN`]).
pub(crate) fn shutting_down() -> bool { SHUTTING_DOWN.load(Ordering::SeqCst) }

/// The bounded retry of a stage that answered [`Error::Pending`].
#[derive(Clone)]
pub(crate) struct PendingWait { first: Duration, every: Duration, total: Duration, cancel: Option<Arc<AtomicBool>>,
    /// Waits begun between attempts (tests read it).
    sleeps: Arc<AtomicUsize> }
impl Default for PendingWait {
    fn default() -> Self { Self { first: PENDING_FIRST_RETRY, every: PENDING_RETRY_EVERY, total: PENDING_WAIT_TOTAL, cancel: None,
        sleeps: Arc::default() } }
}
impl PendingWait {
    /// Tests: a shorter schedule and a private cancel flag.
    #[cfg(test)]
    pub(crate) fn for_test(first: Duration, every: Duration, total: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self { first, every, total, cancel: Some(cancel), sleeps: Arc::default() }
    }
    /// Tests: how many waits between attempts began (shared by clones).
    #[cfg(test)]
    pub(crate) fn sleeps(&self) -> usize { self.sleeps.load(Ordering::SeqCst) }
    fn cancelled(&self) -> bool {
        SHUTTING_DOWN.load(Ordering::SeqCst) || self.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
    }
    /// Runs `attempt` again while it answers `Pending`: after `first`, then
    /// every `every`, while the next attempt still starts within `total`.
    /// Still pending at the end, `PendingLimit` (never waited for: only a
    /// probe in flight for this employee holds the thread), or cancelled:
    /// `Err(Pending)` (never another error, so the caller never compensates).
    /// Any other answer is returned.
    ///
    /// Cancellation (shutdown) is checked right before and right after every
    /// attempt, not only while sleeping: once shutdown has begun no attempt
    /// starts, and the answer of an attempt in flight is discarded — a Ready
    /// and a hard refusal included — as `Err(Pending)`, so nothing publishes,
    /// reads back or compensates (`Disable`) during shutdown. Discarding is
    /// what process death right after the attempt would do: no receipt is
    /// recorded, the step stays 0, and the next apply after restart re-runs
    /// the same (re-validated) stage with the same binding. For a discarded
    /// Ready spawnd's row is Ready (or still Reserved) and its journal entry
    /// is keyed by that binding, so the repeated Allocate is answered
    /// idempotently from it (replay), never a second employee; a discarded
    /// hard refusal is seen again then and compensated by a live gateway.
    pub(crate) fn run<T>(&self, mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
        let started = Instant::now();
        let mut delay = self.first;
        loop {
            if self.cancelled() { return Err(Error::Pending); }
            let answer = attempt();
            if self.cancelled() { return Err(Error::Pending); }
            match answer {
                Err(Error::Pending) => {}
                Err(Error::PendingLimit) => return Err(Error::Pending),
                other => return other,
            }
            if started.elapsed() + delay > self.total || !self.sleep(delay) { return Err(Error::Pending); }
            delay = self.every;
        }
    }
    /// `false` when cancelled before `delay` elapsed.
    fn sleep(&self, delay: Duration) -> bool {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        let until = Instant::now() + delay;
        loop {
            if self.cancelled() { return false; }
            let now = Instant::now();
            if now >= until { return true; }
            std::thread::sleep((until - now).min(PENDING_CANCEL_POLL));
        }
    }
}

/// The outer adapter boundary covers synchronous publication/compensation too,
/// including filesystem work that runs before or after an async bridge call.
pub(super) fn operation<T>(run: impl FnOnce() -> Result<T>) -> Result<T> {
    operation_for(ADAPTER_TIMEOUT, run)
}

fn operation_for<T>(timeout: Duration, run: impl FnOnce() -> Result<T>) -> Result<T> {
    let proposed = Instant::now() + timeout;
    let deadline = duduclaw_core::fs_lock::current_lock_deadline().map_or(proposed, |old| old.min(proposed));
    duduclaw_core::fs_lock::with_lock_deadline(deadline, || {
        if Instant::now() >= deadline { return Err(Error::Apply); }
        let result = run();
        if Instant::now() >= deadline { return Err(Error::Apply); }
        result
    })
}

pub(super) fn bounded<F: Future>(handle: &tokio::runtime::Handle, future: F) -> Result<F::Output> {
    bounded_for(handle, ADAPTER_TIMEOUT, future)
}

fn bounded_for<F: Future>(handle: &tokio::runtime::Handle, timeout: Duration, future: F) -> Result<F::Output> {
    // Keep ownership of the future here: expiry drops it before compensation can
    // begin. Do not spawn a detached publisher that survives this timeout.
    operation_for(timeout, || {
        let deadline = duduclaw_core::fs_lock::current_lock_deadline().ok_or(Error::Apply)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Both the async timer and nested synchronous file locks inherit the
        // outer adapter's remaining budget; a nested call never renews it.
        handle.block_on(async { tokio::time::timeout(remaining, future).await.map_err(|_| Error::Apply) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn timeout_drops_operation_before_returning_failure() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let operation = async move { let _guard = Dropped(dropped); std::future::pending::<()>().await };
            assert!(matches!(bounded_for(&handle, Duration::from_millis(10), operation), Err(Error::Apply)));
        }).await.unwrap();
        assert!(observed.load(Ordering::SeqCst));
    }
    #[tokio::test]
    async fn synchronous_product_lock_inherits_bridge_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        let held = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(dir.path().join("profile.lock")).unwrap();
        held.lock().unwrap();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let result = bounded_for(&handle, Duration::from_millis(20), async {
                duduclaw_core::fs_lock::with_file_lock(&path, || {
                    panic!("contended product writer must not execute");
                    #[allow(unreachable_code)] Ok(())
                }).map_err(|_| Error::Apply)
            });
            assert!(matches!(result, Err(Error::Apply) | Ok(Err(Error::Apply))));
            assert!(start.elapsed() < Duration::from_secs(1));
        }).await.unwrap();
    }

    #[tokio::test]
    async fn outer_adapter_bounds_publication_after_async_install_and_nested_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        let held = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(dir.path().join("artifact.lock")).unwrap();
        held.lock().unwrap();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let result = operation_for(Duration::from_millis(20), || {
                bounded_for(&handle, Duration::from_secs(1), async {})?;
                // Artifact publication / rollback uses a normal synchronous
                // writer after the async installer has already returned.
                duduclaw_core::fs_lock::with_file_lock(&path, || {
                    panic!("late publication must inherit the outer adapter deadline");
                    #[allow(unreachable_code)] Ok(())
                }).map_err(|_| Error::Apply)
            });
            assert!(matches!(result, Err(Error::Apply)));
            assert!(start.elapsed() < Duration::from_millis(500));
            let start = Instant::now();
            let result = operation_for(Duration::from_millis(20), || {
                bounded_for(&handle, Duration::from_secs(1), std::future::pending::<()>())
            });
            assert!(matches!(result, Err(Error::Apply)));
            assert!(start.elapsed() < Duration::from_millis(500));
        }).await.unwrap();
    }

}
