//! One-shot PTY invocation — spawn a CLI under a PTY, capture stdout, await exit.
//!
//! Unlike [`crate::session::PtySession`] (which keeps the child alive for many
//! invokes), `oneshot_pty_invoke` mirrors the lifecycle of a classic
//! `tokio::process::Command` call: spawn → drain → reap. This is the function
//! the gateway will plug in at the `call_claude_cli_rotated` wedge point.
//!
//! Cross-platform notes:
//! - Uses [`crate::pty::PtyHandle`] under the hood → ConPTY on Windows 10
//!   1809+, openpty on Unix. The CLI sees a real TTY in every case.
//! - We do NOT inject sentinel framing here. One-shot invocation lets the
//!   child print whatever it wants to stdout (e.g. Claude CLI's
//!   `--output-format stream-json` stream). The caller parses it.
//! - Stdout is read until EOF (= child exit). A `deadline` caps the maximum
//!   wall-clock time to avoid hanging on a stuck CLI.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::error::PtyError;
use crate::pty::{PtyCommand, PtyHandle};

/// Spawn parameters for [`oneshot_pty_invoke`].
#[derive(Debug, Clone)]
pub struct OneshotInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub rows: u16,
    pub cols: u16,
    /// Hard deadline for the whole call. The child is force-killed if it
    /// hasn't exited by then.
    pub deadline: Duration,
}

impl OneshotInvocation {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            rows: 24,
            cols: 200,
            deadline: Duration::from_secs(300),
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for a in args {
            self.args.push(a.into());
        }
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    pub fn envs(mut self, kvs: HashMap<String, String>) -> Self {
        self.env.extend(kvs);
        self
    }

    pub fn cwd(mut self, p: PathBuf) -> Self {
        self.cwd = Some(p);
        self
    }

    pub fn deadline(mut self, d: Duration) -> Self {
        self.deadline = d;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneshotOutput {
    /// Concatenated stdout from spawn to EOF. May contain ANSI escape codes
    /// if the CLI emitted them despite `NO_COLOR=1`; callers parsing
    /// structured output should strip them or rely on the CLI honouring the
    /// hint.
    pub stdout: String,
    /// Number of bytes received before EOF.
    pub bytes: usize,
    /// Wall-clock time the child took.
    pub elapsed: Duration,
}

/// Spawn `invocation.program` under a fresh PTY, drain stdout until EOF or
/// the deadline elapses, and return the captured output.
///
/// On timeout, the child is killed and the call returns [`PtyError::ReadTimeout`].
/// All other I/O errors surface as [`PtyError::Io`] / [`PtyError::Closed`].
pub async fn oneshot_pty_invoke(invocation: OneshotInvocation) -> Result<OneshotOutput, PtyError> {
    // NO_COLOR + TERM=xterm-256color are useful defaults even when caller
    // didn't bother setting them; CLI's stream-json output stays cleanest.
    let mut env = invocation.env.clone();
    env.entry("NO_COLOR".to_string())
        .or_insert_with(|| "1".to_string());
    env.entry("TERM".to_string())
        .or_insert_with(|| "xterm-256color".to_string());

    let mut pty_cmd = PtyCommand::new(&invocation.program)
        .args(invocation.args.clone())
        .size(invocation.rows, invocation.cols);
    if let Some(cwd) = invocation.cwd.clone() {
        pty_cmd = pty_cmd.cwd(cwd);
    }
    for (k, v) in env {
        pty_cmd = pty_cmd.env(k, v);
    }

    let pty = PtyHandle::spawn(pty_cmd)?;
    let start = Instant::now();
    let result = drain_until_eof(&pty, invocation.deadline).await;

    // Regardless of outcome, give the child a chance to reap. drain_until_eof
    // already saw EOF in the happy path; on timeout we kill explicitly.
    let elapsed = start.elapsed();
    match result {
        Ok(stdout) => {
            let bytes = stdout.len();
            Ok(OneshotOutput {
                stdout,
                bytes,
                elapsed,
            })
        }
        Err(PtyError::ReadTimeout(d)) => {
            warn!(timeout = ?d, "oneshot_pty_invoke: deadline exceeded — killing child");
            pty.shutdown().await;
            Err(PtyError::ReadTimeout(d))
        }
        Err(e) => {
            pty.shutdown().await;
            Err(e)
        }
    }
}

/// Read all bytes from the PTY until EOF or `deadline`.
async fn drain_until_eof(pty: &PtyHandle, deadline: Duration) -> Result<String, PtyError> {
    let start = Instant::now();
    let mut buf = String::new();
    loop {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .ok_or(PtyError::ReadTimeout(deadline))?;
        if remaining.is_zero() {
            return Err(PtyError::ReadTimeout(deadline));
        }

        // Use a sentinel that will never appear in well-formed CLI output;
        // read_until returns the prefix before the sentinel, OR errors on
        // timeout / EOF. EOF is the success terminator here.
        const UNREACHABLE_SENTINEL: &str = "\u{FFFD}DUDUCLAW_NEVER_EMIT\u{FFFD}";
        match pty.read_until(UNREACHABLE_SENTINEL, remaining).await {
            Ok(prefix) => {
                // The marker matched somehow (it shouldn't have). Append
                // whatever was before and keep reading.
                buf.push_str(&prefix);
                debug!("oneshot: unreachable sentinel matched — continuing");
            }
            Err(PtyError::Closed) => {
                // EOF — success. Append any leftover buffered bytes.
                buf.push_str(&pty.drain_buffer());
                return Ok(buf);
            }
            Err(PtyError::ReadTimeout(d)) => {
                // No new data but no EOF either. Append whatever buffered up.
                buf.push_str(&pty.drain_buffer());
                return Err(PtyError::ReadTimeout(d));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_program(text: &str) -> (String, Vec<String>) {
        #[cfg(unix)]
        {
            ("echo".to_string(), vec![text.to_string()])
        }
        #[cfg(windows)]
        {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "echo".to_string(), text.to_string()],
            )
        }
    }

    #[tokio::test]
    async fn captures_echo_output() {
        let (program, args) = echo_program("hello-oneshot");
        let inv = OneshotInvocation::new(program)
            .args(args)
            .deadline(Duration::from_secs(5));
        let out = oneshot_pty_invoke(inv).await.expect("invoke ok");
        assert!(out.stdout.contains("hello-oneshot"), "stdout = {:?}", out.stdout);
        assert!(out.bytes > 0);
        assert!(out.elapsed > Duration::ZERO);
    }

    #[tokio::test]
    async fn deadline_kills_long_child() {
        // `sleep 10` doesn't print anything; deadline 200ms must abort.
        #[cfg(unix)]
        let (program, args) = ("sleep".to_string(), vec!["10".to_string()]);
        #[cfg(windows)]
        let (program, args) = (
            "timeout".to_string(),
            vec!["/T".to_string(), "10".to_string()],
        );

        let inv = OneshotInvocation::new(program)
            .args(args)
            .deadline(Duration::from_millis(200));
        let result = oneshot_pty_invoke(inv).await;
        assert!(
            matches!(result, Err(PtyError::ReadTimeout(_))),
            "expected timeout, got {result:?}"
        );
    }
}
