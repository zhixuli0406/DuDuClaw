//! DEFECT 26: the trusted boot assembly converges without a restart. The first
//! attempt runs inline (as before); while trusted inputs are still missing a
//! background task repeats the whole attempt on a bounded schedule until it
//! succeeds once, a missing producer can no longer appear in this process, or
//! the gateway's shutdown gate closes. Each attempt is all-or-nothing: a failed
//! one only replaces the closed not-ready reasons, it never publishes a
//! partially assembled service. A success is never repeated.
use std::{future::Future, pin::Pin, sync::Mutex, time::{Duration, Instant}};
use super::{BootStatus, MissingProducer};

/// A failed attempt. `retry: false` when no later attempt in this process can
/// succeed (the reasons stay published, the loop ends).
pub(super) struct Failure {
    pub(super) missing: Vec<MissingProducer>,
    pub(super) retry: bool,
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

    #[test]
    fn boot_retry_schedule_is_two_seconds_for_a_minute_then_ten() {
        assert_eq!(SCHEDULE.delay(Duration::ZERO), Duration::from_secs(2));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(59)), Duration::from_secs(2));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(60)), Duration::from_secs(10));
        assert_eq!(SCHEDULE.delay(Duration::from_secs(86_400)), Duration::from_secs(10));
        assert!(SCHEDULE.poll > Duration::ZERO && SCHEDULE.poll < SCHEDULE.early);
    }
}
