//! `host.rs` unit tests (moved out unchanged in the HX security fix round).
use super::*;
fn entry() -> ScanEntry {
    ScanEntry { account: EmployeeAccount { employee: EmployeeId([1; 16]), uid: 70000, gid: 70000,
        state: crate::registry::AccountState::Ready }, run: None, cgroup: None, revokers: vec![],
        cancelled: Arc::new(AtomicBool::new(false)), deadline: None }
}
#[test]
fn employee_idle_reports_processes_and_attachments() {
    let (me, other) = (EmployeeId([1; 16]), EmployeeId([2; 16]));
    let uid = 70_000;
    let lease = |employee, byte| Attachment { employee, run: RunHandle(RunId([byte; 16])), token: RequestId([byte; 16]),
        device_major: 7, device_minor: 1 };
    // Nothing anywhere: idle.
    let quiet = idle_report(me, uid, &[], &[], &[], 0, &[]);
    assert!(quiet.idle());
    // Another employee's run, lease and unit never count against `me`.
    let known = [KnownRun { run: RunId([9; 16]), employee: other, open: true }];
    let foreign = idle_report(me, uid, &known, &[], &[lease(other, 9)], 0, &[RunId([9; 16])]);
    assert!(foreign.idle(), "{foreign:?}");
    // A process with the employee's uid or gid (counted by the scanner).
    let busy = idle_report(me, uid, &[], &[], &[], 2, &[]);
    assert_eq!((busy.processes, busy.idle()), (2, false));
    // A durable lease left behind (no run) is not idle.
    let leased = idle_report(me, uid, &[], &[], &[lease(me, 3), lease(other, 4)], 0, &[]);
    assert_eq!((leased.attachments, leased.idle()), (1, false));
    // Open root runs and crash-journal records (by id or by uid), deduplicated.
    let known = [KnownRun { run: RunId([5; 16]), employee: me, open: true },
        KnownRun { run: RunId([6; 16]), employee: me, open: false }];
    let journal = [RunRecord { run: RunId([5; 16]), employee: me, uid },
        RunRecord { run: RunId([7; 16]), employee: EmployeeId([3; 16]), uid }];
    let running = idle_report(me, uid, &known, &journal, &[], 0, &[]);
    assert_eq!(running.runs, 2, "run 5 once, plus the journal record carrying this uid");
    // A closed run is not live; its leftover unit still is.
    let known = [KnownRun { run: RunId([6; 16]), employee: me, open: false }];
    let unit = idle_report(me, uid, &known, &[], &[], 0, &[RunId([6; 16])]);
    assert_eq!((unit.runs, unit.units, unit.idle()), (0, 1, false));
    // A unit root cannot attribute to anyone counts against every employee.
    let orphan = idle_report(me, uid, &[], &[], &[], 0, &[RunId([8; 16])]);
    assert_eq!((orphan.units, orphan.idle()), (1, false));
    // Counts saturate instead of wrapping.
    assert_eq!(idle_report(me, uid, &[], &[], &[], usize::MAX, &[]).processes, u16::MAX);
}
/// R8y defect: the admitted set was computed only in `LinuxHost::open`, so
/// an employee allocated after spawnd started was refused at every
/// `ReserveRun` (`isolation_unavailable`, no unit, no journal line) until
/// spawnd restarted. `Core::refresh` now republishes it from the registry
/// (`PublishedView::update`); retire removes it the same way.
#[test]
fn employee_allocated_after_open_is_admitted_after_the_next_refresh() {
    let employee = EmployeeId([7; 16]);
    let report = EvidenceReport::current(Readiness::default(), 1).unwrap();
    let account = |state| EmployeeAccount { employee, uid: 70_007, gid: 70_007, state };
    // At open: no row yet (the empty registry of a fresh install).
    let mut view = PublishedView { report: report.clone(), policy: None,
        employees: admitted_employees(&crate::registry::Registry::default()) };
    let refusal = view.admit(employee).unwrap_err();
    assert_eq!((refusal.step.to_string(), refusal.code()), ("employee_not_admitted".to_string(), ErrorCode::IsolationUnavailable));
    // A reservation is not admitted.
    let reserved = crate::registry::Registry { employees: vec![account(crate::registry::AccountState::Reserved)],
        ..crate::registry::Registry::default() };
    view.update(&report, None, &reserved);
    assert_eq!(view.admit(employee).unwrap_err().step.to_string(), "employee_not_admitted");
    // The allocation made it Ready: the next refresh admits it (the next
    // gate, here the missing policy, is what refuses now).
    let ready = crate::registry::Registry { employees: vec![account(crate::registry::AccountState::Ready)],
        ..crate::registry::Registry::default() };
    view.update(&report, None, &ready);
    assert!(view.employees.contains(&employee));
    assert_eq!(view.admit(employee).unwrap_err().step.to_string(), "policy_missing");
    // Retired (tombstoned): no longer admitted.
    let retired = crate::registry::Registry { employees: vec![account(crate::registry::AccountState::Tombstone)],
        ..crate::registry::Registry::default() };
    view.update(&report, None, &retired);
    assert_eq!(view.admit(employee).unwrap_err().step.to_string(), "employee_not_admitted");
}
#[test]
fn readiness_gate_of_the_uid_monitor_is_labelled() {
    let employee = EmployeeId([1; 16]);
    let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Ok(vec![employee]) }));
    let mut reader = ScanReader { state: state.clone(), entries: Arc::new(Mutex::new(vec![entry()])) };
    assert_eq!(reader.health_step(employee).unwrap_err().step.to_string(), "intruder");
    state.lock().unwrap().result = Err(ErrorCode::IsolationUnavailable);
    assert_eq!(reader.health_step(employee).unwrap_err().step.to_string(), "scan_failed");
    state.lock().unwrap().at = Instant::now() - Duration::from_secs(3);
    assert_eq!(reader.health_step(employee).unwrap_err().step.to_string(), "scan_stale");
    let guard = state.lock().unwrap();
    assert_eq!(reader.health_step(employee).unwrap_err().step.to_string(), "scan_busy");
    drop(guard);
}
#[test]
fn cgroup_receipt_is_converted_to_absolute_proc_membership() {
    assert_eq!(proc_cgroup(std::path::Path::new("/sys/fs/cgroup/duduclaw.slice/run.service")).unwrap(), "/duduclaw.slice/run.service");
    assert!(proc_cgroup(std::path::Path::new("/sys/fs/cgroup")).is_err());
    assert!(proc_cgroup(std::path::Path::new("/other/run.service")).is_err());
}
#[test]
fn readiness_does_not_consume_intrusion_events_and_stale_scan_denies() {
    let employee = EmployeeId([1; 16]);
    let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Ok(vec![employee]) }));
    let mut reader = ScanReader { state: state.clone(), entries: Arc::new(Mutex::new(vec![entry()])) };
    assert!(reader.health(employee).is_err());
    assert!(reader.health(employee).is_err());
    assert_eq!(reader.scan().unwrap(), vec![employee]);
    assert!(reader.health(employee).is_ok());
    state.lock().unwrap().at = Instant::now() - Duration::from_secs(3);
    assert!(reader.health(employee).is_err());
    assert!(reader.scan().is_err());
}
#[test]
fn cancelling_a_queued_start_sets_the_same_fence_given_to_trampoline() {
    let employee = EmployeeId([1; 16]); let run = RunHandle(RunId([2; 16]));
    let entries = Arc::new(Mutex::new(vec![entry()]));
    let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Ok(vec![]) }));
    let mut reader = ScanReader { state: state.clone(), entries: entries.clone() };
    let monitor = Monitor { entries, state, stop: Arc::new(AtomicBool::new(false)) };
    reader.register(run, employee, 1000).unwrap();
    let b2_fence = monitor.cancellation(run).unwrap();
    assert!(monitor.check_run(run).is_ok());
    reader.cancel(run).unwrap();
    assert!(b2_fence.load(Ordering::Acquire));
    assert!(monitor.check_run(run).is_err());
    assert!(reader.register(RunHandle(RunId([3; 16])), employee, 1000).is_err());
    monitor.set_run(employee, None, None, vec![]).unwrap();
    reader.register(RunHandle(RunId([3; 16])), employee, 1000).unwrap();
    assert!(b2_fence.load(Ordering::Acquire));
}
/// C2 fake of `Core::retry_reconcile`: the updater is asked first (an absent
/// one answers like `MissingPublication::anchors`, generation unavailable);
/// only then do the reconciliation's side effects run.
struct FakeStartup { updater_answers: bool, attempts: u32, side_effects: u32 }
impl FakeStartup {
    fn new() -> Self { Self { updater_answers: false, attempts: 0, side_effects: 0 } }
    fn attempt(&mut self) -> bool {
        self.attempts += 1;
        let updater = if self.updater_answers { Ok(()) } else { MissingPublication.anchors().map(|_| ()) };
        if updater.is_err() { return false; }
        self.side_effects += 1;
        true
    }
}
#[test]
fn startup_reconciliation_retries_until_the_updater_answers_then_latches() {
    let t0 = Instant::now();
    let mut fake = FakeStartup::new();
    // `LinuxHost::open`: the first attempt fails, the generation is unavailable.
    let mut startup = StartupReconcile::opened(t0, fake.attempt());
    assert!(!startup.latched);
    assert_eq!((fake.attempts, fake.side_effects), (1, 0), "no side effect before the updater answers");
    assert!(!startup.retry(t0 + STARTUP_RETRY, false, || fake.attempt()));
    assert_eq!((fake.attempts, fake.side_effects), (2, 0));
    fake.updater_answers = true;
    assert!(startup.retry(t0 + STARTUP_RETRY * 2, false, || fake.attempt()), "this call latched success");
    assert!(startup.latched);
    assert_eq!((fake.attempts, fake.side_effects), (3, 1));
    // Latched: never run again, however much time passes.
    for seconds in [3, 10, 3600] {
        assert!(!startup.retry(t0 + Duration::from_secs(seconds), false, || fake.attempt()));
    }
    assert_eq!((fake.attempts, fake.side_effects), (3, 1));
    assert!(startup.latched);
    // A success at start-up is never repeated either.
    let mut opened = StartupReconcile::opened(t0, true);
    assert!(!opened.retry(t0 + Duration::from_secs(60), false, || fake.attempt()));
    assert_eq!(fake.attempts, 3);
}
#[test]
fn poisoned_core_never_retries_startup_reconciliation() {
    let t0 = Instant::now();
    let mut fake = FakeStartup::new();
    fake.updater_answers = true;
    let mut startup = StartupReconcile::opened(t0, false);
    for seconds in [2, 4, 60, 3600] {
        assert!(!startup.due(t0 + Duration::from_secs(seconds), true));
        assert!(!startup.retry(t0 + Duration::from_secs(seconds), true, || fake.attempt()));
    }
    assert_eq!(fake.attempts, 0, "a poisoned core does not recover by retry");
    assert!(!startup.latched);
}
#[test]
fn startup_retry_cadence_is_bounded() {
    let t0 = Instant::now();
    let mut fake = FakeStartup::new();
    let mut startup = StartupReconcile::opened(t0, false);
    // A refresh every 250 ms for 10 s: one attempt per `STARTUP_RETRY`.
    for tick in 1..=40u32 {
        startup.retry(t0 + Duration::from_millis(250) * tick, false, || fake.attempt());
    }
    assert_eq!(STARTUP_RETRY, Duration::from_secs(2));
    assert_eq!(fake.attempts, 5, "attempts at 2, 4, 6, 8 and 10 s");
    assert!(!startup.due(t0 + Duration::from_millis(11_999), false));
    assert!(startup.due(t0 + Duration::from_secs(12), false));
    // A clock that reads earlier than the last attempt never makes one due.
    assert!(!StartupReconcile::opened(t0 + Duration::from_secs(5), false).due(t0, false));
}
