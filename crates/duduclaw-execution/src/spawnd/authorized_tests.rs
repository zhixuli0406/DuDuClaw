use super::*;
use std::{fs::File, os::{fd::AsRawFd, unix::fs::MetadataExt}, sync::atomic::{AtomicBool, Ordering}};
use duduclaw_isolation::ipc::{Envelope, SpawnRequest, SpawnResponse};
use crate::workspace::{RootWorkspaceReceipt, RootWorkspaceLease};
use nix::sys::socket::{self, AddressFamily, MsgFlags, SockFlag, SockType};

struct Authority { live: Arc<AtomicBool>, identity: (u64, u64) }
impl ExecutionAuthority for Authority {
    fn verify_live(&self) -> Result<(), BackendError> {
        if self.live.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) }
    }
    fn verify_workspace(&self, handle: &str, directory: &File) -> Result<(), BackendError> {
        self.verify_live()?;
        let m = directory.metadata().map_err(|_| BackendError::Io)?;
        if handle == "selected" && (m.dev(), m.ino()) == self.identity { Ok(()) } else { Err(BackendError::Denied) }
    }
}
struct Lease;
impl RootWorkspaceLease for Lease {
    fn verify(&self, _: &RootWorkspaceRequest, _: &File) -> Result<(), BackendError> { Ok(()) }
}
struct Bridge { file: File, wrong_run: bool }
impl RootWorkspaceBridge for Bridge {
    fn prepare(&self, mut request: RootWorkspaceRequest) -> BackendFuture<'_, RootWorkspaceReceipt> {
        Box::pin(async move {
            if self.wrong_run { request.run = RunHandle(RunId([9; 16])); }
            RootWorkspaceReceipt::from_root_attestation(request, self.file.try_clone().map_err(|_| BackendError::Io)?, Arc::new(Lease))
        })
    }
}
fn policy() -> SpawnPolicy {
    SpawnPolicy { workspace_delivery: None, run: RunHandle(RunId([1; 16])), employee: EmployeeId([2; 16]),
        class: ExecutionClass::EmployeeTool, runtime: RuntimeKind::Registered(Text::new("fixture".into()).unwrap()),
        argv_allowlist: vec![], environment_allowlist: BTreeMap::new(),
        workspace: WorkspacePolicy { workspace: Text::new("selected".into()).unwrap(), relative_cwd: Text::new(".".into()).unwrap() },
        resources: ResourceLimits { memory_bytes: 1024, cpu_quota_percent: 100, tasks: 16, open_files: 64, ptys: 0,
            disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 }, io_read_bytes_per_sec: 1024,
            io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10, io_write_ops_per_sec: 10,
            output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
        network: NetworkPolicy::Offline, pty: false,
        capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
        timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000, pty_idle_ms: None, termination_grace_ms: 100 },
        inputs: RunInputs { prompt: tests::input(b"", 0), attachments: vec![] } }
}
async fn fixture(bridge: Option<Arc<dyn RootWorkspaceBridge>>) -> (SpawndBackend, OwnedFd) {
    let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
    let slots = Arc::new(Semaphore::new(2));
    let permit = slots.clone().acquire_owned().await.unwrap();
    let p = policy();
    let entry = Entry { employee: p.employee, reservation_expiry: 0, connection: Arc::new(Connection::test_connection(left)), _permit: permit,
        state: Mutex::new(State { expires: Instant::now() + Duration::from_secs(5), started: false,
            task_ref: None, pty: false, mcp: false, output_bytes: 0, output_buffer: 0, io_received: false,
            stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None, workspace: None }) };
    (SpawndBackend { runs: StdMutex::new(BTreeMap::from([(p.run.0, Arc::new(entry))])), slots, admission: None, workspace_bridge: bridge }, right)
}
async fn receive(fd: &OwnedFd) -> Envelope<SpawnRequest> {
    let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
    loop {
        match socket::recv(fd.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT) {
            Ok(n) if n > 0 => return ipc::decode(&bytes[..n]).unwrap(),
            Err(nix::errno::Errno::EAGAIN) => tokio::time::sleep(Duration::from_millis(1)).await,
            result => panic!("unexpected root request: {result:?}"),
        }
    }
}
fn respond(fd: &OwnedFd, id: duduclaw_isolation::ids::RequestId, response: SpawnResponse) {
    let bytes = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: id, message: response }).unwrap();
    assert_eq!(socket::send(fd.as_raw_fd(), &bytes, MsgFlags::MSG_NOSIGNAL).unwrap(), bytes.len());
}
fn authority(file: &File) -> (Arc<dyn ExecutionAuthority>, Arc<AtomicBool>) {
    let m = file.metadata().unwrap();
    let live = Arc::new(AtomicBool::new(true));
    (Arc::new(Authority { live: live.clone(), identity: (m.dev(), m.ino()) }), live)
}

#[tokio::test]
async fn authorized_spawn_requires_explicit_bridge_and_exact_root_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let file = File::open(temp.path()).unwrap();
    let (auth, _) = authority(&file);
    let (backend, _root) = fixture(None).await;
    assert!(!backend.supports_authorized_workspace());
    assert_eq!(backend.spawn_authorized(policy(), auth.clone()).await, Err(BackendError::Unavailable));
    let (backend, root) = fixture(Some(Arc::new(Bridge { file, wrong_run: true }))).await;
    assert!(backend.supports_authorized_workspace());
    assert_eq!(backend.spawn_authorized(policy(), auth).await, Err(BackendError::Denied));
    let mut bytes = [0; 1];
    assert_eq!(socket::recv(root.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT), Err(nix::errno::Errno::EAGAIN));
}

#[tokio::test]
async fn authorized_spawn_holds_live_authority_and_cancels_queued_run_on_revoke() {
    let temp = tempfile::tempdir().unwrap();
    let file = File::open(temp.path()).unwrap();
    let (auth, live) = authority(&file);
    let (backend, root) = fixture(Some(Arc::new(Bridge { file, wrong_run: false }))).await;
    let server = tokio::spawn(async move {
        let request = receive(&root).await;
        let SpawnRequest::StartRun { run, .. } = request.message else { panic!("StartRun required"); };
        respond(&root, request.request_id, SpawnResponse::Accepted { run, status: RunStatus::Starting });
        let cancelled = tokio::time::timeout(Duration::from_secs(1), receive(&root)).await.unwrap();
        assert!(matches!(cancelled.message, SpawnRequest::Cancel { run: actual } if actual == run));
        respond(&root, cancelled.request_id, SpawnResponse::Ack);
    });
    let run = backend.spawn_authorized(policy(), auth).await.unwrap();
    assert_eq!(run, policy().run);
    live.store(false, Ordering::SeqCst);
    assert_eq!(backend.wait(run, WaitOrigin::Gateway).await, Err(BackendError::Denied));
    assert!(matches!(backend.take_stdio(run).await, Err(BackendError::Denied)));
    server.await.unwrap();
}

struct Cancel { live: Arc<AtomicBool> }
impl CancelAuthority for Cancel {
    fn verify_cancel(&self, employee: EmployeeId, run: RunHandle) -> Result<(), BackendError> {
        if employee == policy().employee && run == policy().run && self.live.load(Ordering::SeqCst) {
            Ok(())
        } else { Err(BackendError::Denied) }
    }
}

#[tokio::test]
async fn authorized_cancel_rechecks_revocation_after_transport_wait() {
    let (backend, root) = fixture(None).await;
    let backend = Arc::new(backend);
    let busy = backend.clone();
    let status = tokio::spawn(async move { busy.status(policy().run).await });
    let request = receive(&root).await;
    assert!(matches!(request.message, SpawnRequest::Status { .. }));
    let live = Arc::new(AtomicBool::new(true));
    let cancel = backend.kill_authorized(policy().run, Arc::new(Cancel { live: live.clone() }));
    tokio::pin!(cancel);
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut cancel).await.is_err());
    live.store(false, Ordering::SeqCst);
    respond(&root, request.request_id, SpawnResponse::Status { run: policy().run, status: RunStatus::Running });
    assert_eq!(status.await.unwrap(), Ok(RunStatus::Running));
    assert_eq!(cancel.await, Err(BackendError::Denied));
    // Refusal sent no Cancel and must not cancel the target through EOF.
    let mut bytes = [0; 16];
    assert_eq!(socket::recv(root.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT), Err(nix::errno::Errno::EAGAIN));
}

#[tokio::test]
async fn authorized_cancel_sends_only_for_live_bound_authority() {
    let (backend, root) = fixture(None).await;
    let server = tokio::spawn(async move {
        let request = receive(&root).await;
        assert!(matches!(request.message, SpawnRequest::Cancel { run } if run == policy().run));
        respond(&root, request.request_id, SpawnResponse::Ack);
    });
    backend.kill_authorized(policy().run, Arc::new(Cancel { live: Arc::new(AtomicBool::new(true)) })).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn authorized_cancel_abandoned_before_send_preserves_target_connection() {
    let (backend, root) = fixture(None).await;
    let backend = Arc::new(backend);
    let busy = backend.clone();
    let status = tokio::spawn(async move { busy.status(policy().run).await });
    let request = receive(&root).await;
    let pending = backend.clone();
    let cancel = tokio::spawn(async move {
        pending.kill_authorized(policy().run, Arc::new(Cancel { live: Arc::new(AtomicBool::new(true)) })).await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.abort();
    assert!(cancel.await.unwrap_err().is_cancelled());
    respond(&root, request.request_id, SpawnResponse::Status { run: policy().run, status: RunStatus::Running });
    assert_eq!(status.await.unwrap(), Ok(RunStatus::Running));
    // The abandoned blocking closure cannot send after acquiring the lock.
    let query = backend.clone();
    let status = tokio::spawn(async move { query.status(policy().run).await });
    let request = receive(&root).await;
    assert!(matches!(request.message, SpawnRequest::Status { .. }));
    respond(&root, request.request_id, SpawnResponse::Status { run: policy().run, status: RunStatus::Running });
    assert_eq!(status.await.unwrap(), Ok(RunStatus::Running));
}
