//! Compatibility backend for installations without employee OS isolation.
//!
//! A native command can be registered before consuming its reservation. This
//! retains non-UTF-8 arguments, environment removals, stdio and pre-exec hooks;
//! reconstructing these from a UTF-8 policy would change legacy behavior.
use std::{collections::BTreeMap, path::{Component, PathBuf}, process::Stdio,
    sync::{Arc, Mutex, atomic::{AtomicU64, AtomicBool, Ordering}}, time::{Duration, Instant}};
use duduclaw_isolation::{execution::*, ids::{EmployeeId, RunId}, ipc};
use portable_pty::{CommandBuilder, MasterPty};
use tokio::{process::Command, sync::{mpsc, oneshot, watch}};
use crate::io::{NativeWriter, NativeIoErrors, NativeIoError, read_input};
#[cfg(test)]
use crate::io::Reader;

const MAX_LOCAL_RUNS: usize = 1024;
const LOGIN_SESSION_ENV: &[&str] = &[
    "DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS", "SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_AGENT_INFO",
];
const TERMINAL_RETENTION: Duration = Duration::from_secs(300);
/// The PTY master. `None` once the Windows supervisor has closed the
/// pseudoconsole after the child exited (see `start_pty`); Unix never clears it.
type Master = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;
/// Windows only: how long the supervisor waits for the ConPTY output pipe to
/// reach EOF after it closed the pseudoconsole, before it reports the outcome
/// anyway. Closing should complete promptly because the reader thread keeps
/// draining; this bound only guarantees `wait`/`kill` can never hang on it.
#[cfg(windows)]
const CONPTY_CLOSE_GRACE: Duration = Duration::from_secs(5);

struct Entry {
    employee: EmployeeId,
    reserved_until: Option<Instant>,
    command: Option<Command>,
    pty_command: Option<(CommandBuilder, PtySize)>,
    outcome: watch::Receiver<Option<RunOutcome>>,
    completion: Option<watch::Sender<Option<RunOutcome>>>,
    cancel: Option<mpsc::Sender<RunOutcome>>,
    stdio: Option<RunStdio>,
    pty: Option<RunPty>,
    master: Option<Master>,
    terminal_at: Option<Instant>,
    spawn_error: Option<std::io::Error>,
    pid: Option<u32>,
    io_errors: NativeIoErrors,
}

/// Local execution never constitutes protected-mode readiness.
#[derive(Default)]
pub struct LocalBackend { runs: Mutex<BTreeMap<RunId, Entry>> }

// Preparing a native builder consumes its reservation even if the future is
// cancelled while reading input. Never reconstruct a lost builder on retry.
struct Preparing<'a> { backend: &'a LocalBackend, run: RunHandle }
impl Drop for Preparing<'_> {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.backend.runs.lock() {
            if let Some(entry) = runs.get_mut(&self.run.0) {
                if let Some(done) = entry.completion.take() {
                    done.send_replace(Some(RunOutcome::Rejected));
                    entry.terminal_at = Some(Instant::now());
                }
            }
        }
    }
}

impl Drop for LocalBackend {
    fn drop(&mut self) {
        if let Ok(runs) = self.runs.get_mut() {
            for entry in runs.values() {
                if let Some(cancel) = &entry.cancel {
                    let _ = cancel.try_send(RunOutcome::Cancelled);
                }
            }
        }
    }
}

impl LocalBackend {
    pub fn new() -> Self { Self::default() }

    /// Keep the exact command built by a legacy caller. It must already configure
    /// its stdio; absent pipes are represented by closed input/empty output.
    pub fn register_command(&self, run: RunHandle, command: Command) -> Result<(), BackendError> {
        let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
        check_reservation(entry)?;
        if entry.command.is_some() || entry.pty_command.is_some() { return Err(BackendError::Denied); }
        entry.command = Some(command);
        Ok(())
    }

    /// PTY equivalent of register_command; preserves the native builder and size.
    pub fn register_pty(&self, run: RunHandle, command: CommandBuilder, size: PtySize)
        -> Result<(), BackendError>
    {
        size.validate().map_err(|_| BackendError::Denied)?;
        let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
        check_reservation(entry)?;
        if entry.command.is_some() || entry.pty_command.is_some() { return Err(BackendError::Denied); }
        entry.pty_command = Some((command, size));
        Ok(())
    }

    pub fn registered_snapshot(&self, run: RunHandle) -> Result<crate::command::NativeCommandSnapshot, BackendError> {
        let runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let command = runs.get(&run.0).and_then(|entry| entry.command.as_ref()).ok_or(BackendError::NotFound)?;
        Ok(crate::command::command_snapshot(command))
    }

    pub fn registered_pty_snapshot(&self, run: RunHandle) -> Result<crate::command::NativeCommandSnapshot, BackendError> {
        let runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let (command, _) = runs.get(&run.0).and_then(|entry| entry.pty_command.as_ref()).ok_or(BackendError::NotFound)?;
        Ok(crate::command::pty_snapshot(command))
    }

    pub fn native_io_errors(&self, run: RunHandle) -> Option<NativeIoErrors> {
        self.runs.lock().ok()?.get(&run.0).map(|entry| entry.io_errors.clone())
    }

    pub fn local_pid(&self, run: RunHandle) -> Option<u32> {
        self.runs.lock().ok()?.get(&run.0)?.pid
    }

    pub fn take_spawn_error(&self, run: RunHandle) -> Option<std::io::Error> {
        self.runs.lock().ok()?.get_mut(&run.0)?.spawn_error.take()
    }

    /// Release a completed handle after the caller has drained its output.
    pub fn forget(&self, run: RunHandle) -> Result<(), BackendError> {
        let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let entry = runs.get(&run.0).ok_or(BackendError::NotFound)?;
        if entry.outcome.borrow().is_none() { return Err(BackendError::Denied); }
        runs.remove(&run.0);
        Ok(())
    }
}

fn scrub_login_command(command: &mut Command, class: ExecutionClass) {
    if class == ExecutionClass::InteractiveLogin {
        for key in LOGIN_SESSION_ENV { command.env_remove(key); }
    }
}

fn scrub_login_pty(command: &mut CommandBuilder, class: ExecutionClass) {
    if class == ExecutionClass::InteractiveLogin {
        for key in LOGIN_SESSION_ENV { command.env_remove(key); }
    }
}

fn check_reservation(entry: &Entry) -> Result<(), BackendError> {
    match entry.reserved_until {
        Some(deadline) if Instant::now() < deadline => Ok(()),
        _ => Err(BackendError::Denied),
    }
}

fn local_cwd(policy: &SpawnPolicy) -> Result<PathBuf, BackendError> {
    let root = PathBuf::from(policy.workspace.workspace.as_str());
    let relative = PathBuf::from(policy.workspace.relative_cwd.as_str());
    if !root.is_absolute() || relative.components().any(|part|
        matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err(BackendError::Denied);
    }
    Ok(root.join(relative))
}

fn policy_command(policy: &SpawnPolicy) -> Result<Command, BackendError> {
    let program = policy.argv_allowlist.first().ok_or(BackendError::Denied)?.as_str();
    if program.is_empty() { return Err(BackendError::Denied); }
    let mut command = Command::new(program);
    command.args(policy.argv_allowlist.iter().skip(1).map(|arg| arg.as_str()));
    command.env_clear();
    command.envs(policy.environment_allowlist.iter().map(|(key, value)| (key, value.as_str())));
    command.current_dir(local_cwd(policy)?);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(command)
}

fn policy_pty(policy: &SpawnPolicy) -> Result<(CommandBuilder, PtySize), BackendError> {
    let program = policy.argv_allowlist.first().ok_or(BackendError::Denied)?.as_str();
    if program.is_empty() { return Err(BackendError::Denied); }
    let mut command = CommandBuilder::new(program);
    command.args(policy.argv_allowlist.iter().skip(1).map(|arg| arg.as_str()));
    command.env_clear();
    for (key, value) in &policy.environment_allowlist { command.env(key, value.as_str()); }
    command.cwd(local_cwd(policy)?);
    Ok((command, PtySize { rows: 24, columns: 200 }))
}

impl ExecutionBackend for LocalBackend {
    fn reserve(&self, employee: EmployeeId) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            let now = Instant::now();
            let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
            runs.retain(|_, entry| {
                if entry.reserved_until.is_some_and(|deadline| deadline <= now) { return false; }
                if entry.outcome.borrow().is_some() {
                    let finished = entry.terminal_at.get_or_insert(now);
                    return now.duration_since(*finished) < TERMINAL_RETENTION;
                }
                true
            });
            if runs.len() >= MAX_LOCAL_RUNS { return Err(BackendError::Unavailable); }
            let run = RunHandle(RunId(*uuid::Uuid::new_v4().as_bytes()));
            let (completion, outcome) = watch::channel(None);
            runs.insert(run.0, Entry { employee,
                reserved_until: Some(now + Duration::from_millis(ipc::RESERVATION_TIMEOUT_MS)),
                command: None, pty_command: None, outcome, completion: Some(completion),
                cancel: None, stdio: None, pty: None, master: None, terminal_at: None, spawn_error: None, pid: None, io_errors: NativeIoErrors::default() });
            Ok(run)
        })
    }

    fn spawn(&self, policy: SpawnPolicy) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            if !policy.inputs.attachments.is_empty() || policy.capabilities.mcp_bridge
                || policy.capabilities.egress_forwarder {
                // Native command callers already own their legacy MCP wiring.
                // Capability handover and attachment-fd delivery require spawnd.
                return Err(BackendError::Denied);
            }
            let run = policy.run;
            // Validate/create the command before awaiting input, while the policy
            // is intact. Native registration takes precedence over UTF-8 fields.
            let (command, pty_command, deadline) = {
                let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
                let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
                check_reservation(entry)?;
                if entry.employee != policy.employee { return Err(BackendError::Denied); }
                let deadline = entry.reserved_until.ok_or(BackendError::Denied)?;
                let builders = if policy.pty {
                    if entry.command.is_some() { return Err(BackendError::Denied); }
                    let (mut command, size) = match entry.pty_command.take() {
                        Some(command) => command,
                        None => policy_pty(&policy)?,
                    };
                    scrub_login_pty(&mut command, policy.class);
                    (None, Some((command, size)))
                } else {
                    if entry.pty_command.is_some() { return Err(BackendError::Denied); }
                    let mut command = match entry.command.take() {
                        Some(command) => command,
                        None => policy_command(&policy)?,
                    };
                    scrub_login_command(&mut command, policy.class);
                    (Some(command), None)
                };
                entry.reserved_until = None;
                (builders.0, builders.1, deadline)
            };
            let _preparing = Preparing { backend: self, run };
            let input = read_input(policy.inputs.prompt, ipc::MAX_PROMPT_BYTES);
            let prompt = if policy.timeouts.startup_ms == 0 {
                input.await?
            } else {
                tokio::time::timeout(Duration::from_millis(policy.timeouts.startup_ms), input)
                    .await.map_err(|_| BackendError::Timeout)??
            };
            let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
            let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
            if entry.outcome.borrow().is_some() || Instant::now() >= deadline {
                return Err(BackendError::Denied);
            }
            let completion = entry.completion.take().ok_or(BackendError::Denied)?;
            let result = if let Some(command) = command {
                start_pipe(entry, command, prompt, completion.clone(), policy.timeouts.runtime_ms, policy.resources.output_total_bytes)
            } else if let Some((command, size)) = pty_command {
                start_pty(entry, command, size, prompt, completion.clone(), policy.timeouts.runtime_ms, policy.resources.output_total_bytes)
            } else { Err(BackendError::Protocol) };
            if let Err(error) = result {
                completion.send_replace(Some(RunOutcome::Rejected));
                return Err(error);
            }
            Ok(run)
        })
    }

    fn wait(&self, run: RunHandle, origin: WaitOrigin) -> BackendFuture<'_, RunOutcome> {
        Box::pin(async move {
            let mut outcome = {
                let runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
                let target = runs.get(&run.0).ok_or(BackendError::NotFound)?;
                if let WaitOrigin::Run(caller) = origin {
                    let source = runs.get(&caller.0).ok_or(BackendError::NotFound)?;
                    if source.employee == target.employee && source.outcome.borrow().is_none() {
                        return Err(BackendError::WouldDeadlock);
                    }
                }
                if target.reserved_until.is_some() { return Err(BackendError::Denied); }
                target.outcome.clone()
            };
            loop {
                if let Some(value) = outcome.borrow_and_update().clone() { return Ok(value); }
                outcome.changed().await.map_err(|_| BackendError::Unavailable)?;
            }
        })
    }

    fn kill(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let (cancel, mut outcome) = {
                let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
                let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
                if entry.outcome.borrow().is_some() { return Ok(()); }
                if entry.completion.is_some() {
                    entry.reserved_until = None;
                    entry.command = None;
                    entry.pty_command = None;
                    if let Some(done) = entry.completion.take() {
                        done.send_replace(Some(RunOutcome::Cancelled));
                    }
                    return Ok(());
                }
                (entry.cancel.clone().ok_or(BackendError::Unavailable)?, entry.outcome.clone())
            };
            // Observe completion even when the child exits concurrently with a
            // cancel send; a completed cancellation is idempotently successful.
            let _ = cancel.send(RunOutcome::Cancelled).await;
            loop {
                if outcome.borrow_and_update().is_some() { return Ok(()); }
                outcome.changed().await.map_err(|_| BackendError::Unavailable)?;
            }
        })
    }

    fn take_stdio(&self, run: RunHandle) -> BackendFuture<'_, RunStdio> {
        Box::pin(async move {
            self.runs.lock().map_err(|_| BackendError::Unavailable)?
                .get_mut(&run.0).ok_or(BackendError::NotFound)?
                .stdio.take().ok_or(BackendError::Denied)
        })
    }

    fn take_pty(&self, run: RunHandle) -> BackendFuture<'_, RunPty> {
        Box::pin(async move {
            self.runs.lock().map_err(|_| BackendError::Unavailable)?
                .get_mut(&run.0).ok_or(BackendError::NotFound)?
                .pty.take().ok_or(BackendError::Denied)
        })
    }

    fn resize(&self, run: RunHandle, size: PtySize) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            size.validate().map_err(|_| BackendError::Denied)?;
            let master = self.runs.lock().map_err(|_| BackendError::Unavailable)?
                .get(&run.0).ok_or(BackendError::NotFound)?.master.clone()
                .ok_or(BackendError::Denied)?;
            let guard = master.lock().map_err(|_| BackendError::Unavailable)?;
            // `None`: the Windows pseudoconsole was already closed after exit.
            guard.as_ref().ok_or(BackendError::Denied)?.resize(pty_size(size))
                .map_err(|_| BackendError::Io)
        })
    }
}

fn exit_outcome(status: std::process::ExitStatus) -> RunOutcome {
    if let Some(code) = status.code() { return RunOutcome::Exited(code); }
    #[cfg(unix)] {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() { return RunOutcome::Signalled(signal as u32); }
    }
    RunOutcome::IsolationFailure
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|id| i32::try_from(id).ok()).filter(|id| *id > 1) {
        // SAFETY: the child was created as its own process-group leader. A
        // negative pid targets that group; it can never address our own group.
        unsafe { libc::kill(-pid, libc::SIGKILL); }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

// If the runtime tears down the supervising future, terminate the still-owned
// group before Tokio reaps/drops its leader. Clear after the normal kill path.
struct GroupOnDrop(Option<u32>);
impl GroupOnDrop { fn disarm(&mut self) { self.0 = None; } }
impl Drop for GroupOnDrop { fn drop(&mut self) { kill_group(self.0.take()); } }

struct PtyOnDrop(Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>);
impl PtyOnDrop { fn disarm(&mut self) { self.0 = None; } }
impl Drop for PtyOnDrop {
    fn drop(&mut self) { if let Some(mut killer) = self.0.take() { let _ = killer.kill(); } }
}

fn start_pipe(entry: &mut Entry, mut command: Command, prompt: Vec<u8>,
    completion: watch::Sender<Option<RunOutcome>>, runtime_ms: u64, output_limit: u64) -> Result<(), BackendError>
{
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let mut job = {
        let job = crate::windows_job::WindowsJob::new().map_err(|error| { entry.spawn_error = Some(error); BackendError::Io })?;
        command.creation_flags(crate::windows_job::WindowsJob::CREATION_FLAGS);
        job
    };
    let mut child = command.spawn().map_err(|error| { entry.spawn_error = Some(error); BackendError::Io })?;
    #[cfg(windows)]
    {
        use std::os::windows::io::BorrowedHandle;
        let assigned = match child.raw_handle() {
            // SAFETY: the live child owns this process handle for the call.
            Some(raw) => job.assign_and_resume(unsafe { BorrowedHandle::borrow_raw(raw) }),
            None => Err(std::io::Error::other("child process handle missing")),
        };
        if let Err(error) = assigned {
            entry.spawn_error = Some(error);
            drop(job);
            let _ = child.start_kill();
            tokio::spawn(async move { let _ = child.wait().await; });
            return Err(BackendError::Io);
        }
    }
    let pid = child.id();
    entry.pid = pid;
    let input: Box<dyn RunWriter> = match child.stdin.take() {
        Some(stdin) if prompt.is_empty() => Box::new(NativeWriter { inner: stdin, error: entry.io_errors.stdin.clone() }),
        Some(mut stdin) => {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(&prompt).await;
                let _ = stdin.shutdown().await;
            });
            Box::new(ClosedWriter)
        },
        None if prompt.is_empty() => Box::new(ClosedWriter),
        None => {
            kill_group(pid);
            let _ = child.start_kill();
            return Err(BackendError::Denied);
        },
    };
    let (cancel, mut cancelled) = mpsc::channel(1);
    let budget = Arc::new(OutputBudget::new(output_limit, cancel.clone()));
    let (stdout, mut stdout_pump) = pipe_output(child.stdout.take(), budget.clone(), entry.io_errors.stdout.clone());
    let (stderr, mut stderr_pump) = pipe_output(child.stderr.take(), budget.clone(), entry.io_errors.stderr.clone());
    entry.stdio = Some(RunStdio { stdin: input, stdout, stderr });
    entry.cancel = Some(cancel);
    let mut group = GroupOnDrop(pid);
    tokio::spawn(async move {
        let deadline = tokio::time::sleep(Duration::from_millis(runtime_ms));
        tokio::pin!(deadline);
        let (mut outcome, interrupted) = tokio::select! {
            result = child.wait() => {
                kill_group(pid);
                (result.map(exit_outcome).unwrap_or(RunOutcome::IsolationFailure), false)
            },
            reason = cancelled.recv() => {
                kill_group(pid);
                let _ = child.kill().await;
                (reason.unwrap_or(RunOutcome::Cancelled), true)
            },
            _ = &mut deadline, if runtime_ms != 0 => {
                kill_group(pid);
                let _ = child.kill().await;
                (RunOutcome::RuntimeTimeout, true)
            },
        };
        group.disarm();
        #[cfg(windows)]
        let _ = job.terminate();
        if interrupted {
            stdout_pump.abort();
            stderr_pump.abort();
        }
        if !interrupted {
            tokio::select! {
                _ = async { let _ = tokio::join!(&mut stdout_pump, &mut stderr_pump); } => {},
                reason = cancelled.recv() => {
                    stdout_pump.abort(); stderr_pump.abort();
                    outcome = reason.unwrap_or(RunOutcome::Cancelled);
                },
                _ = &mut deadline, if runtime_ms != 0 => {
                    stdout_pump.abort(); stderr_pump.abort();
                    outcome = RunOutcome::RuntimeTimeout;
                },
            }
        }
        let outcome = if budget.exceeded.load(Ordering::Acquire) { RunOutcome::Rejected } else { outcome };
        completion.send_replace(Some(outcome));
    });
    Ok(())
}

struct OutputBudget {
    remaining: AtomicU64,
    exceeded: AtomicBool,
    stopping: AtomicBool,
    cancel: mpsc::Sender<RunOutcome>,
}
impl OutputBudget {
    fn new(limit: u64, cancel: mpsc::Sender<RunOutcome>) -> Self {
        Self { remaining: AtomicU64::new(limit), exceeded: AtomicBool::new(false), stopping: AtomicBool::new(false), cancel }
    }
    fn charge(&self, count: usize) -> bool {
        let mut remaining = self.remaining.load(Ordering::Acquire);
        while let Some(next) = remaining.checked_sub(count as u64) {
            match self.remaining.compare_exchange_weak(remaining, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(actual) => remaining = actual,
            }
        }
        self.exceeded.store(true, Ordering::Release);
        let _ = self.cancel.try_send(RunOutcome::Rejected);
        false
    }
}

fn pipe_output<T: tokio::io::AsyncRead + Unpin + Send + 'static>(pipe: Option<T>, budget: Arc<OutputBudget>, errors: NativeIoError)
    -> (Box<dyn RunReader>, tokio::task::JoinHandle<()>)
{
    let (tx, rx) = mpsc::channel(16);
    let reader_budget = budget.clone();
    let pump = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let Some(mut pipe) = pipe else { return; };
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    if !budget.charge(count) {
                        let _ = tx.send(Err(BackendError::Denied)).await;
                        break;
                    }
                    if tx.send(Ok(buffer[..count].to_vec())).await.is_err() { break; }
                },
                Err(error) => {
                    if let Ok(mut stored) = errors.lock() { *stored = Some(error); }
                    let _ = tx.send(Err(BackendError::Io)).await; break;
                },
            }
        }
    });
    (Box::new(PtyReader { rx, pending: Vec::new(), offset: 0, budget: reader_budget }), pump)
}

fn pty_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize { rows: size.rows, cols: size.columns, pixel_width: 0, pixel_height: 0 }
}

fn start_pty(entry: &mut Entry, command: CommandBuilder, size: PtySize, prompt: Vec<u8>,
    completion: watch::Sender<Option<RunOutcome>>, runtime_ms: u64, output_limit: u64) -> Result<(), BackendError>
{
    let pair = portable_pty::native_pty_system().openpty(pty_size(size)).map_err(|_| BackendError::Io)?;
    // Obtain all fallible handles before creating the process.
    let mut reader = pair.master.try_clone_reader().map_err(|_| BackendError::Io)?;
    let mut writer = pair.master.take_writer().map_err(|_| BackendError::Io)?;
    let mut child = pair.slave.spawn_command(command).map_err(|_| BackendError::Io)?;
    drop(pair.slave);
    let pid = child.process_id();
    entry.pid = pid;
    let mut killer = child.clone_killer();
    let master = Arc::new(Mutex::new(Some(pair.master)));
    // Windows: the supervisor closes the pseudoconsole once the child is gone
    // (see below), so it keeps its own handle to the shared master slot.
    #[cfg(windows)]
    let console = master.clone();
    entry.master = Some(master);
    let (cancel, mut cancelled) = mpsc::channel(1);
    entry.cancel = Some(cancel.clone());
    let budget = Arc::new(OutputBudget::new(output_limit, cancel));
    let reader_budget = budget.clone();
    let (read_done, read_completion) = oneshot::channel();
    let (read_tx, read_rx) = mpsc::channel(16);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if !reader_budget.charge(count) {
                        let _ = read_tx.try_send(Err(BackendError::Denied));
                        break;
                    }
                    if !pty_send(&read_tx, Ok(buffer[..count].to_vec()), &reader_budget) { break; }
                },
                Err(_error) => {
                    // Unix PTYs commonly report EIO when the last slave closes.
                    #[cfg(unix)]
                    if _error.raw_os_error() == Some(libc::EIO) { break; }
                    // ConPTY's equivalent: the output pipe reports a broken
                    // pipe once the pseudoconsole has been closed.
                    #[cfg(windows)]
                    if _error.kind() == std::io::ErrorKind::BrokenPipe { break; }
                    let _ = read_tx.try_send(Err(BackendError::Io));
                    break;
                },
            }
        }
        // Windows: ClosePseudoConsole may block until conhost has written its
        // final output, and the master still holds its own read handle, so the
        // output pipe must keep being drained while the supervisor closes it.
        // When forwarding stopped early (cancel, output budget, receiver gone)
        // keep reading and discard until the pipe reports EOF / an error,
        // which happens once the pseudoconsole is closed. After a real EOF or
        // error the next read returns immediately, so this is a no-op there.
        #[cfg(windows)]
        while matches!(reader.read(&mut buffer), Ok(count) if count > 0) {}
        let _ = read_done.send(());
    });
    let (write_tx, mut write_rx) = mpsc::channel::<PtyWrite>(16);
    std::thread::spawn(move || {
        use std::io::Write;
        if !prompt.is_empty() && writer.write_all(&prompt).is_err() { return; }
        while let Some(message) = write_rx.blocking_recv() {
            match message {
                PtyWrite::Bytes(bytes, ack) => {
                    let result = writer.write(&bytes).and_then(|count| {
                        writer.flush()?;
                        Ok(count)
                    }).map_err(|_| BackendError::Io);
                    let failed = result.is_err();
                    let _ = ack.send(result);
                    if failed { break; }
                },
                PtyWrite::Close(ack) => { drop(writer); let _ = ack.send(()); return; },
            }
        }
    });
    entry.pty = Some(RunPty { input: Box::new(PtyWriter(Some(write_tx))),
        output: Box::new(PtyReader { rx: read_rx, pending: Vec::new(), offset: 0, budget: budget.clone() }) });
    // The blocking waiter never owns the cancellation handle or master lock.
    let waiter = tokio::task::spawn_blocking(move || child.wait());
    let mut group = GroupOnDrop(pid);
    let mut teardown = PtyOnDrop(Some(killer.clone_killer()));
    tokio::spawn(async move {
        tokio::pin!(waiter);
        let deadline = tokio::time::sleep(Duration::from_millis(runtime_ms));
        tokio::pin!(deadline);
        let mut outcome = tokio::select! {
            result = &mut waiter => {
                kill_group(pid);
                match result {
                    Ok(Ok(status)) => RunOutcome::Exited(status.exit_code() as i32),
                    _ => RunOutcome::IsolationFailure,
                }
            },
            reason = cancelled.recv() => {
                budget.stopping.store(true, Ordering::Release);
                kill_group(pid);
                let _ = killer.kill();
                let _ = (&mut waiter).await;
                reason.unwrap_or(RunOutcome::Cancelled)
            },
            _ = &mut deadline, if runtime_ms != 0 => {
                budget.stopping.store(true, Ordering::Release);
                kill_group(pid);
                let _ = killer.kill();
                let _ = (&mut waiter).await;
                RunOutcome::RuntimeTimeout
            },
        };
        group.disarm();
        teardown.disarm();
        // Windows ConPTY: unlike a Unix PTY (EIO once the last slave closes),
        // the output pipe never reaches EOF while the pseudoconsole exists,
        // even after the child has exited. The master lives in the backend
        // entry, so without closing it here the wait below never finishes —
        // and after a cancel (already consumed by the select above) with no
        // runtime deadline, `kill`/`wait` hung forever. The child is reaped at
        // this point, so close the pseudoconsole now: off the async runtime,
        // because ClosePseudoConsole can block while conhost flushes, and
        // outside the master lock so a concurrent `resize` is not held up.
        // The reader thread keeps draining the pipe meanwhile (and, after a
        // natural exit, still forwards the final output to the consumer).
        #[cfg(windows)]
        {
            drop(tokio::task::spawn_blocking(move || {
                let closing = console.lock().ok().and_then(|mut slot| slot.take());
                drop(closing);
            }));
        }
        // Defense in depth: bound the Windows wait for the output EOF so a
        // stalled close can never wedge `kill`/`wait`. Unix is unchanged.
        #[cfg(windows)]
        let close_grace = tokio::time::sleep(CONPTY_CLOSE_GRACE);
        #[cfg(not(windows))]
        let close_grace = std::future::pending::<()>();
        tokio::pin!(close_grace);
        tokio::select! {
            _ = read_completion => {},
            reason = cancelled.recv() => {
                budget.stopping.store(true, Ordering::Release);
                outcome = reason.unwrap_or(RunOutcome::Cancelled);
            },
            _ = &mut deadline, if runtime_ms != 0 => {
                budget.stopping.store(true, Ordering::Release);
                outcome = RunOutcome::RuntimeTimeout;
            },
            _ = &mut close_grace => {},
        }
        let outcome = if budget.exceeded.load(Ordering::Acquire) { RunOutcome::Rejected } else { outcome };
        completion.send_replace(Some(outcome));
    });
    Ok(())
}

fn pty_send(tx: &mpsc::Sender<Result<Vec<u8>, BackendError>>, mut message: Result<Vec<u8>, BackendError>,
    budget: &OutputBudget) -> bool
{
    loop {
        if budget.stopping.load(Ordering::Acquire) { return false; }
        match tx.try_send(message) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(value)) => {
                message = value;
                std::thread::sleep(Duration::from_millis(5));
            },
        }
    }
}

struct ClosedWriter;
impl RunWriter for ClosedWriter {
    fn write<'a>(&'a mut self, _buf: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async { Err(BackendError::Io) })
    }
    fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}

enum PtyWrite {
    Bytes(Vec<u8>, oneshot::Sender<Result<usize, BackendError>>),
    Close(oneshot::Sender<()>),
}
struct PtyWriter(Option<mpsc::Sender<PtyWrite>>);
impl RunWriter for PtyWriter {
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            let sender = self.0.as_ref().ok_or(BackendError::Io)?;
            let (ack, result) = oneshot::channel();
            let count = buf.len().min(8192);
            sender.send(PtyWrite::Bytes(buf[..count].to_vec(), ack)).await.map_err(|_| BackendError::Io)?;
            result.await.map_err(|_| BackendError::Io)?
        })
    }
    fn close(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            if let Some(sender) = self.0.take() {
                let (ack, result) = oneshot::channel();
                sender.send(PtyWrite::Close(ack)).await.map_err(|_| BackendError::Io)?;
                result.await.map_err(|_| BackendError::Io)?;
            }
            Ok(())
        })
    }
}
struct PtyReader {
    rx: mpsc::Receiver<Result<Vec<u8>, BackendError>>,
    pending: Vec<u8>,
    offset: usize,
    budget: Arc<OutputBudget>,
}
impl RunReader for PtyReader {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            if buf.is_empty() { return Ok(0); }
            if self.offset == self.pending.len() {
                match self.rx.recv().await {
                    Some(Ok(bytes)) => { self.pending = bytes; self.offset = 0; },
                    Some(Err(error)) => return Err(error),
                    None if self.budget.exceeded.load(Ordering::Acquire) => return Err(BackendError::Denied),
                    None => return Ok(0),
                }
            }
            let count = buf.len().min(self.pending.len() - self.offset);
            buf[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::bounded::Text;

    fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
    fn policy(run: RunHandle, employee: EmployeeId) -> SpawnPolicy {
        SpawnPolicy {
            workspace_delivery: None, run, employee, class: ExecutionClass::EmployeeMain, runtime: RuntimeKind::Claude,
            argv_allowlist: vec![text("unused")], environment_allowlist: BTreeMap::new(),
            workspace: WorkspacePolicy { workspace: text(if cfg!(windows) { "C:\\" } else { "/" }), relative_cwd: text("") },
            resources: ResourceLimits { memory_bytes: 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 128, ptys: 1, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 128 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024,
                io_read_ops_per_sec: 16, io_write_ops_per_sec: 16, output_buffer_bytes: 1024,
                output_total_bytes: 65536, memfd_total_bytes: 65536 },
            network: NetworkPolicy::Offline, pty: false,
            capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
            timeouts: RunTimeouts { queue_ms: 30000, startup_ms: 5000, runtime_ms: 5000,
                pty_idle_ms: None, termination_grace_ms: 100 },
            inputs: RunInputs { prompt: RunInput { reader: Box::new(Reader(tokio::io::empty())), max_bytes: 0 }, attachments: vec![] },
        }
    }

    #[test]
    fn policy_keeps_argument_environment_and_cwd_boundaries() {
        let mut value = policy(RunHandle(RunId([1; 16])), EmployeeId([2; 16]));
        value.argv_allowlist = vec![text("cli"), text("--prompt"), text("繁體中文 $HOME; a\nb")];
        value.environment_allowlist.insert("SETTING".into(), text("two words\nnext line"));
        let command = policy_command(&value).unwrap();
        assert_eq!(command.as_std().get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("--prompt"), std::ffi::OsStr::new("繁體中文 $HOME; a\nb")]);
        assert_eq!(command.as_std().get_envs().collect::<Vec<_>>(),
            vec![(std::ffi::OsStr::new("SETTING"), Some(std::ffi::OsStr::new("two words\nnext line")))]);
        assert_eq!(command.as_std().get_current_dir(), Some(std::path::Path::new(value.workspace.workspace.as_str())));
        value.workspace.relative_cwd = text("../outside");
        assert!(policy_command(&value).is_err());
    }

    struct FailingInput;
    impl RunReader for FailingInput {
        fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async { Err(BackendError::Io) })
        }
    }

    #[tokio::test]
    async fn input_failure_consumes_native_builder_without_policy_fallback() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, Command::new("native-command-must-not-run")).unwrap();
        let mut value = policy(run, employee);
        value.inputs.prompt.reader = Box::new(FailingInput);
        assert_eq!(backend.spawn(value).await, Err(BackendError::Io));
        assert_eq!(backend.spawn(policy(run, employee)).await, Err(BackendError::Denied));
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Rejected);
    }

    #[tokio::test]
    async fn cancelled_preparation_consumes_native_builder() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, Command::new("native-command-must-not-run")).unwrap();
        let (_writer, reader) = tokio::io::duplex(1);
        let mut value = policy(run, employee);
        value.inputs.prompt.reader = Box::new(Reader(reader));
        assert!(tokio::time::timeout(Duration::from_millis(10), backend.spawn(value)).await.is_err());
        assert_eq!(backend.spawn(policy(run, employee)).await, Err(BackendError::Denied));
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Rejected);
    }

    #[tokio::test]
    async fn reservations_reject_wrong_employee_reuse_and_expiry() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        assert_eq!(backend.spawn(policy(run, EmployeeId([2; 16]))).await, Err(BackendError::Denied));
        backend.kill(run).await.unwrap();
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Cancelled);
        assert_eq!(backend.spawn(policy(run, employee)).await, Err(BackendError::Denied));
        let expired = backend.reserve(employee).await.unwrap();
        backend.runs.lock().unwrap().get_mut(&expired.0).unwrap().reserved_until = Some(Instant::now());
        assert_eq!(backend.spawn(policy(expired, employee)).await, Err(BackendError::Denied));
    }

    #[tokio::test]
    async fn same_employee_wait_refuses_without_waiting_for_child() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let first = backend.reserve(employee).await.unwrap();
        let second = backend.reserve(employee).await.unwrap();
        assert_eq!(backend.wait(second, WaitOrigin::Run(first)).await, Err(BackendError::WouldDeadlock));
        backend.kill(first).await.unwrap();
        backend.kill(second).await.unwrap();
        assert_eq!(backend.wait(second, WaitOrigin::Run(first)).await.unwrap(), RunOutcome::Cancelled);
    }

    #[tokio::test]
    async fn unimplemented_local_capabilities_fail_before_spawn() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        let mut value = policy(run, employee);
        value.capabilities.mcp_bridge = true;
        assert_eq!(backend.spawn(value).await, Err(BackendError::Denied));
        backend.kill(run).await.unwrap();
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script).stdin(Stdio::piped())
            .stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    #[cfg(unix)]
    async fn read_all(reader: &mut dyn RunReader) -> Vec<u8> {
        let mut result = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let count = reader.read(&mut buffer).await.unwrap();
            if count == 0 { return result; }
            result.extend_from_slice(&buffer[..count]);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn registered_command_preserves_native_bytes_and_streams_once() {
        use std::os::unix::ffi::OsStringExt;
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        let mut command = shell("printf '%s' \"$1\"; printf '%s' \"$EXACT_ENV\" >&2");
        command.arg("fixture").arg(std::ffi::OsString::from_vec(vec![0xff, b'x']));
        command.env_clear().env("EXACT_ENV", "exact 值");
        backend.register_command(run, command).unwrap();
        backend.spawn(policy(run, employee)).await.unwrap();
        let mut streams = backend.take_stdio(run).await.unwrap();
        assert!(backend.take_stdio(run).await.is_err());
        assert_eq!(read_all(streams.stdout.as_mut()).await, vec![0xff, b'x']);
        assert_eq!(read_all(streams.stderr.as_mut()).await, "exact 值".as_bytes());
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Exited(0));
        backend.forget(run).unwrap();
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await, Err(BackendError::NotFound));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_interrupts_wait_without_holding_child_lock() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("sleep 60")).unwrap();
        backend.spawn(policy(run, employee)).await.unwrap();
        let (outcome, killed) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(backend.wait(run, WaitOrigin::Gateway), async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                backend.kill(run).await
            })
        }).await.unwrap();
        killed.unwrap();
        assert_eq!(outcome.unwrap(), RunOutcome::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_deadline_returns_typed_outcome() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("sleep 60")).unwrap();
        let mut value = policy(run, employee);
        value.timeouts.runtime_ms = 25;
        backend.spawn(value).await.unwrap();
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::RuntimeTimeout);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_is_not_truncated_and_reaches_stdin() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("cat")).unwrap();
        let mut value = policy(run, employee);
        value.inputs.prompt = RunInput { reader: Box::new(Reader(std::io::Cursor::new(b"exact\0prompt".to_vec()))), max_bytes: 32 };
        backend.spawn(value).await.unwrap();
        let mut streams = backend.take_stdio(run).await.unwrap();
        assert_eq!(read_all(streams.stdout.as_mut()).await, b"exact\0prompt");
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Exited(0));
        let run = backend.reserve(employee).await.unwrap();
        let mut value = policy(run, employee);
        value.inputs.prompt = RunInput { reader: Box::new(Reader(std::io::Cursor::new(vec![1, 2]))), max_bytes: 1 };
        assert_eq!(backend.spawn(value).await, Err(BackendError::Denied));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pty_resize_is_real_and_handles_are_single_take() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "read value; stty size"]);
        backend.register_pty(run, command, PtySize { rows: 24, columns: 80 }).unwrap();
        let mut value = policy(run, employee);
        value.pty = true;
        backend.spawn(value).await.unwrap();
        let mut streams = backend.take_pty(run).await.unwrap();
        assert!(backend.take_pty(run).await.is_err());
        backend.resize(run, PtySize { rows: 37, columns: 93 }).await.unwrap();
        streams.input.write(b"go\n").await.unwrap();
        let output = read_all(streams.output.as_mut()).await;
        assert!(String::from_utf8_lossy(&output).contains("37 93"));
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Exited(0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_budget_is_aggregate_and_overflow_rejects_run() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("printf '1234'; printf '5678' >&2; sleep 60")).unwrap();
        let mut value = policy(run, employee);
        value.resources.output_total_bytes = 6;
        backend.spawn(value).await.unwrap();
        let mut streams = backend.take_stdio(run).await.unwrap();
        async fn drain(reader: &mut dyn RunReader) -> (usize, bool) {
            let mut bytes = 0;
            let mut buffer = [0; 32];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => return (bytes, false),
                    Err(_) => return (bytes, true),
                    Ok(count) => bytes += count,
                }
            }
        }
        let (stdout, stderr, outcome) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(drain(streams.stdout.as_mut()), drain(streams.stderr.as_mut()),
                backend.wait(run, WaitOrigin::Gateway))
        }).await.unwrap();
        assert!(stdout.0 + stderr.0 <= 6);
        assert!(stdout.1 || stderr.1, "overflow must remain observable on the streams");
        assert_eq!(outcome.unwrap(), RunOutcome::Rejected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pty_output_overflow_terminates_without_a_reader() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "printf 'oversized-output'; sleep 60"]);
        backend.register_pty(run, command, PtySize { rows: 24, columns: 80 }).unwrap();
        let mut value = policy(run, employee);
        value.pty = true;
        value.resources.output_total_bytes = 4;
        backend.spawn(value).await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2),
            backend.wait(run, WaitOrigin::Gateway)).await.unwrap().unwrap();
        assert_eq!(outcome, RunOutcome::Rejected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_a_run_terminates_its_process_group_descendant() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("sleep 60 & printf '%s\n' \"$!\"; wait")).unwrap();
        backend.spawn(policy(run, employee)).await.unwrap();
        let mut streams = backend.take_stdio(run).await.unwrap();
        let mut bytes = Vec::new();
        let descendant = tokio::time::timeout(Duration::from_secs(2), async {
            let mut buffer = [0; 32];
            while !bytes.contains(&b'\n') {
                let count = streams.stdout.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            String::from_utf8(bytes).unwrap().trim().parse::<i32>().unwrap()
        }).await.unwrap();
        backend.kill(run).await.unwrap();
        assert_eq!(backend.wait(run, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Cancelled);
        for _ in 0..100 {
            // SAFETY: signal zero observes existence without sending a signal.
            if unsafe { libc::kill(descendant, 0) } == -1 { return; }
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{descendant}/stat")).ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, state)| state.split_whitespace().next() == Some("Z")))
                == Some(true) { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("process-group descendant survived cancellation");
    }


    #[cfg(unix)]
    #[tokio::test]
    async fn output_exactly_at_budget_returns_normal_eof() {
        let backend = LocalBackend::new();
        let employee = EmployeeId([1; 16]);
        let run = backend.reserve(employee).await.unwrap();
        backend.register_command(run, shell("printf '1234'; printf '56' >&2")).unwrap();
        let mut value = policy(run, employee);
        value.resources.output_total_bytes = 6;
        backend.spawn(value).await.unwrap();
        let mut streams = backend.take_stdio(run).await.unwrap();
        let (stdout, stderr, outcome) = tokio::join!(read_all(streams.stdout.as_mut()),
            read_all(streams.stderr.as_mut()), backend.wait(run, WaitOrigin::Gateway));
        assert_eq!(stdout, b"1234");
        assert_eq!(stderr, b"56");
        assert_eq!(outcome.unwrap(), RunOutcome::Exited(0));
    }


    #[test]
    fn interactive_login_scrubs_native_command_and_pty_session_environment() {
        let mut command = Command::new("fixture");
        let mut pty = CommandBuilder::new("fixture");
        for key in LOGIN_SESSION_ENV {
            command.env(key, "private-session");
            pty.env(key, "private-session");
        }
        command.env("PUBLIC_SETTING", "kept");
        pty.env("PUBLIC_SETTING", "kept");
        scrub_login_command(&mut command, ExecutionClass::EmployeeMain);
        scrub_login_pty(&mut pty, ExecutionClass::EmployeeMain);
        for key in LOGIN_SESSION_ENV {
            assert!(command.as_std().get_envs().any(|(name, value)|
                name == std::ffi::OsStr::new(key) && value == Some(std::ffi::OsStr::new("private-session"))));
            assert_eq!(pty.get_env(key), Some(std::ffi::OsStr::new("private-session")));
        }
        scrub_login_command(&mut command, ExecutionClass::InteractiveLogin);
        scrub_login_pty(&mut pty, ExecutionClass::InteractiveLogin);
        for key in LOGIN_SESSION_ENV {
            assert!(command.as_std().get_envs().any(|(name, value)|
                name == std::ffi::OsStr::new(key) && value.is_none()));
            assert_eq!(pty.get_env(key), None);
        }
        assert!(command.as_std().get_envs().any(|(name, value)|
            name == "PUBLIC_SETTING" && value == Some(std::ffi::OsStr::new("kept"))));
        assert_eq!(pty.get_env("PUBLIC_SETTING"), Some(std::ffi::OsStr::new("kept")));
    }

}
