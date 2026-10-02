//! Bounded subprocess transport shared by attempts and evaluators.
//! Child groups are terminated on normal exit, timeout and future cancellation.

use std::io;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub stopped: bool,
    pub output_truncated: bool,
    pub wall_secs: f64,
}

struct GroupGuard(u32);
impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = duduclaw_core::platform::kill_process_group(self.0);
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.0.to_string(), "/T", "/F"])
                .output();
        }
    }
}

async fn drain<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n.min(cap.saturating_sub(bytes.len()))]);
    }
    Ok(bytes)
}

struct AbortTask<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) { if let Some(task) = &self.0 { task.abort(); } }
}
async fn finish_stderr(
    mut task: AbortTask<io::Result<Vec<u8>>>, remaining: Duration,
) -> io::Result<(Vec<u8>, bool)> {
    match tokio::time::timeout(remaining, task.0.as_mut().expect("stderr task present")).await {
        Ok(result) => {
            task.0.take();
            Ok((result.map_err(io::Error::other)??, false))
        }
        Err(_) => Ok((b"stderr drain exceeded process deadline".to_vec(), true)),
    }
}

/// `on_event` receives each complete stdout line (bounded at 1 MiB).
/// Return false to stop a process whose live cost exceeds its shared budget.
pub async fn run<F>(
    command: Command,
    input: &[u8],
    timeout: Duration,
    stdout_cap: usize,
    mut on_event: F,
) -> io::Result<ProcessOutput>
where
    F: FnMut(&[u8]) -> bool + Send,
{
    let started = Instant::now();
    let mut cmd = tokio::process::Command::from(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    let mut child = cmd.spawn()?;
    let group = GroupGuard(
        child
            .id()
            .ok_or_else(|| io::Error::other("missing child id"))?,
    );
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing stdin"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    let input_writer = async move {
        let result = match stdin.write_all(input).await {
            Ok(()) => stdin.shutdown().await,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err(e),
        };
        drop(stdin);
        result
    };
    let mut output = Vec::new();
    let mut stopped = false;
    let mut truncated = false;
    let event_reader = async {
        let mut line = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                if !line.is_empty() && !on_event(&line) {
                    stopped = true;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "event requested stop"));
                }
                return Ok::<_, io::Error>(());
            }
            if output.len() + n > stdout_cap {
                truncated = true;
            }
            output.extend_from_slice(&buf[..n.min(stdout_cap.saturating_sub(output.len()))]);
            for b in &buf[..n] {
                if *b == b'\n' {
                    if !on_event(&line) {
                        stopped = true;
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "event requested stop"));
                    }
                    line.clear();
                } else if line.len() < MAX_EVENT_BYTES {
                    line.push(*b);
                } else {
                    truncated = true;
                    stopped = true;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "event exceeded limit"));
                }
            }
        }
    };
    let err_task = AbortTask(Some(tokio::spawn(drain(stderr, 64 * 1024))));
    // stderr is drained concurrently; stdout and stdin cannot deadlock one another.
    let communicate = async { tokio::try_join!(input_writer, event_reader).map(|_| ()) };
    let mut timed_out = match tokio::time::timeout(timeout, communicate).await {
        Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => false,
        Ok(result) => {
            result?;
            false
        }
        Err(_) => true,
    };
    if timed_out || stopped {
        drop(group);
        let _ = child.start_kill();
    } else {
        // The CLI may close stdout before it exits. Include its exit in the cap.
        let remaining = timeout.saturating_sub(started.elapsed());
        if tokio::time::timeout(remaining, child.wait()).await.is_err() {
            drop(group);
            let _ = child.start_kill();
            let status = child.wait().await?;
            let (stderr, _) = finish_stderr(err_task, timeout.saturating_sub(started.elapsed())).await?;
            return Ok(ProcessOutput {
                status,
                stdout: String::from_utf8_lossy(&output).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                timed_out: true,
                stopped,
                output_truncated: truncated,
                wall_secs: started.elapsed().as_secs_f64(),
            });
        }
        drop(group);
    }
    let status = child.wait().await?;
    let remaining = timeout.saturating_sub(started.elapsed());
    // A requested stop must not wait out the original execution allowance
    // merely because a descendant retained stderr. Preserve the explicit
    // stop reason and mark the incomplete diagnostic stream as truncated.
    let drain_deadline = if stopped { remaining.min(Duration::from_millis(100)) } else { remaining };
    let (stderr, stderr_timeout) = finish_stderr(err_task, drain_deadline).await?;
    if stopped { truncated |= stderr_timeout; } else { timed_out |= stderr_timeout; }
    Ok(ProcessOutput {
        status,
        stdout: String::from_utf8_lossy(&output).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
        stopped,
        output_truncated: truncated,
        wall_secs: started.elapsed().as_secs_f64(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stderr_is_drained_and_stdout_events_continue_after_capture_cap() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "i=0; while [ $i -lt 1000 ]; do echo event; echo error >&2; i=$((i+1)); done",
        ]);
        let mut events = 0;
        let out = run(cmd, b"", Duration::from_secs(5), 12, |_| {
            events += 1;
            true
        })
        .await
        .unwrap();
        assert!(out.status.success());
        assert_eq!(events, 1000);
        assert!(out.output_truncated);
        assert_eq!(out.stdout.len(), 12);
    }
    #[tokio::test]
    async fn input_writer_closes_pipe_for_children_that_read_to_eof() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "cat; echo done"]);
        let out = run(cmd, b"prompt\n", Duration::from_secs(2), 1024, |_| true)
            .await.unwrap();
        assert!(out.status.success());
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "prompt\ndone\n");
    }
    #[tokio::test]
    async fn event_stop_interrupts_a_blocked_large_stdin_write() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo stop; sleep 30"]);
        let input = vec![b'x'; 4 * 1024 * 1024];
        let out = run(cmd, &input, Duration::from_secs(10), 1024, |_| false)
            .await.unwrap();
        assert!(out.stopped);
        assert!(!out.timed_out);
        assert!(out.wall_secs < 3.0);
    }
    #[tokio::test]
    async fn detached_stderr_holder_cannot_extend_the_transport_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let mut cmd = Command::new("python3");
        cmd.arg("-c").arg("import os,sys,time; p=os.fork();\nif p==0:\n os.setsid(); open(sys.argv[1],'w').write(str(os.getpid())); time.sleep(5)\n")
            .arg(&pid_file);
        let out = run(cmd, b"", Duration::from_millis(500), 1024, |_| true).await.unwrap();
        if let Ok(pid) = std::fs::read_to_string(pid_file) {
            let _ = duduclaw_core::platform::kill_process(pid.parse().unwrap());
        }
        assert!(out.timed_out);
        assert!(out.wall_secs < 2.0);
    }
    #[tokio::test]
    async fn timeout_includes_descendants_holding_the_pipes() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let out = run(cmd, b"", Duration::from_millis(150), 1024, |_| true)
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(out.wall_secs < 3.0);
    }
}
