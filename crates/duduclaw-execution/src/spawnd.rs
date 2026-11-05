//! Linux gateway client. Root derives executable, argv, environment and mounts.
//! A control connection belongs to one reservation/run; a cancelled RPC closes it
//! rather than allowing a late reply to be mistaken for a later transaction.

mod transport;
mod fd_io;

use std::{collections::BTreeMap, os::fd::OwnedFd, sync::{Arc, Mutex as StdMutex}, time::Duration};
use duduclaw_isolation::{bounded::{List, Text}, execution::*, ids::{EmployeeId, RunId}, ipc::{self, SpawnRequest, SpawnResponse, RunStatus}};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use transport::{Connection, Packet};

const MAX_TRACKED_RUNS: usize = 1024;
const TERMINAL_RETENTION: Duration = Duration::from_secs(300);
const MAX_MEMFD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEMFD_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Spawnd is mandatory in protected mode; construction never chooses a local
/// backend. The endpoint and root peer identity are not configurable by a caller.
pub struct SpawndBackend {
    runs: StdMutex<BTreeMap<RunId, Arc<Entry>>>,
    slots: Arc<Semaphore>,
    admission: Option<fn() -> Result<(), BackendError>>,
}

/// Read-only host metadata; generated only after peer and reply validation.
#[derive(Clone, Debug)]
pub struct ReservationMetadata {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub control: ipc::ControlConnectionId,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug)]
pub struct McpReceipt {
    pub control: ipc::ControlConnectionId,
    pub response: SpawnResponse,
}

struct Entry {
    employee: EmployeeId,
    reservation_expiry: u64,
    connection: Connection,
    state: Mutex<State>,
    _permit: OwnedSemaphorePermit,
}

struct State {
    expires: Instant,
    started: bool,
    task_ref: Option<Text<128>>,
    pty: bool,
    mcp: bool,
    output_bytes: u64,
    output_buffer: u32,
    io_received: bool,
    stdio: Option<RunStdio>,
    terminal: Option<RunPty>,
    mcp_fd: Option<OwnedFd>,
    io_receipt: Option<SpawnResponse>,
    outcome: Option<RunOutcome>,
    terminal_at: Option<Instant>,
}

impl SpawndBackend {
    /// Checks the root-owned endpoint and peer before advertising availability.
    /// Package B performs readiness checks for the trampoline, egress and quotas.
    pub async fn connect() -> Result<Self, BackendError> {
        let _probe = transport::connect()?;
        Ok(Self { runs: StdMutex::new(BTreeMap::new()), slots: Arc::new(Semaphore::new(MAX_TRACKED_RUNS)), admission: None })
    }

    /// Gateway boot uses this constructor so retained backend Arcs cannot
    /// bypass a later readiness change. Cleanup operations remain available.
    pub async fn connect_with_admission(check: fn() -> Result<(), BackendError>) -> Result<Self, BackendError> {
        // Boot may install the backend before the remaining services initialize;
        // admission is checked at each reserve/spawn, never cached at connect.
        let mut backend = Self::connect().await?;
        backend.admission = Some(check);
        Ok(backend)
    }

    fn check_admission(&self) -> Result<(), BackendError> {
        self.admission.map_or(Ok(()), |check| check())
    }

    /// Exact metadata retained from ReserveRun on the authenticated control
    /// connection. D must read this before issuing a pending capability.
    pub async fn reservation(&self, run: RunHandle) -> Result<ReservationMetadata, BackendError> {
        let entry = self.entry(run)?;
        let state = entry.state.lock().await;
        if entry.connection.is_closed() || state.started || state.outcome.is_some()
            || Instant::now() >= state.expires { return Err(BackendError::Denied); }
        Ok(ReservationMetadata { run, employee: entry.employee,
            control: entry.connection.id, expires_at_unix_ms: entry.reservation_expiry })
    }

    /// Synchronous authority adapter: original control identity plus a fresh
    /// root Status reply, never a cached Running state. Concurrent RPCs serialize
    /// on the original control socket. The transaction may block up to 250 ms;
    /// callers should use a blocking pool rather than block_on a Tokio runtime.
    pub fn live_snapshot(&self, run: RunHandle) -> Result<(ReservationMetadata, RunStatus), BackendError> {
        let entry = self.entry(run)?;
        let status = entry.connection.live_status(run, entry.reservation_expiry)?;
        Ok((ReservationMetadata { run, employee: entry.employee,
            control: entry.connection.id, expires_at_unix_ms: entry.reservation_expiry }, status))
    }

    /// Retains the actual validated TakeIo response, rather than reconstructing
    /// an Io reply from a caller's request. The endpoint itself transfers once.
    pub async fn mcp_receipt(&self, run: RunHandle) -> Result<McpReceipt, BackendError> {
        let entry = self.entry(run)?;
        let state = entry.state.lock().await;
        if entry.connection.is_closed() || state.outcome.is_some() || !state.mcp {
            return Err(BackendError::Denied);
        }
        let response = state.io_receipt.clone().ok_or(BackendError::Unavailable)?;
        Ok(McpReceipt { control: entry.connection.id, response })
    }

    fn entry(&self, run: RunHandle) -> Result<Arc<Entry>, BackendError> {
        self.runs.lock().map_err(|_| BackendError::Unavailable)?.get(&run.0)
            .cloned().ok_or(BackendError::NotFound)
    }

    fn prune_reservations(&self) -> Result<(), BackendError> {
        self.runs.lock().map_err(|_| BackendError::Unavailable)?.retain(|_, entry| {
            if entry.connection.is_closed() { return false; }
            match entry.state.try_lock() {
                Ok(state) => retain_state(&state, Instant::now()),
                Err(_) => true,
            }
        });
        Ok(())
    }

    /// Explicit lifecycle seam: callers release retained terminal records after
    /// draining I/O and revoking D's capability. Live runs cannot be forgotten.
    pub async fn forget_finished(&self, run: RunHandle) -> Result<(), BackendError> {
        let entry = self.entry(run)?;
        if entry.state.lock().await.outcome.is_none() { return Err(BackendError::Denied); }
        self.runs.lock().map_err(|_| BackendError::Unavailable)?.remove(&run.0);
        Ok(())
    }

    /// Bind gateway-verified task context before StartRun. Package D supplies this
    /// value from its immutable request context, never from employee arguments.
    pub async fn bind_task(&self, run: RunHandle, task: Option<Text<128>>) -> Result<(), BackendError> {
        let entry = self.entry(run)?;
        let mut state = entry.state.lock().await;
        bind_task(&mut state, task)
    }

    /// Public queue status contains no PID, path, command or secret.
    pub async fn status(&self, run: RunHandle) -> Result<RunStatus, BackendError> {
        let entry = self.entry(run)?;
        let Packet { response, .. } = entry.connection.request(SpawnRequest::Status { run }, &[]).await?;
        match response {
            SpawnResponse::Status { status, .. } => {
                if let RunStatus::Ended(ref outcome) = status {
                    let mut state = entry.state.lock().await;
                    state.outcome = Some(outcome.clone());
                    state.terminal_at.get_or_insert_with(Instant::now);
                    state.mcp_fd.take();
                }
                Ok(status)
            }
            _ => Err(BackendError::Protocol),
        }
    }

    async fn receive_io(entry: &Entry, run: RunHandle, state: &mut State) -> Result<(), BackendError> {
        if state.io_received { return Ok(()); }
        if !state.started || state.outcome.is_some() { return Err(BackendError::Denied); }
        let packet = entry.connection.request(SpawnRequest::TakeIo { run }, &[]).await?;
        let SpawnResponse::Io { delivery, .. } = &packet.response else { return Err(BackendError::Protocol); };
        if delivery.pty != state.pty || delivery.mcp != state.mcp {
            entry.connection.close().await;
            return Err(BackendError::Protocol);
        }
        let mut fds = packet.fds.into_iter();
        let mcp_fd = if delivery.mcp { fds.next() } else { None };
        let io = match fd_io::make_io(delivery.pty, fds.collect(), state.output_bytes, state.output_buffer) {
            Ok(io) => io,
            Err(error) => { entry.connection.close().await; return Err(error); }
        };
        match io {
            fd_io::DeliveredIo::Stdio(stdio) => state.stdio = Some(stdio),
            fd_io::DeliveredIo::Pty(terminal) => state.terminal = Some(terminal),
        }
        state.mcp_fd = mcp_fd;
        state.io_receipt = Some(packet.response);
        state.io_received = true;
        Ok(())
    }

    /// Package D must prepare the pending record between reserve and spawn, then
    /// bind/activate it on this exact socket after verifying live run status.
    /// This endpoint is transferred once. An unready queued run returns Unavailable
    /// and can be retried; no capability is exposed while it remains queued.
    pub async fn take_mcp(&self, run: RunHandle) -> Result<OwnedFd, BackendError> {
        let entry = self.entry(run)?;
        let mut state = entry.state.lock().await;
        if !state.mcp { return Err(BackendError::Denied); }
        Self::receive_io(&entry, run, &mut state).await?;
        state.mcp_fd.take().ok_or(BackendError::Denied)
    }
}

impl ExecutionBackend for SpawndBackend {
    fn reserve(&self, employee: EmployeeId) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            self.check_admission()?;
            self.prune_reservations()?;
            let permit = self.slots.clone().try_acquire_owned().map_err(|_| BackendError::Unavailable)?;
            let connection = transport::connect()?;
            let packet = connection.request(SpawnRequest::ReserveRun { employee_id: employee }, &[]).await?;
            if let Err(error) = self.check_admission() {
                connection.close().await;
                return Err(error);
            }
            let SpawnResponse::Reserved { run, expires_at_unix_ms } = packet.response else { return Err(BackendError::Protocol); };
            let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| BackendError::Unavailable)?.as_millis();
            let remaining = (expires_at_unix_ms as u128).checked_sub(now_ms)
                .filter(|ms| *ms > 0 && *ms <= ipc::RESERVATION_TIMEOUT_MS as u128)
                .ok_or(BackendError::Protocol)?;
            let entry = Arc::new(Entry { employee, reservation_expiry: expires_at_unix_ms, connection, _permit: permit, state: Mutex::new(State {
                expires: Instant::now() + Duration::from_millis(remaining as u64), started: false, task_ref: None,
                pty: false, mcp: false, output_bytes: 0, output_buffer: 0,
                io_received: false, stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None,
            }) });
            let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
            if runs.contains_key(&run.0)
                || runs.values().filter(|entry| entry.employee == employee
                    && entry.state.try_lock().map_or(true, |state| state.outcome.is_none())).count()
                    >= ipc::DEFAULT_QUEUE_LENGTH as usize {
                return Err(BackendError::Denied);
            }
            runs.insert(run.0, entry);
            Ok(run)
        })
    }

    fn spawn(&self, policy: SpawnPolicy) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            self.check_admission()?;
            let entry = self.entry(policy.run)?;
            let mut state = entry.state.lock().await;
            if entry.employee != policy.employee || state.started || state.expires <= Instant::now() {
                return Err(BackendError::Denied);
            }
            if policy.resources.output_total_bytes == 0 || policy.resources.output_buffer_bytes == 0
                || policy.resources.memfd_total_bytes == 0 {
                return Err(BackendError::Denied);
            }
            // Preparation cannot outlive the server reservation or hold a local
            // reservation indefinitely through an adversarial input stream.
            let budget = policy.resources.memfd_total_bytes.min(MAX_MEMFD_TOTAL_BYTES);
            let prepared = tokio::time::timeout_at(state.expires, prepare_inputs(policy.inputs, budget))
                .await.map_err(|_| BackendError::Timeout)??;
            let request = SpawnRequest::StartRun { run: policy.run, employee_id: policy.employee,
                runtime_request: policy.runtime, task_ref: state.task_ref.clone(), prompt_bytes: prepared.prompt_bytes,
                input_fds: List::new(prepared.attachments).map_err(|_| BackendError::Denied)?, pty: policy.pty };
            request.validate().map_err(|_| BackendError::Denied)?;
            // Input preparation awaits untrusted bounded streams. Recheck at
            // the final boundary before handing execution to the root service.
            if let Err(error) = self.check_admission() {
                entry.connection.close().await;
                return Err(error);
            }
            let packet = entry.connection.request(request, &prepared.fds).await?;
            let SpawnResponse::Accepted { status, .. } = packet.response else { return Err(BackendError::Protocol); };
            if let Err(error) = self.check_admission() {
                entry.connection.close().await;
                return Err(error);
            }
            state.started = true;
            state.pty = policy.pty;
            state.mcp = policy.capabilities.mcp_bridge;
            state.output_bytes = policy.resources.output_total_bytes;
            state.output_buffer = policy.resources.output_buffer_bytes;
            if let RunStatus::Ended(outcome) = status {
                state.outcome = Some(outcome);
                state.terminal_at = Some(Instant::now());
            }
            Ok(policy.run)
        })
    }

    fn wait(&self, run: RunHandle, origin: WaitOrigin) -> BackendFuture<'_, RunOutcome> {
        Box::pin(async move {
            let entry = self.entry(run)?;
            if let WaitOrigin::Run(caller) = origin {
                let caller = self.entry(caller)?;
                if caller.employee == entry.employee { return Err(BackendError::WouldDeadlock); }
            }
            loop {
                if let Some(outcome) = entry.state.lock().await.outcome.clone() { return Ok(outcome); }
                // Status polling releases the transaction lock between requests;
                // Cancel never waits behind a server-side blocking Wait call.
                if let RunStatus::Ended(outcome) = self.status(run).await? { return Ok(outcome); }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }

    fn kill(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let entry = self.entry(run)?;
            entry.connection.request(SpawnRequest::Cancel { run }, &[]).await?;
            // Ack requests termination; only Status::Ended proves completion.
            entry.state.lock().await.mcp_fd.take();
            Ok(())
        })
    }

    fn take_stdio(&self, run: RunHandle) -> BackendFuture<'_, RunStdio> {
        Box::pin(async move {
            let entry = self.entry(run)?;
            let mut state = entry.state.lock().await;
            if state.pty { return Err(BackendError::Denied); }
            Self::receive_io(&entry, run, &mut state).await?;
            state.stdio.take().ok_or(BackendError::Denied)
        })
    }

    fn take_pty(&self, run: RunHandle) -> BackendFuture<'_, RunPty> {
        Box::pin(async move {
            let entry = self.entry(run)?;
            let mut state = entry.state.lock().await;
            if !state.pty { return Err(BackendError::Denied); }
            Self::receive_io(&entry, run, &mut state).await?;
            state.terminal.take().ok_or(BackendError::Denied)
        })
    }

    fn resize(&self, run: RunHandle, size: PtySize) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            size.validate().map_err(|_| BackendError::Denied)?;
            let entry = self.entry(run)?;
            if !entry.state.lock().await.pty { return Err(BackendError::Denied); }
            entry.connection.request(SpawnRequest::Resize { run, size }, &[]).await?;
            Ok(())
        })
    }
}

fn retain_state(state: &State, now: Instant) -> bool {
    if let Some(ended) = state.terminal_at { return now.duration_since(ended) < TERMINAL_RETENTION; }
    state.started || state.expires > now
}

fn bind_task(state: &mut State, task: Option<Text<128>>) -> Result<(), BackendError> {
    if state.started || state.expires <= Instant::now() { return Err(BackendError::Denied); }
    state.task_ref = task;
    Ok(())
}

struct PreparedInputs { prompt_bytes: u64, attachments: Vec<ipc::InputFd>, fds: Vec<OwnedFd> }

async fn prepare_inputs(inputs: RunInputs, budget: u64) -> Result<PreparedInputs, BackendError> {
    if inputs.attachments.len() > ipc::MAX_INPUT_FDS { return Err(BackendError::Denied); }
    let (prompt, prompt_bytes) = copy_memfd(inputs.prompt, ipc::MAX_PROMPT_BYTES.min(budget)).await?;
    let mut prepared = PreparedInputs { prompt_bytes, attachments: Vec::new(), fds: vec![prompt] };
    let mut remaining = budget.checked_sub(prompt_bytes).ok_or(BackendError::Denied)?;
    for input in inputs.attachments {
        // A trusted gateway snapshot provider is required for larger attachments.
        // Never substitute an unsealed writable file or silently truncate input.
        if input.max_bytes > MAX_MEMFD_BYTES { return Err(BackendError::Unavailable); }
        let (fd, bytes) = copy_memfd(input, remaining.min(MAX_MEMFD_BYTES)).await?;
        remaining = remaining.checked_sub(bytes).ok_or(BackendError::Denied)?;
        prepared.attachments.push(ipc::InputFd::SealedMemfd { bytes });
        prepared.fds.push(fd);
    }
    Ok(prepared)
}

async fn copy_memfd(mut input: RunInput, cap: u64) -> Result<(OwnedFd, u64), BackendError> {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use nix::{fcntl::{fcntl, FcntlArg, SealFlag}, sys::memfd::{memfd_create, MemFdCreateFlag}};
    if input.max_bytes > cap { return Err(BackendError::Denied); }
    let fd = memfd_create(c"duduclaw-input", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING)
        .map_err(|_| BackendError::Io)?;
    let mut file = std::fs::File::from(fd);
    let mut buffer = [0u8; 16 * 1024];
    let mut bytes = 0u64;
    loop {
        // Read one byte past the declared limit to distinguish exact EOF from
        // overflow without allocating or accepting truncated prompts.
        let read_cap = input.max_bytes.saturating_sub(bytes).saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let read = input.reader.read(&mut buffer[..read_cap]).await?;
        if read > read_cap { return Err(BackendError::Protocol); }
        if read == 0 { break; }
        bytes = bytes.checked_add(read as u64).ok_or(BackendError::Denied)?;
        if bytes > input.max_bytes { return Err(BackendError::Denied); }
        file.write_all(&buffer[..read]).map_err(|_| BackendError::Io)?;
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| BackendError::Io)?;
    let seals = SealFlag::F_SEAL_SEAL | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_WRITE;
    fcntl(file.as_raw_fd(), FcntlArg::F_ADD_SEALS(seals)).map_err(|_| BackendError::Io)?;
    let actual = fcntl(file.as_raw_fd(), FcntlArg::F_GET_SEALS).map_err(|_| BackendError::Io)?;
    if !SealFlag::from_bits_retain(actual).contains(seals) { return Err(BackendError::Io); }
    Ok((file.into(), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;

    struct Bytes(std::io::Cursor<Vec<u8>>);
    impl RunReader for Bytes {
        fn read<'a>(&'a mut self, bytes: &'a mut [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async move { Read::read(&mut self.0, bytes).map_err(|_| BackendError::Io) })
        }
    }
    fn input(bytes: &[u8], max_bytes: u64) -> RunInput {
        RunInput { reader: Box::new(Bytes(std::io::Cursor::new(bytes.to_vec()))), max_bytes }
    }

    async fn metadata_fixture() -> (SpawndBackend, RunHandle, Arc<Entry>, OwnedFd) {
        let slots = Arc::new(Semaphore::new(1));
        let (connection, peer) = Connection::test_pair();
        let run = RunHandle(RunId([19; 16]));
        let entry = Arc::new(Entry { employee: EmployeeId([23; 16]), reservation_expiry: 123456,
            connection, _permit: slots.clone().acquire_owned().await.unwrap(),
            state: Mutex::new(State {
                expires: Instant::now() + Duration::from_secs(30), started: false, task_ref: None,
                pty: false, mcp: false, output_bytes: 1, output_buffer: 1, io_received: false,
                stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None,
            }) });
        let backend = SpawndBackend { runs: StdMutex::new(BTreeMap::from([(run.0, entry.clone())])), slots, admission: None };
        (backend, run, entry, peer)
    }

    #[tokio::test]
    async fn retained_backend_checks_admission_before_connecting_to_root() {
        let (mut backend, _, _, _peer) = metadata_fixture().await;
        backend.admission = Some(|| Err(BackendError::Denied));
        assert!(matches!(backend.reserve(EmployeeId([23; 16])).await, Err(BackendError::Denied)));
    }

    #[tokio::test]
    async fn live_snapshot_reads_each_root_status_and_preserves_control_identity() {
        let (backend, run, entry, peer) = metadata_fixture().await;
        let thread = std::thread::spawn(move || {
            use nix::sys::socket::{self, MsgFlags};
            for status in [RunStatus::Running, RunStatus::Ended(RunOutcome::Exited(0))] {
                let mut pollfd = libc::pollfd { fd: peer.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one live pollfd with its exact array length.
                assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 2000) }, 1);
                let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
                let size = socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT).unwrap();
                bytes.truncate(size);
                let request: ipc::Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
                assert!(matches!(request.message, SpawnRequest::Status { run: actual } if actual == run));
                let frame = ipc::encode(&ipc::Envelope { version: ipc::PROTOCOL_VERSION,
                    request_id: request.request_id, message: SpawnResponse::Status { run, status } }).unwrap();
                socket::send(peer.as_raw_fd(), &frame, MsgFlags::MSG_NOSIGNAL).unwrap();
            }
        });
        // The authority path must never acquire this async state mutex.
        let _state = entry.state.lock().await;
        for expected_running in [true, false] {
            let (metadata, status) = backend.live_snapshot(run).unwrap();
            assert_eq!(metadata.employee, entry.employee);
            assert_eq!(metadata.control, entry.connection.id);
            assert_eq!(metadata.expires_at_unix_ms, entry.reservation_expiry);
            assert_eq!(matches!(status, RunStatus::Running), expected_running);
        }
        thread.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_snapshot_serializes_with_wait_without_revoking_a_live_run() {
        let (backend, run, _, peer) = metadata_fixture().await;
        let backend = Arc::new(backend);
        let (started, received) = tokio::sync::oneshot::channel();
        let peer_thread = std::thread::spawn(move || {
            use nix::sys::socket::{self, MsgFlags};
            let mut started = Some(started);
            // Wait and the synchronous authority probe each receive their own
            // root response; the third poll really observes termination.
            for index in 0..3 {
                let mut pollfd = libc::pollfd { fd: peer.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: the kernel receives exactly one valid pollfd.
                assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 2000) }, 1);
                let mut bytes = vec![0; ipc::MAX_FRAME_BYTES + 4];
                let size = socket::recv(peer.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT).unwrap();
                bytes.truncate(size);
                let request: ipc::Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
                assert!(matches!(request.message, SpawnRequest::Status { run: actual } if actual == run));
                if let Some(started) = started.take() {
                    started.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(30));
                }
                let status = if index == 2 { RunStatus::Ended(RunOutcome::Exited(0)) } else { RunStatus::Running };
                let frame = ipc::encode(&ipc::Envelope { version: ipc::PROTOCOL_VERSION,
                    request_id: request.request_id, message: SpawnResponse::Status { run, status } }).unwrap();
                socket::send(peer.as_raw_fd(), &frame, MsgFlags::MSG_NOSIGNAL).unwrap();
            }
        });
        let waiter = backend.clone();
        let wait = tokio::spawn(async move { waiter.wait(run, WaitOrigin::Gateway).await });
        received.await.unwrap();
        // This deliberately blocks the only Tokio worker while wait's first
        // Status is still in flight. Its blocking I/O owner must progress.
        let (metadata, status) = backend.live_snapshot(run).unwrap();
        assert_eq!(metadata.run, run);
        assert!(matches!(status, RunStatus::Running));
        assert!(!backend.entry(run).unwrap().connection.is_closed());
        assert_eq!(wait.await.unwrap().unwrap(), RunOutcome::Exited(0));
        peer_thread.join().unwrap();
    }

    #[tokio::test]
    async fn reservation_metadata_preserves_authenticated_identity_and_refuses_started_run() {
        let (backend, run, entry, _peer) = metadata_fixture().await;
        let metadata = backend.reservation(run).await.unwrap();
        assert_eq!(metadata.run, run);
        assert_eq!(metadata.employee, EmployeeId([23; 16]));
        assert_eq!(metadata.control, ipc::ControlConnectionId([17; 16]));
        assert_eq!(metadata.expires_at_unix_ms, 123456);
        entry.state.lock().await.started = true;
        assert!(matches!(backend.reservation(run).await, Err(BackendError::Denied)));
    }

    #[tokio::test]
    async fn reservation_metadata_refuses_expired_and_disconnected_control() {
        let (backend, run, entry, _peer) = metadata_fixture().await;
        entry.state.lock().await.expires = Instant::now();
        assert!(matches!(backend.reservation(run).await, Err(BackendError::Denied)));
        entry.state.lock().await.expires = Instant::now() + Duration::from_secs(30);
        entry.connection.close().await;
        assert!(matches!(backend.reservation(run).await, Err(BackendError::Denied)));
    }

    #[tokio::test]
    async fn mcp_receipt_requires_actual_delivery_and_live_control() {
        let (backend, run, entry, _peer) = metadata_fixture().await;
        assert!(backend.mcp_receipt(run).await.is_err());
        entry.state.lock().await.mcp = true;
        assert!(matches!(backend.mcp_receipt(run).await, Err(BackendError::Unavailable)));
        // The real receive_io path stores the validated Io packet verbatim.
        let response = SpawnResponse::Io { run, delivery: ipc::IoDelivery { pty: false, mcp: true } };
        entry.state.lock().await.io_receipt = Some(response.clone());
        let receipt = backend.mcp_receipt(run).await.unwrap();
        assert_eq!(receipt.control, ipc::ControlConnectionId([17; 16]));
        assert_eq!(receipt.response, response);
        entry.connection.close().await;
        assert!(matches!(backend.mcp_receipt(run).await, Err(BackendError::Denied)));
    }

    #[test]
    fn task_binding_is_immutable_after_start_or_expiry() {
        let mut state = State {
            expires: Instant::now() + Duration::from_secs(30), started: false, task_ref: None,
            pty: false, mcp: false, output_bytes: 1, output_buffer: 1,
            io_received: false, stdio: None, terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None,
        };
        let task = Some(Text::new("task-1".into()).unwrap());
        bind_task(&mut state, task.clone()).unwrap();
        state.started = true;
        assert!(bind_task(&mut state, None).is_err());
        assert_eq!(state.task_ref, task);
        state.started = false;
        state.expires = Instant::now();
        assert!(bind_task(&mut state, None).is_err());
        assert_eq!(state.task_ref, task);
    }

    #[test]
    fn terminal_records_expire_without_an_extra_trait_method() {
        let now = Instant::now();
        let mut state = State {
            expires: now, started: true, task_ref: None, pty: false, mcp: false,
            output_bytes: 1, output_buffer: 1, io_received: false, stdio: None,
            terminal: None, mcp_fd: None, io_receipt: None, outcome: None, terminal_at: None,
        };
        assert!(retain_state(&state, now + TERMINAL_RETENTION));
        state.outcome = Some(RunOutcome::Exited(0));
        state.terminal_at = Some(now);
        assert!(retain_state(&state, now));
        assert!(!retain_state(&state, now + TERMINAL_RETENTION));
    }

    #[tokio::test]
    async fn memfd_is_sealed_rewound_and_overflow_is_rejected() {
        let (fd, size) = copy_memfd(input(b"hello", 5), 5).await.unwrap();
        assert_eq!(size, 5);
        assert!(nix::unistd::write(&fd, b"replace").is_err());
        let mut content = Vec::new();
        std::fs::File::from(fd).read_to_end(&mut content).unwrap();
        assert_eq!(content, b"hello");
        assert!(matches!(copy_memfd(input(b"longer", 5), 5).await, Err(BackendError::Denied)));
        assert!(matches!(copy_memfd(input(b"", 6), 5).await, Err(BackendError::Denied)));
        let (empty, size) = copy_memfd(input(b"", 0), 0).await.unwrap();
        assert_eq!(size, 0);
        assert!(nix::unistd::ftruncate(&empty, 1).is_err());
        assert!(nix::fcntl::fcntl(empty.as_raw_fd(), nix::fcntl::FcntlArg::F_GET_SEALS).unwrap() > 0);
    }

    #[tokio::test]
    async fn aggregate_input_budget_and_large_attachment_seam_fail_closed() {
        let inputs = RunInputs { prompt: input(b"123", 3), attachments: vec![input(b"456", 3)] };
        assert!(matches!(prepare_inputs(inputs, 5).await, Err(BackendError::Denied)));
        let inputs = RunInputs { prompt: input(b"", 0), attachments: vec![input(b"", MAX_MEMFD_BYTES + 1)] };
        assert!(matches!(prepare_inputs(inputs, MAX_MEMFD_TOTAL_BYTES).await, Err(BackendError::Unavailable)));
        let inputs = RunInputs { prompt: input(b"p", 1), attachments: vec![input(b"a", 1)] };
        let prepared = prepare_inputs(inputs, 2).await.unwrap();
        assert_eq!(prepared.prompt_bytes, 1);
        assert_eq!(prepared.fds.len(), 2);
        assert_eq!(prepared.attachments, vec![ipc::InputFd::SealedMemfd { bytes: 1 }]);
    }
}
