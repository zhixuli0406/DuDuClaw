use super::*;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use duduclaw_isolation::{bounded::List, ipc::{IoDelivery, QueueClass}};

#[derive(Default)]
struct Counters {
    scans: AtomicUsize,
    cancels: AtomicUsize,
    registered: AtomicUsize,
    cleanup: AtomicUsize,
    io: AtomicUsize,
    forgotten: AtomicUsize,
}
struct FakeScan(Arc<Counters>);
impl Scan for FakeScan {
    fn scan(&mut self) -> Result<Vec<EmployeeId>> {
        self.0.scans.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    fn health(&mut self, _: EmployeeId) -> Result<()> { Ok(()) }
    fn register(&mut self, _: RunHandle, _: EmployeeId, _: u64) -> Result<()> {
        self.0.registered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn cancel(&mut self, _: RunHandle) -> Result<()> {
        self.0.cancels.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Barrier { entered: SyncSender<()>, release: Receiver<()> }
impl Barrier {
    fn block(self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
fn barrier() -> (Barrier, Receiver<()>, SyncSender<()>) {
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    (Barrier { entered, release: wait }, observed, release)
}
struct FakeHost {
    counters: Arc<Counters>, start_barrier: Option<Barrier>, poll_barrier: Option<Barrier>,
    start_error: Option<ErrorCode>, io_taken: bool,
}
impl FakeHost {
    fn new(counters: Arc<Counters>) -> Self {
        Self { counters, start_barrier: None, poll_barrier: None, start_error: None, io_taken: false }
    }
}
impl Host for FakeHost {
    type Inputs = Vec<OwnedFd>; type Io = Vec<OwnedFd>;
    fn ready(&mut self, _: EmployeeId) -> Result<()> { Ok(()) }
    fn policy(&mut self, _: EmployeeId, _: &RuntimeKind, _: bool) -> Result<LaunchPolicy> { Ok(policy()) }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> Result<CleanupReadback> {
        self.counters.cleanup.fetch_add(1, Ordering::SeqCst);
        Ok(proof())
    }
    fn start(&mut self, _: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: Self::Inputs) -> Result<()> {
        if let Some(barrier) = self.start_barrier.take() { barrier.block(); }
        match self.start_error { Some(error) => Err(error), None => Ok(()) }
    }
    fn poll(&mut self, _: RunHandle) -> Result<HostProgress> {
        if let Some(barrier) = self.poll_barrier.take() { barrier.block(); }
        Ok(HostProgress::Running { last_activity_ms: 100 })
    }
    fn revoke(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
    fn kill(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
    fn take_io(&mut self, _: RunHandle) -> Result<Self::Io> {
        self.counters.io.fetch_add(1, Ordering::SeqCst);
        if self.io_taken { return Err(ErrorCode::AlreadyTaken); }
        self.io_taken = true;
        Ok(vec![std::fs::File::open("/dev/null").unwrap().into()])
    }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> Result<()> { Ok(()) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { panic!("worker must use independent Scan") }
    fn forget(&mut self, _: RunHandle) -> Result<()> {
        self.counters.forgotten.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn run(byte: u8) -> RunHandle { RunHandle(RunId([byte; 16])) }
fn employee() -> EmployeeId { EmployeeId([3; 16]) }
fn policy() -> LaunchPolicy {
    LaunchPolicy { queue_class: QueueClass::Interactive, queue_ms: 1000, startup_ms: 1000,
        runtime_ms: 5000, pty_idle_ms: None, ptys: 0, pty_size: None, delivery: IoDelivery { mcp: false, pty: false } }
}
fn request() -> SpawnRequest {
    SpawnRequest::StartRun { run: run(1), employee_id: employee(), runtime_request: RuntimeKind::Codex,
        task_ref: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }
}
fn proof() -> CleanupReadback {
    CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true }
}
fn host(fake: FakeHost, counters: Arc<Counters>) -> WorkerHost {
    WorkerHost::spawn(fake, Box::new(FakeScan(counters))).unwrap()
}
fn until<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match operation() {
            Err(ErrorCode::NotReady) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            result => return result,
        }
    }
}

#[test]
fn blocked_start_does_not_block_control_or_immediate_scan_cancellation() {
    let counters = Arc::new(Counters::default());
    let (block, entered, release) = barrier();
    let mut fake = FakeHost::new(counters.clone());
    fake.start_barrier = Some(block);
    let mut worker = host(fake, counters.clone());
    worker.start(run(1), employee(), &request(), &policy(), vec![]).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let (finished, done) = mpsc::sync_channel(1);
    let controller = std::thread::spawn(move || {
        assert!(matches!(worker.poll(run(1)), Ok(HostProgress::Starting)));
        assert_eq!(worker.revoke(run(1)), Err(ErrorCode::NotReady));
        assert_eq!(worker.kill(run(1)), Err(ErrorCode::NotReady));
        assert!(worker.detect_intruders().unwrap().is_empty());
        assert!(finished.send(worker).is_ok());
    });
    let independent = done.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    controller.join().unwrap();
    let _worker = independent.expect("control waited for the blocking lifecycle worker");
    assert_eq!(counters.registered.load(Ordering::SeqCst), 1);
    assert_eq!(counters.cancels.load(Ordering::SeqCst), 2);
    assert_eq!(counters.scans.load(Ordering::SeqCst), 1);
}

#[test]
fn startup_failure_is_reported_by_poll_without_running_result() {
    let counters = Arc::new(Counters::default());
    let mut fake = FakeHost::new(counters.clone());
    fake.start_error = Some(ErrorCode::IsolationUnavailable);
    let mut worker = host(fake, counters);
    worker.start(run(1), employee(), &request(), &policy(), vec![]).unwrap();
    let result = until(|| match worker.poll(run(1)) {
        Ok(HostProgress::Starting) => Err(ErrorCode::NotReady),
        other => other,
    });
    assert!(matches!(result, Err(ErrorCode::IsolationUnavailable)));
}

#[test]
fn pending_poll_is_not_ready_and_does_not_revert_a_running_run_to_starting() {
    let counters = Arc::new(Counters::default());
    let (block, entered, release) = barrier();
    let mut fake = FakeHost::new(counters.clone());
    fake.poll_barrier = Some(block);
    let mut worker = host(fake, counters);
    assert!(matches!(worker.poll(run(1)), Err(ErrorCode::NotReady)));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(worker.poll(run(1)), Err(ErrorCode::NotReady)));
    release.send(()).unwrap();
    assert!(matches!(until(|| worker.poll(run(1))), Ok(HostProgress::Running { last_activity_ms: 100 })));
}

#[test]
fn cleanup_discards_stale_proof_and_requires_a_new_host_readback() {
    let counters = Arc::new(Counters::default());
    let mut worker = host(FakeHost::new(counters.clone()), counters.clone());
    let key = Key::Cleanup(employee(), Some(run(1).0));
    let stale = Instant::now() - Duration::from_secs(3);
    worker.slots.insert(key.clone(), Slot { sent: stale, result: Some((Ok(Value::Cleanup(proof())), stale)) });
    assert!(matches!(worker.cleanup(employee(), Some(run(1))), Err(ErrorCode::NotReady)));
    assert!(!worker.slots.contains_key(&key));
    assert_eq!(counters.cleanup.load(Ordering::SeqCst), 0);
    assert!(until(|| worker.cleanup(employee(), Some(run(1)))).unwrap().complete());
    assert_eq!(counters.cleanup.load(Ordering::SeqCst), 1);
}

#[test]
fn one_shot_io_is_not_cloned_and_forget_removes_only_its_run_cache() {
    let counters = Arc::new(Counters::default());
    let mut worker = host(FakeHost::new(counters.clone()), counters.clone());
    let descriptors = until(|| worker.take_io(run(1))).unwrap();
    assert_eq!(descriptors.len(), 1);
    assert!(matches!(until(|| worker.take_io(run(1))), Err(ErrorCode::AlreadyTaken)));
    assert_eq!(counters.io.load(Ordering::SeqCst), 2);
    for key in [Key::Kill(run(1).0), Key::Revoke(run(1).0), Key::Kill(run(2).0)] {
        worker.slots.insert(key, Slot { sent: Instant::now(), result: Some((Ok(Value::Unit), Instant::now())) });
    }
    until(|| worker.forget(run(1))).unwrap();
    assert!(!worker.slots.contains_key(&Key::Kill(run(1).0)));
    assert!(!worker.slots.contains_key(&Key::Revoke(run(1).0)));
    assert!(!worker.slots.contains_key(&Key::Io(run(1).0)));
    assert!(worker.slots.contains_key(&Key::Kill(run(2).0)));
    assert_eq!(counters.forgotten.load(Ordering::SeqCst), 1);
}
