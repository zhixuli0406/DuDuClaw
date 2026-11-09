//! One bounded bridge for every migration authority async adapter. Call only from
//! the blocking pool, after the confirmation intent transaction has committed.
use super::{Error, Result};
use std::{future::Future, time::{Duration, Instant}};
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(30);

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
