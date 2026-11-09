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
