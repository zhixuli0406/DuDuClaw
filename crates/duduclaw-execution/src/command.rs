//! Native-command adapter. Local builders are moved intact; protected launches
//! require host-owned policy and capability context and never execute the builder.
use std::{ffi::OsString, future::Future, io, path::PathBuf, process::{ExitStatus, Output}, sync::Arc};
use duduclaw_isolation::{bounded::Text, execution::*, ids::EmployeeId};
use tokio::{io::{AsyncReadExt}, process::Command, sync::watch};

pub type ChildStdin = crate::io::AsyncWriter;
pub type ChildStdout = crate::io::AsyncReader;
pub type ChildStderr = crate::io::AsyncReader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCommandSnapshot {
    pub program: OsString,
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, Option<OsString>)>,
    pub cwd: Option<PathBuf>,
}

pub fn command_snapshot(command: &Command) -> NativeCommandSnapshot {
    let command = command.as_std();
    NativeCommandSnapshot {
        program: command.get_program().to_owned(),
        argv: command.get_args().map(ToOwned::to_owned).collect(),
        env: command.get_envs().map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned))).collect(),
        cwd: command.get_current_dir().map(ToOwned::to_owned),
    }
}

/// Observable PTY configuration. The original builder (including non-UTF-8
/// environment values not exposed by portable-pty's iterator) stays intact.
pub fn pty_snapshot(command: &portable_pty::CommandBuilder) -> NativeCommandSnapshot {
    NativeCommandSnapshot {
        program: command.get_argv().first().cloned().unwrap_or_default(),
        argv: command.get_argv().iter().skip(1).cloned().collect(),
        env: command.iter_full_env_as_str().map(|(key, value)| (key.into(), Some(value.into()))).collect(),
        cwd: command.get_cwd().map(PathBuf::from),
    }
}

pub async fn local_pty_roundtrip(command: portable_pty::CommandBuilder, size: PtySize) -> io::Result<NativeCommandSnapshot> {
    let backend = crate::LocalBackend::new();
    let run = backend.reserve(EmployeeId([0; 16])).await.map_err(error)?;
    backend.register_pty(run, command, size).map_err(error)?;
    let snapshot = backend.registered_pty_snapshot(run).map_err(error)?;
    backend.kill(run).await.map_err(error)?;
    Ok(snapshot)
}

/// Immutable host routing and Package D capability lifecycle. Implementations
/// must derive identity/policy from trusted gateway state, never child env/argv.
/// `prepare` creates pending capability state before StartRun. `activate` binds
/// the received connection to the exact live run. `revoke` runs on every exit.
pub trait ProtectedLaunch: Send + Sync {
    fn employee(&self) -> EmployeeId;
    fn task(&self) -> Option<Text<128>>;
    /// Caller authority from the active host run, never an employee argument.
    fn wait_origin(&self) -> WaitOrigin;
    /// None only for host-originated work; required even across backend clients.
    fn caller_employee(&self) -> Option<EmployeeId>;
    fn prepare<'a>(&'a self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind,
        native: &'a NativeCommandSnapshot, pty: bool) -> BackendFuture<'a, SpawnPolicy>;
    #[cfg(target_os = "linux")]
    fn activate(&self, run: RunHandle, mcp: Option<std::os::fd::OwnedFd>) -> BackendFuture<'_, ()>;
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()>;
}

tokio::task_local! {
    static CONTEXT: Option<Arc<dyn ProtectedLaunch>>;
    static RUNTIME: RuntimeKind;
    static CLASS: Option<ExecutionClass>;
    static MEMORY_LIMIT: Option<u64>;
}

pub fn capture_context() -> Option<Arc<dyn ProtectedLaunch>> { CONTEXT.try_with(Clone::clone).ok().flatten() }
pub async fn with_context<F: Future>(context: Arc<dyn ProtectedLaunch>, future: F) -> F::Output {
    CONTEXT.scope(Some(context), future).await
}
pub fn capture_runtime() -> RuntimeKind {
    RUNTIME.try_with(Clone::clone).unwrap_or_else(|_| RuntimeKind::Registered(Text::new("legacy".into()).expect("constant")))
}
pub async fn with_runtime<F: Future>(runtime: RuntimeKind, future: F) -> F::Output { RUNTIME.scope(runtime, future).await }

pub fn capture_class_or(default: ExecutionClass) -> ExecutionClass { CLASS.try_with(|class| *class).ok().flatten().unwrap_or(default) }
pub fn capture_class() -> ExecutionClass { capture_class_or(ExecutionClass::EmployeeMain) }
pub async fn with_class<F: Future>(class: ExecutionClass, future: F) -> F::Output { CLASS.scope(Some(class), future).await }
pub fn capture_memory_limit() -> Option<u64> { MEMORY_LIMIT.try_with(|limit| *limit).ok().flatten() }
pub async fn with_memory_limit<F: Future>(limit: u64, future: F) -> F::Output {
    let limit = capture_memory_limit().map_or(limit, |outer| outer.min(limit));
    MEMORY_LIMIT.scope(Some(limit), future).await
}

#[derive(Clone)]
pub struct CapturedLaunch {
    context: Option<Arc<dyn ProtectedLaunch>>,
    runtime: RuntimeKind,
    class: Option<ExecutionClass>,
    memory_limit: Option<u64>,
}
impl std::fmt::Debug for CapturedLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedLaunch").field("protected_context", &self.context.is_some())
            .field("runtime", &self.runtime).field("class", &self.class).finish_non_exhaustive()
    }
}
impl CapturedLaunch {
    pub fn capture() -> Self { Self { context: capture_context(), runtime: capture_runtime(), class: CLASS.try_with(|class| *class).ok().flatten(), memory_limit: capture_memory_limit() } }
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CONTEXT.scope(self.context.clone(), RUNTIME.scope(self.runtime.clone(),
            CLASS.scope(self.class, MEMORY_LIMIT.scope(self.memory_limit, future)))).await
    }
    pub fn sync_scope<T>(&self, f: impl FnOnce() -> T) -> T {
        CONTEXT.sync_scope(self.context.clone(), || RUNTIME.sync_scope(self.runtime.clone(), ||
            CLASS.sync_scope(self.class, || MEMORY_LIMIT.sync_scope(self.memory_limit, f))))
    }
}

pub fn with_context_sync<T>(context: Option<Arc<dyn ProtectedLaunch>>, runtime: RuntimeKind, f: impl FnOnce() -> T) -> T {
    CONTEXT.sync_scope(context, || RUNTIME.sync_scope(runtime, f))
}

fn error(error: BackendError) -> io::Error {
    let kind = match error {
        BackendError::Denied | BackendError::WouldDeadlock => io::ErrorKind::PermissionDenied,
        BackendError::Timeout => io::ErrorKind::TimedOut,
        BackendError::NotFound => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

fn exit_status(outcome: RunOutcome) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match outcome {
            RunOutcome::Exited(code) => Ok(ExitStatus::from_raw(code << 8)),
            RunOutcome::Signalled(signal) => Ok(ExitStatus::from_raw(signal as i32)),
            RunOutcome::Cancelled => Ok(ExitStatus::from_raw(9)),
            RunOutcome::QueueTimeout | RunOutcome::RuntimeTimeout | RunOutcome::IdleTimeout => Err(error(BackendError::Timeout)),
            _ => Err(error(BackendError::Denied)),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        match outcome {
            RunOutcome::Exited(code) => Ok(ExitStatus::from_raw(code as u32)),
            RunOutcome::Signalled(_) | RunOutcome::Cancelled => Ok(ExitStatus::from_raw(1)),
            RunOutcome::QueueTimeout | RunOutcome::RuntimeTimeout | RunOutcome::IdleTimeout => Err(error(BackendError::Timeout)),
            _ => Err(error(BackendError::Denied)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    { let _ = outcome; Err(error(BackendError::Unavailable)) }
}

#[derive(Clone)]
pub struct RunControl {
    backend: Arc<dyn ExecutionBackend>,
    run: RunHandle,
    result: watch::Receiver<Option<Result<RunOutcome, BackendError>>>,
    local_pid: Option<u32>,
    executor: tokio::runtime::Handle,
}
impl RunControl {
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        match self.result.borrow().clone() {
            Some(Ok(outcome)) => exit_status(outcome).map(Some),
            Some(Err(reason)) => Err(error(reason)),
            None => Ok(None),
        }
    }
    pub fn is_finished(&self) -> bool { self.result.borrow().is_some() }
    pub async fn wait(&self) -> io::Result<ExitStatus> {
        let mut result = self.result.clone();
        loop {
            if let Some(outcome) = result.borrow_and_update().clone() { return exit_status(outcome.map_err(error)?); }
            result.changed().await.map_err(|_| error(BackendError::Unavailable))?;
        }
    }
    pub async fn kill(&self) -> io::Result<()> { self.backend.kill(self.run).await.map_err(error) }
    pub fn start_kill(&self) -> io::Result<()> {
        if self.is_finished() { return Ok(()); }
        let backend = self.backend.clone();
        let run = self.run;
        self.executor.spawn(async move { let _ = backend.kill(run).await; });
        Ok(())
    }
    pub async fn resize(&self, size: PtySize) -> io::Result<()> { self.backend.resize(self.run, size).await.map_err(error) }
}

pub struct Child {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    control: RunControl,
    reaped: bool,
}
impl Child {
    pub fn id(&self) -> Option<u32> { if self.reaped { None } else { self.control.local_pid } }
    pub fn control(&self) -> RunControl { self.control.clone() }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.control.try_wait();
        if self.control.is_finished() { self.reaped = true; }
        result
    }
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        let result = self.control.wait().await;
        if self.control.is_finished() { self.reaped = true; }
        result
    }
    pub async fn kill(&mut self) -> io::Result<()> { self.control.kill().await }
    pub fn start_kill(&mut self) -> io::Result<()> { self.control.start_kill() }
    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        self.stdin.take();
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        async fn read(stream: Option<ChildStdout>) -> io::Result<Vec<u8>> {
            let mut output = Vec::new();
            if let Some(mut stream) = stream { stream.read_to_end(&mut output).await?; }
            Ok(output)
        }
        let (status, stdout, stderr) = tokio::try_join!(self.control.wait(), read(stdout), read(stderr))?;
        Ok(Output { status, stdout, stderr })
    }
}
impl Drop for Child { fn drop(&mut self) { let _ = self.control.start_kill(); } }

pub struct PtyChild {
    pub input: Option<ChildStdin>,
    pub output: Option<ChildStdout>,
    control: RunControl,
    reaped: bool,
}
impl PtyChild {
    pub fn id(&self) -> Option<u32> { if self.reaped { None } else { self.control.local_pid } }
    pub fn control(&self) -> RunControl { self.control.clone() }
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let result = self.control.wait().await;
        if self.control.is_finished() { self.reaped = true; }
        result
    }
    pub async fn resize(&self, size: PtySize) -> io::Result<()> { self.control.resize(size).await }
    pub async fn kill(&mut self) -> io::Result<()> { self.control.kill().await }
    pub fn start_kill(&mut self) -> io::Result<()> { self.control.start_kill() }
}
impl Drop for PtyChild { fn drop(&mut self) { let _ = self.control.start_kill(); } }

fn reader(source: Box<dyn RunReader>) -> ChildStdout { crate::io::AsyncReader::new(source) }
fn writer(sink: Box<dyn RunWriter>) -> ChildStdin { crate::io::AsyncWriter::new(sink) }

fn monitor(backend: Arc<dyn ExecutionBackend>, run: RunHandle,
    lifecycle: Option<Arc<dyn ProtectedLaunch>>, local_pid: Option<u32>) -> RunControl {
    let (sender, result) = watch::channel(None);
    let control = RunControl { backend: backend.clone(), run, result, local_pid, executor: tokio::runtime::Handle::current() };
    tokio::spawn(async move {
        // This monitor is the host observer, not the employee waiter. Source
        // handles belong to another spawnd client; protected() checks the trusted
        // caller employee before reserve so same-employee waits never reach here.
        let mut outcome = backend.wait(run, WaitOrigin::Gateway).await;
        if let Some(lifecycle) = lifecycle {
            if let Err(reason) = lifecycle.revoke(run).await { outcome = Err(reason); }
        }
        sender.send_replace(Some(outcome));
    });
    control
}

fn local_policy(run: RunHandle, class: ExecutionClass, pty: bool) -> SpawnPolicy {
    SpawnPolicy {
        run, employee: EmployeeId([0; 16]), class, runtime: capture_runtime(),
        // These fields are not used to reconstruct a registered native builder.
        argv_allowlist: vec![], environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("native-builder".into()).expect("constant"), relative_cwd: Text::new(String::new()).expect("constant") },
        resources: ResourceLimits { memory_bytes: u64::MAX, cpu_quota_percent: u32::MAX,
            tasks: u32::MAX, open_files: u32::MAX, ptys: 1,
            disk: DiskQuota::LoopFilesystem { bytes: u64::MAX, inodes: u64::MAX },
            io_read_bytes_per_sec: u64::MAX, io_write_bytes_per_sec: u64::MAX,
            io_read_ops_per_sec: u64::MAX, io_write_ops_per_sec: u64::MAX,
            output_buffer_bytes: 65536, output_total_bytes: u64::MAX, memfd_total_bytes: 1 },
        network: NetworkPolicy::Offline, pty,
        capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
        timeouts: RunTimeouts { queue_ms: 30000, startup_ms: 0, runtime_ms: 0, pty_idle_ms: None, termination_grace_ms: 0 },
        inputs: RunInputs { prompt: RunInput { reader: Box::new(crate::io::Reader(tokio::io::empty())), max_bytes: 0 }, attachments: vec![] },
    }
}

/// This test helper exercises the same native registration used by spawn. It
/// does not execute anything or recreate a command from the observable fields.
pub async fn local_command_roundtrip(command: Command) -> io::Result<NativeCommandSnapshot> {
    let backend = crate::LocalBackend::new();
    let run = backend.reserve(EmployeeId([0; 16])).await.map_err(error)?;
    backend.register_command(run, command).map_err(error)?;
    let snapshot = backend.registered_snapshot(run).map_err(error)?;
    backend.kill(run).await.map_err(error)?;
    Ok(snapshot)
}

pub async fn spawn(command: &mut Command, class: ExecutionClass, required: bool) -> io::Result<Child> {
    if required && !crate::selection::gateway_exception(class) {
        #[cfg(target_os = "linux")]
        {
            let mut prepared = protected(command_snapshot(command), class, false).await?;
            let (backend, run, lifecycle) = (prepared.backend.clone(), prepared.run, prepared.lifecycle.clone());
            let streams = match prepared.stdio().await {
                Ok(streams) => streams,
                Err(reason) => { rollback(backend.as_ref(), run, lifecycle.as_ref()).await; return Err(error(reason)); },
            };
            activate(&backend, run, lifecycle.as_ref(), prepared.mcp).await?;
            prepared.armed = false;
            return Ok(Child { stdin: Some(writer(streams.stdin)), stdout: Some(reader(streams.stdout)),
                stderr: Some(reader(streams.stderr)), control: monitor(backend, run, Some(lifecycle), None), reaped: false });
        }
        #[cfg(not(target_os = "linux"))]
        { return Err(error(BackendError::Unavailable)); }
    }
    let backend = Arc::new(crate::LocalBackend::new());
    let run = backend.reserve(EmployeeId([0; 16])).await.map_err(error)?;
    // Moving, not rebuilding, preserves env_clear, OS strings, stdio and hooks.
    let command = std::mem::replace(command, Command::new("unused-consumed-command"));
    backend.register_command(run, command).map_err(error)?;
    if let Err(reason) = backend.spawn(local_policy(run, class, false)).await {
        return Err(backend.take_spawn_error(run).unwrap_or_else(|| error(reason)));
    }
    let streams = backend.take_stdio(run).await.map_err(error)?;
    let pid = backend.local_pid(run);
    let errors = backend.native_io_errors(run).ok_or_else(|| error(BackendError::Protocol))?;
    Ok(Child { stdin: Some(writer(streams.stdin).with_native_error(errors.stdin)),
        stdout: Some(reader(streams.stdout).with_native_error(errors.stdout)),
        stderr: Some(reader(streams.stderr).with_native_error(errors.stderr)), control: monitor(backend, run, None, pid), reaped: false })
}

pub async fn spawn_pty(command: portable_pty::CommandBuilder, size: PtySize,
    class: ExecutionClass, required: bool) -> io::Result<PtyChild> {
    if required && !crate::selection::gateway_exception(class) {
        #[cfg(target_os = "linux")]
        {
            let mut prepared = protected(pty_snapshot(&command), class, true).await?;
            let (backend, run, lifecycle) = (prepared.backend.clone(), prepared.run, prepared.lifecycle.clone());
            let streams = match prepared.pty().await {
                Ok(streams) => streams,
                Err(reason) => { rollback(backend.as_ref(), run, lifecycle.as_ref()).await; return Err(error(reason)); },
            };
            activate(&backend, run, lifecycle.as_ref(), prepared.mcp).await?;
            if let Err(reason) = backend.resize(run, size).await {
                rollback(backend.as_ref(), run, lifecycle.as_ref()).await;
                return Err(error(reason));
            }
            prepared.armed = false;
            return Ok(PtyChild { input: Some(writer(streams.input)), output: Some(reader(streams.output)),
                control: monitor(backend, run, Some(lifecycle), None), reaped: false });
        }
        #[cfg(not(target_os = "linux"))]
        { return Err(error(BackendError::Unavailable)); }
    }
    let backend = Arc::new(crate::LocalBackend::new());
    let run = backend.reserve(EmployeeId([0; 16])).await.map_err(error)?;
    backend.register_pty(run, command, size).map_err(error)?;
    backend.spawn(local_policy(run, class, true)).await.map_err(error)?;
    let streams = backend.take_pty(run).await.map_err(error)?;
    let pid = backend.local_pid(run);
    Ok(PtyChild { input: Some(writer(streams.input)), output: Some(reader(streams.output)),
        control: monitor(backend, run, None, pid), reaped: false })
}

#[cfg(target_os = "linux")]
async fn rollback(backend: &crate::SpawndBackend, run: RunHandle, lifecycle: &dyn ProtectedLaunch) {
    let _ = backend.kill(run).await;
    let _ = lifecycle.revoke(run).await;
}

#[cfg(target_os = "linux")]
struct Prepared {
    backend: Arc<crate::SpawndBackend>,
    run: RunHandle,
    lifecycle: Arc<dyn ProtectedLaunch>,
    mcp: bool,
    armed: bool,
    io_deadline: tokio::time::Instant,
}
#[cfg(target_os = "linux")]
impl Prepared {
    async fn stdio(&self) -> Result<RunStdio, BackendError> {
        loop {
            match self.backend.take_stdio(self.run).await {
                Err(BackendError::Unavailable) if tokio::time::Instant::now() < self.io_deadline =>
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(BackendError::Unavailable) => return Err(BackendError::Timeout),
                result => return result,
            }
        }
    }
    async fn pty(&self) -> Result<RunPty, BackendError> {
        loop {
            match self.backend.take_pty(self.run).await {
                Err(BackendError::Unavailable) if tokio::time::Instant::now() < self.io_deadline =>
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(BackendError::Unavailable) => return Err(BackendError::Timeout),
                result => return result,
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Prepared {
    fn drop(&mut self) {
        if !self.armed { return; }
        let backend = self.backend.clone();
        let lifecycle = self.lifecycle.clone();
        let run = self.run;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { rollback(&backend, run, lifecycle.as_ref()).await; });
        }
    }
}

#[cfg(target_os = "linux")]
async fn protected(native: NativeCommandSnapshot, class: ExecutionClass, pty: bool)
    -> io::Result<Prepared>
{
    let lifecycle = capture_context().ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, crate::selection::ISOLATION_UNAVAILABLE))?;
    // This facade synchronously waits for I/O and completion. Nested employee
    // work must use D's asynchronous delegation/existing-run path: waiting for a
    // newly queued run here would deadlock the caller that holds admission.
    if lifecycle.caller_employee() == Some(lifecycle.employee())
        || (matches!(lifecycle.wait_origin(), WaitOrigin::Run(_)) && lifecycle.caller_employee().is_none()) {
        return Err(error(BackendError::WouldDeadlock));
    }
    let backend = Arc::new(crate::SpawndBackend::connect().await.map_err(error)?);
    let employee = lifecycle.employee();
    let run = backend.reserve(employee).await.map_err(error)?;
    let mut prepared = Prepared { backend: backend.clone(), run, lifecycle: lifecycle.clone(), mcp: false, armed: true, io_deadline: tokio::time::Instant::now() };
    let runtime = capture_runtime();
    let policy = match lifecycle.prepare(run, class, runtime.clone(), &native, pty).await {
        Ok(policy) => policy,
        Err(reason) => { rollback(&backend, run, lifecycle.as_ref()).await; return Err(error(reason)); },
    };
    if !valid_policy_binding(&policy, run, employee, class, &runtime, pty, capture_memory_limit()) {
        rollback(&backend, run, lifecycle.as_ref()).await;
        return Err(error(BackendError::Denied));
    }
    if let Err(reason) = backend.bind_task(run, lifecycle.task()).await {
        rollback(&backend, run, lifecycle.as_ref()).await;
        return Err(error(reason));
    }
    prepared.mcp = policy.capabilities.mcp_bridge;
    prepared.io_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(policy.timeouts.queue_ms.saturating_add(policy.timeouts.startup_ms));
    if let Err(reason) = backend.spawn(policy).await {
        rollback(&backend, run, lifecycle.as_ref()).await;
        return Err(error(reason));
    }
    Ok(prepared)
}

#[cfg(any(target_os = "linux", test))]
fn valid_policy_binding(policy: &SpawnPolicy, run: RunHandle, employee: EmployeeId,
    class: ExecutionClass, runtime: &RuntimeKind, pty: bool, memory_limit: Option<u64>) -> bool {
    policy.run == run && policy.employee == employee && policy.class == class && policy.pty == pty
        && &policy.runtime == runtime
        && memory_limit.is_none_or(|limit| limit > 0 && policy.resources.memory_bytes > 0 && policy.resources.memory_bytes <= limit)
}

#[cfg(target_os = "linux")]
async fn activate(backend: &crate::SpawndBackend, run: RunHandle, lifecycle: &dyn ProtectedLaunch, mcp_requested: bool) -> io::Result<()> {
    let mcp = if mcp_requested { Some(backend.take_mcp(run).await.map_err(error)?) } else { None };
    lifecycle.activate(run, mcp).await.map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::Stdio;

    #[tokio::test]
    async fn native_registration_preserves_argv_environment_removal_and_cwd() {
        let mut command = Command::new("runtime-wrapper");
        command.args(["--model", "registered-family", "繁體中文\n$HOME; literal"])
            .env_clear().env("EXPLICIT", "two words\nvalue").env_remove("REMOVED")
            .current_dir("relative-workspace");
        let before = command_snapshot(&command);
        assert_eq!(before.argv[2], "繁體中文\n$HOME; literal");
        assert_eq!(local_command_roundtrip(command).await.unwrap(), before);
    }

    #[tokio::test]
    async fn pty_registration_preserves_arguments_environment_and_cwd() {
        let mut command = portable_pty::CommandBuilder::new("runtime-wrapper");
        command.args(["--model", "grok-fixture", "繁體中文\n$HOME"]);
        command.env_clear();
        command.env("EXPLICIT", "exact value");
        command.cwd("relative-workspace");
        let before = pty_snapshot(&command);
        assert_eq!(local_pty_roundtrip(command, PtySize { rows: 25, columns: 89 }).await.unwrap(), before);
    }

    #[tokio::test]
    async fn missing_native_executable_retains_not_found_for_runtime_fallback() {
        let mut command = Command::new("/does-not-exist-duduclaw-adapter-fixture");
        let error = spawn(&mut command, ExecutionClass::EmployeeTool, false).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn protected_without_context_never_consumes_or_executes_native_builder() {
        let mut command = Command::new("native-must-never-run");
        command.arg("sensitive-prompt");
        let before = command_snapshot(&command);
        assert!(spawn(&mut command, ExecutionClass::EmployeeMain, true).await.is_err());
        assert_eq!(before, command_snapshot(&command));
    }

    #[tokio::test]
    async fn runtime_scope_is_nested_and_does_not_derive_from_executable() {
        let before = capture_runtime();
        with_runtime(RuntimeKind::Codex, async {
            assert_eq!(capture_runtime(), RuntimeKind::Codex);
            with_runtime(RuntimeKind::Grok, async { assert_eq!(capture_runtime(), RuntimeKind::Grok); }).await;
            assert_eq!(capture_runtime(), RuntimeKind::Codex);
        }).await;
        assert_eq!(capture_runtime(), before);
    }

    #[tokio::test]
    async fn captured_launch_preserves_runtime_class_and_narrowed_limits() {
        let absent = CapturedLaunch::capture();
        with_runtime(RuntimeKind::Grok, with_class(ExecutionClass::Scheduled, with_memory_limit(4096, async {
            with_memory_limit(8192, async {
                assert_eq!(capture_memory_limit(), Some(4096));
                let captured = CapturedLaunch::capture();
                std::thread::spawn(move || captured.sync_scope(|| {
                    assert_eq!(capture_runtime(), RuntimeKind::Grok);
                    assert_eq!(capture_class(), ExecutionClass::Scheduled);
                    assert_eq!(capture_memory_limit(), Some(4096));
                })).join().unwrap();
            }).await;
            absent.scope(async {
                assert_eq!(capture_class_or(ExecutionClass::Judge), ExecutionClass::Judge);
                assert_eq!(capture_memory_limit(), None);
            }).await;
        }))).await;
    }

    #[test]
    fn protected_policy_cannot_widen_requested_memory_ceiling() {
        let run = RunHandle(duduclaw_isolation::ids::RunId([1; 16]));
        let mut policy = local_policy(run, ExecutionClass::ParseWorker, false);
        policy.resources.memory_bytes = 4096;
        let runtime = policy.runtime.clone();
        assert!(valid_policy_binding(&policy, run, policy.employee, policy.class, &runtime, false, Some(4096)));
        assert!(!valid_policy_binding(&policy, run, policy.employee, policy.class, &runtime, false, Some(4095)));
        assert!(!valid_policy_binding(&policy, run, policy.employee, policy.class, &runtime, false, Some(0)));
    }

    #[cfg(target_os = "linux")]
    struct NestedContext;
    #[cfg(target_os = "linux")]
    impl ProtectedLaunch for NestedContext {
        fn employee(&self) -> EmployeeId { EmployeeId([7; 16]) }
        fn caller_employee(&self) -> Option<EmployeeId> { Some(self.employee()) }
        fn wait_origin(&self) -> WaitOrigin { WaitOrigin::Run(RunHandle(duduclaw_isolation::ids::RunId([9; 16]))) }
        fn task(&self) -> Option<Text<128>> { None }
        fn prepare<'a>(&'a self, _: RunHandle, _: ExecutionClass, _: RuntimeKind, _: &'a NativeCommandSnapshot, _: bool) -> BackendFuture<'a, SpawnPolicy> {
            panic!("same-employee refusal must happen before reservation or capability preparation")
        }
        fn activate(&self, _: RunHandle, _: Option<std::os::fd::OwnedFd>) -> BackendFuture<'_, ()> { panic!("must not activate") }
        fn revoke(&self, _: RunHandle) -> BackendFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn protected_same_employee_is_refused_before_connecting_or_waiting_for_io() {
        let mut command = Command::new("must-not-execute");
        let error = with_context(Arc::new(NestedContext), spawn(&mut command, ExecutionClass::EmployeeTool, true))
            .await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.get_ref().and_then(|error| error.downcast_ref::<BackendError>()), Some(&BackendError::WouldDeadlock));
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_adapter_streams_exact_bytes_cwd_and_environment() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap();
        let mut command = shell("cat; printf '%s' \"$EXPLICIT\"; printf 'error' >&2; test -f marker");
        std::fs::write(dir.path().join("marker"), b"fixture").unwrap();
        command.current_dir(dir.path()).env_clear().env("EXPLICIT", " exact 值");
        let mut child = spawn(&mut command, ExecutionClass::EmployeeTool, false).await.unwrap();
        assert!(child.id().is_some());
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"raw\0input").await.unwrap();
        stdin.shutdown().await.unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"raw\0input exact \xe5\x80\xbc");
        assert_eq!(output.stderr, b"error");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn early_stdin_close_retains_native_broken_pipe_kind() {
        use tokio::io::AsyncWriteExt;
        let mut child = spawn(&mut shell("exec 0<&-; printf ready; sleep 60"), ExecutionClass::Discovery, false).await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut ready = [0; 5];
        stdout.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        let error = child.stdin.as_mut().unwrap().write_all(b"input").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        child.kill().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn control_can_cancel_from_thread_without_a_tokio_runtime() {
        let mut child = spawn(&mut shell("sleep 60"), ExecutionClass::Scheduled, false).await.unwrap();
        let control = child.control();
        std::thread::spawn(move || control.start_kill().unwrap()).join().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await.unwrap().unwrap();
        assert!(!status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_adapter_preserves_non_utf8_native_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let mut command = shell("printf '%s' \"$1\"");
        command.arg("fixture").arg(OsString::from_vec(vec![0xff, b'x']));
        let child = spawn(&mut command, ExecutionClass::EmployeeMain, false).await.unwrap();
        assert_eq!(child.wait_with_output().await.unwrap().stdout, vec![0xff, b'x']);
    }
}
