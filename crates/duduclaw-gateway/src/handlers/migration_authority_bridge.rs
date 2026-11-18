//! One bounded bridge for every migration authority async adapter. Call only from
//! the blocking pool, after the confirmation intent transaction has committed.
use super::{Error, Result};
use duduclaw_migration::confirmation::{EffectGate, EffectPermit};
use std::{future::Future, sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, atomic::{AtomicBool, AtomicUsize, Ordering}},
    time::{Duration, Instant}};
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

/// R7d round 4: the process-wide effect gate of the migration confirmation
/// flow, closed once when the gateway starts shutting down
/// (`begin_migration_apply_shutdown`). Every pending wait stops at its next
/// poll (the item stays `Applying`, nothing is compensated).
pub(crate) static SHUTDOWN_GATE: ShutdownGate = ShutdownGate::new();

/// Guarantee (R7d round 4), for the confirmation flow (approve, retry, the
/// retry of a `Failed` item, reject) and every pending-allocation attempt:
/// (a) once [`Self::close`] has returned, no new external effect
///     (Allocate/stage, publish, read back, Disable) can start. Every adapter
///     call runs inside a permit from [`EffectGate::enter`]
///     (`ConfirmationStore::{apply_next_until, retry_until, reject_until}`,
///     the Employee branch of `retry_failed`, each attempt of
///     [`PendingWait::run`]), and `enter` tests `closed` and registers the
///     effect under the same lock `close` takes. An effect therefore either
///     registered before `close()` (it is in progress and `running` counts
///     it) or is refused — however long its thread was descheduled. This is
///     enforced by the lock, not by timing. A refused call keeps the item's
///     state and answers retry-safe.
/// (b) an effect registered before `close()` may still complete. The
///     shutdown wait ([`Self::wait_idle`], `APPLY_SHUTDOWN_WAIT` = 40 s) is
///     best effort: it only gives such effects time to finish cleanly, is
///     not what enforces (a), and on expiry shutdown continues.
/// (c) if the process exits while an effect is in flight, its committed
///     intent stays in the journal and the next apply / retry / reject
///     repeats the same effect idempotently with the same binding.
pub(crate) struct ShutdownGate { state: Mutex<GateState>, idle: Condvar }
struct GateState { closed: bool, running: usize }
impl ShutdownGate {
    pub(crate) const fn new() -> Self {
        Self { state: Mutex::new(GateState { closed: false, running: 0 }), idle: Condvar::new() }
    }
    /// A panic elsewhere never disables the gate: the state is two plain
    /// fields, consistent after any unwinding.
    fn lock(&self) -> MutexGuard<'_, GateState> { self.state.lock().unwrap_or_else(PoisonError::into_inner) }
    /// Closes the gate (idempotent). Once this returns no `enter` succeeds.
    pub(crate) fn close(&self) { self.lock().closed = true; }
    /// Effects registered and not finished yet.
    pub(crate) fn running(&self) -> usize { self.lock().running }
    /// Blocks until no effect is registered (`true`) or `deadline` passes
    /// (`false`). Never call it on an async worker thread.
    pub(crate) fn wait_idle(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        loop {
            if state.running == 0 { return true; }
            let now = Instant::now();
            if now >= deadline { return false; }
            state = self.idle.wait_timeout(state, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}
impl EffectGate for ShutdownGate {
    fn enter(&self) -> Option<EffectPermit<'_>> {
        {
            let mut state = self.lock();
            if state.closed { return None; }
            state.running += 1;
        }
        Some(EffectPermit::new(move || {
            self.lock().running -= 1;
            self.idle.notify_all();
        }))
    }
    fn is_closed(&self) -> bool { self.lock().closed }
}
pub(crate) fn begin_shutdown() { SHUTDOWN_GATE.close(); }

/// Tests: the review's interleaving made deterministic. The `at`-th `enter`
/// (1-based) stops — a thread descheduled right before registering its
/// effect — until the test has closed the gate
/// ([`Self::close_while_descheduled`]), then proceeds into the real
/// [`ShutdownGate::enter`].
#[cfg(test)]
pub(crate) struct Descheduled { pub(crate) gate: ShutdownGate, at: usize, enters: AtomicUsize,
    /// (descheduled, resumed)
    signal: Mutex<(bool, bool)>, changed: Condvar }
#[cfg(test)]
impl Descheduled {
    pub(crate) fn new(at: usize) -> Self {
        Self { gate: ShutdownGate::new(), at, enters: AtomicUsize::new(0), signal: Mutex::new((false, false)), changed: Condvar::new() }
    }
    /// `enter` calls so far.
    pub(crate) fn enters(&self) -> usize { self.enters.load(Ordering::SeqCst) }
    /// Waits (≤ 10 s) until the worker is descheduled in its `at`-th
    /// `enter`, closes the gate, then lets the worker resume.
    pub(crate) fn close_while_descheduled(&self) {
        let (mut signal, _) = self.changed.wait_timeout_while(self.signal.lock().unwrap(), Duration::from_secs(10),
            |signal| !signal.0).unwrap();
        assert!(signal.0, "the worker never reached enter #{}", self.at);
        self.gate.close();
        signal.1 = true;
        self.changed.notify_all();
    }
}
#[cfg(test)]
impl EffectGate for Descheduled {
    fn enter(&self) -> Option<EffectPermit<'_>> {
        if self.enters.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            let mut signal = self.signal.lock().unwrap();
            signal.0 = true;
            self.changed.notify_all();
            let (signal, _) = self.changed.wait_timeout_while(signal, Duration::from_secs(10), |signal| !signal.1).unwrap();
            assert!(signal.1, "the test never closed the gate");
        }
        self.gate.enter()
    }
    fn is_closed(&self) -> bool { self.gate.is_closed() }
}

/// The bounded retry of a stage that answered [`Error::Pending`].
#[derive(Clone)]
pub(crate) struct PendingWait { first: Duration, every: Duration, total: Duration, cancel: Option<Arc<AtomicBool>>,
    /// Tests: a private gate instead of [`SHUTDOWN_GATE`].
    gate: Option<Arc<dyn EffectGate + Send + Sync>>,
    /// Waits begun between attempts (tests read it).
    sleeps: Arc<AtomicUsize> }
impl Default for PendingWait {
    fn default() -> Self { Self { first: PENDING_FIRST_RETRY, every: PENDING_RETRY_EVERY, total: PENDING_WAIT_TOTAL, cancel: None,
        gate: None, sleeps: Arc::default() } }
}
impl PendingWait {
    /// Tests: a shorter schedule and a private cancel flag.
    #[cfg(test)]
    pub(crate) fn for_test(first: Duration, every: Duration, total: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self { first, every, total, cancel: Some(cancel), gate: None, sleeps: Arc::default() }
    }
    /// Tests: a private effect gate.
    #[cfg(test)]
    pub(crate) fn with_gate(mut self, gate: Arc<dyn EffectGate + Send + Sync>) -> Self { self.gate = Some(gate); self }
    /// Tests: how many waits between attempts began (shared by clones).
    #[cfg(test)]
    pub(crate) fn sleeps(&self) -> usize { self.sleeps.load(Ordering::SeqCst) }
    fn gate(&self) -> &dyn EffectGate {
        match &self.gate { Some(gate) => &**gate, None => &SHUTDOWN_GATE }
    }
    /// The tests' private cancel flag (never set in production).
    fn cancel_flag(&self) -> bool { self.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) }
    fn cancelled(&self) -> bool { self.gate().is_closed() || self.cancel_flag() }
    /// Runs `attempt` again while it answers `Pending`: after `first`, then
    /// every `every`, while the next attempt still starts within `total`.
    /// Still pending at the end, `PendingLimit` (never waited for: only a
    /// probe in flight for this employee holds the thread), or cancelled:
    /// `Err(Pending)` (never another error, so the caller never compensates).
    /// Any other answer is returned.
    ///
    /// Each attempt is an effect: it runs only inside a permit of the effect
    /// gate (R7d round 4), so once the gate is closed no attempt starts (a
    /// refused `enter` is `Err(Pending)` without attempting). Right after
    /// every attempt the gate is looked at again: if it is closed by now,
    /// the answer of that attempt is discarded — a Ready
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
            if self.cancel_flag() { return Err(Error::Pending); }
            let Some(running) = self.gate().enter() else { return Err(Error::Pending); };
            let answer = attempt();
            drop(running);
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

    /// R7d round 4: an open gate admits (each permit counted while held);
    /// once closed it refuses; `close` is idempotent; an effect registered
    /// before `close` stays in progress until its permit drops.
    #[test]
    fn shutdown_gate_admits_until_closed() {
        let gate = ShutdownGate::new();
        assert!(!gate.is_closed());
        let first = gate.enter().expect("open gate");
        let second = gate.enter().expect("open gate");
        assert_eq!(gate.running(), 2);
        drop(first);
        assert_eq!(gate.running(), 1);
        gate.close();
        gate.close();
        assert!(gate.is_closed());
        assert!(gate.enter().is_none());
        assert_eq!(gate.running(), 1, "registered before close: still in progress");
        drop(second);
        assert_eq!(gate.running(), 0);
        assert!(gate.enter().is_none());
        assert_eq!(gate.running(), 0, "a refused enter registers nothing");
    }

    /// `wait_idle` answers at once when idle, `false` at the deadline while
    /// a permit is held, and `true` only once the last permit dropped.
    #[test]
    fn shutdown_gate_wait_idle_ends_at_last_permit_or_deadline() {
        let gate = ShutdownGate::new();
        let started = Instant::now();
        assert!(gate.wait_idle(started + Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_millis(500));
        let (held, release) = (std::sync::Barrier::new(2), std::sync::Barrier::new(2));
        std::thread::scope(|s| {
            s.spawn(|| {
                let first = gate.enter().unwrap();
                let second = gate.enter().unwrap();
                held.wait();
                release.wait();
                drop(first);
                std::thread::sleep(Duration::from_millis(100));
                drop(second);
            });
            held.wait();
            gate.close();
            let started = Instant::now();
            assert!(!gate.wait_idle(started + Duration::from_millis(150)));
            let elapsed = started.elapsed();
            assert!(elapsed >= Duration::from_millis(150) && elapsed < Duration::from_secs(2), "{elapsed:?}");
            assert_eq!(gate.running(), 2);
            release.wait();
            let started = Instant::now();
            assert!(gate.wait_idle(started + Duration::from_secs(10)));
            assert_eq!(gate.running(), 0, "returned with the last permit");
            assert!(started.elapsed() < Duration::from_secs(5));
        });
    }

    /// `close` racing `enter` from many threads: an `enter` begun after
    /// `close` returned (the worker read `close_returned == true` first)
    /// never succeeds. Deterministic: `close` released the gate's lock
    /// before `close_returned` was stored, so such an `enter` sees `closed`.
    #[test]
    fn shutdown_gate_close_racing_enter_admits_nothing_after_close_returns() {
        const THREADS: usize = 8;
        for _ in 0..20 {
            let gate = ShutdownGate::new();
            let close_returned = AtomicBool::new(false);
            let late = AtomicUsize::new(0);
            let started = std::sync::Barrier::new(THREADS + 1);
            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    s.spawn(|| {
                        drop(gate.enter().expect("open until every worker is running"));
                        started.wait();
                        loop {
                            let after_close = close_returned.load(Ordering::SeqCst);
                            let Some(running) = gate.enter() else { break };
                            if after_close { late.fetch_add(1, Ordering::SeqCst); }
                            drop(running);
                        }
                        assert!(gate.enter().is_none());
                    });
                }
                started.wait();
                gate.close();
                close_returned.store(true, Ordering::SeqCst);
            });
            assert_eq!(late.load(Ordering::SeqCst), 0, "an effect started after close() returned");
            assert_eq!(gate.running(), 0);
            assert!(gate.enter().is_none());
        }
    }

    /// The review's interleaving for a pending-allocation attempt: the
    /// worker is descheduled right before registering the attempt (the
    /// first, or the one after a pending answer) while the gate closes; it
    /// is refused and the attempt never runs.
    #[test]
    fn pending_wait_attempt_descheduled_across_close_never_runs() {
        for at in [1usize, 2] {
            let gate = Arc::new(Descheduled::new(at));
            let wait = PendingWait::for_test(Duration::from_millis(5), Duration::from_millis(5), Duration::from_secs(30),
                Arc::new(AtomicBool::new(false))).with_gate(gate.clone());
            let attempts = AtomicUsize::new(0);
            let answer = std::thread::scope(|s| {
                let worker = s.spawn(|| wait.run(|| { attempts.fetch_add(1, Ordering::SeqCst); Err::<(), _>(Error::Pending) }));
                gate.close_while_descheduled();
                worker.join().unwrap()
            });
            assert!(matches!(answer, Err(Error::Pending)), "at {at}");
            assert_eq!(attempts.load(Ordering::SeqCst), at - 1, "no attempt after close (at {at})");
            assert_eq!((gate.enters(), gate.gate.running()), (at, 0));
        }
    }

    /// The gate closing during an attempt discards its answer (a Ready
    /// included) as `Err(Pending)`; a closed gate starts no attempt.
    #[test]
    fn pending_wait_gate_closed_during_attempt_discards_answer() {
        let gate = Arc::new(ShutdownGate::new());
        let wait = PendingWait::for_test(Duration::from_millis(5), Duration::from_millis(5), Duration::from_secs(30),
            Arc::new(AtomicBool::new(false))).with_gate(gate.clone());
        let attempts = AtomicUsize::new(0);
        let answer = wait.run(|| { attempts.fetch_add(1, Ordering::SeqCst); gate.close(); Ok(7) });
        assert!(matches!(answer, Err(Error::Pending)));
        assert!(matches!(wait.run(|| { attempts.fetch_add(1, Ordering::SeqCst); Ok(7) }), Err(Error::Pending)));
        assert_eq!((attempts.load(Ordering::SeqCst), gate.running()), (1, 0));
    }
}
