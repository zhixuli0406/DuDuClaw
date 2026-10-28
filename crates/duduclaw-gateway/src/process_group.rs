//! Kill a spawned CLI together with everything it started.
//!
//! `kill_on_drop` only signals the direct child. The Claude CLI starts its own
//! children (MCP servers, tool subprocesses), and a stub or wrapper script
//! may run the real work in a grandchild; when a generation job is cancelled
//! or times out, those used to survive as orphans. The CLI is spawned as the
//! leader of its own process group ([`isolate`]), and [`GroupKillGuard`]
//! sends `SIGKILL` to the whole group when it is dropped while still armed —
//! which is exactly what happens when the awaiting task is aborted, or when a
//! timeout path returns early. Normal completion releases the guard; Windows
//! also terminates any remaining descendants when closing its job handle.
//!
//! Windows owns a kill-on-close Job Object assigned before resuming the child. Unix
//! uses an isolated process group. Cancellation also schedules leader reaping.

/// Put the command's child in a new process group led by itself (Unix).
pub fn isolate(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Kills the child's process group on drop unless [`disarm`]ed.
///
/// Declare it AFTER the `Child` it watches, so it drops first: the leader is
/// then still unreaped (at worst a zombie), its pid cannot have been reused,
/// and the group id is still valid.
///
/// [`disarm`]: GroupKillGuard::disarm
pub struct GroupKillGuard {
    #[cfg(not(windows))]
    pgid: Option<u32>,
    #[cfg(windows)]
    job: Option<duduclaw_core::platform::WindowsJob>,
}

impl GroupKillGuard {
    /// `pid` is the child's pid (= its process-group id after [`isolate`]).
    #[cfg(not(windows))]
    pub fn new(pid: Option<u32>) -> Self {
        Self { pgid: pid }
    }

    /// Kill the whole group now (timeout / error paths that then reap the
    /// leader themselves) and disarm.
    pub fn kill_now(&mut self) {
        self.kill_group();
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            match killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(e) => tracing::warn!(pgid, error = %e, "killpg of CLI process group failed"),
            }
        }
        #[cfg(windows)]
        if let Some(mut job) = self.job.take() {
            if let Err(error) = job.terminate() {
                tracing::warn!(%error, "terminating CLI job failed; closed kill-on-close handle");
            }
        }
        #[cfg(not(any(unix, windows)))]
        { self.pgid = None; }
    }

    /// Release ownership after reaping the leader. Windows closes the job and
    /// terminates any remaining descendants even on a normal exit.
    pub fn disarm(&mut self) {
        #[cfg(not(windows))]
        { self.pgid = None; }
        #[cfg(windows)]
        { self.kill_group(); }
    }
}

impl Drop for GroupKillGuard {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// Spawn with tree ownership established before any child code executes.
/// Declare the returned guard after the child so tree termination runs first.
pub fn spawn_guarded(cmd: &mut tokio::process::Command)
    -> std::io::Result<(tokio::process::Child, GroupKillGuard)>
{
    isolate(cmd);
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::io::BorrowedHandle;
        let job = duduclaw_core::platform::WindowsJob::new()?;
        // Current managed callers and async_command_for set no Windows flags.
        // Keep creation flags owned here: std/Tokio provide no flags getter.
        cmd.creation_flags(duduclaw_core::platform::WindowsJob::CREATION_FLAGS);
        let mut child = cmd.spawn()?;
        let result = match child.raw_handle() {
            Some(raw) => {
                // SAFETY: child owns this live handle until after the synchronous call.
                job.assign_and_resume(unsafe { BorrowedHandle::borrow_raw(raw) })
            }
            None => Err(std::io::Error::other("spawned child has no process handle")),
        };
        if let Err(error) = result {
            // Assignment/resume failure must never leave an unmanaged child.
            drop(job);
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { let _ = child.wait().await; });
            }
            return Err(error);
        }
        Ok((child, GroupKillGuard { job: Some(job) }))
    }
    #[cfg(not(windows))]
    {
        let child = cmd.spawn()?;
        let guard = GroupKillGuard::new(child.id());
        Ok((child, guard))
    }
}

/// Own the leader until its process group has been terminated or disarmed.
/// Unlike dropping `wait_with_output`, cancellation explicitly kills the tree
/// before handing the leader to a reaper task.
pub struct ManagedChild {
    child: Option<tokio::process::Child>,
    guard: GroupKillGuard,
}

impl ManagedChild {
    pub fn spawn(cmd: &mut tokio::process::Command) -> std::io::Result<Self> {
        let (child, guard) = spawn_guarded(cmd)?;
        Ok(Self { child: Some(child), guard })
    }

    pub fn child(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("managed leader is present")
    }

    /// The leader has exited; do not retain a reaped leader's PGID while
    /// waiting for pipe EOF. No descendant may outlive this one-shot call.
    pub fn leader_exited(&mut self) {
        self.guard.kill_now();
    }

    pub fn disarm(&mut self) {
        self.guard.disarm();
        self.child = None;
    }

    pub async fn kill_and_reap(&mut self) {
        self.guard.kill_now();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            // Tokio's Child drop remains a final backstop if this wait itself
            // is cancelled. Normal timeout/error paths reap explicitly.
            let _ = child.wait().await;
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.guard.kill_now();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { let _ = child.wait().await; });
            }
        }
    }
}

#[derive(Debug)]
pub enum OutputError {
    Timeout,
    Io(std::io::Error),
}

const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Always consume EOF, retaining either a bounded prefix (stdout) or bounded
/// tail (diagnostics). Returning early on overflow would deadlock the writer.
async fn drain_bounded<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    retained: &mut Vec<u8>,
    limit: usize,
    tail: bool,
) -> std::io::Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 { return Ok(truncated); }
        let bytes = &buffer[..count];
        if retained.len().saturating_add(count) > limit && !truncated {
            truncated = true;
            tracing::warn!(limit, stream = if tail { "stderr" } else { "stdout" },
                "CLI output retention exceeded; continuing to drain the pipe");
        }
        if tail {
            let remove = retained.len().saturating_add(count).saturating_sub(limit).min(retained.len());
            retained.drain(..remove);
            retained.extend_from_slice(&bytes[count.saturating_sub(limit)..]);
        } else {
            let keep = count.min(limit.saturating_sub(retained.len()));
            retained.extend_from_slice(&bytes[..keep]);
        }
    }
}

/// Collect a CLI while keeping ownership of its leader. The deadline includes
/// writing stdin and draining both pipes, so a stalled reader cannot outlive it.
pub async fn output(
    cmd: &mut tokio::process::Command,
    input: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, OutputError> {
    use tokio::io::AsyncWriteExt;
    use std::process::Stdio;
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut managed = ManagedChild::spawn(cmd).map_err(OutputError::Io)?;
    let mut stdin = managed.child().stdin.take();
    let mut stdout = managed.child().stdout.take().expect("piped stdout");
    let mut stderr = managed.child().stderr.take().expect("piped stderr");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            async {
                if let (Some(sink), Some(bytes)) = (stdin.as_mut(), input) {
                    // A CLI may close stdin early after reading its prompt.
                    let _ = sink.write_all(bytes).await;
                    let _ = sink.shutdown().await;
                }
                drop(stdin.take());
                Ok::<(), std::io::Error>(())
            },
            drain_bounded(&mut stdout, &mut out, MAX_STDOUT_BYTES, false),
            drain_bounded(&mut stderr, &mut err, MAX_STDERR_BYTES, true),
            async {
                let status = managed.child().wait().await?;
                managed.leader_exited();
                Ok::<_, std::io::Error>(status)
            },
        )
    }).await;
    match result {
        Ok(Ok((_, stdout_truncated, _, status))) => {
            if status.success() && !stdout_truncated { managed.disarm(); }
            else { managed.kill_and_reap().await; }
            if stdout_truncated {
                // A partial JSON/text reply must never be accepted as a complete
                // utility answer, even if the subprocess reported exit zero.
                return Err(OutputError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData, "CLI stdout exceeded the 8 MiB retention limit",
                )));
            }
            Ok(std::process::Output { status, stdout: out, stderr: err })
        }
        result => {
            managed.kill_and_reap().await;
            match result {
                Err(_) => Err(OutputError::Timeout),
                Ok(Err(error)) => Err(OutputError::Io(error)),
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    #[tokio::test]
    async fn bounded_readers_drain_all_bytes_and_keep_prefix_or_tail() {
        let bytes = vec![b'x'; 20000];
        let mut source = bytes.as_slice();
        let mut prefix = Vec::new();
        assert!(drain_bounded(&mut source, &mut prefix, 16, false).await.unwrap());
        assert!(source.is_empty(), "overflow must not stop draining");
        assert_eq!(prefix, vec![b'x'; 16]);

        let bytes = [bytes, b"last diagnostic".to_vec()].concat();
        let mut source = bytes.as_slice();
        let mut tail = Vec::new();
        assert!(drain_bounded(&mut source, &mut tail, 15, true).await.unwrap());
        assert!(source.is_empty());
        assert_eq!(tail, b"last diagnostic");
    }

    #[tokio::test]
    async fn bounded_reader_preserves_normal_bytes_and_exact_limit() {
        for tail in [false, true] {
            let mut source = &b"normal output"[..];
            let mut retained = Vec::new();
            assert!(!drain_bounded(&mut source, &mut retained, 13, tail).await.unwrap());
            assert_eq!(retained, b"normal output");
            assert!(source.is_empty());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    #[tokio::test]
    async fn output_stdout_overflow_drains_pipe_then_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", dir.path()).current_dir(dir.path())
            .args(["-c", "/bin/dd if=/dev/zero bs=1048576 count=9 2>/dev/null; printf drained > finished"]);
        let error = output(&mut cmd, None, std::time::Duration::from_secs(5)).await.unwrap_err();
        assert!(matches!(error, OutputError::Io(ref error) if error.kind() == std::io::ErrorKind::InvalidData), "{error:?}");
        assert_eq!(std::fs::read(dir.path().join("finished")).unwrap(), b"drained");
    }

    #[tokio::test]
    async fn output_stderr_overflow_keeps_tail_and_valid_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", dir.path()).current_dir(dir.path())
            .args(["-c", "/bin/dd if=/dev/zero bs=65536 count=4 >&2 2>/dev/null; printf tail >&2; printf ok"]);
        let result = output(&mut cmd, None, std::time::Duration::from_secs(5)).await.unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"ok");
        assert_eq!(result.stderr.len(), MAX_STDERR_BYTES);
        assert!(result.stderr.ends_with(b"tail"));
    }

    async fn assert_gone(pid: i32) {
        for _ in 0..100 {
            if !alive(pid) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("grandchild {pid} survived process cleanup");
    }

    fn hanging_tree(dir: &std::path::Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", dir).current_dir(dir)
            .args(["-c", "/bin/sleep 30 & echo $! > pid; wait"]);
        cmd
    }

    #[tokio::test]
    async fn output_timeout_kills_grandchild_and_reaps_leader() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = hanging_tree(dir.path());
        assert!(matches!(output(&mut cmd, None, std::time::Duration::from_millis(300)).await,
            Err(OutputError::Timeout)));
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap().trim().parse().unwrap();
        assert_gone(pid).await;
    }

    #[tokio::test]
    async fn output_cancellation_kills_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = hanging_tree(dir.path());
        let task = tokio::spawn(async move { output(&mut cmd, None, std::time::Duration::from_secs(60)).await });
        let mut pid = None;
        for _ in 0..100 {
            pid = std::fs::read_to_string(dir.path().join("pid")).ok()
                .and_then(|text| text.trim().parse().ok());
            if pid.is_some() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pid = pid.expect("fixture grandchild started");
        assert!(alive(pid));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_gone(pid).await;
    }

    #[tokio::test]
    async fn output_nonzero_exit_kills_detached_pipe_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", dir.path()).current_dir(dir.path())
            .args(["-c", "/bin/sleep 30 </dev/null >/dev/null 2>&1 & echo $! > pid; exit 1"]);
        let result = output(&mut cmd, None, std::time::Duration::from_secs(2)).await.unwrap();
        assert!(!result.status.success());
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap().trim().parse().unwrap();
        assert_gone(pid).await;
    }

    /// A grandchild (`sleep` started by the shell) dies with the group.
    #[tokio::test]
    async fn dropping_an_armed_guard_kills_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("/bin/sleep 30 & echo $! > '{}'; wait", pidfile.display()))
            .kill_on_drop(true);
        isolate(&mut cmd);
        let child = cmd.spawn().unwrap();
        let guard = GroupKillGuard::new(child.id());
        let mut grandchild = 0;
        for _ in 0..100 {
            if let Some(p) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                grandchild = p;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(grandchild > 0 && alive(grandchild));
        drop(guard);
        drop(child);
        // The killed sleep is reparented and reaped by init; wait for it.
        let mut gone = false;
        for _ in 0..100 {
            if !alive(grandchild) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "grandchild {grandchild} survived the group kill");
    }

    #[tokio::test]
    async fn a_disarmed_guard_signals_nothing() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 0");
        isolate(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let mut guard = GroupKillGuard::new(child.id());
        assert!(child.wait().await.unwrap().success());
        guard.disarm();
        drop(guard); // no panic, no signal
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const ROLE: &str = "DUDUCLAW_WINDOWS_JOB_TEST_ROLE";
    const PIDFILE: &str = "DUDUCLAW_WINDOWS_JOB_TEST_PIDFILE";
    const FIXTURE: &str = "process_group::windows_tests::tree_fixture";

    // Run the test executable itself to avoid shell/PowerShell availability,
    // quoting, or Start-Process detachment semantics in the regression fixture.
    #[test]
    fn tree_fixture() {
        let Ok(role) = std::env::var(ROLE) else { return; };
        if role == "sleeper" {
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        assert!(role == "leader" || role == "middle");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", FIXTURE, "--nocapture"])
            .env(ROLE, if role == "leader" { "middle" } else { "sleeper" })
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        if role == "middle" {
            std::fs::write(std::env::var_os(PIDFILE).unwrap(), child.id().to_string()).unwrap();
            // Intentionally leave the sleeping descendant alive after its
            // parent and grandparent have exited. The job must retain it.
        } else {
            assert!(child.wait().unwrap().success());
        }
    }

    #[tokio::test]
    async fn job_kills_grandchild_after_leader_exit_on_kill_and_drop() {
        for explicit_kill in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let pidfile = dir.path().join("grandchild.pid");
            let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--exact", FIXTURE, "--nocapture"])
                .env(ROLE, "leader").env(PIDFILE, &pidfile)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            let mut managed = ManagedChild::spawn(&mut cmd).unwrap();
            let status = tokio::time::timeout(Duration::from_secs(15), managed.child().wait())
                .await.expect("fixture leader exits").unwrap();
            assert!(status.success());
            let grandchild = std::fs::read_to_string(pidfile).unwrap().parse::<u32>().unwrap();
            assert!(duduclaw_core::platform::process_alive(grandchild),
                "grandchild must still be alive after leader exit to test retained job ownership");
            if explicit_kill { managed.kill_and_reap().await; }
            drop(managed);
            let mut gone = false;
            for _ in 0..100 {
                if !duduclaw_core::platform::process_alive(grandchild) {
                    gone = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(gone, "grandchild {grandchild} survived job cleanup");
        }
    }
}
