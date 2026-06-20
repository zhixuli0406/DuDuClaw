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
/// On timeout the child is killed and a [`TestOutcome`] with
/// `exit_code = TIMEOUT_EXIT_CODE`, `timed_out = true` is returned.
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

    let child = cmd
        .spawn()
        .map_err(|e| ForkError::Executor(format!("spawn test command: {e}")))?;

    let dur = Duration::from_secs(timeout_s.max(1));
    match tokio::time::timeout(dur, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let code = output.status.code().unwrap_or(-1);
            Ok(Some(TestOutcome {
                exit_code: code,
                stdout_tail: tail(&output.stdout),
                stderr_tail: tail(&output.stderr),
                timed_out: false,
            }))
        }
        Ok(Err(e)) => Err(ForkError::Executor(format!("test command io error: {e}"))),
        Err(_elapsed) => {
            // Timed out: the child handle was consumed by wait_with_output, which
            // on timeout we can't reach — rely on Stdio::piped drop + kill_on_drop.
            Ok(Some(TestOutcome {
                exit_code: TIMEOUT_EXIT_CODE,
                stdout_tail: String::new(),
                stderr_tail: format!("timed out after {timeout_s}s"),
                timed_out: true,
            }))
        }
    }
}

/// Build a shell invocation for the host platform.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c.kill_on_drop(true);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
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
    // Take the last TAIL_BYTES worth, then snap to a char boundary from the front.
    let start = s.len() - TAIL_BYTES;
    let slice = &s[start..];
    // slice may start mid-char; truncate_bytes from a safe substring.
    duduclaw_core::truncate_bytes(slice, TAIL_BYTES).to_string()
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
    async fn nonexistent_workspace_errors() {
        let p = Path::new("/nonexistent/duduclaw_fork_tr");
        assert!(run_test(p, Some("true"), 5).await.is_err());
    }
}
