//! Native process supervision, including root-bound credential revocation.
use super::*;

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

/// Windows: takes the pseudoconsole out of the shared slot (lock held only for
/// the take, never while closing).
#[cfg(windows)]
fn take_console(master: &Master) -> Option<Box<dyn MasterPty + Send>> {
    master.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// Windows: closes the pseudoconsole if the PTY supervisor ends without having
/// done so (task dropped at runtime shutdown, `spawn_blocking` unavailable).
/// The close runs on a fresh thread because ClosePseudoConsole can block.
#[cfg(windows)]
struct ConsoleOnDrop(Master);
#[cfg(windows)]
impl ConsoleOnDrop {
    fn close_detached(&self) {
        if let Some(console) = take_console(&self.0) {
            // If the thread cannot be spawned the closure (and console) is
            // dropped here; blocking is the only remaining option then.
            let _ = std::thread::Builder::new().spawn(move || drop(console));
        }
    }
}
#[cfg(windows)]
impl Drop for ConsoleOnDrop { fn drop(&mut self) { self.close_detached(); } }

struct PtyOnDrop(Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>);
impl PtyOnDrop { fn disarm(&mut self) { self.0 = None; } }
impl Drop for PtyOnDrop {
    fn drop(&mut self) { if let Some(mut killer) = self.0.take() { let _ = killer.kill(); } }
}

#[cfg(not(windows))]
fn finish_unix_binding<T, E>(binding: &SpawnBinding, bound: std::io::Result<()>,
    try_wait: impl FnOnce() -> Result<Option<T>, E>) -> std::io::Result<bool>
{
    bound.map(|()| false).or_else(|error| {
        if !matches!(try_wait(), Ok(Some(_))) { return Err(error); }
        // Our child has exited, so its output/status remain valid. Revoke before
        // continuing so descendants cannot use an inherited run credential.
        binding.revoke();
        Ok(true)
    })
}

pub(super) fn start_pipe(entry: &mut Entry, mut command: Command, prompt: Vec<u8>,
    completion: watch::Sender<Option<RunOutcome>>, runtime_ms: u64, output_limit: u64) -> Result<(), BackendError>
{
    let binding_lifetime = RevokeOnDrop(entry.binding.clone());
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
            Some(raw) => {
                let handle = unsafe { BorrowedHandle::borrow_raw(raw) };
                job.assign(handle).and_then(|()| {
                    if let Some(binding) = &entry.binding { binding.bind_spawn(RootProcess::from_handle(handle)?)?; }
                    job.resume(handle)
                })
            },
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
    #[cfg(not(windows))]
    if let Some(binding) = &entry.binding {
        let bound = pid.ok_or_else(|| std::io::Error::other("child PID missing"))
            .and_then(RootProcess::capture).and_then(|root| binding.bind_spawn(root));
        let bound = finish_unix_binding(binding, bound, || child.try_wait()).map(|recovered| {
            if recovered { kill_group(pid); }
        });
        if let Err(error) = bound {
            entry.spawn_error = Some(error);
            kill_group(pid);
            let _ = child.start_kill();
            tokio::spawn(async move { let _ = child.wait().await; });
            return Err(BackendError::Denied);
        }
    }
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
                if let Some(binding) = &binding_lifetime.0 { binding.revoke(); }
                kill_group(pid);
                let _ = child.kill().await;
                (reason.unwrap_or(RunOutcome::Cancelled), true)
            },
            _ = &mut deadline, if runtime_ms != 0 => {
                if let Some(binding) = &binding_lifetime.0 { binding.revoke(); }
                kill_group(pid);
                let _ = child.kill().await;
                (RunOutcome::RuntimeTimeout, true)
            },
        };
        drop(binding_lifetime);
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

pub(super) fn pty_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize { rows: size.rows, cols: size.columns, pixel_width: 0, pixel_height: 0 }
}

pub(super) fn start_pty(entry: &mut Entry, command: CommandBuilder, size: PtySize, prompt: Vec<u8>,
    completion: watch::Sender<Option<RunOutcome>>, runtime_ms: u64, output_limit: u64) -> Result<(), BackendError>
{
    let binding_lifetime = RevokeOnDrop(entry.binding.clone());
    let pair = portable_pty::native_pty_system().openpty(pty_size(size)).map_err(|_| BackendError::Io)?;
    // Obtain all fallible handles before creating the process.
    let reader = pair.master.try_clone_reader().map_err(|_| BackendError::Io)?;
    let writer = pair.master.take_writer().map_err(|_| BackendError::Io)?;
    #[cfg(windows)]
    let (reader, writer) = crate::conpty::startup_io(reader, writer);
    let (mut reader, mut writer) = (reader, writer);
    let mut child = pair.slave.spawn_command(command).map_err(|_| BackendError::Io)?;
    let pid = child.process_id();
    if let Some(binding) = &entry.binding {
        #[cfg(windows)]
        let root = child.as_raw_handle().ok_or_else(|| std::io::Error::other("PTY client handle missing"))
            .and_then(|raw| {
                // SAFETY: portable-pty owns the actual client process handle, not conhost.
                RootProcess::from_handle(unsafe { std::os::windows::io::BorrowedHandle::borrow_raw(raw) })
            });
        #[cfg(not(windows))]
        let root = pid.ok_or_else(|| std::io::Error::other("PTY client PID missing")).and_then(RootProcess::capture);
        let bound = root.and_then(|root| binding.bind_spawn(root));
        #[cfg(not(windows))]
        let bound = finish_unix_binding(binding, bound, || child.try_wait()).map(|recovered| {
            if recovered { kill_group(pid); }
        });
        if let Err(error) = bound {
            entry.spawn_error = Some(error);
            kill_group(pid);
            let _ = child.kill();
            tokio::task::spawn_blocking(move || { let _ = child.wait(); });
            return Err(BackendError::Denied);
        }
    }
    drop(pair.slave);
    entry.pid = pid;
    let mut killer = child.clone_killer();
    let master = Arc::new(Mutex::new(Some(pair.master)));
    // Windows: the supervisor closes the pseudoconsole once the child is gone
    // (see below), so it keeps its own handle to the shared master slot. The
    // guard also closes it if the supervisor is dropped before doing so.
    #[cfg(windows)]
    let console = ConsoleOnDrop(master.clone());
    entry.master = Some(master);
    // Windows: the reader stamps every successful read (millis since `epoch`)
    // so the supervisor can tell when conhost has stopped rendering output.
    #[cfg(windows)]
    let (epoch, last_read) = (Instant::now(), Arc::new(AtomicU64::new(0)));
    #[cfg(windows)]
    let read_stamp = last_read.clone();
    let (cancel, mut cancelled) = mpsc::channel(1);
    entry.cancel = Some(cancel.clone());
    #[cfg(windows)]
    let writer_cancel = cancel.clone();
    let budget = Arc::new(OutputBudget::new(output_limit, cancel));
    let reader_budget = budget.clone();
    let (read_done, read_completion) = oneshot::channel();
    let (read_tx, read_rx) = mpsc::channel(16);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        // Windows: output is "active" both when it is read and while it is
        // being delivered, so a reader held up by a slow consumer (blocked in
        // the send below) is not mistaken for an idle console.
        #[cfg(windows)]
        let stamp = || read_stamp.store(epoch.elapsed().as_millis() as u64, Ordering::Release);
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    #[cfg(windows)]
                    stamp();
                    if !reader_budget.charge(count) {
                        let _ = read_tx.try_send(Err(BackendError::Denied));
                        break;
                    }
                    #[cfg(not(windows))]
                    let sent = pty_send(&read_tx, Ok(buffer[..count].to_vec()), &reader_budget);
                    #[cfg(windows)]
                    let sent = pty_send_stamped(&read_tx, Ok(buffer[..count].to_vec()), &reader_budget, &stamp);
                    if !sent { break; }
                },
                Err(_error) => {
                    // Unix PTYs commonly report EIO when the last slave closes.
                    #[cfg(unix)]
                    if _error.raw_os_error() == Some(libc::EIO) { break; }
                    // ConPTY's equivalent: the output pipe reports a broken
                    // (ERROR_BROKEN_PIPE) or disconnected pipe
                    // (ERROR_PIPE_NOT_CONNECTED, 233) once the pseudoconsole
                    // has been closed.
                    #[cfg(windows)]
                    if _error.kind() == std::io::ErrorKind::BrokenPipe || _error.raw_os_error() == Some(233) {
                        break;
                    }
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
        // The sender is dropped first so the consumer sees EOF right away.
        #[cfg(windows)]
        {
            drop(read_tx);
            while matches!(reader.read(&mut buffer), Ok(count) if count > 0) {}
        }
        let _ = read_done.send(());
    });
    let (write_tx, mut write_rx) = mpsc::channel::<PtyWrite>(16);
    std::thread::spawn(move || {
        use std::io::Write;
        if !prompt.is_empty() {
            let result = writer.write_all(&prompt);
            if result.is_err() {
                #[cfg(windows)]
                if pty_write_timed_out(&result) {
                    let _ = writer_cancel.try_send(RunOutcome::IsolationFailure);
                }
                return;
            }
        }
        while let Some(message) = write_rx.blocking_recv() {
            match message {
                PtyWrite::Bytes(bytes, ack) => {
                    let result = writer.write(&bytes).and_then(|count| {
                        writer.flush()?;
                        Ok(count)
                    });
                    #[cfg(windows)]
                    let timed_out = pty_write_timed_out(&result);
                    let result = result.map_err(|_| BackendError::Io);
                    let failed = result.is_err();
                    let _ = ack.send(result);
                    if failed {
                        #[cfg(windows)]
                        if timed_out {
                            let _ = writer_cancel.try_send(RunOutcome::IsolationFailure);
                        }
                        break;
                    }
                },
                PtyWrite::Close(ack) => { drop(writer); let _ = ack.send(()); return; },
            }
        }
    });
    entry.pty = Some(RunPty { input: Box::new(PtyWriter(Some(write_tx))),
        output: Box::new(PtyReader { rx: read_rx, pending: Vec::new(), offset: 0, budget: budget.clone() }) });
    // The blocking waiter never owns the cancellation handle or master lock.
    // Revoke in the waiter itself, before output draining or async scheduling.
    let waiter_binding = entry.binding.clone();
    let waiter = tokio::task::spawn_blocking(move || {
        let result = child.wait();
        if let Some(binding) = waiter_binding { binding.revoke(); }
        result
    });
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
                if let Some(binding) = &binding_lifetime.0 { binding.revoke(); }
                budget.stopping.store(true, Ordering::Release);
                kill_group(pid);
                let _ = killer.kill();
                let _ = (&mut waiter).await;
                reason.unwrap_or(RunOutcome::Cancelled)
            },
            _ = &mut deadline, if runtime_ms != 0 => {
                if let Some(binding) = &binding_lifetime.0 { binding.revoke(); }
                budget.stopping.store(true, Ordering::Release);
                kill_group(pid);
                let _ = killer.kill();
                let _ = (&mut waiter).await;
                RunOutcome::RuntimeTimeout
            },
        };
        drop(binding_lifetime);
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
        // Set when a cancel or the runtime deadline cut the tail idle-wait
        // short (see below); the EOF wait is then bounded as well.
        #[cfg(windows)]
        let mut tail_cut = false;
        #[cfg(windows)]
        let close_stalled = {
            // Natural exit only: conhost renders asynchronously and some
            // Windows builds do not flush on close, so first let the output
            // go idle (bounded). Cancel / timeout / budget close immediately.
            if !budget.stopping.load(Ordering::Acquire) && !budget.exceeded.load(Ordering::Acquire) {
                let reaped = epoch.elapsed();
                loop {
                    let now = epoch.elapsed();
                    let active = Duration::from_millis(last_read.load(Ordering::Acquire)).max(reaped);
                    if now.saturating_sub(active) >= CONPTY_TAIL_IDLE || now.saturating_sub(reaped) >= CONPTY_TAIL_CAP {
                        break;
                    }
                    // The child already exited on its own; this wait is only
                    // extra delay we add for conhost. A cancel or the runtime
                    // deadline therefore just ends it (close now) and must
                    // not turn the natural exit into Cancelled/RuntimeTimeout
                    // or stop forwarding the tail.
                    tokio::select! {
                        _ = tokio::time::sleep(CONPTY_TAIL_POLL) => {},
                        _ = cancelled.recv() => { tail_cut = true; break; },
                        _ = &mut deadline, if runtime_ms != 0 => {
                            // A completed `Sleep` stays ready; push it out so
                            // the EOF wait below does not report a timeout.
                            deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 60 * 60));
                            tail_cut = true;
                            break;
                        },
                    }
                }
            }
            // Bound only the close itself; the output EOF is awaited below
            // exactly as on Unix, so a slow consumer still gets every byte.
            let slot = console.0.clone();
            let closing = tokio::task::spawn_blocking(move || drop(take_console(&slot)));
            match tokio::time::timeout(CONPTY_CLOSE_GRACE, closing).await {
                Ok(Ok(())) => false,
                // The blocking pool refused or lost the task: close on a
                // dedicated thread instead and treat it like a stall.
                Ok(Err(_)) => { console.close_detached(); true },
                Err(_) => true,
            }
        };
        // Only if the close stalled (or a reader blocked by a consumer is
        // keeping it from completing) stop waiting for EOF after a bounded
        // fallback, so `kill`/`wait` can never hang. Unix never needs this.
        //
        // The same bound applies when the tail wait was cut (`tail_cut`):
        // the child is reaped and the console was just closed, so the reader
        // normally reaches EOF promptly and `read_completion` wins. But a
        // cancel consumed above cannot be observed again here, and the
        // deadline was disarmed, so if the reader is stuck delivering to a
        // consumer that stopped reading, nothing else would end this wait
        // and the `kill` that sent the cancel would hang.
        #[cfg(windows)]
        let eof_fallback = async move {
            if close_stalled || tail_cut { tokio::time::sleep(CONPTY_CLOSE_GRACE).await } else { std::future::pending::<()>().await }
        };
        #[cfg(not(windows))]
        let eof_fallback = std::future::pending::<()>();
        tokio::pin!(eof_fallback);
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
            _ = &mut eof_fallback => {},
        }
        let outcome = if budget.exceeded.load(Ordering::Acquire) { RunOutcome::Rejected } else { outcome };
        completion.send_replace(Some(outcome));
    });
    Ok(())
}

#[cfg(not(windows))]
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

/// Windows `pty_send`: additionally calls `stamp` after each delivery and on
/// every retry while the channel is full, keeping the ConPTY tail idle-wait
/// from treating a consumer-blocked reader as an idle console.
#[cfg(windows)]
fn pty_send_stamped(tx: &mpsc::Sender<Result<Vec<u8>, BackendError>>, mut message: Result<Vec<u8>, BackendError>,
    budget: &OutputBudget, stamp: &dyn Fn()) -> bool
{
    loop {
        if budget.stopping.load(Ordering::Acquire) { return false; }
        match tx.try_send(message) {
            Ok(()) => { stamp(); return true },
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(value)) => {
                message = value;
                stamp();
                std::thread::sleep(Duration::from_millis(5));
            },
        }
    }
}

// Only the ConPTY startup-handshake timeout requires cancelling the child.
#[cfg(windows)]
fn pty_write_timed_out<T>(result: &std::io::Result<T>) -> bool {
    matches!(result, Err(error) if error.kind() == std::io::ErrorKind::TimedOut)
}

#[cfg(all(test, unix))]
mod binding_tests {
    use super::*;

    #[tokio::test]
    async fn exited_before_binding_preserves_output_and_revokes() {
        let binding = SpawnBinding::pending(Instant::now() + Duration::from_secs(30));
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf 'output\\n'; printf 'error\\n' >&2; exit 7"])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // Synchronize on exit without sleeps; inject the capture/bind failure
        // so this exercises the recovery even on platforms with different identities.
        assert_eq!(child.wait().await.unwrap().code(), Some(7));
        let bound = Err(std::io::Error::other("process identity unavailable or exited"));
        assert!(finish_unix_binding(&binding, bound, || child.try_wait()).unwrap());
        assert!(binding.root().is_none());
        assert!(binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).is_err());
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"output\n");
        assert_eq!(output.stderr, b"error\n");
    }

    #[test]
    fn unconfirmed_exit_preserves_binding_error() {
        for status in [Ok(None::<()>), Err(std::io::Error::other("wait failed"))] {
            let binding = SpawnBinding::pending(Instant::now() + Duration::from_secs(30));
            let bound = Err(std::io::Error::other("binding failed"));
            let result: std::io::Result<bool> = finish_unix_binding(&binding, bound, || status);
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), "binding failed");
            // The caller's existing failure path retains responsibility for
            // killing the child and dropping its revocation guard.
            drop(RevokeOnDrop(Some(binding.clone())));
            assert!(binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).is_err());
        }
    }
}

#[cfg(all(test, windows))]
mod pty_write_tests {
    use super::pty_write_timed_out;

    #[test]
    fn only_startup_timeout_requires_cancellation() {
        use std::io::{Error, ErrorKind};
        assert!(pty_write_timed_out(&Err::<(), _>(Error::from(ErrorKind::TimedOut))));
        for kind in [ErrorKind::BrokenPipe, ErrorKind::UnexpectedEof, ErrorKind::Other] {
            assert!(!pty_write_timed_out(&Err::<(), _>(Error::from(kind))));
        }
        assert!(!pty_write_timed_out(&Ok(())));
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

