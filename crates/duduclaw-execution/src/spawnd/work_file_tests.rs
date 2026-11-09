//! H5 client tests: a socketpair stands in for spawnd's control connection.
use super::*;
use super::super::{Entry, State};
use std::{collections::BTreeMap, io::{IoSlice, Read, Write}, os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Mutex as StdMutex};
use duduclaw_isolation::{execution::RunHandle, ids::RunId, ipc::{Envelope, WorkFileMetadata}};
use nix::sys::socket::{self, ControlMessage, MsgFlags, UnixAddr};
use tokio::sync::{Mutex, Semaphore};

const RUN: RunHandle = RunHandle(RunId([31; 16]));
const EMPLOYEE: EmployeeId = EmployeeId([37; 16]);

fn fixture(features: Option<FeatureSource>) -> (SpawndBackend, std::sync::Arc<Entry>, OwnedFd) {
    fixture_with(features, SpawndFeatures::WORK_BROKER)
}

/// `reserved` = the bits root sent in this connection's `Reserved.features`.
fn fixture_with(features: Option<FeatureSource>, reserved: SpawndFeatures)
    -> (SpawndBackend, std::sync::Arc<Entry>, OwnedFd) {
    let slots = std::sync::Arc::new(Semaphore::new(1));
    let (connection, peer) = super::super::transport::Connection::test_pair();
    let entry = std::sync::Arc::new(Entry { employee: EMPLOYEE, reservation_expiry: 1, features: reserved,
        connection: std::sync::Arc::new(connection), _permit: slots.clone().try_acquire_owned().unwrap(),
        state: Mutex::new(State {
            expires: tokio::time::Instant::now() + Duration::from_secs(30), started: true, task_ref: None,
            pty: false, mcp: false, output_bytes: 1, output_buffer: 1, io_received: false,
            stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None, workspace: None,
        }) });
    let backend = SpawndBackend { runs: StdMutex::new(BTreeMap::from([(RUN.0, entry.clone())])), slots,
        admission: None, workspace_bridge: None, features };
    (backend, entry, peer)
}

fn advertised(names: &'static [&'static str]) -> Option<FeatureSource> {
    Some(std::sync::Arc::new(move || Ok(features_from_advertised(names.iter().copied()))))
}

/// Answer exactly one request with `reply(request)` and the given rights.
fn root(peer: OwnedFd, reply: impl FnOnce(&SpawnRequest) -> (SpawnResponse, Vec<OwnedFd>) + Send + 'static)
    -> std::thread::JoinHandle<SpawnRequest> {
    std::thread::spawn(move || {
        let mut poll = libc::pollfd { fd: peer.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one live pollfd with its exact array length.
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
        let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
        let size = socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT).unwrap();
        bytes.truncate(size);
        let request: Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
        let (message, rights) = reply(&request.message);
        let frame = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id, message }).unwrap();
        let raw: Vec<_> = rights.iter().map(AsRawFd::as_raw_fd).collect();
        let controls = if raw.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&raw)] };
        socket::sendmsg::<UnixAddr>(peer.as_raw_fd(), &[IoSlice::new(&frame)], &controls, MsgFlags::MSG_NOSIGNAL, None).unwrap();
        request.message
    })
}

/// H2's snapshot: sealed memfd reopened O_RDONLY through procfs.
fn snapshot(bytes: &[u8]) -> (OwnedFd, File) {
    use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
    let fd = memfd_create(c"h5-snapshot", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING).unwrap();
    let mut writable = File::from(fd);
    writable.write_all(bytes).unwrap();
    fcntl(writable.as_raw_fd(), FcntlArg::F_ADD_SEALS(REQUIRED_SEALS)).unwrap();
    let read_only = File::open(format!("/proc/self/fd/{}", writable.as_raw_fd())).unwrap();
    (read_only.into(), writable)
}

fn metadata(file: &File) -> WorkFileMetadata {
    let stat = file.metadata().unwrap();
    WorkFileMetadata { size: stat.len(), inode: stat.ino(), device: stat.dev() }
}

fn nothing_sent(peer: &OwnedFd) -> bool {
    let mut bytes = [0u8; 16];
    socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT) == Err(nix::errno::Errno::EAGAIN)
}

#[test]
fn h5_bridge_requires_feature_bit() {
    let work = SpawndFeatures::WORK_BROKER;
    assert_eq!(features_from_advertised(["work_broker"]), work);
    assert!(!features_from_advertised(["helper_calls", "work_broker_v2", "WORK_BROKER"]).contains(work));
    assert!(!SpawndFeatures::default().contains(work));
    let failing: FeatureSource = std::sync::Arc::new(|| Err(BackendError::Unavailable));
    // No source (today's default), an older spawnd without the bit, and an
    // unreadable advertisement: nothing crosses the root boundary.
    for features in [None, advertised(&[]), advertised(&["helper_calls"]), Some(failing)] {
        let (backend, entry, peer) = fixture(features);
        assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", ipc::MAX_WORK_FILE_BYTES).unwrap_err(),
            WorkFileError::BrokerUnavailable);
        assert!(nothing_sent(&peer));
        assert!(!entry.connection.is_closed(), "refusing locally must not fence the run");
    }
    // HX4/HX5: the source only narrows; a connection whose `Reserved.features`
    // lacked the bit never sends the op, whatever the readiness view says.
    let (backend, entry, peer) = fixture_with(advertised(&["work_broker", "helper_calls"]), SpawndFeatures::HELPER_CALLS);
    assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", u64::MAX).unwrap_err(), WorkFileError::BrokerUnavailable);
    assert!(nothing_sent(&peer));
    assert!(!entry.connection.is_closed());
    // With the bit, identity mismatches and invalid paths are still refused locally.
    let (backend, entry, peer) = fixture(advertised(&["work_broker"]));
    assert_eq!(backend.open_work_file(RUN, EmployeeId([1; 16]), "a.txt", u64::MAX).unwrap_err(), WorkFileError::NotLive);
    assert_eq!(backend.open_work_file(RunHandle(RunId([2; 16])), EMPLOYEE, "a.txt", u64::MAX).unwrap_err(),
        WorkFileError::NotLive);
    for path in ["", "/etc/passwd", "../x", "a//b", ".forks/x", "a\u{85}b"] {
        assert_eq!(backend.open_work_file(RUN, EMPLOYEE, path, u64::MAX).unwrap_err(), WorkFileError::Refused, "{path:?}");
    }
    assert!(nothing_sent(&peer));
    assert!(!entry.connection.is_closed());
}

/// W1: the per-run view a gateway run broker binds to is the run's own
/// admission (`Reserved.features`), only ever narrowed by the readiness source;
/// the control id is that connection's; a fenced connection has no view.
#[test]
fn w1_feature_source_comes_from_run_admission() {
    let both = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS);
    // Readiness advertises both, admission granted only helper calls.
    let (backend, entry, peer) = fixture_with(advertised(&["work_broker", "helper_calls"]), SpawndFeatures::HELPER_CALLS);
    assert!(backend.has_feature_source());
    let view = backend.run_transport(RUN).unwrap();
    assert_eq!(view.employee, EMPLOYEE);
    assert_eq!(view.control, entry.connection.id);
    assert_eq!(view.features, SpawndFeatures::HELPER_CALLS, "readiness can never add a bit admission lacked");
    assert_eq!(backend.features(), both, "the host-wide view is separate from the run's");
    // Admission granted both, readiness lost the broker bit: narrowed.
    let (backend, _entry, _peer2) = fixture_with(advertised(&["helper_calls"]), both);
    assert_eq!(backend.run_transport(RUN).unwrap().features, SpawndFeatures::HELPER_CALLS);
    // Unreadable readiness: nothing.
    let failing: FeatureSource = std::sync::Arc::new(|| Err(BackendError::Unavailable));
    let (backend, _entry, _peer3) = fixture_with(Some(failing), both);
    assert_eq!(backend.run_transport(RUN).unwrap().features, SpawndFeatures::NONE);
    // No source installed: work ops stay impossible (H5 default).
    let (backend, _entry, _peer4) = fixture_with(None, both);
    assert!(!backend.has_feature_source());
    assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", u64::MAX).unwrap_err(), WorkFileError::BrokerUnavailable);
    // Unknown run or a fenced connection: no view at all.
    let (backend, entry, _peer5) = fixture_with(advertised(&["work_broker"]), both);
    assert!(backend.run_transport(RunHandle(RunId([2; 16]))).is_none());
    entry.connection.close_now();
    assert!(backend.run_transport(RUN).is_none());
    assert!(nothing_sent(&peer));
}

/// W1: an open pinned to the control connection a run broker latched is
/// refused (`NotLive`, nothing sent, run not fenced) once the run handle maps
/// to another connection; on the latched connection it is sent as usual.
#[test]
fn w1_open_refused_when_control_connection_changed() {
    let (backend, entry, peer) = fixture(advertised(&["work_broker"]));
    let mut other = entry.connection.id;
    other.0[0] ^= 0xff;
    assert_eq!(backend.open_work_file_on(RUN, Some(other), EMPLOYEE, "a.txt", u64::MAX).unwrap_err(),
        WorkFileError::NotLive);
    assert!(nothing_sent(&peer));
    assert!(!entry.connection.is_closed(), "refusing locally must not fence the run");
    // The latched connection itself: the op reaches root.
    let worker = root(peer, |_| (SpawnResponse::Error { code: ErrorCode::WorkFileNotFound }, vec![]));
    assert_eq!(backend.open_work_file_on(RUN, Some(entry.connection.id), EMPLOYEE, "a.txt", u64::MAX).unwrap_err(),
        WorkFileError::NotFound);
    let SpawnRequest::OpenWorkFile { run, employee_id, .. } = worker.join().unwrap() else { panic!("not a work op") };
    assert_eq!((run, employee_id), (RUN, EMPLOYEE));
}

#[test]
fn h5_bridge_validates_reply_and_single_fd() {
    // Accepted: exactly one sealed read-only memfd whose fstat equals the reply.
    let (backend, entry, peer) = fixture(advertised(&["work_broker"]));
    let worker = root(peer, |_| {
        let (fd, writable) = snapshot(b"brokered");
        let file = metadata(&writable);
        (SpawnResponse::WorkFile { run: RUN, file }, vec![fd])
    });
    let mut snapshot_file = backend.open_work_file(RUN, EMPLOYEE, "notes/員工.md", ipc::MAX_WORK_FILE_BYTES).unwrap();
    assert_eq!(worker.join().unwrap(), SpawnRequest::OpenWorkFile { run: RUN, employee_id: EMPLOYEE,
        path: WorkPath::new("notes/員工.md".into()).unwrap(), mode: WorkOpenMode::ReadOnly });
    assert_eq!((snapshot_file.size, snapshot_file.nlink), (8, 0), "nlink is reported as read: 0");
    let mut text = String::new();
    snapshot_file.file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "brokered");
    assert!(snapshot_file.file.write_all(b"x").is_err());
    assert!(!entry.connection.is_closed());

    // Root refusals are typed and keep the run's control connection.
    for (code, expected) in [(ErrorCode::WorkFileNotFound, WorkFileError::NotFound),
        (ErrorCode::WorkFileRefused, WorkFileError::Refused), (ErrorCode::WorkFileTooLarge, WorkFileError::TooLarge),
        (ErrorCode::WorkspaceBrokerUnavailable, WorkFileError::BrokerUnavailable),
        (ErrorCode::NotReady, WorkFileError::NotLive), (ErrorCode::Unauthorized, WorkFileError::NotLive),
        (ErrorCode::Internal, WorkFileError::Unavailable)] {
        let (backend, entry, peer) = fixture(advertised(&["work_broker"]));
        let worker = root(peer, move |_| (SpawnResponse::Error { code }, vec![]));
        assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", u64::MAX).unwrap_err(), expected, "{code:?}");
        worker.join().unwrap();
        assert!(!entry.connection.is_closed(), "{code:?}");
    }

    // Everything else fails closed and fences the control connection.
    type Reply = Box<dyn FnOnce() -> (SpawnResponse, Vec<OwnedFd>) + Send>;
    fn case(reply: impl FnOnce() -> (SpawnResponse, Vec<OwnedFd>) + Send + 'static) -> Reply { Box::new(reply) }
    let unsealed = || {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain"), b"plain").unwrap();
        let file = File::open(dir.path().join("plain")).unwrap();
        let meta = metadata(&file);
        (meta, OwnedFd::from(file))
    };
    let cases: Vec<(&str, Reply)> = vec![
        ("wrong run", case(|| { let (fd, w) = snapshot(b"x");
            (SpawnResponse::WorkFile { run: RunHandle(RunId([2; 16])), file: metadata(&w) }, vec![fd]) })),
        ("no fd", case(|| { let (_, w) = snapshot(b"x"); (SpawnResponse::WorkFile { run: RUN, file: metadata(&w) }, vec![]) })),
        ("two fds", case(|| { let (fd, w) = snapshot(b"x"); let (extra, _) = snapshot(b"y");
            (SpawnResponse::WorkFile { run: RUN, file: metadata(&w) }, vec![fd, extra]) })),
        ("unsealed regular file", case(move || { let (meta, fd) = unsealed();
            (SpawnResponse::WorkFile { run: RUN, file: meta }, vec![fd]) })),
        ("writable memfd", case(|| { let (_, w) = snapshot(b"x"); let meta = metadata(&w);
            (SpawnResponse::WorkFile { run: RUN, file: meta }, vec![OwnedFd::from(w)]) })),
        ("identity mismatch", case(|| { let (fd, w) = snapshot(b"x"); let mut meta = metadata(&w); meta.inode ^= 1;
            (SpawnResponse::WorkFile { run: RUN, file: meta }, vec![fd]) })),
        ("size mismatch", case(|| { let (fd, w) = snapshot(b"x"); let mut meta = metadata(&w); meta.size = 2;
            (SpawnResponse::WorkFile { run: RUN, file: meta }, vec![fd]) })),
        ("refusal with fd", case(|| { let (fd, _) = snapshot(b"x");
            (SpawnResponse::Error { code: ErrorCode::WorkFileNotFound }, vec![fd]) })),
        ("unrelated reply", case(|| (SpawnResponse::Ack, vec![]))),
    ];
    for (name, reply) in cases {
        let (backend, entry, peer) = fixture(advertised(&["work_broker"]));
        let worker = root(peer, move |_| reply());
        assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", u64::MAX).unwrap_err(), WorkFileError::Unavailable, "{name}");
        worker.join().unwrap();
        assert!(entry.connection.is_closed(), "{name}");
    }

    // A valid snapshot above the caller's limit is refused as too large.
    let (backend, _entry, peer) = fixture(advertised(&["work_broker"]));
    let worker = root(peer, |_| { let (fd, w) = snapshot(b"0123456789");
        (SpawnResponse::WorkFile { run: RUN, file: metadata(&w) }, vec![fd]) });
    assert_eq!(backend.open_work_file(RUN, EMPLOYEE, "a.txt", 9).unwrap_err(), WorkFileError::TooLarge);
    worker.join().unwrap();
}
