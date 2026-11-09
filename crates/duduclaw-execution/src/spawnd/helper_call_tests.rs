//! HX5 client tests: a SEQPACKET socketpair stands in for spawnd's control
//! connection of the run; the "root" thread answers each request in order.
use super::*;
use super::super::{Entry, State, SpawndBackend};
use std::{collections::BTreeMap, io::IoSlice, os::fd::AsRawFd, sync::Mutex as StdMutex};
use duduclaw_isolation::{bounded::Text, execution::{HelperPlan, RunReader}, ids::RunId,
    ipc::{Envelope, HelperSlot, SpawndFeatures}};
use nix::sys::socket::{self, ControlMessage, MsgFlags, UnixAddr};
use tokio::sync::{Mutex, Semaphore};

const RUN: RunHandle = RunHandle(RunId([41; 16]));
const EMPLOYEE: EmployeeId = EmployeeId([43; 16]);

fn call_id() -> HelperCallId { HelperCallId::new([7; 16]).unwrap() }

fn fixture(reserved: SpawndFeatures) -> (SpawndBackend, Arc<Entry>, OwnedFd) {
    let slots = Arc::new(Semaphore::new(1));
    let (connection, peer) = super::super::transport::Connection::test_pair();
    let entry = Arc::new(Entry { employee: EMPLOYEE, reservation_expiry: 1, features: reserved,
        connection: Arc::new(connection), _permit: slots.clone().try_acquire_owned().unwrap(),
        state: Mutex::new(State {
            expires: tokio::time::Instant::now() + Duration::from_secs(30), started: true, task_ref: None,
            pty: false, mcp: false, output_bytes: 1, output_buffer: 1, io_received: true,
            stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None, workspace: None,
        }) });
    let backend = SpawndBackend { runs: StdMutex::new(BTreeMap::from([(RUN.0, entry.clone())])), slots,
        admission: None, workspace_bridge: None, features: None };
    (backend, entry, peer)
}

fn plan(input: &[u8]) -> HelperPlan {
    HelperPlan { run: RUN, employee: EMPLOYEE, helper: RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()),
        slot: HelperSlot::Foreground, timeout_ms: 60_000, input: input.to_vec(), output_limit: 1024 }
}

type Reply = Box<dyn FnOnce(&SpawnRequest, usize) -> (SpawnResponse, Vec<OwnedFd>) + Send>;

/// Answers `replies.len()` requests in order and returns what root received
/// (request, number of fds).
fn root(peer: OwnedFd, replies: Vec<Reply>) -> std::thread::JoinHandle<Vec<(SpawnRequest, usize)>> {
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let mut poll = libc::pollfd { fd: peer.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one live pollfd with its exact array length.
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
            let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
            let mut space = nix::cmsg_space!([std::os::fd::RawFd; 4]);
            let mut iov = [std::io::IoSliceMut::new(&mut bytes)];
            let message = socket::recvmsg::<UnixAddr>(peer.as_raw_fd(), &mut iov, Some(&mut space),
                MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_CMSG_CLOEXEC).unwrap();
            let size = message.bytes;
            let mut fds = Vec::new();
            for cmsg in message.cmsgs().unwrap() {
                if let socket::ControlMessageOwned::ScmRights(raw) = cmsg {
                    // SAFETY: SCM_RIGHTS installed these descriptors for us.
                    fds.extend(raw.into_iter().map(|fd| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) }));
                }
            }
            let request: Envelope<SpawnRequest> = ipc::decode(&bytes[..size]).unwrap();
            let (message, rights) = reply(&request.message, fds.len());
            let frame = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id, message }).unwrap();
            let raw: Vec<_> = rights.iter().map(AsRawFd::as_raw_fd).collect();
            let controls = if raw.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&raw)] };
            socket::sendmsg::<UnixAddr>(peer.as_raw_fd(), &[IoSlice::new(&frame)], &controls, MsgFlags::MSG_NOSIGNAL, None).unwrap();
            seen.push((request.message, fds.len()));
        }
        seen
    })
}

fn nothing_sent(peer: &OwnedFd) -> bool {
    let mut bytes = [0u8; 16];
    socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT) == Err(nix::errno::Errno::EAGAIN)
}

async fn read_all(reader: &mut Box<dyn RunReader>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        let count = reader.read(&mut buffer).await.unwrap();
        if count == 0 { return output; }
        output.extend_from_slice(&buffer[..count]);
    }
}

/// [F17] Without `helper_calls` in this connection's `Reserved.features`
/// nothing is sent (an older spawnd would drop the run's control connection).
#[tokio::test]
async fn hx5_helper_client_requires_connection_feature_bit() {
    for reserved in [SpawndFeatures::NONE, SpawndFeatures::WORK_BROKER] {
        let (backend, entry, peer) = fixture(reserved);
        assert_eq!(backend.helper_start(plan(b"{}")).await.err(), Some(HelperError::FeatureMissing));
        assert_eq!(backend.helper_wait(RUN, call_id()).await.err(), Some(HelperError::FeatureMissing));
        assert_eq!(backend.helper_control(Control::Cancel, RUN, call_id()).await.err(), Some(HelperError::FeatureMissing));
        assert!(nothing_sent(&peer));
        assert!(!entry.connection.is_closed());
    }
    // A readiness source can narrow but never add a bit root did not send.
    let (mut backend, _entry, peer) = fixture(SpawndFeatures::NONE);
    let source: super::super::FeatureSource = Arc::new(|| Ok::<_, BackendError>(SpawndFeatures::HELPER_CALLS));
    backend.features = Some(source);
    assert_eq!(backend.helper_start(plan(b"{}")).await.err(), Some(HelperError::FeatureMissing));
    assert!(nothing_sent(&peer));
    // Another employee's plan, an unknown run and an empty input never reach root.
    let (backend, _entry, peer) = fixture(SpawndFeatures::HELPER_CALLS);
    let mut foreign = plan(b"{}");
    foreign.employee = EmployeeId([1; 16]);
    assert_eq!(backend.helper_start(foreign).await.err(), Some(HelperError::Denied));
    let mut other = plan(b"{}");
    other.run = RunHandle(RunId([2; 16]));
    assert_eq!(backend.helper_start(other).await.err(), Some(HelperError::NotLive));
    assert_eq!(backend.helper_start(plan(b"")).await.err(), Some(HelperError::Denied));
    assert!(nothing_sent(&peer));
}

/// Start → sealed memfd + `[StdoutRead, StderrRead]`, wait polls until
/// `HelperEnded`, cancel is `HelperCancel` of the call: never `Cancel{run}`.
#[tokio::test]
async fn hx5_helper_client_start_wait_cancel_on_the_run_connection() {
    let (backend, entry, peer) = fixture(SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::WORK_BROKER));
    let (out_r, out_w) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).unwrap();
    let (err_r, err_w) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).unwrap();
    nix::unistd::write(&out_w, b"{\"ok\":true}").unwrap();
    drop(out_w);
    drop(err_w);
    let started: Reply = Box::new(move |request, fds| {
        assert_eq!(fds, 1, "exactly the sealed input memfd");
        let SpawnRequest::HelperCall { run, employee_id, input_bytes, slot, .. } = request else { panic!("{request:?}") };
        assert_eq!((*run, *employee_id, *input_bytes, *slot), (RUN, EMPLOYEE, 2, HelperSlot::Foreground));
        (SpawnResponse::HelperStarted { run: RUN, call: call_id(), effective_timeout_ms: 30_000 }, vec![out_r, err_r])
    });
    let not_ready: Reply = Box::new(|_, _| (SpawnResponse::Error { code: ErrorCode::NotReady }, vec![]));
    let ended: Reply = Box::new(|_, _| (SpawnResponse::HelperEnded { run: RUN, call: call_id(),
        outcome: HelperOutcome::Exited(0) }, vec![]));
    let ack: Reply = Box::new(|_, _| (SpawnResponse::Ack, vec![]));
    let worker = root(peer, vec![started, not_ready, ended, ack]);
    let mut start = backend.helper_start(plan(b"{}")).await.unwrap();
    assert_eq!((start.call, start.effective_timeout_ms), (call_id(), 30_000));
    assert_eq!(read_all(&mut start.stdout).await, b"{\"ok\":true}");
    assert!(read_all(&mut start.stderr).await.is_empty());
    assert_eq!(backend.helper_wait(RUN, call_id()).await.unwrap(), HelperOutcome::Exited(0));
    backend.helper_control(Control::Cancel, RUN, call_id()).await.unwrap();
    let seen = worker.join().unwrap();
    assert!(matches!(seen[1].0, SpawnRequest::HelperWait { run, employee_id, call }
        if run == RUN && employee_id == EMPLOYEE && call == call_id()));
    assert!(matches!(seen[2].0, SpawnRequest::HelperWait { employee_id, .. } if employee_id == EMPLOYEE));
    assert!(matches!(seen[3].0, SpawnRequest::HelperCancel { run, employee_id, call }
        if run == RUN && employee_id == EMPLOYEE && call == call_id()));
    assert!(seen.iter().all(|(request, _)| !matches!(request, SpawnRequest::Cancel { .. })));
    assert!(!entry.connection.is_closed(), "per-call ops never fence the run");
}

/// HX4 fix round: `Stop`/`Cancel` may be `Ack`ed while delivery is still
/// pending. `Ack` is acceptance only; the call ends at `HelperEnded`, which
/// the client still polls for with `HelperWait`.
#[tokio::test]
async fn hx5_control_ack_is_acceptance_not_end() {
    let (backend, entry, peer) = fixture(SpawndFeatures::HELPER_CALLS);
    let ack: Reply = Box::new(|_, _| (SpawnResponse::Ack, vec![]));
    let pending: Reply = Box::new(|_, _| (SpawnResponse::Error { code: ErrorCode::NotReady }, vec![]));
    let ended: Reply = Box::new(|_, _| (SpawnResponse::HelperEnded { run: RUN, call: call_id(),
        outcome: HelperOutcome::Signalled(9) }, vec![]));
    let worker = root(peer, vec![ack, pending, ended]);
    backend.helper_control(Control::Cancel, RUN, call_id()).await.unwrap();
    assert_eq!(backend.helper_wait(RUN, call_id()).await.unwrap(), HelperOutcome::Signalled(9));
    let seen = worker.join().unwrap();
    assert!(matches!(seen[0].0, SpawnRequest::HelperCancel { .. }));
    assert!(seen[1..].iter().all(|(request, _)| matches!(request, SpawnRequest::HelperWait { .. })));
    assert!(!entry.connection.is_closed());
}

/// Root refusals are typed per-call results and keep the run's connection.
#[tokio::test]
async fn hx5_helper_client_typed_refusals_keep_control() {
    for (code, expected) in [(ErrorCode::HelperBusy, HelperError::Busy),
        (ErrorCode::RunTimeExhausted, HelperError::RunTimeExhausted),
        (ErrorCode::HelperUnavailable, HelperError::Unavailable),
        (ErrorCode::HelperCgroupUnavailable, HelperError::CgroupUnavailable),
        // HX6 security fix round (finding 4): spawnd's own recording gate.
        (ErrorCode::HelperRecordingUnsupported, HelperError::RecordingUnsupported),
        (ErrorCode::NotReady, HelperError::NotLive), (ErrorCode::Unauthorized, HelperError::Denied)] {
        let (backend, entry, peer) = fixture(SpawndFeatures::HELPER_CALLS);
        let worker = root(peer, vec![Box::new(move |_, _| (SpawnResponse::Error { code }, vec![]))]);
        assert_eq!(backend.helper_start(plan(b"{}")).await.err(), Some(expected), "{code:?}");
        worker.join().unwrap();
        assert!(!entry.connection.is_closed(), "{code:?}");
    }
}

/// The single constructor of the per-call control requests; it carries the
/// run's admitted employee (HX4 security round, finding 8).
#[test]
fn hx5_control_requests_have_one_constructor() {
    let (run, employee_id, call) = (RUN, EMPLOYEE, call_id());
    assert_eq!(control_request(Control::Wait, RUN, call, EMPLOYEE), SpawnRequest::HelperWait { run, employee_id, call });
    assert_eq!(control_request(Control::Stop, RUN, call, EMPLOYEE), SpawnRequest::HelperStop { run, employee_id, call });
    assert_eq!(control_request(Control::Cancel, RUN, call, EMPLOYEE), SpawnRequest::HelperCancel { run, employee_id, call });
    for request in [Control::Wait, Control::Stop, Control::Cancel].map(|control| control_request(control, RUN, call, EMPLOYEE)) {
        request.validate().unwrap();
    }
    let memfd = sealed_input(b"abc").unwrap();
    assert!(nix::unistd::write(&memfd, b"x").is_err(), "sealed input is immutable");
}
