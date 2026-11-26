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
use crate::{process_identity::RootProcess, spawn_binding::{SpawnBinding, RevokeOnDrop}};
use crate::io::{NativeWriter, NativeIoErrors, NativeIoError, read_input};
#[cfg(test)]
use crate::io::Reader;

mod processes;
use processes::{start_pipe, start_pty, pty_size};

const MAX_LOCAL_RUNS: usize = 1024;
const LOGIN_SESSION_ENV: &[&str] = &[
    "DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS", "SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_AGENT_INFO",
];
const TERMINAL_RETENTION: Duration = Duration::from_secs(300);
/// The PTY master. `None` once the Windows supervisor has closed the
/// pseudoconsole after the child exited (see `start_pty`); Unix never clears it.
type Master = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;
/// Windows only: bound on ClosePseudoConsole itself. Closing should complete
/// promptly because the reader thread keeps draining; if it does not, the
/// supervisor waits at most this long again for the output EOF and then
/// reports the outcome anyway, so `wait`/`kill` can never hang on a stall.
#[cfg(windows)]
const CONPTY_CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Windows only: after a natural exit, conhost may still be rendering the
/// child's final output. Close only once the output has been idle this long
/// (measured from the later of reap and the last read) ...
#[cfg(windows)]
const CONPTY_TAIL_IDLE: Duration = Duration::from_millis(200);
/// ... or this long after the child was reaped, whichever comes first.
#[cfg(windows)]
const CONPTY_TAIL_CAP: Duration = Duration::from_secs(1);
#[cfg(windows)]
const CONPTY_TAIL_POLL: Duration = Duration::from_millis(20);

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
    binding: Option<SpawnBinding>,
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
                    if let Some(binding) = &entry.binding { binding.revoke(); }
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
                if let Some(binding) = &entry.binding { binding.revoke(); }
                if let Some(cancel) = &entry.cancel {
                    let _ = cancel.try_send(RunOutcome::Cancelled);
                }
            }
        }
    }
}

impl LocalBackend {
    pub fn new() -> Self { Self::default() }

    pub fn register_binding(&self, run: RunHandle, binding: Option<SpawnBinding>) -> Result<(), BackendError> {
        let mut runs = self.runs.lock().map_err(|_| BackendError::Unavailable)?;
        let entry = runs.get_mut(&run.0).ok_or(BackendError::NotFound)?;
        check_reservation(entry)?;
        if entry.binding.is_some() { return Err(BackendError::Denied); }
        entry.binding = binding;
        Ok(())
    }

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
                cancel: None, stdio: None, pty: None, master: None, terminal_at: None, spawn_error: None, pid: None, binding: None, io_errors: NativeIoErrors::default() });
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
                if let Some(binding) = &entry.binding { binding.revoke(); }
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
