//! Shared subprocess execution for OSS scanners.
//!
//! Every scanner is spawned the same way: bounded wall-clock timeout, bounded
//! stdout capture, and a small stderr tail for diagnostics. stdout is always
//! returned (even on a non-zero exit) because several scanners exit non-zero
//! *by design* when they find something (gitleaks/semgrep/cargo-audit all use
//! "exit 1 = findings present") — the caller decides success by whether the
//! captured stdout parses as the expected JSON shape, not by exit code.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

/// Default wall-clock budget per scanner invocation (task spec: "timeout 5 分鐘").
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Default stdout capture cap (task spec: "輸出上限 16MB").
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// stderr is only kept for diagnostics on failure — a small tail is plenty.
const STDERR_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct RunLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub stdout: String,
    /// `true` when stdout was cut off at `max_output_bytes` (the process may
    /// have produced more — closing the read end after the cap causes the
    /// child to see EPIPE on its next write, which naturally ends it well
    /// before the wall-clock timeout in practice).
    pub stdout_truncated: bool,
    pub stderr_tail: String,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    pub timed_out: bool,
    /// Set when the process could not even be spawned (binary vanished
    /// between `which_cli` detection and spawn, permission error, ...).
    pub spawn_error: Option<String>,
}

impl RunOutcome {
    fn spawn_failed(err: String, duration: Duration) -> Self {
        RunOutcome {
            stdout: String::new(),
            stdout_truncated: false,
            stderr_tail: String::new(),
            exit_code: None,
            duration,
            timed_out: false,
            spawn_error: Some(err),
        }
    }

    fn timed_out(duration: Duration) -> Self {
        RunOutcome {
            stdout: String::new(),
            stdout_truncated: false,
            stderr_tail: String::new(),
            exit_code: None,
            duration,
            timed_out: true,
            spawn_error: None,
        }
    }
}

/// Spawn `program args...` in `cwd`, capped by `limits`. Never panics — every
/// failure mode (spawn error, timeout, truncated output) is represented in
/// [`RunOutcome`] instead of an `Err`, since a scanner being unavailable or
/// misbehaving is an expected, reportable outcome for `secaudit`, not an
/// infra failure of the command itself.
pub async fn run_capped(program: &str, args: &[&str], cwd: &Path, limits: &RunLimits) -> RunOutcome {
    let start = Instant::now();
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::spawn_failed(e.to_string(), start.elapsed()),
    };

    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return RunOutcome::spawn_failed("child had no stdout pipe".to_string(), start.elapsed()),
    };
    let mut stderr = match child.stderr.take() {
        Some(s) => s,
        None => return RunOutcome::spawn_failed("child had no stderr pipe".to_string(), start.elapsed()),
    };
    let max = limits.max_output_bytes;

    // stdout and stderr are read CONCURRENTLY (tokio::join!), not
    // sequentially: a scanner that interleaves large stdout with stderr
    // diagnostics can block on a full stderr pipe if nobody drains it while
    // we're still waiting on stdout to finish — sequential reads risk a
    // deadlock that only resolves via the wall-clock timeout. Concurrent
    // reads avoid that.
    let capture = async move {
        let stdout_fut = async {
            let mut buf = Vec::new();
            // Read one byte past the cap so we can tell truncated-vs-exact.
            let _ = (&mut stdout)
                .take((max as u64).saturating_add(1))
                .read_to_end(&mut buf)
                .await;
            let truncated = buf.len() > max;
            buf.truncate(max);
            (buf, truncated)
        };
        let stderr_fut = async {
            let mut buf = Vec::new();
            let _ = (&mut stderr)
                .take(STDERR_TAIL_BYTES as u64)
                .read_to_end(&mut buf)
                .await;
            buf
        };
        let ((stdout_buf, stdout_truncated), stderr_buf) = tokio::join!(stdout_fut, stderr_fut);
        // Both pipes have hit EOF or their cap by this point (dropping
        // `stdout`/`stderr` below closes the read end; a child still writing
        // past the cap sees EPIPE on its next write instead of blocking).
        let status = child.wait().await;
        (stdout_buf, stdout_truncated, stderr_buf, status)
    };

    match tokio::time::timeout(limits.timeout, capture).await {
        Ok((stdout_buf, stdout_truncated, stderr_buf, status)) => RunOutcome {
            stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
            stdout_truncated,
            stderr_tail: String::from_utf8_lossy(&stderr_buf).into_owned(),
            exit_code: status.ok().and_then(|s| s.code()),
            duration: start.elapsed(),
            timed_out: false,
            spawn_error: None,
        },
        Err(_) => RunOutcome::timed_out(start.elapsed()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let outcome = run_capped(
            "sh",
            &["-c", "printf hello"],
            Path::new("."),
            &RunLimits::default(),
        )
        .await;
        assert_eq!(outcome.stdout, "hello");
        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.timed_out);
        assert!(outcome.spawn_error.is_none());
    }

    #[tokio::test]
    async fn nonzero_exit_still_returns_stdout() {
        // Mirrors gitleaks/semgrep/cargo-audit's "exit 1 = findings present"
        // convention — the caller must not treat this as a failure by itself.
        let outcome = run_capped(
            "sh",
            &["-c", "printf data; exit 1"],
            Path::new("."),
            &RunLimits::default(),
        )
        .await;
        assert_eq!(outcome.stdout, "data");
        assert_eq!(outcome.exit_code, Some(1));
    }

    #[tokio::test]
    async fn spawn_error_for_missing_binary() {
        let outcome = run_capped(
            "duduclaw-secaudit-definitely-not-a-real-binary",
            &[],
            Path::new("."),
            &RunLimits::default(),
        )
        .await;
        assert!(outcome.spawn_error.is_some());
    }

    #[tokio::test]
    async fn timeout_is_reported_not_hung() {
        let limits = RunLimits {
            timeout: Duration::from_millis(50),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        };
        let outcome = run_capped("sh", &["-c", "sleep 5"], Path::new("."), &limits).await;
        assert!(outcome.timed_out);
    }

    #[tokio::test]
    async fn output_beyond_cap_is_truncated_and_flagged() {
        let limits = RunLimits {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: 10,
        };
        let outcome = run_capped(
            "sh",
            &["-c", "printf '%050d' 0"],
            Path::new("."),
            &limits,
        )
        .await;
        assert_eq!(outcome.stdout.len(), 10);
        assert!(outcome.stdout_truncated);
    }

    #[tokio::test]
    async fn stderr_tail_is_captured() {
        let outcome = run_capped(
            "sh",
            &["-c", "printf oops 1>&2"],
            Path::new("."),
            &RunLimits::default(),
        )
        .await;
        assert_eq!(outcome.stderr_tail, "oops");
    }
}
