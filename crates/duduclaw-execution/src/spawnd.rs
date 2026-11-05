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
}

struct Entry {
    employee: EmployeeId,
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
    outcome: Option<RunOutcome>,
    terminal_at: Option<Instant>,
}

impl SpawndBackend {
    /// Checks the root-owned endpoint and peer before advertising availability.
    /// Package B performs readiness checks for the trampoline, egress and quotas.
    pub async fn connect() -> Result<Self, BackendError> {
        let _probe = transport::connect()?;
        Ok(Self { runs: StdMutex::new(BTreeMap::new()), slots: Arc::new(Semaphore::new(MAX_TRACKED_RUNS)) })
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
        let SpawnResponse::Io { delivery, .. } = packet.response else { return Err(BackendError::Protocol); };
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
            self.prune_reservations()?;
            let permit = self.slots.clone().try_acquire_owned().map_err(|_| BackendError::Unavailable)?;
            let connection = transport::connect()?;
            let packet = connection.request(SpawnRequest::ReserveRun { employee_id: employee }, &[]).await?;
            let SpawnResponse::Reserved { run, expires_at_unix_ms } = packet.response else { return Err(BackendError::Protocol); };
            let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| BackendError::Unavailable)?.as_millis();
            let remaining = (expires_at_unix_ms as u128).checked_sub(now_ms)
                .filter(|ms| *ms > 0 && *ms <= ipc::RESERVATION_TIMEOUT_MS as u128)
                .ok_or(BackendError::Protocol)?;
            let entry = Arc::new(Entry { employee, connection, _permit: permit, state: Mutex::new(State {
                expires: Instant::now() + Duration::from_millis(remaining as u64), started: false, task_ref: None,
                pty: false, mcp: false, output_bytes: 0, output_buffer: 0,
                io_received: false, stdio: None, terminal: None, mcp_fd: None, outcome: None, terminal_at: None,
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
            let packet = entry.connection.request(request, &prepared.fds).await?;
            let SpawnResponse::Accepted { status, .. } = packet.response else { return Err(BackendError::Protocol); };
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

    #[test]
    fn task_binding_is_immutable_after_start_or_expiry() {
        let mut state = State {
            expires: Instant::now() + Duration::from_secs(30), started: false, task_ref: None,
            pty: false, mcp: false, output_bytes: 1, output_buffer: 1,
            io_received: false, stdio: None, terminal: None, mcp_fd: None, outcome: None, terminal_at: None,
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
            terminal: None, mcp_fd: None, outcome: None, terminal_at: None,
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
