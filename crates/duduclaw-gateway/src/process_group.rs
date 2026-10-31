//! Shared CLI process-tree ownership and bounded output collection.
//!
//! Stdio MCP clients use the same core helper, including the Windows Job Object.
pub use duduclaw_core::process_tree::{GroupKillGuard, ManagedChild, isolate, spawn_guarded};

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
/// Version probes use this path: it does not take an autonomy lease.
pub async fn output(
    cmd: &mut tokio::process::Command,
    input: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, OutputError> {
    collect_output(cmd, input, timeout, false).await
}

/// Model spawn. One admission lease is held across `spawn` and released before
/// stdin or pipe draining, so a pause cannot land between the check and a
/// running process, and the lease is not held for the whole deadline.
pub async fn output_for_model(
    cmd: &mut tokio::process::Command,
    input: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, OutputError> {
    collect_output(cmd, input, timeout, true).await
}

async fn collect_output(
    cmd: &mut tokio::process::Command,
    input: Option<&[u8]>,
    timeout: std::time::Duration,
    admit: bool,
) -> Result<std::process::Output, OutputError> {
    use tokio::io::AsyncWriteExt;
    use std::process::Stdio;
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    let admission = if admit {
        Some(
            crate::autonomous_runtime::admit_current()
                .await
                .map_err(|error| OutputError::Io(std::io::Error::other(error)))?,
        )
    } else {
        None
    };
    let mut managed = ManagedChild::spawn(cmd).map_err(OutputError::Io)?;
    if admission.is_some() {
        crate::autonomous_runtime::mark_handoff();
    }
    drop(admission);
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
        let (child, guard) = spawn_guarded(&mut cmd).unwrap();
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
        let (mut child, mut guard) = spawn_guarded(&mut cmd).unwrap();
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
