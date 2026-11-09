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
    start_error: Option<ErrorCode>, io_taken: bool, copy_barrier: Option<Barrier>,
}
impl FakeHost {
    fn new(counters: Arc<Counters>) -> Self {
        Self { counters, start_barrier: None, poll_barrier: None, start_error: None, io_taken: false, copy_barrier: None }
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
    fn broker(&mut self, job: BrokerJob) -> Result<BrokerStep> {
        match job {
            BrokerJob::OpenWorkFile { .. } => {
                let barrier = self.copy_barrier.take();
                Ok(BrokerStep::Deferred(Box::new(move || {
                    if let Some(barrier) = barrier { barrier.block(); }
                    Ok(BrokerReply::WorkFile { metadata: duduclaw_isolation::ipc::WorkFileMetadata { size: 0, inode: 1, device: 1 },
                        file: std::fs::File::open("/dev/null").unwrap(), source: None })
                })))
            }
            BrokerJob::WorkAnchor { .. } => Ok(BrokerStep::Done(BrokerReply::WorkAnchor(
                duduclaw_isolation::ipc::WorkAnchorIdentity { device: 1, inode: 2 }))),
            BrokerJob::EmployeeIdle { .. } => Err(ErrorCode::NotFound),
        }
    }
}

fn poll_broker(worker: &mut WorkerHost, ticket: Ticket) -> Result<BrokerReply> {
    until(|| match worker.broker_poll(ticket) { Ok(None) => Err(ErrorCode::NotReady), Ok(Some(reply)) => Ok(reply), Err(e) => Err(e) })
}

#[test]
fn h4_snapshot_copy_never_blocks_lifecycle_worker() {
    let counters = Arc::new(Counters::default());
    let (barrier, entered, release) = barrier();
    let mut fake = FakeHost::new(counters.clone());
    fake.copy_barrier = Some(barrier);
    let mut worker = host(fake, counters);
    let path = duduclaw_isolation::ipc::WorkPath::new("a.txt".into()).unwrap();
    worker.broker_submit(Ticket(1), BrokerJob::OpenWorkFile { run: run(1), employee: employee(), path: path.clone() }).unwrap();
    assert_eq!(worker.broker_submit(Ticket(1), BrokerJob::WorkAnchor { employee: employee() }), Err(ErrorCode::Internal),
        "a ticket is never reused");
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    // The copy is blocked; the lifecycle worker still answers other jobs.
    worker.broker_submit(Ticket(2), BrokerJob::WorkAnchor { employee: employee() }).unwrap();
    assert!(matches!(poll_broker(&mut worker, Ticket(2)).unwrap(), BrokerReply::WorkAnchor(_)));
    assert_eq!(until(|| worker.ready(employee())), Ok(()));
    worker.broker_submit(Ticket(3), BrokerJob::EmployeeIdle { employee: employee() }).unwrap();
    assert_eq!(poll_broker(&mut worker, Ticket(3)).unwrap_err(), ErrorCode::NotFound);
    assert!(worker.broker_poll(Ticket(1)).unwrap().is_none());
    release.send(()).unwrap();
    assert!(matches!(poll_broker(&mut worker, Ticket(1)).unwrap(), BrokerReply::WorkFile { .. }));
    // An abandoned ticket's late result is dropped, never re-delivered.
    worker.broker_submit(Ticket(4), BrokerJob::OpenWorkFile { run: run(1), employee: employee(), path }).unwrap();
    worker.broker_abandon(Ticket(4));
    std::thread::sleep(Duration::from_millis(100));
    worker.receive();
    assert!(!worker.slots.contains_key(&Key::Broker(4)));
    assert_eq!(worker.broker_poll(Ticket(4)).unwrap_err(), ErrorCode::Internal);
}
fn run(byte: u8) -> RunHandle { RunHandle(RunId([byte; 16])) }
fn employee() -> EmployeeId { EmployeeId([3; 16]) }
fn policy() -> LaunchPolicy {
    LaunchPolicy { queue_class: QueueClass::Interactive, queue_ms: 1000, startup_ms: 1000,
        runtime_ms: 5000, pty_idle_ms: None, ptys: 0, pty_size: None, delivery: IoDelivery { mcp: false, pty: false } }
}
fn request() -> SpawnRequest {
    SpawnRequest::StartRun { run: run(1), employee_id: employee(), runtime_request: RuntimeKind::Codex,
        task_ref: None, workspace: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }
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

/// H4 end to end below the socket: coordinator authority → worker mailbox →
/// lifecycle-thread step → copy worker → H2 sealed snapshot of a real
/// fixture tree. Stands in for `Core::broker_open` with the same pieces
/// (`BrokerRoot` lease, `copy_snapshot`, the run's fence as `live`); the
/// root-only lease/mount half is `tests/work_broker_chain.rs`.
#[cfg(target_os = "linux")]
mod h4_fixture {
    use super::*;
    use std::{io::Read, os::unix::fs::MetadataExt, sync::{Mutex, atomic::AtomicBool}};
    use duduclaw_isolation::ipc::{ControlConnectionId, RunStatus, SpawnResponse, WorkPath};
    use crate::{admission::{Coordinator, Limits}, work_broker::{check_work_path, copy_snapshot, BrokerRoot, WorkExpectation}};

    struct FixtureHost { inner: FakeHost, root: Arc<Mutex<BrokerRoot>>, expected: WorkExpectation, fence: Arc<AtomicBool> }
    impl Host for FixtureHost {
        type Inputs = Vec<OwnedFd>; type Io = Vec<OwnedFd>;
        fn ready(&mut self, e: EmployeeId) -> Result<()> { self.inner.ready(e) }
        fn policy(&mut self, e: EmployeeId, r: &RuntimeKind, p: bool) -> Result<LaunchPolicy> { self.inner.policy(e, r, p) }
        fn cleanup(&mut self, e: EmployeeId, r: Option<RunHandle>) -> Result<CleanupReadback> { self.inner.cleanup(e, r) }
        fn start(&mut self, r: RunHandle, e: EmployeeId, q: &SpawnRequest, p: &LaunchPolicy, i: Self::Inputs) -> Result<()> { self.inner.start(r, e, q, p, i) }
        fn poll(&mut self, r: RunHandle) -> Result<HostProgress> { self.inner.poll(r) }
        fn revoke(&mut self, r: RunHandle) -> Result<()> { self.inner.revoke(r) }
        fn kill(&mut self, r: RunHandle) -> Result<()> { self.inner.kill(r) }
        fn take_io(&mut self, r: RunHandle) -> Result<Self::Io> { self.inner.take_io(r) }
        fn resize(&mut self, r: RunHandle, s: PtySize) -> Result<()> { self.inner.resize(r, s) }
        fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { self.inner.detect_intruders() }
        fn forget(&mut self, r: RunHandle) -> Result<()> { self.inner.forget(r) }
        fn broker(&mut self, job: BrokerJob) -> Result<BrokerStep> {
            let BrokerJob::OpenWorkFile { path, .. } = job else { return Err(ErrorCode::WorkspaceBrokerUnavailable) };
            check_work_path(path.as_str()).map_err(|refusal| refusal.code())?;
            let lease = self.root.lock().map_err(|_| ErrorCode::Internal)?.lease().map_err(|refusal| refusal.code())?;
            let (expected, fence) = (self.expected, self.fence.clone());
            Ok(BrokerStep::Deferred(Box::new(move || {
                let live = || !fence.load(Ordering::Acquire);
                let snapshot = copy_snapshot(lease, &path, &expected, &live).map_err(|refusal| refusal.code())?;
                Ok(BrokerReply::WorkFile { metadata: snapshot.metadata, file: snapshot.file, source: None })
            })))
        }
    }

    fn answer(c: &mut Coordinator<WorkerHost>, ticket: Ticket, now: u64) -> crate::admission::BrokerOutcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = c.poll_ticket(ticket, now).unwrap() { return outcome; }
            assert!(Instant::now() < deadline, "no broker answer");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn h4_open_work_file_end_to_end_with_fixture() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes/report.csv"), b"a,b\n1,2\n").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("notes/escape")).unwrap();
        let meta = std::fs::metadata(dir.path()).unwrap();
        let expected = WorkExpectation { uid: meta.uid(), device_major: libc::major(meta.dev()), device_minor: libc::minor(meta.dev()) };
        let root = Arc::new(Mutex::new(BrokerRoot::new(Some(std::fs::File::open(dir.path()).unwrap().into()))));
        let fence = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let fixture = FixtureHost { inner: FakeHost::new(counters.clone()), root: root.clone(), expected, fence: fence.clone() };
        let mut c = Coordinator::new(host_any(fixture, counters), Limits::default()).unwrap();
        let connection = ControlConnectionId([1; 16]);
        let now = 1_000;
        until(|| c.reserve(connection, employee(), run(1), now)).unwrap();
        until(|| c.start(connection, request(), vec![], now)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.status(run(1), connection) != Ok(RunStatus::Running) {
            assert!(Instant::now() < deadline, "fixture run never reached Running");
            let _ = c.tick(now);
            std::thread::sleep(Duration::from_millis(1));
        }
        let path = |p: &str| WorkPath::new(p.into()).unwrap();
        // Happy path: one sealed, read-only snapshot fd with honest metadata.
        let ticket = c.open_work_file(connection, run(1), employee(), path("notes/report.csv"), now).unwrap();
        let outcome = answer(&mut c, ticket, now);
        let response = outcome.response;
        let SpawnResponse::WorkFile { run: delivered, file: metadata } = response else { panic!("not a WorkFile reply") };
        assert_eq!(delivered, run(1));
        let mut file = outcome.file.expect("exactly one WorkFileRead fd");
        let actual = file.metadata().unwrap();
        assert_eq!((metadata.size, metadata.inode, metadata.device), (actual.len(), actual.ino(), actual.dev()));
        assert_ne!(actual.ino(), std::fs::metadata(dir.path().join("notes/report.csv")).unwrap().ino(), "a copy, not the employee inode");
        let seals = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&file), libc::F_GET_SEALS) };
        let required = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        assert_eq!(seals & required, required);
        // The employee rewrites the file afterwards; the snapshot is pinned.
        std::fs::write(dir.path().join("notes/report.csv"), b"changed later").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"a,b\n1,2\n");
        // Refusals keep their H1 wire codes and never carry an fd.
        for (relative, code) in [("notes/missing.csv", ErrorCode::WorkFileNotFound), ("notes/escape", ErrorCode::WorkFileRefused),
            ("notes", ErrorCode::WorkFileRefused)] {
            let ticket = c.open_work_file(connection, run(1), employee(), path(relative), now).unwrap();
            let outcome = answer(&mut c, ticket, now);
            assert_eq!(outcome.response, SpawnResponse::Error { code }, "{relative}");
            assert!(outcome.file.is_none());
        }
        assert_eq!(root.lock().unwrap().in_flight(), 0, "every copy gave its hold back");
        // The run's fence goes up (revoke/kill/intrusion): nothing more is delivered.
        fence.store(true, Ordering::Release);
        let ticket = c.open_work_file(connection, run(1), employee(), path("notes/report.csv"), now).unwrap();
        assert_eq!(answer(&mut c, ticket, now).response, SpawnResponse::Error { code: ErrorCode::NotReady });
        fence.store(false, Ordering::Release);
        // Cleanup released the clone: the broker fails closed for this run.
        root.lock().unwrap().release().unwrap();
        let ticket = c.open_work_file(connection, run(1), employee(), path("notes/report.csv"), now).unwrap();
        assert_eq!(answer(&mut c, ticket, now).response, SpawnResponse::Error { code: ErrorCode::WorkspaceBrokerUnavailable });
    }

    fn host_any<H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>> + Send + 'static>(fake: H, counters: Arc<Counters>) -> WorkerHost {
        WorkerHost::spawn(fake, Box::new(FakeScan(counters))).unwrap()
    }
}
