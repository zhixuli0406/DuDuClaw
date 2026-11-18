//! DEFECT 26: the trusted boot assembly converges without a restart. The first
//! attempt runs in a background task (review round 1: off the gateway's
//! start-up path); while trusted inputs are still missing that task repeats
//! the whole attempt on a bounded schedule until it
//! succeeds once, a missing producer can no longer appear in this process, or
//! the gateway's shutdown gate closes. Each attempt is all-or-nothing: a failed
//! one only replaces the closed not-ready reasons, it never publishes a
//! partially assembled service. A success is never repeated. The spawnd
//! backend connection has its own retry (`retry_backend`) on the same pause.
use std::{future::Future, pin::Pin, sync::Mutex, time::{Duration, Instant}};
use duduclaw_migration::confirmation::EffectGate;
use super::{BootStatus, MissingProducer};

/// A failed attempt. `retry: false` when no later attempt in this process can
/// succeed (the reasons stay published, the loop ends).
pub(super) struct Failure {
    pub(super) missing: Vec<MissingProducer>,
    pub(super) retry: bool,
}
impl Failure {
    /// R8j round 2 B3: the attempt ended at the shutdown gate. Nothing is
    /// published or logged and the boot task ends (no retry).
    pub(super) fn closed() -> Self { Self { missing: Vec::new(), retry: false } }
    /// Only [`Failure::closed`] has an empty missing set.
    fn is_closed(&self) -> bool { self.missing.is_empty() && !self.retry }
}

/// The bounded wait for the updater's generation at the start of an attempt:
/// at most `tries` checks `interval` apart (production: [`GENERATION_WAIT`],
/// 50 × 100 ms = 5 s). When it does not appear the attempt continues (and
/// reports it missing). R8j round 2 B3: the gate is checked before every
/// check, so a shutdown ends the wait within one `interval`
/// (`Err(Failure::closed())`).
pub(super) async fn wait_for_generation(present: impl Fn() -> bool, closed: &impl Fn() -> bool, tries: u32,
    interval: Duration) -> Result<(), Failure> {
    for _ in 0..tries {
        if closed() { return Err(Failure::closed()); }
        if present() { return Ok(()); }
        tokio::time::sleep(interval).await;
    }
    if closed() { Err(Failure::closed()) } else { Ok(()) }
}
/// Production [`wait_for_generation`] bound: 50 checks, 100 ms apart.
pub(super) const GENERATION_WAIT: (u32, Duration) = (50, Duration::from_millis(100));

/// R8j round 3 F3: runs `effects` — every process-wide side effect of one
/// attempt (singleton installations, assembly) and its publication — inside
/// ONE permit of `gate` (`EffectGate::enter`, which tests "closed" and
/// registers the effect atomically). A gate that refuses (closed) ends the
/// attempt with [`Failure::closed`] and `effects` never runs, so nothing is
/// installed; a gate closing after the permit was taken lets the attempt
/// complete (shutdown waits for it, bounded). The permit drops after
/// `effects` returned, publication included. `effects` must be synchronous
/// local construction (no network or child-process wait).
pub(super) fn under_permit<T>(gate: &(impl EffectGate + ?Sized), effects: impl FnOnce() -> Result<T, Failure>)
    -> Result<T, Failure> {
    let Some(permit) = gate.enter() else { return Err(Failure::closed()) };
    let result = effects();
    drop(permit);
    result
}

/// The whole boot task: nothing at all once the gate is closed (R8j round 2
/// B3: checked before the first attempt too), then the first attempt and,
/// while it answers `Retry`, [`retry_until_assembled`]. `true` once assembled.
pub(super) async fn run(boot: &mut impl BootAttempt, state: &BootState, started: Instant, schedule: Schedule,
    closed: impl Fn() -> bool) -> bool {
    if closed() { return false; }
    match step(boot, state, started).await {
        Step::Assembled => true,
        Step::Stop => false,
        Step::Retry => retry_until_assembled(boot, state, started, schedule, closed).await,
    }
}

pub(super) type AttemptFuture<'a> = Pin<Box<dyn Future<Output = Result<(), Failure>> + Send + 'a>>;

pub(super) trait BootAttempt: Send {
    /// One complete attempt: validates every input afresh and, only when all
    /// pass, publishes the assembled service and its constructed status.
    fn attempt(&mut self) -> AttemptFuture<'_>;
    /// Called exactly once, after the first (and only) successful attempt.
    fn assembled(&mut self);
}

/// The boot status, replaced by every attempt until the first success (never a
/// latched failure), and the last missing set that was reported.
pub(super) struct BootState {
    status: Mutex<Option<BootStatus>>,
    reported: Mutex<Option<Vec<MissingProducer>>>,
}
impl BootState {
    pub(super) const fn new() -> Self { Self { status: Mutex::new(None), reported: Mutex::new(None) } }
    /// `None` (or a poisoned lock) reads as not constructed.
    pub(super) fn status(&self) -> Option<BootStatus> { self.status.lock().ok().and_then(|status| status.clone()) }
    pub(super) fn constructed(&self) {
        if let Ok(mut status) = self.status.lock() { *status = Some(BootStatus { constructed: true, missing: vec![] }); }
    }
    /// Publishes the not-ready reasons of a failed attempt. `true` when the
    /// missing set differs from the last one reported (log only then).
    fn failed(&self, missing: &[MissingProducer]) -> bool {
        if let Ok(mut status) = self.status.lock() {
            *status = Some(BootStatus { constructed: false, missing: missing.to_vec() });
        }
        let Ok(mut reported) = self.reported.lock() else { return true };
        if reported.as_deref() == Some(missing) { return false; }
        *reported = Some(missing.to_vec());
        true
    }
}

pub(super) enum Step { Assembled, Retry, Stop }

/// One attempt plus its reporting: ERROR only when the missing set changed,
/// one INFO (elapsed seconds since boot start) on success.
pub(super) async fn step(boot: &mut impl BootAttempt, state: &BootState, started: Instant) -> Step {
    match boot.attempt().await {
        Ok(()) => {
            tracing::info!(elapsed_secs = started.elapsed().as_secs(),
                "隔離工具服務已完成可信任的啟動組裝，團隊執行的就緒檢查已恢復。");
            boot.assembled();
            Step::Assembled
        }
        // Shutdown: the previous status stays, nothing is logged.
        Err(failure) if failure.is_closed() => Step::Stop,
        Err(Failure { missing, retry }) => {
            if state.failed(&missing) {
                if matches!(missing.as_slice(), [MissingProducer::Initialization]) {
                    tracing::error!("隔離工具服務初始化未通過，團隊執行維持暫停。");
                } else {
                    tracing::error!(missing = ?missing, "隔離工具服務尚缺可信任的啟動資料，團隊執行維持暫停。");
                }
            }
            if retry { Step::Retry } else { Step::Stop }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Schedule {
    pub(super) early: Duration,
    pub(super) early_for: Duration,
    pub(super) late: Duration,
    /// Granularity at which a pause notices shutdown (never zero).
    pub(super) poll: Duration,
}
/// Every 2 s during the first minute after boot start, then every 10 s.
pub(super) const SCHEDULE: Schedule = Schedule { early: Duration::from_secs(2), early_for: Duration::from_secs(60),
    late: Duration::from_secs(10), poll: Duration::from_millis(250) };
impl Schedule {
    fn delay(&self, elapsed: Duration) -> Duration { if elapsed < self.early_for { self.early } else { self.late } }
}

/// Retries after a first attempt that answered `Step::Retry`. `true` once
/// assembled; `false` when the shutdown gate closed or no retry can succeed.
pub(super) async fn retry_until_assembled(boot: &mut impl BootAttempt, state: &BootState, started: Instant,
    schedule: Schedule, closed: impl Fn() -> bool) -> bool {
    loop {
        if !pause(schedule.delay(started.elapsed()), schedule.poll, &closed).await { return false; }
        match step(boot, state, started).await {
            Step::Assembled => return true,
            Step::Stop => return false,
            Step::Retry => {}
        }
    }
}

/// The spawnd backend connection (a local socket connect, no waits): every
/// 500 ms during the first minute after boot start, then every 5 s.
pub(super) const BACKEND_SCHEDULE: Schedule = Schedule { early: Duration::from_millis(500),
    early_for: Duration::from_secs(60), late: Duration::from_secs(5), poll: Duration::from_millis(100) };

/// Retries the spawnd backend connection after a failed inline attempt at
/// boot. `connect` answers `true` once a backend is installed (by this attempt
/// or an earlier one; it never replaces one). The first `true` marks the
/// gateway-local readiness step `Backend` in `local` and ends the loop
/// (`true`); `false` when the shutdown gate closed first (the step stays unset).
pub(super) async fn retry_backend<F, Fut>(mut connect: F, local: &crate::update_readback::LocalReadiness,
    started: Instant, schedule: Schedule, closed: impl Fn() -> bool) -> bool
where F: FnMut() -> Fut, Fut: Future<Output = bool> {
    loop {
        if !pause(schedule.delay(started.elapsed()), schedule.poll, &closed).await { return false; }
        if connect().await {
            local.mark(crate::update_readback::LocalStep::Backend);
            return true;
        }
    }
}

/// `false` when `closed` answered true before `delay` elapsed.
async fn pause(delay: Duration, poll: Duration, closed: &impl Fn() -> bool) -> bool {
    let poll = poll.max(Duration::from_millis(1));
    let until = Instant::now() + delay;
    loop {
        if closed() { return false; }
        let now = Instant::now();
        if now >= until { return true; }
        tokio::time::sleep((until - now).min(poll)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    const FAST: Schedule = Schedule { early: Duration::from_millis(2), early_for: Duration::from_secs(60),
        late: Duration::from_millis(5), poll: Duration::from_millis(1) };
    const MISSING: [MissingProducer; 2] = [MissingProducer::RuntimePublication, MissingProducer::MainPlans];

    /// Inputs missing for the first `missing_for` attempts, then present. Like
    /// production, only a complete attempt publishes (service + constructed).
    struct Fake {
        state: Arc<BootState>,
        missing_for: usize,
        retry: bool,
        attempts: Arc<AtomicUsize>,
        published: Arc<AtomicUsize>,
        reconciles: Arc<AtomicUsize>,
    }
    impl Fake {
        fn new(state: &Arc<BootState>, missing_for: usize) -> Self {
            Self { state: state.clone(), missing_for, retry: true, attempts: Arc::default(),
                published: Arc::default(), reconciles: Arc::default() }
        }
    }
    impl BootAttempt for Fake {
        fn attempt(&mut self) -> AttemptFuture<'_> {
            Box::pin(async move {
                let attempt = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
                assert_eq!(self.published.load(Ordering::Acquire), 0, "a success is never re-run");
                if attempt <= self.missing_for {
                    return Err(Failure { missing: MISSING.to_vec(), retry: self.retry });
                }
                self.published.fetch_add(1, Ordering::AcqRel);
                self.state.constructed();
                Ok(())
            })
        }
        fn assembled(&mut self) { self.reconciles.fetch_add(1, Ordering::AcqRel); }
    }

    #[tokio::test]
    async fn boot_retry_converges_after_missing_inputs_assembles_once_and_reconciles_once() {
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, 4);
        let started = Instant::now();
        assert!(matches!(step(&mut boot, &state, started).await, Step::Retry));
        assert!(state.status().is_some_and(|status| !status.constructed && status.missing == MISSING));
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 0, "no reconcile before a successful boot");
        assert!(retry_until_assembled(&mut boot, &state, started, FAST, || false).await);
        assert_eq!(boot.attempts.load(Ordering::Acquire), 5);
        assert_eq!(boot.published.load(Ordering::Acquire), 1);
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 1);
        // The status the readiness reads turned constructed without a restart.
        assert!(state.status().is_some_and(|status| status.constructed && status.missing.is_empty()));
    }

    #[tokio::test]
    async fn boot_retry_success_is_never_rerun() {
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, 1);
        let started = Instant::now();
        assert!(matches!(step(&mut boot, &state, started).await, Step::Retry));
        assert!(retry_until_assembled(&mut boot, &state, started, FAST, || false).await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(boot.attempts.load(Ordering::Acquire), 2, "the loop ended at the success");
        assert_eq!(boot.published.load(Ordering::Acquire), 1);
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 1);
        // A first-attempt success never starts a retry at all.
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, 0);
        assert!(matches!(step(&mut boot, &state, Instant::now()).await, Step::Assembled));
        assert_eq!((boot.attempts.load(Ordering::Acquire), boot.reconciles.load(Ordering::Acquire)), (1, 1));
    }

    #[tokio::test]
    async fn boot_retry_never_assembles_without_inputs_and_stops_at_shutdown() {
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, usize::MAX);
        let attempts = boot.attempts.clone();
        let started = Instant::now();
        assert!(matches!(step(&mut boot, &state, started).await, Step::Retry));
        assert!(!retry_until_assembled(&mut boot, &state, started, FAST,
            || attempts.load(Ordering::Acquire) >= 6).await);
        assert_eq!(boot.attempts.load(Ordering::Acquire), 6, "no attempt after the gate closed");
        assert_eq!(boot.published.load(Ordering::Acquire), 0);
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 0);
        assert!(state.status().is_some_and(|status| !status.constructed && status.missing == MISSING));
        // A gate already closed: no further attempt at all.
        assert!(!retry_until_assembled(&mut boot, &state, started, FAST, || true).await);
        assert_eq!(boot.attempts.load(Ordering::Acquire), 6);
    }

    #[tokio::test]
    async fn boot_retry_failing_attempt_publishes_nothing_and_final_failure_stops() {
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, usize::MAX);
        boot.retry = false;
        let started = Instant::now();
        assert!(matches!(step(&mut boot, &state, started).await, Step::Stop));
        assert!(!retry_until_assembled(&mut boot, &state, started, FAST, || false).await);
        assert_eq!(boot.attempts.load(Ordering::Acquire), 2);
        assert_eq!(boot.published.load(Ordering::Acquire), 0);
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 0);
        assert!(state.status().is_some_and(|status| !status.constructed && status.missing == MISSING));
    }

    /// R8j round 2 B3: a gate already closed when the boot task starts: no
    /// attempt at all, nothing published.
    #[tokio::test]
    async fn boot_gate_closed_before_the_first_attempt_attempts_nothing() {
        let state = Arc::new(BootState::new());
        let mut boot = Fake::new(&state, 0);
        assert!(!run(&mut boot, &state, Instant::now(), FAST, || true).await);
        assert_eq!(boot.attempts.load(Ordering::Acquire), 0);
        assert_eq!(boot.published.load(Ordering::Acquire), 0);
        assert_eq!(boot.reconciles.load(Ordering::Acquire), 0);
        assert!(state.status().is_none());
        // Open gate: the same task assembles (first attempt, then retries).
        let mut boot = Fake::new(&state, 2);
        assert!(run(&mut boot, &state, Instant::now(), FAST, || false).await);
        assert_eq!((boot.attempts.load(Ordering::Acquire), boot.published.load(Ordering::Acquire)), (3, 1));
    }

    /// A gate whose `enter` refuses once `.0` answers closed (the peek and the
    /// permit read the same counter here; the real gate does both atomically).
    struct Peek<F: Fn() -> bool>(F);
    impl<F: Fn() -> bool> EffectGate for Peek<F> {
        fn enter(&self) -> Option<duduclaw_migration::confirmation::EffectPermit<'_>> {
            (!(self.0)()).then(duduclaw_migration::confirmation::EffectPermit::untracked)
        }
        fn is_closed(&self) -> bool { (self.0)() }
    }

    /// Mirrors `attempt_installed`'s gated steps: the generation wait (peeks),
    /// then the side-effectful assembly inside one permit (`under_permit`).
    struct Gated { gate: Arc<AtomicUsize>, closes_at: usize, generation_checks: Arc<AtomicUsize>,
        assembled: Arc<AtomicUsize> }
    impl Gated {
        /// Closed from the `closes_at`-th gate check on.
        fn closed(&self) -> bool { self.gate.fetch_add(1, Ordering::AcqRel) + 1 >= self.closes_at }
    }
    impl BootAttempt for Gated {
        fn attempt(&mut self) -> AttemptFuture<'_> {
            Box::pin(async move {
                let checks = self.generation_checks.clone();
                let closed = || self.closed();
                // The updater's generation never appears.
                wait_for_generation(|| { checks.fetch_add(1, Ordering::AcqRel); false }, &closed,
                    GENERATION_WAIT.0, Duration::from_millis(20)).await?;
                let assembled = self.assembled.clone();
                under_permit(&Peek(&closed), || { assembled.fetch_add(1, Ordering::AcqRel); Ok(()) })
            })
        }
        fn assembled(&mut self) {}
    }

    #[tokio::test]
    async fn boot_gate_closed_during_the_generation_wait_never_assembles() {
        let state = Arc::new(BootState::new());
        let mut boot = Gated { gate: Arc::default(), closes_at: 4, generation_checks: Arc::default(),
            assembled: Arc::default() };
        let gate = boot.gate.clone();
        let started = Instant::now();
        // The task's own gate (before the first attempt) is still open.
        assert!(!run(&mut boot, &state, started, FAST, || false).await);
        assert_eq!(boot.assembled.load(Ordering::Acquire), 0, "never assembled after the gate closed");
        // Ended promptly: 3 generation checks, not the 50-check bound (1 s here).
        assert_eq!(boot.generation_checks.load(Ordering::Acquire), 3);
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
        assert_eq!(gate.load(Ordering::Acquire), 4);
        // Nothing published or logged for a shutdown.
        assert!(state.status().is_none());
        // The generation appears but the gate closes right before the permit.
        let assembled = Arc::new(AtomicUsize::new(0));
        let closed_at_assembly = AtomicUsize::new(0);
        let closed = || closed_at_assembly.fetch_add(1, Ordering::AcqRel) >= 1;
        let outcome = async {
            wait_for_generation(|| true, &closed, GENERATION_WAIT.0, Duration::from_millis(20)).await?;
            under_permit(&Peek(&closed), || { assembled.fetch_add(1, Ordering::AcqRel); Ok(()) })
        }.await;
        assert!(outcome.is_err_and(|failure| failure.is_closed()));
        assert_eq!(assembled.load(Ordering::Acquire), 0);
        // An open gate and no generation: the bounded wait ends and the attempt continues.
        assert!(wait_for_generation(|| false, &|| false, 3, Duration::from_millis(1)).await.is_ok());
        assert_eq!(GENERATION_WAIT, (50, Duration::from_millis(100)));
    }

    /// R8j round 3 F3, with the real process-wide gate type: a gate closed
    /// before the permit refuses it, and nothing (no singleton, no
    /// publication) is installed.
    #[test]
    fn boot_permit_refused_by_a_closed_gate_installs_nothing() {
        use crate::handlers::MigrationShutdownGate as ShutdownGate;
        let gate = ShutdownGate::new();
        gate.close();
        let installed = AtomicUsize::new(0);
        let outcome = under_permit(&gate, || { installed.fetch_add(1, Ordering::AcqRel); Ok(()) });
        assert!(outcome.is_err_and(|failure| failure.is_closed()));
        assert_eq!(installed.load(Ordering::Acquire), 0, "no singleton installed");
        assert_eq!(gate.running(), 0);
    }

    /// The gate closes AFTER the permit was taken: the attempt completes
    /// (installs and publishes), shutdown sees it running until then, and the
    /// permit is released after the publication.
    #[test]
    fn boot_permit_taken_before_close_completes_the_attempt() {
        use crate::handlers::MigrationShutdownGate as ShutdownGate;
        let gate = ShutdownGate::new();
        let (installed, published) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let outcome = under_permit(&gate, || {
            installed.fetch_add(1, Ordering::AcqRel);
            assert_eq!(gate.running(), 1, "registered while it installs");
            gate.close();
            assert!(!gate.wait_idle(Instant::now() + Duration::from_millis(20)), "shutdown waits for the permit");
            published.fetch_add(1, Ordering::AcqRel);
            Ok(())
        });
        assert!(outcome.is_ok());
        assert_eq!((installed.load(Ordering::Acquire), published.load(Ordering::Acquire)), (1, 1));
        assert_eq!(gate.running(), 0, "the permit dropped after the publication");
        assert!(gate.wait_idle(Instant::now()));
        // Every later attempt is refused.
        assert!(under_permit(&gate, || -> Result<(), Failure> { panic!("never runs after close") })
            .is_err_and(|failure| failure.is_closed()));
    }

    /// The worker is descheduled inside `enter` while shutdown closes the
    /// gate: when it resumes `enter` answers None, the attempt ends and
    /// nothing is installed; nothing stays registered.
    #[test]
    fn boot_permit_descheduled_across_close_installs_nothing() {
        use crate::handlers::MigrationDescheduledGate as Descheduled;
        let gate = Descheduled::new(1);
        let installed = AtomicUsize::new(0);
        let outcome = std::thread::scope(|s| {
            let worker = s.spawn(|| under_permit(&gate, || { installed.fetch_add(1, Ordering::AcqRel); Ok(()) }));
            gate.close_while_descheduled();
            worker.join().unwrap()
        });
        assert!(outcome.is_err_and(|failure| failure.is_closed()));
        assert_eq!(installed.load(Ordering::Acquire), 0, "no singleton installed");
        assert_eq!((gate.enters(), gate.gate.running()), (1, 0));
    }

    #[test]
    fn boot_retry_reports_missing_set_only_on_change() {
        let state = BootState::new();
        assert!(state.status().is_none());
        assert!(state.failed(&MISSING));
        assert!(!state.failed(&MISSING), "unchanged set: no second ERROR");
        assert!(!state.failed(&MISSING));
        assert!(state.failed(&[MissingProducer::MainPlans]), "changed set: reported");
        assert_eq!(state.status().unwrap().missing, vec![MissingProducer::MainPlans]);
        assert!(!state.failed(&[MissingProducer::MainPlans]));
        assert!(state.failed(&MISSING), "back to an earlier set is a change");
        assert!(state.failed(&[MissingProducer::Initialization]));
        assert!(!state.failed(&[MissingProducer::Initialization]));
    }

    /// Review round 1: spawnd not reachable at first, then reachable. The
    /// backend is installed exactly once, the local step is marked (with the
    /// other steps the latch is set) and the loop ends; no attempt after.
    #[tokio::test]
    async fn backend_retry_installs_once_sets_the_latch_and_ends() {
        use crate::update_readback::{LocalReadiness, LocalStep};
        let local = LocalReadiness::new();
        local.mark(LocalStep::Stores);
        local.mark(LocalStep::Service);
        let (attempts, installs) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let connect = || {
            let (attempts, installs) = (attempts.clone(), installs.clone());
            async move {
                // Reachable from the fourth attempt on; installed once (set-once slot).
                if attempts.fetch_add(1, Ordering::AcqRel) + 1 < 4 { return false; }
                if installs.load(Ordering::Acquire) == 0 { installs.fetch_add(1, Ordering::AcqRel); }
                true
            }
        };
        assert!(!local.ready(), "the inline attempt failed: not ready");
        assert!(retry_backend(connect, &local, Instant::now(), FAST, || false).await);
        assert!(local.ready());
        assert_eq!((attempts.load(Ordering::Acquire), installs.load(Ordering::Acquire)), (4, 1));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(attempts.load(Ordering::Acquire), 4, "the loop ended at the success");
    }

    /// spawnd never reachable: the latch is never set and the loop ends once
    /// the shutdown gate closes (and at once if it is already closed).
    #[tokio::test]
    async fn backend_retry_never_connecting_never_sets_the_latch_and_stops_at_shutdown() {
        use crate::update_readback::{LocalReadiness, LocalStep};
        let local = LocalReadiness::new();
        local.mark(LocalStep::Stores);
        local.mark(LocalStep::Service);
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect = || { let attempts = attempts.clone(); async move { attempts.fetch_add(1, Ordering::AcqRel); false } };
        let gate = attempts.clone();
        assert!(!retry_backend(connect, &local, Instant::now(), FAST, || gate.load(Ordering::Acquire) >= 5).await);
        assert_eq!(attempts.load(Ordering::Acquire), 5, "no attempt after the gate closed");
        assert!(!local.ready());
        let connect = || { let attempts = attempts.clone(); async move { attempts.fetch_add(1, Ordering::AcqRel); true } };
        assert!(!retry_backend(connect, &local, Instant::now(), FAST, || true).await);
        assert_eq!(attempts.load(Ordering::Acquire), 5);
        assert!(!local.ready());
    }

    #[test]
    fn backend_retry_schedule_is_bounded_and_fast_at_start() {
        assert_eq!(BACKEND_SCHEDULE.delay(Duration::ZERO), Duration::from_millis(500));
        assert_eq!(BACKEND_SCHEDULE.delay(Duration::from_secs(60)), Duration::from_secs(5));
        assert!(BACKEND_SCHEDULE.poll > Duration::ZERO && BACKEND_SCHEDULE.poll < BACKEND_SCHEDULE.early);
    }

    #[test]
    fn boot_retry_schedule_is_two_seconds_for_a_minute_then_ten() {
        assert_eq!(SCHEDULE.delay(Duration::ZERO), Duration::from_secs(2));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(59)), Duration::from_secs(2));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(60)), Duration::from_secs(10));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(86_400)), Duration::from_secs(10));
        assert!(SCHEDULE.poll > Duration::ZERO && SCHEDULE.poll < SCHEDULE.early);
    }
}
