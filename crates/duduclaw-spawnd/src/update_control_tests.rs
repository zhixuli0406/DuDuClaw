use super::*;
use std::{io::IoSlice, sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc, Mutex}};
use duduclaw_isolation::{bounded::List, execution::{RunHandle, RunOutcome, RuntimeKind, PtySize},
    ids::{EmployeeId, RequestId, RunId},
    ipc::{CleanupReadback, ControlConnectionId, ErrorCode, IoDelivery, QueueClass, SpawnRequest, MAX_FRAME_BYTES},
    update::{Architecture, OperatingSystem, ProtocolCompatibility, ReleaseChannel}};
use duduclaw_updated::activation_contract::validate_response;
use nix::sys::socket::{AddressFamily, ControlMessage, MsgFlags, SockType};
use crate::admission::{Coordinator, Host, HostProgress, LaunchPolicy, Limits};

// ---------------------------------------------------------------- fixtures

#[derive(Debug, PartialEq)]
enum Event { Persisted { already_closed: bool }, Cleared { already_open: bool } }
#[derive(Default)]
struct Disk { bytes: Option<Vec<u8>>, fail: bool, events: Vec<Event>, watch: Option<DrainBarrier> }
/// In-memory "disk" that records, at each durable step, what the barrier
/// already said: the protocol requires the record before the closure and the
/// removal before the reopening.
#[derive(Clone, Default)]
struct MemoryStore(Arc<Mutex<Disk>>);
impl MemoryStore {
    fn watching(barrier: &DrainBarrier) -> Self {
        let store = Self::default();
        store.0.lock().unwrap().watch = Some(barrier.clone());
        store
    }
}
impl DrainStore for MemoryStore {
    fn load(&mut self) -> io::Result<Option<Sha256Digest>> {
        self.0.lock().unwrap().bytes.as_deref().map(parse_record).transpose()
    }
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.fail { return Err(io::Error::other("injected write failure")); }
        let already_closed = disk.watch.as_ref().is_some_and(|b| b.closure() == Some(DrainClosure::Generation(generation)));
        disk.events.push(Event::Persisted { already_closed });
        disk.bytes = Some(encode_record(generation)?);
        Ok(())
    }
    fn clear(&mut self) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.fail { return Err(io::Error::other("injected unlink failure")); }
        let already_open = disk.watch.as_ref().is_some_and(|b| !b.is_closed());
        disk.events.push(Event::Cleared { already_open });
        disk.bytes = None;
        Ok(())
    }
}

#[derive(Default)]
struct FakeEffects { calls: Vec<&'static str>, fail: bool }
impl Effects for FakeEffects {
    fn prepare(&mut self, _: &Preparation) -> io::Result<()> {
        self.calls.push("prepare");
        if self.fail { Err(invalid()) } else { Ok(()) }
    }
    fn apply(&mut self, _: &Preparation) -> io::Result<()> {
        self.calls.push("apply");
        if self.fail { Err(invalid()) } else { Ok(()) }
    }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> {
        self.calls.push("readback");
        Ok(vec![])
    }
}
/// No generation effects (drain-only root tests).
struct NoEffects;
impl Effects for NoEffects {
    fn prepare(&mut self, _: &Preparation) -> io::Result<()> { Err(invalid()) }
    fn apply(&mut self, _: &Preparation) -> io::Result<()> { Err(invalid()) }
    fn readback(&mut self, _: Sha256Digest, _: bool) -> io::Result<Vec<ComponentReadback>> { Err(invalid()) }
}

/// Stands in for the admission loop: one iteration every 5 ms with a fixed run count.
struct Observer { stop: Arc<AtomicBool>, runs: Arc<AtomicUsize>, worker: Option<std::thread::JoinHandle<()>> }
impl Observer {
    fn new(barrier: &DrainBarrier, runs: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(runs));
        let (barrier, flag, admitted) = (barrier.clone(), stop.clone(), count.clone());
        let worker = std::thread::spawn(move || while !flag.load(Ordering::Acquire) {
            let epoch = barrier.epoch();
            barrier.observe(epoch, admitted.load(Ordering::Acquire), true);
            std::thread::sleep(Duration::from_millis(5));
        });
        Self { stop, runs: count, worker: Some(worker) }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

fn digest(n: u8) -> Sha256Digest { Sha256Digest([n; 32]) }
fn request(command: ControlCommand) -> ControlRequest { ControlRequest { schema_version: 1, nonce: RequestId([3; 16]), command } }
fn preparation(generation: Sha256Digest) -> Preparation { Preparation { generation, migrations: vec![], runtime_entries: vec![] } }
fn outcome<S: DrainStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) -> ControlOutcome {
    let request = request(command);
    let response = control.respond(&request);
    assert_eq!(response.nonce, request.nonce);
    assert_eq!(response.generation, request.command.generation());
    response.outcome
}
fn accepted<S: DrainStore, E: Effects>(control: &mut UpdateControl<S, E>, command: ControlCommand) {
    let request = request(command);
    let response = control.respond(&request);
    validate_response(&request, &response).unwrap_or_else(|_| panic!("refused: {:?}", response.outcome));
}

// ---------------------------------------------------------------- drain record

#[test]
fn update_drain_record_parse_is_strict() {
    let good = encode_record(digest(7)).unwrap();
    assert_eq!(parse_record(&good).unwrap(), digest(7));
    let text = String::from_utf8(good.clone()).unwrap();
    let mut spaced = good.clone(); spaced.push(b'\n');
    for bad in [
        Vec::new(), spaced, vec![b'{'; MAX_RECORD_BYTES + 1],
        text.replace("\"version\":1", "\"version\":2").into_bytes(),
        text.replace('{', "{\"extra\":1,").into_bytes(),
        text.replace('{', "{\"version\":1,").into_bytes(),
        encode_record(digest(0)).unwrap(),
        text.replace(":[", ": [").into_bytes(),
        b"null".to_vec(),
    ] {
        assert!(parse_record(&bad).is_err(), "{}", String::from_utf8_lossy(&bad));
    }
}

#[test]
fn update_drain_persists_record_before_closing_and_replying() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::watching(&barrier);
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), None);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    // Exactly one durable write, and it happened while the barrier was still open.
    assert_eq!(store.0.lock().unwrap().events, vec![Event::Persisted { already_closed: false }]);
    assert_eq!(store.0.lock().unwrap().bytes.as_deref().map(parse_record).transpose().unwrap(), Some(digest(7)));
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // Resume removes the record before admission reopens.
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.0.lock().unwrap().events.last(), Some(&Event::Cleared { already_open: false }));
    assert_eq!(barrier.closure(), None);
    assert!(store.0.lock().unwrap().bytes.is_none());
}

#[test]
fn update_drain_write_failure_refuses_and_announces_nothing() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::watching(&barrier);
    store.0.lock().unwrap().fail = true;
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert!(matches!(outcome(&mut control, ControlCommand::Drain { generation: digest(7) }), ControlOutcome::Refused));
    assert_eq!(barrier.closure(), None);
    // A Resume whose unlink fails keeps the barrier closed.
    store.0.lock().unwrap().fail = false;
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    store.0.lock().unwrap().fail = true;
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
}

#[test]
fn update_drain_without_admission_loop_times_out_refused() {
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(MemoryStore::default(), barrier.clone(), FakeEffects::default()).unwrap();
    // No admission iteration can confirm the count: never report a drain.
    assert!(matches!(outcome(&mut control, ControlCommand::Drain { generation: digest(7) }), ControlOutcome::Refused));
    // The durable record still closed admission (fail closed), keyed to the request.
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
}

#[test]
fn update_barrier_survives_restart_and_only_matching_resume_lifts_it() {
    let store = MemoryStore::default();
    {
        let barrier = DrainBarrier::default();
        let _admission = Observer::new(&barrier, 0);
        let mut control = UpdateControl::new(store.clone(), barrier, FakeEffects::default()).unwrap();
        accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    }
    // "Restart": a new process reads the record before any admission.
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(8) }), ControlOutcome::Refused));
    assert!(barrier.is_closed());
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert!(!barrier.is_closed());
    // Idempotent: a retried Resume after the record is gone succeeds.
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });

    // An unreadable record closes admission with no generation; no Resume lifts
    // it, a new Drain rewrites the record and re-keys it.
    store.0.lock().unwrap().bytes = Some(b"{\"version\":1,\"generation\":".to_vec());
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    assert_eq!(barrier.closure(), Some(DrainClosure::Unreadable));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    assert!(barrier.is_closed());
    accepted(&mut control, ControlCommand::Drain { generation: digest(9) });
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(9))));
    accepted(&mut control, ControlCommand::Resume { generation: digest(9) });
    assert!(!barrier.is_closed());
}

#[test]
fn update_drain_rekeys_to_the_latest_generation() {
    // A rolled-back activation drains the committed generation over a pending one.
    let barrier = DrainBarrier::default();
    let store = MemoryStore::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: digest(2) });
    accepted(&mut control, ControlCommand::Drain { generation: digest(1) });
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(2) }), ControlOutcome::Refused));
    accepted(&mut control, ControlCommand::Resume { generation: digest(1) });
}

#[test]
fn update_apply_requires_closed_recorded_generation_and_no_runs() {
    let barrier = DrainBarrier::default();
    let store = MemoryStore::default();
    let admission = Observer::new(&barrier, 1);
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), FakeEffects::default()).unwrap();
    let apply = |generation| ControlCommand::Apply { preparation: preparation(generation) };
    // Not drained at all.
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    // Drained with an admitted run: the client sees a valid barrier but keeps retrying.
    match outcome(&mut control, ControlCommand::Drain { generation: digest(7) }) {
        ControlOutcome::Drained { admission_closed: true, active_runs: 1 } => {}
        other => panic!("{other:?}"),
    }
    let request_with_runs = request(ControlCommand::Drain { generation: digest(7) });
    assert!(validate_response(&request_with_runs, &control.respond(&request_with_runs)).is_err());
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    admission.runs.store(0, Ordering::Release);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    // Another generation's barrier never authorizes publication.
    assert!(matches!(outcome(&mut control, apply(digest(8))), ControlOutcome::Refused));
    // The durable record must agree with memory.
    let saved = store.0.lock().unwrap().bytes.take();
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
    store.0.lock().unwrap().bytes = saved;
    assert!(control.effects.calls.is_empty());
    accepted(&mut control, apply(digest(7)));
    assert_eq!(control.effects.calls, vec!["apply"]);
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert!(matches!(outcome(&mut control, apply(digest(7))), ControlOutcome::Refused));
}

#[test]
fn update_schema_zero_generation_and_effect_errors_are_refused() {
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(MemoryStore::default(), barrier.clone(), FakeEffects::default()).unwrap();
    let mut wrong = request(ControlCommand::Prepare { preparation: preparation(digest(1)) });
    wrong.schema_version = 2;
    let response = control.respond(&wrong);
    assert!(matches!(response.outcome, ControlOutcome::Refused));
    assert_eq!(response.schema_version, 1);
    for command in [ControlCommand::Prepare { preparation: preparation(digest(0)) },
        ControlCommand::Drain { generation: digest(0) }, ControlCommand::Readback { generation: digest(0) },
        ControlCommand::Resume { generation: digest(0) }] {
        assert!(matches!(outcome(&mut control, command), ControlOutcome::Refused));
    }
    assert!(control.effects.calls.is_empty());
    assert!(!barrier.is_closed());
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(digest(1)) });
    accepted(&mut control, ControlCommand::Readback { generation: digest(1) });
    control.effects.fail = true;
    assert!(matches!(outcome(&mut control, ControlCommand::Prepare { preparation: preparation(digest(1)) }), ControlOutcome::Refused));
}

// ---------------------------------------------------------------- admission

#[derive(Default)]
struct OneRunHost { end: bool }
impl Host for OneRunHost {
    type Inputs = ();
    type Io = ();
    fn ready(&mut self, _: EmployeeId) -> crate::Result<()> { Ok(()) }
    fn policy(&mut self, _: EmployeeId, _: &RuntimeKind, _: bool) -> crate::Result<LaunchPolicy> {
        Ok(LaunchPolicy { queue_class: QueueClass::Background, queue_ms: 60_000, startup_ms: 60_000, runtime_ms: 60_000,
            pty_idle_ms: None, ptys: 0, pty_size: None, delivery: IoDelivery { mcp: false, pty: false } })
    }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> crate::Result<CleanupReadback> {
        Ok(CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true })
    }
    fn start(&mut self, _: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: ()) -> crate::Result<()> { Ok(()) }
    fn poll(&mut self, _: RunHandle) -> crate::Result<HostProgress> {
        Ok(if self.end { HostProgress::Ended(RunOutcome::Exited(0)) } else { HostProgress::Running { last_activity_ms: 1 } })
    }
    fn revoke(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn kill(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn take_io(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> crate::Result<()> { Ok(()) }
    fn detect_intruders(&mut self) -> crate::Result<Vec<EmployeeId>> { Ok(vec![]) }
    fn forget(&mut self, _: RunHandle) -> crate::Result<()> { Ok(()) }
}
fn connection() -> ControlConnectionId { ControlConnectionId([1; 16]) }
fn start_request(n: u8) -> SpawnRequest {
    SpawnRequest::StartRun { run: RunHandle(RunId([n; 16])), employee_id: EmployeeId([n; 16]), runtime_request: RuntimeKind::Codex,
        task_ref: None, workspace: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false,
        requested_max_turns: None }
}
fn reserve(coordinator: &mut Coordinator<OneRunHost>, n: u8, now: u64) {
    coordinator.reserve(connection(), EmployeeId([n; 16]), RunHandle(RunId([n; 16])), now).unwrap();
}
/// Answer one request on its own thread (as in production) while this thread
/// is the admission loop: tick, then report the admitted runs of the iteration.
fn respond_while_admitting<S: DrainStore + Send + 'static, E: Effects + Send + 'static>(control: UpdateControl<S, E>,
    command: ControlCommand, coordinator: &mut Coordinator<OneRunHost>, barrier: &DrainBarrier, now: &mut u64)
    -> (UpdateControl<S, E>, ControlRequest, ControlResponse) {
    let request = request(command);
    let sent = request.clone();
    let worker = std::thread::spawn(move || { let mut control = control; let response = control.respond(&sent); (control, response) });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "update control did not answer");
        let epoch = barrier.epoch();
        *now += 1;
        let healthy = coordinator.tick(*now).is_ok();
        barrier.observe(epoch, coordinator.admitted_runs(), healthy);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (control, response) = worker.join().unwrap();
    (control, request, response)
}

/// Shared by the unit test (memory store) and the root test (disk store).
fn drain_waits_for_active_run<S: DrainStore + Clone + Send + 'static>(store: S) {
    let barrier = DrainBarrier::default();
    let mut coordinator = Coordinator::new(OneRunHost::default(), Limits::default()).unwrap();
    coordinator.set_drain_barrier(barrier.clone());
    let mut now = 100;
    reserve(&mut coordinator, 1, now);
    coordinator.start(connection(), start_request(1), (), now).unwrap();
    coordinator.tick(now).unwrap();
    reserve(&mut coordinator, 2, now);
    let control = UpdateControl::new(store.clone(), barrier.clone(), NoEffects).unwrap();
    let (control, request, response) = respond_while_admitting(control, ControlCommand::Drain { generation: digest(7) },
        &mut coordinator, &barrier, &mut now);
    // The barrier is closed and durable, but one admitted run remains: the
    // updater's client rejects this reply and retries (activation.rs:72-80).
    assert!(matches!(response.outcome, ControlOutcome::Drained { admission_closed: true, active_runs: 1 }));
    assert!(validate_response(&request, &response).is_err());
    assert_eq!(store.clone().load().unwrap(), Some(digest(7)));
    // New runs are refused; the admitted one keeps running.
    assert_eq!(coordinator.start(connection(), start_request(2), (), now), Err(ErrorCode::NotReady));
    assert_eq!(coordinator.admitted_runs(), 1);
    coordinator.host_mut_for_test().end = true;
    let (control, request, response) = respond_while_admitting(control, ControlCommand::Drain { generation: digest(7) },
        &mut coordinator, &barrier, &mut now);
    validate_response(&request, &response).unwrap();
    assert_eq!(coordinator.admitted_runs(), 0);
    drop(control);
    // Restart: the record closes a fresh admission loop before its first run.
    let barrier = DrainBarrier::default();
    let mut control = UpdateControl::new(store.clone(), barrier.clone(), NoEffects).unwrap();
    let mut coordinator = Coordinator::new(OneRunHost::default(), Limits::default()).unwrap();
    coordinator.set_drain_barrier(barrier.clone());
    reserve(&mut coordinator, 3, now);
    assert_eq!(coordinator.start(connection(), start_request(3), (), now), Err(ErrorCode::NotReady));
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    assert_eq!(store.clone().load().unwrap(), None);
    coordinator.start(connection(), start_request(3), (), now).unwrap();
}

#[test]
fn update_drain_waits_for_admitted_runs_and_refuses_new_starts() {
    drain_waits_for_active_run(MemoryStore::default());
}

// ---------------------------------------------------------------- endpoint frames

fn pair() -> (OwnedFd, OwnedFd) {
    let (client, server) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
    transport::timeout(&client).unwrap();
    (client, server)
}
fn me() -> (u32, u32) {
    // SAFETY: getuid/getgid take no arguments and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}
fn raw(fd: &OwnedFd, bytes: &[u8]) {
    socket::sendmsg::<UnixAddr>(fd.as_raw_fd(), &[IoSlice::new(bytes)], &[], MsgFlags::MSG_NOSIGNAL, None).unwrap();
}
fn endpoint() -> UpdateControl<MemoryStore, FakeEffects> {
    UpdateControl::new(MemoryStore::default(), DrainBarrier::default(), FakeEffects::default()).unwrap()
}
/// The server refused the connection: it returned an error and sent nothing.
fn refused(client: &OwnedFd, result: io::Result<()>) {
    assert!(result.is_err());
    assert!(transport::receive::<ControlResponse>(client).is_err());
}

#[test]
fn update_endpoint_answers_one_valid_frame() {
    let (client, server) = pair();
    let request = request(ControlCommand::Resume { generation: digest(4) });
    transport::send(&client, &request).unwrap();
    serve_one(server, me(), &mut endpoint()).unwrap();
    let response: ControlResponse = transport::receive(&client).unwrap();
    validate_response(&request, &response).unwrap();
}

#[test]
fn update_endpoint_refuses_other_peers() {
    for peer in [(me().0.wrapping_add(1), me().1), (me().0, me().1.wrapping_add(1))] {
        let (client, server) = pair();
        transport::send(&client, &request(ControlCommand::Resume { generation: digest(4) })).unwrap();
        refused(&client, serve_one(server, peer, &mut endpoint()));
    }
}

#[test]
fn update_endpoint_refuses_unknown_fields_and_methods() {
    let valid = serde_json::to_value(request(ControlCommand::Resume { generation: digest(4) })).unwrap();
    let mut top = valid.clone(); top["extra"] = 1.into();
    let mut nested = valid.clone(); nested["command"]["path"] = "/tmp/x".into();
    let mut method = valid.clone(); method["command"]["method"] = "reboot".into();
    let mut flagged = valid;
    flagged["command"] = serde_json::json!({"method": "drain", "generation": serde_json::to_value(digest(4)).unwrap(), "force": true});
    for value in [top, nested, method, flagged] {
        let (client, server) = pair();
        raw(&client, &duduclaw_isolation::ipc::frame_payload(&serde_json::to_vec(&value).unwrap()).unwrap());
        refused(&client, serve_one(server, me(), &mut endpoint()));
    }
}

#[test]
fn update_endpoint_refuses_ancillary_data() {
    let (client, server) = pair();
    let frame = duduclaw_isolation::ipc::frame_payload(&serde_json::to_vec(&request(ControlCommand::Resume { generation: digest(4) })).unwrap()).unwrap();
    let file = File::open("/dev/null").unwrap();
    let rights = [file.as_raw_fd()];
    socket::sendmsg::<UnixAddr>(client.as_raw_fd(), &[IoSlice::new(&frame)], &[ControlMessage::ScmRights(&rights)],
        MsgFlags::MSG_NOSIGNAL, None).unwrap();
    refused(&client, serve_one(server, me(), &mut endpoint()));
}

#[test]
fn update_endpoint_refuses_short_long_and_mismatched_frames() {
    let body = serde_json::to_vec(&request(ControlCommand::Resume { generation: digest(4) })).unwrap();
    let mut trailing = duduclaw_isolation::ipc::frame_payload(&body).unwrap(); trailing.push(b' ');
    let mut claims_more = ((body.len() + 10) as u32).to_be_bytes().to_vec(); claims_more.extend_from_slice(&body);
    let mut too_long = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(); too_long.extend_from_slice(&body);
    for frame in [vec![0u8; 3], 0u32.to_be_bytes().to_vec(), trailing, claims_more, too_long, body] {
        let (client, server) = pair();
        raw(&client, &frame);
        refused(&client, serve_one(server, me(), &mut endpoint()));
    }
}

#[test]
fn update_endpoint_times_out_a_silent_peer() {
    let (client, server) = pair();
    let started = Instant::now();
    refused(&client, serve_one(server, me(), &mut endpoint()));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(10), "{elapsed:?}");
}

// ---------------------------------------------------------------- readback and publication

fn manifest_with(artifacts: Vec<ReleaseArtifact>) -> ReleaseManifest {
    ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: ReleaseVersion { major: 1, minor: 83, patch: 0 },
        minimum_version: ReleaseVersion { major: 1, minor: 82, patch: 0 }, serial: 1, channel: ReleaseChannel::Stable,
        arch: if cfg!(target_arch = "aarch64") { Architecture::Aarch64 } else { Architecture::X86_64 },
        os: OperatingSystem::Linux, created_at_unix: 1, expires_at_unix: 4_000_000_000,
        protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1, mcp_bridge: 1,
            parse_worker: 1, importer: 1 }, artifacts, migrations: vec![] }
}
fn all_targets() -> Vec<InstallTarget> {
    vec![InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline, InstallTarget::Egress,
        InstallTarget::EgressForwarder, InstallTarget::McpBridge, InstallTarget::ParseWorker, InstallTarget::Importer,
        InstallTarget::Migration, InstallTarget::UpdateSlot]
}
fn fixed_manifest() -> ReleaseManifest {
    let mut targets = all_targets();
    targets.push(InstallTarget::RuntimeEntry { registry_id: "api-worker-v1".into() });
    manifest_with(targets.into_iter().enumerate().map(|(index, target)| ReleaseArtifact {
        id: artifact_name(&target).unwrap(), sha256: Sha256Digest([index as u8 + 1; 32]), size_bytes: 1, mode: 0o755,
        install_target: target }).collect())
}

#[test]
fn update_readback_reports_only_spawnd_side_components_and_never_guesses() {
    let manifest = fixed_manifest();
    let trampoline = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Trampoline).unwrap().sha256;
    let observed = |own_image, live, euid, trampoline| Observed { own_image, live, euid, trampoline };
    let reports = components(&manifest, observed(Some(digest(42)), true, 0, Some(trampoline)));
    assert_eq!(reports.len(), manifest.artifacts.len() - 3);
    assert!(!reports.iter().any(|r| matches!(r.target, InstallTarget::Gateway | InstallTarget::Egress | InstallTarget::UpdateSlot)));
    let find = |reports: &[ComponentReadback], target: InstallTarget| reports.iter().find(|r| r.target == target).unwrap().clone();
    let own = find(&reports, InstallTarget::Spawnd);
    assert!(own.ready);
    assert_eq!((own.build_sha256, own.effective_uid, own.protocol), (digest(42), 0, duduclaw_isolation::ipc::PROTOCOL_VERSION));
    assert_eq!(own.version, duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    assert_eq!(own.artifact_id, "duduclaw-spawnd");
    let run = find(&reports, InstallTarget::Trampoline);
    assert!(run.ready);
    assert_eq!((run.build_sha256, run.version, run.effective_uid), (trampoline, manifest.version, 0));
    for target in [InstallTarget::ParseWorker, InstallTarget::McpBridge, InstallTarget::Importer, InstallTarget::Migration,
        InstallTarget::EgressForwarder, InstallTarget::RuntimeEntry { registry_id: "api-worker-v1".into() }] {
        let report = find(&reports, target);
        assert!(!report.ready);
        assert_eq!((report.build_sha256, report.effective_uid), (Sha256Digest([0; 32]), u32::MAX));
    }
    // Each missing or wrong observation makes the record not ready.
    assert!(!find(&components(&manifest, observed(Some(digest(42)), false, 0, Some(trampoline))), InstallTarget::Spawnd).ready);
    let unread = find(&components(&manifest, observed(None, true, 0, Some(trampoline))), InstallTarget::Spawnd);
    assert!(!unread.ready && unread.build_sha256 == Sha256Digest([0; 32]));
    let unprivileged = components(&manifest, observed(Some(digest(42)), true, 1000, Some(trampoline)));
    assert!(!find(&unprivileged, InstallTarget::Spawnd).ready && !find(&unprivileged, InstallTarget::Trampoline).ready);
    let replaced = find(&components(&manifest, observed(Some(digest(42)), true, 0, Some(digest(9)))), InstallTarget::Trampoline);
    assert!(!replaced.ready);
    assert_eq!((replaced.build_sha256, replaced.version), (digest(9), ReleaseVersion { major: 0, minor: 0, patch: 0 }));
    assert!(!find(&components(&manifest, observed(Some(digest(42)), true, 0, None)), InstallTarget::Trampoline).ready);
}

#[test]
fn update_publication_matches_the_installed_layout() {
    let published: Vec<String> = publication().iter()
        .map(|(target, directory)| format!("/{directory}/{}", artifact_name(target).unwrap())).collect();
    assert_eq!(published, [
        "/usr/lib/duduclaw/duduclaw-spawnd", "/usr/lib/duduclaw/duduclaw-run", "/usr/libexec/duduclaw/duduclaw-parse-worker",
        "/usr/lib/duduclaw/current/duduclaw-import", "/usr/lib/duduclaw/current/duduclaw-migrate",
        "/usr/lib/duduclaw/current/duduclaw-mcp-bridge", "/opt/duduclaw/bin/duduclaw-egress", "/opt/duduclaw/bin/duduclaw"]);
}

#[test]
fn update_preparation_must_be_the_manifest_preparation_without_migrations() {
    let mut manifest = fixed_manifest();
    let exact = Preparation::for_manifest(&manifest, digest(5));
    bound(&exact, &manifest).unwrap();
    let mut missing_entry = exact.clone(); missing_entry.runtime_entries.clear();
    assert!(bound(&missing_entry, &manifest).is_err());
    manifest.migrations.push(duduclaw_isolation::update::DataMigration { id: "m1".into(), sha256: digest(6), reversible: false });
    assert!(bound(&Preparation::for_manifest(&manifest, digest(5)), &manifest).is_err());
    assert!(runtime_entries_shape(&exact).is_ok());
    let mut not_runtime = exact; not_runtime.runtime_entries[0].install_target = InstallTarget::Gateway;
    assert!(runtime_entries_shape(&not_runtime).is_err());
}

// ---------------------------------------------------------------- root (native Linux)

/// An isolated root-owned tree under /run: install root, publication root and
/// spawnd state, all satisfying `Directory::open`'s ancestor rules.
struct RootTree { _temp: tempfile::TempDir, install: PathBuf, publish: PathBuf, state: PathBuf }
impl RootTree {
    fn new() -> Self {
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
        let temp = tempfile::Builder::new().prefix("duduclaw-update-control-").tempdir_in("/run").unwrap();
        let mkdir = |path: &Path| {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let install = temp.path().join("install");
        let publish = temp.path().join("publish");
        let state = temp.path().join("state");
        mkdir(&install.join("generations"));
        for (_, directory) in publication() {
            let mut path = publish.clone();
            for part in directory.split('/') { path.push(part); mkdir(&path); }
        }
        mkdir(&state);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { _temp: temp, install, publish, state }
    }
    fn content(target: &InstallTarget, seed: u8) -> Vec<u8> {
        format!("#!/bin/false\n{} generation {seed}\n", artifact_name(target).unwrap()).into_bytes()
    }
    /// Stage a complete generation the way the updater leaves it.
    fn stage(&self, seed: u8) -> (Sha256Digest, ReleaseManifest) {
        let manifest = manifest_with(all_targets().into_iter().map(|target| {
            let bytes = Self::content(&target, seed);
            ReleaseArtifact { id: artifact_name(&target).unwrap(), sha256: Sha256Digest(Sha256::digest(&bytes).into()),
                size_bytes: bytes.len() as u64, mode: 0o755, install_target: target }
        }).collect());
        let mut manifest = manifest; manifest.serial = u64::from(seed);
        let bytes = duduclaw_updated::metadata::canonical_bytes(&manifest).unwrap();
        let generation = Sha256Digest(Sha256::digest(&bytes).into());
        let directory = self.install.join("generations").join(hex(&generation));
        std::fs::create_dir(&directory).unwrap();
        for artifact in &manifest.artifacts {
            let path = directory.join(artifact_name(&artifact.install_target).unwrap());
            std::fs::write(&path, Self::content(&artifact.install_target, seed)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = directory.join("manifest.json");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        (generation, manifest)
    }
    fn assert_published(&self, seed: u8) {
        for (target, directory) in publication() {
            let path = self.publish.join(directory).join(artifact_name(&target).unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), Self::content(&target, seed), "{}", path.display());
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.gid(), metadata.nlink()), (0o755, 0, 0, 1));
            assert!(std::fs::symlink_metadata(path.with_file_name(format!("{}.next", artifact_name(&target).unwrap()))).is_err());
        }
    }
    fn control(&self, barrier: &DrainBarrier) -> UpdateControl<DiskDrainStore, InstalledEffects> {
        UpdateControl::new(DiskDrainStore::at(&self.state), barrier.clone(),
            InstalledEffects::for_test(&self.install, &self.publish)).unwrap()
    }
}

fn activate(tree: &RootTree, control: &mut UpdateControl<DiskDrainStore, InstalledEffects>, manifest: &ReleaseManifest,
    generation: Sha256Digest, seed: u8) {
    let preparation = Preparation::for_manifest(manifest, generation);
    accepted(control, ControlCommand::Prepare { preparation: preparation.clone() });
    accepted(control, ControlCommand::Drain { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), Some(generation));
    accepted(control, ControlCommand::Apply { preparation: preparation.clone() });
    tree.assert_published(seed);
    // A retried Apply (lost reply) is idempotent.
    accepted(control, ControlCommand::Apply { preparation });
    tree.assert_published(seed);
    let request = request(ControlCommand::Readback { generation });
    let response = control.respond(&request);
    validate_response(&request, &response).unwrap();
    let ControlOutcome::Readback { components } = response.outcome else { panic!("no readback") };
    assert_eq!(components.len(), manifest.artifacts.len() - 3);
    let own = components.iter().find(|r| r.target == InstallTarget::Spawnd).unwrap();
    assert!(own.ready && own.effective_uid == 0 && Some(own.build_sha256) == running_image().ok());
    let run = components.iter().find(|r| r.target == InstallTarget::Trampoline).unwrap();
    let expected = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Trampoline).unwrap();
    assert!(run.ready && run.build_sha256 == expected.sha256);
    accepted(control, ControlCommand::Resume { generation });
    assert_eq!(DiskDrainStore::at(&tree.state).load().unwrap(), None);
}

#[test]
#[ignore = "requires root native Linux: publishes into an isolated root-owned /run tree"]
fn iso_root_update_apply_end_to_end_two_generations() {
    let tree = RootTree::new();
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = tree.control(&barrier);
    // The updater's first preflight precedes staging; the directory does not exist yet.
    let (first, manifest) = tree.stage(1);
    let absent = Sha256Digest([0xab; 32]);
    accepted(&mut control, ControlCommand::Prepare { preparation: preparation(absent) });
    activate(&tree, &mut control, &manifest, first, 1);
    let (second, next) = tree.stage(2);
    activate(&tree, &mut control, &next, second, 2);
    // A stale or foreign generation can never be applied once admission reopened.
    assert!(matches!(outcome(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&manifest, first) }),
        ControlOutcome::Refused));
    tree.assert_published(2);
    // A tampered staged artifact is caught by Prepare and by Apply's copy.
    let (third, last) = tree.stage(3);
    let staged = tree.install.join("generations").join(hex(&third)).join("duduclaw-run");
    std::fs::write(&staged, b"#!/bin/false\ntampered\n").unwrap();
    assert!(matches!(outcome(&mut control, ControlCommand::Prepare { preparation: Preparation::for_manifest(&last, third) }),
        ControlOutcome::Refused));
    accepted(&mut control, ControlCommand::Drain { generation: third });
    assert!(matches!(outcome(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&last, third) }),
        ControlOutcome::Refused));
    accepted(&mut control, ControlCommand::Resume { generation: third });
}

#[test]
#[ignore = "requires root native Linux: durable drain record in an isolated root-owned /run tree"]
fn iso_root_update_drain_waits_for_active_run() {
    let tree = RootTree::new();
    drain_waits_for_active_run(DiskStoreHandle(tree.state.clone()));
}
/// Cloneable handle over the same on-disk record (each call reopens it).
#[derive(Clone)]
struct DiskStoreHandle(PathBuf);
impl DrainStore for DiskStoreHandle {
    fn load(&mut self) -> io::Result<Option<Sha256Digest>> { DiskDrainStore::at(&self.0).load() }
    fn persist(&mut self, generation: Sha256Digest) -> io::Result<()> { DiskDrainStore::at(&self.0).persist(generation) }
    fn clear(&mut self) -> io::Result<()> { DiskDrainStore::at(&self.0).clear() }
}

#[test]
#[ignore = "requires root native Linux: simulated power loss in an isolated root-owned /run tree"]
fn iso_root_update_power_loss_each_phase() {
    let tree = RootTree::new();
    let record = tree.state.join(DRAIN_RECORD);
    let restart = || { let barrier = DrainBarrier::default(); let control = tree.control(&barrier); (barrier, control) };
    // 1. Loss before the record's rename: only a stale temporary exists. No
    //    record means no barrier, and the next Drain discards the temporary.
    std::fs::write(tree.state.join("drain.json.next"), b"{\"version\":1,\"gen").unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), None);
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    assert!(std::fs::symlink_metadata(tree.state.join("drain.json.next")).is_err());
    let metadata = std::fs::symlink_metadata(&record).unwrap();
    assert_eq!((metadata.mode() & 0o7777, metadata.uid(), metadata.nlink()), (0o600, 0, 1));
    drop(_admission);
    drop(control);
    // 2. Loss after the record landed, before the reply: closed after restart.
    let (barrier, _control) = restart();
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // 3. A torn or tampered record: closed with no generation; only Drain re-keys.
    std::fs::write(&record, b"{\"version\":1,\"generation\":[7,7").unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), Some(DrainClosure::Unreadable));
    assert!(matches!(outcome(&mut control, ControlCommand::Resume { generation: digest(7) }), ControlOutcome::Refused));
    let _admission = Observer::new(&barrier, 0);
    accepted(&mut control, ControlCommand::Drain { generation: digest(7) });
    assert_eq!(barrier.closure(), Some(DrainClosure::Generation(digest(7))));
    // 4. Loss after Resume's unlink, before the reply: open after restart, and
    //    the retried Resume is accepted.
    std::fs::remove_file(&record).unwrap();
    let (barrier, mut control) = restart();
    assert_eq!(barrier.closure(), None);
    accepted(&mut control, ControlCommand::Resume { generation: digest(7) });
    // 5. Loss in the middle of Apply: two components already renamed, a torn
    //    temporary for the third. The retried Apply completes every component.
    let (first, manifest) = tree.stage(1);
    let barrier = DrainBarrier::default();
    let _admission = Observer::new(&barrier, 0);
    let mut control = tree.control(&barrier);
    activate(&tree, &mut control, &manifest, first, 1);
    let (second, next) = tree.stage(2);
    for target in [InstallTarget::Spawnd, InstallTarget::Trampoline] {
        let path = tree.publish.join("usr/lib/duduclaw").join(artifact_name(&target).unwrap());
        let staged = tree.install.join("generations").join(hex(&second)).join(artifact_name(&target).unwrap());
        let temporary = path.with_file_name("partial.tmp");
        std::fs::copy(&staged, &temporary).unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&temporary, &path).unwrap();
    }
    std::fs::write(tree.publish.join("usr/libexec/duduclaw/duduclaw-parse-worker.next"), b"torn").unwrap();
    accepted(&mut control, ControlCommand::Drain { generation: second });
    accepted(&mut control, ControlCommand::Apply { preparation: Preparation::for_manifest(&next, second) });
    tree.assert_published(2);
    accepted(&mut control, ControlCommand::Resume { generation: second });
}
