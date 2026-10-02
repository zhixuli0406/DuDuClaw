//! Test runner — run a branch's configured `test_command` against its workspace
//! snapshot and feed the exit code into the judge (RFC-26 §3.3, P2).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::error::{ForkError, Result};

/// Synthetic exit code used when the test command is killed for exceeding its
/// timeout (mirrors the conventional `timeout(1)` exit status).
pub const TIMEOUT_EXIT_CODE: i32 = 124;

const TAIL_BYTES: usize = 4096;

/// Outcome of running a branch's test command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    pub exit_code: i32,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub timed_out: bool,
}

impl TestOutcome {
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Run `command` in `workspace`. Returns `Ok(None)` when no command is configured
/// (skip ⇒ branch's `test_exit_code` stays `None`, neutral in the judge).
///
/// On timeout the command's whole process group (Unix) / process tree
/// (Windows) is killed — background grandchildren included — and a
/// [`TestOutcome`] with `exit_code = TIMEOUT_EXIT_CODE`, `timed_out = true` is
/// returned. Captured output is bounded to the last 64 KiB per stream while
/// running, and reported as a CJK-safe tail.
pub async fn run_test(
    workspace: &Path,
    command: Option<&str>,
    timeout_s: u64,
) -> Result<Option<TestOutcome>> {
    let command = match command.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => c,
        None => return Ok(None),
    };
    if !workspace.is_dir() {
        return Err(ForkError::Executor(format!(
            "test workspace is not a directory: {}",
            workspace.display()
        )));
    }

    let mut cmd = shell_command(command);
    cmd.current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| ForkError::Executor(format!("spawn test command: {e}")))?;
    let pid = child.id();

    // Drain both pipes concurrently into bounded tail buffers so a chatty test
    // can neither deadlock on a full pipe nor grow memory without limit.
    let stdout_task = child.stdout.take().map(|s| tokio::spawn(read_bounded_tail(s)));
    let stderr_task = child.stderr.take().map(|s| tokio::spawn(read_bounded_tail(s)));

    let dur = Duration::from_secs(timeout_s.max(1));
    let (exit_code, timed_out) = match tokio::time::timeout(dur, child.wait()).await {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(e)) => {
            kill_process_tree(pid).await;
            let _ = child.start_kill();
            return Err(ForkError::Executor(format!("test command io error: {e}")));
        }
        Err(_elapsed) => {
            // The child is still alive (not reaped), so its pid — which is also
            // its process-group id — cannot have been reused: killing the group
            // reaches every descendant that stayed in it (e.g. `sleep 300 &`).
            kill_process_tree(pid).await;
            let _ = child.start_kill();
            let _ = child.wait().await; // reap
            (TIMEOUT_EXIT_CODE, true)
        }
    };

    // A descendant that escaped the group (its own `setsid`) could hold a pipe
    // open forever; never let that hang the fork.
    let stdout = join_tail(stdout_task).await;
    let mut stderr = join_tail(stderr_task).await;
    if timed_out {
        if !stderr.is_empty() {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(format!("timed out after {timeout_s}s").as_bytes());
    }
    Ok(Some(TestOutcome {
        exit_code,
        stdout_tail: tail(&stdout),
        stderr_tail: tail(&stderr),
        timed_out,
    }))
}

/// Maximum bytes kept per stream while the test runs (only the tail survives).
const BUFFER_CAP: usize = 64 * 1024;

/// How long to wait for the output readers after the command ended.
const READER_GRACE: Duration = Duration::from_secs(3);

/// Read `stream` to EOF keeping at most the last [`BUFFER_CAP`] bytes.
async fn read_bounded_tail<R: tokio::io::AsyncRead + Unpin>(mut stream: R) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > BUFFER_CAP * 2 {
                    let drop_n = buf.len() - BUFFER_CAP;
                    buf.drain(..drop_n);
                }
            }
        }
    }
    if buf.len() > BUFFER_CAP {
        let drop_n = buf.len() - BUFFER_CAP;
        buf.drain(..drop_n);
    }
    buf
}

async fn join_tail(task: Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    let Some(mut task) = task else { return Vec::new() };
    match tokio::time::timeout(READER_GRACE, &mut task).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => Vec::new(),
        Err(_) => {
            task.abort();
            Vec::new()
        }
    }
}

/// Kill the test command together with everything it spawned.
///
/// Unix: the command runs as the leader of its own process group
/// (`process_group(0)`), so `kill(-pgid, SIGKILL)` reaches every descendant
/// that stayed in the group. Windows: the command gets its own process group
/// (`CREATE_NEW_PROCESS_GROUP`) and `taskkill /T /F` kills the process tree.
async fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // The leader's pid is its pgid (`process_group(0)`). The helper
        // refuses 0/1/out-of-range values instead of signalling them.
        if let Err(e) = duduclaw_core::platform::kill_process_group(pid) {
            tracing::debug!("kill test process group {pid}: {e}");
        }
    }
    #[cfg(windows)]
    {
        let res = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Err(e) = res {
            tracing::debug!("taskkill test process tree {pid}: {e}");
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

/// Build a shell invocation for the host platform, in its own process group so
/// a timeout can kill the whole tree.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c.creation_flags(CREATE_NEW_PROCESS_GROUP);
        c.kill_on_drop(true);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        #[cfg(unix)]
        c.process_group(0);
        c.kill_on_drop(true);
        c
    }
}

/// CJK-safe tail of captured output (last `TAIL_BYTES`, walked back to a char
/// boundary by `truncate_bytes`).
fn tail(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= TAIL_BYTES {
        return s.into_owned();
    }
    // Take the last TAIL_BYTES worth, moving the start forward to a char
    // boundary (a raw byte index can land mid-character on CJK/emoji output).
    let mut start = s.len() - TAIL_BYTES;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s.get(start..).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_command_skips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_test(dir.path(), None, 10).await.unwrap(), None);
        assert_eq!(run_test(dir.path(), Some("   "), 10).await.unwrap(), None);
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn passing_command_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_test(dir.path(), Some("true"), 10).await.unwrap().unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.passed());
        assert!(!out.timed_out);
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn failing_command_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_test(dir.path(), Some("exit 3"), 10).await.unwrap().unwrap();
        assert_eq!(out.exit_code, 3);
        assert!(!out.passed());
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn runs_in_workspace_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "hi").unwrap();
        // `test -f marker.txt` passes only if cwd is the workspace.
        let out = run_test(dir.path(), Some("test -f marker.txt"), 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn timeout_kills_and_marks() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_test(dir.path(), Some("sleep 5"), 1).await.unwrap().unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, TIMEOUT_EXIT_CODE);
        assert!(!out.passed());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_kills_background_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_test(dir.path(), Some("sleep 30 & echo $! > bg.pid; wait"), 1)
            .await
            .unwrap()
            .unwrap();
        assert!(out.timed_out);
        let pid = std::fs::read_to_string(dir.path().join("bg.pid")).unwrap();
        let pid = pid.trim().to_string();
        assert!(!pid.is_empty());
        // The orphaned grandchild is reaped by init/launchd shortly after the
        // SIGKILL; poll until `kill -0` reports it gone.
        let mut alive = true;
        for _ in 0..50 {
            let st = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap();
            if !st.success() {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive, "background grandchild {pid} survived the timeout");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn output_is_captured_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        // ~200 KB of output: only the tail is kept.
        let out = run_test(
            dir.path(),
            Some("i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789012345; i=$((i+1)); done; echo END"),
            20,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout_tail.len() <= TAIL_BYTES);
        assert!(out.stdout_tail.trim_end().ends_with("END"));
    }

    #[tokio::test]
    async fn nonexistent_workspace_errors() {
        let p = Path::new("/nonexistent/duduclaw_fork_tr");
        assert!(run_test(p, Some("true"), 5).await.is_err());
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{
                OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        };

        // Keep the original process object pinned: a reused PID cannot make a
        // surviving descendant look gone (or target another process in cleanup).
        struct TestProcess {
            handle: HANDLE,
            pid: u32,
        }
        impl TestProcess {
            fn open(pid: u32) -> Self {
                assert!(pid > 1, "invalid fixture PID");
                let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
                assert!(
                    !handle.is_null(),
                    "open fixture PID {pid}: {}",
                    std::io::Error::last_os_error()
                );
                Self { handle, pid }
            }

            fn running(&self) -> bool {
                match unsafe { WaitForSingleObject(self.handle, 0) } {
                    WAIT_TIMEOUT => true,
                    WAIT_OBJECT_0 => false,
                    other => panic!("fixture process wait failed: {other}"),
                }
            }
        }
        impl Drop for TestProcess {
            fn drop(&mut self) {
                // On assertion failure clean only this test's pinned processes.
                unsafe {
                    if WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT {
                        // Holding the process handle prevents PID reuse. Also
                        // clean a child born just before a startup assertion.
                        let _ = std::process::Command::new("taskkill")
                            .args(["/T", "/F", "/PID", &self.pid.to_string()])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                        TerminateProcess(self.handle, 1);
                        WaitForSingleObject(self.handle, 5_000);
                    }
                    CloseHandle(self.handle);
                }
            }
        }

        #[tokio::test]
        async fn passing_command_exit_zero() {
            let dir = tempfile::tempdir().unwrap();
            let out = run_test(dir.path(), Some("echo WINDOWS_OK & exit /b 0"), 10)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(out.exit_code, 0);
            assert!(out.passed());
            assert!(!out.timed_out);
            assert!(out.stdout_tail.contains("WINDOWS_OK"));
        }

        #[tokio::test]
        async fn failing_command_nonzero() {
            let dir = tempfile::tempdir().unwrap();
            let out = run_test(dir.path(), Some("exit /b 3"), 10)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(out.exit_code, 3);
            assert!(!out.passed());
            assert!(!out.timed_out);
        }

        #[tokio::test]
        async fn runs_in_workspace_cwd() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("marker.txt"), "hi").unwrap();
            let out = run_test(
                dir.path(),
                Some("if exist marker.txt (exit /b 0) else (exit /b 3)"),
                10,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(out.exit_code, 0);
        }

        #[tokio::test]
        async fn timeout_kills_and_marks() {
            let dir = tempfile::tempdir().unwrap();
            let out = run_test(dir.path(), Some("powershell.exe -NoLogo -NoProfile -NonInteractive -Command Start-Sleep -Seconds 30"), 1)
                    .await.unwrap().unwrap();
            assert!(out.timed_out);
            assert_eq!(out.exit_code, TIMEOUT_EXIT_CODE);
            assert!(!out.passed());
        }

        #[tokio::test]
        async fn timeout_kills_child_and_grandchild() {
            let dir = tempfile::tempdir().unwrap();
            // cmd -> PowerShell parent -> child -> grandchild. Each process
            // waits for the test to pin its handle before creating the next.
            // No scripts or credentials outside this private workspace are used.
            std::fs::write(dir.path().join("tree.ps1"), r#"
    param([int]$Depth)
    $ErrorActionPreference = 'Stop'
    [System.IO.File]::WriteAllText((Join-Path $PSScriptRoot "pid-$Depth"), [string]$PID)
    while (-not (Test-Path (Join-Path $PSScriptRoot "go-$Depth"))) { Start-Sleep -Milliseconds 25 }
    if ($Depth -lt 2) {
        $executable = (Get-Process -Id $PID).Path
        $arguments = @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'), '-Depth', [string]($Depth + 1))
        Start-Process -FilePath $executable -ArgumentList $arguments -NoNewWindow -PassThru | Out-Null
    }
    Start-Sleep -Seconds 120
    "#).unwrap();
            let command = "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File tree.ps1 -Depth 0";
            let execution = run_test(dir.path(), Some(command), 20);
            tokio::pin!(execution);
            let mut processes = Vec::new();
            let startup_deadline = tokio::time::Instant::now() + Duration::from_secs(12);
            for depth in 0..3 {
                let pid_file = dir.path().join(format!("pid-{depth}"));
                let pid = loop {
                    if let Ok(value) = std::fs::read_to_string(&pid_file) {
                        if let Ok(pid) = value.trim().parse::<u32>() {
                            break pid;
                        }
                    }
                    assert!(
                        tokio::time::Instant::now() < startup_deadline,
                        "fixture process {depth} did not start"
                    );
                    tokio::select! {
                        result = &mut execution => panic!("test command ended before the process tree was ready: {result:?}"),
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                    }
                };
                let process = TestProcess::open(pid);
                assert!(
                    process.running(),
                    "fixture process {depth} exited before timeout"
                );
                processes.push(process);
                std::fs::write(dir.path().join(format!("go-{depth}")), "ready").unwrap();
            }
            assert!(
                processes.iter().all(TestProcess::running),
                "fixture tree is not alive before timeout"
            );
            let out = tokio::time::timeout(Duration::from_secs(30), execution)
                .await
                .expect("Windows timeout cleanup hung")
                .unwrap()
                .unwrap();
            assert!(out.timed_out);
            assert_eq!(out.exit_code, TIMEOUT_EXIT_CODE);
            assert!(!out.passed());
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while processes.iter().any(TestProcess::running) && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            for (depth, process) in processes.iter().enumerate() {
                assert!(
                    !process.running(),
                    "Windows descendant {depth} survived the timeout"
                );
            }
        }
    }
}
