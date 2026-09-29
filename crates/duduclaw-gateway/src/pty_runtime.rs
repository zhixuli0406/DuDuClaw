//! One-shot PTY invocation adapter between [`duduclaw_cli_runtime`] and the
//! gateway.
//!
//! Some CLIs refuse to run when stdout is a plain pipe — they check for a TTY.
//! [`invoke_oneshot`] spawns such a CLI under a real pseudo-terminal
//! (`portable-pty`: ConPTY on Windows 10 1809+, openpty on Unix), drains its
//! stdout to EOF, and hands the bytes back. It mirrors the lifecycle of
//! `tokio::process::Command::spawn → wait → capture`; nothing is pooled and no
//! session outlives the call.
//!
//! **Removed in 2026-09: the PTY session pool.** This module used to also own
//! a pool of long-lived, sentinel-framed interactive `claude` REPL sessions
//! (`RuntimeMode::PtyPool`, `acquire_and_invoke`, an out-of-process
//! `duduclaw-cli-worker`, a demotion breaker, `GET /api/runtime/status`, and
//! the `pty_pool_*` Prometheus family). It existed as zero-code-change standby
//! for Anthropic's 2026-06-15 programmatic-usage split, which was paused on the
//! day and never re-activated in the 15 months since; meanwhile the pool keyed
//! sessions without a conversation dimension, so a multi-conversation agent
//! bled context between conversations and it could never be turned on safely.
//! Every agent now runs the default fresh-spawn `claude -p` path. See
//! `docs/features/27-pty-pool-runtime.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use duduclaw_cli_runtime::{OneshotInvocation, OneshotOutput, PtyError, oneshot_pty_invoke};

/// True when `err` describes a **transport** failure of the PTY layer itself
/// (the terminal, the spawn, the pipe) rather than a failure attributable to
/// the AI account that was being used.
///
/// The distinction matters for account rotation: booking a PTY-layer failure
/// against an OAuth account's health is what turned one wedged spawn into
/// "All accounts exhausted" on single-account installs.
///
/// The markers below are verbatim slices of the `Display` impls of
/// [`duduclaw_cli_runtime::PtyError`] — the complete set of strings this layer
/// can produce. Keep them in sync when a variant is added there.
///
/// Matching is on whole phrases (project convention #2: no unanchored
/// substring checks for routing decisions) — a bare `contains("timed out")`
/// would misfire on a user asking about HTTP timeouts.
pub fn is_pty_transport_error(err: &str) -> bool {
    const TRANSPORT_MARKERS: [&str; 7] = [
        "failed to open pty",
        "failed to spawn child process",
        "pty i/o error",
        "pty closed unexpectedly",
        "read timed out after",
        "write timed out after",
        "background task panicked",
    ];
    let low = err.to_ascii_lowercase();
    TRANSPORT_MARKERS.iter().any(|m| low.contains(m))
}

/// Returns true when `DUDUCLAW_PTY_DISABLE_RETRY=1` is set. Operators
/// flip this when empty-payload retries cause runaway token usage or
/// other pathological behaviour. Default off — retry is on.
pub fn is_pty_retry_disabled() -> bool {
    is_env_truthy("DUDUCLAW_PTY_DISABLE_RETRY")
}

fn is_env_truthy(var: &str) -> bool {
    matches!(
        std::env::var(var)
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// True when an extracted CLI "answer" still contains our own prompt framing —
/// i.e. the reader latched onto the TUI's echo of typed input instead of a
/// model-authored payload. Such text is a protocol failure and must NEVER be
/// returned as a user-visible reply.
pub fn answer_leaks_prompt_scaffold(answer: &str) -> bool {
    const MARKERS: [&str; 3] = [
        "<conversation_history>",
        "</conversation_history>",
        "<current_message>",
    ];
    MARKERS.iter().any(|m| answer.contains(m))
}

/// Invoke `claude` (or any CLI) one-shot through a PTY. Mirrors the lifecycle
/// of `tokio::process::Command::spawn → wait → capture`, but routes through
/// `portable-pty` so the child sees a real TTY on every platform.
///
/// Caller is responsible for assembling `args` and `env_vars` exactly the way
/// a direct `Command` spawn would — this module makes no assumptions about
/// flags / output formats / system prompt placement. The returned
/// `OneshotOutput.stdout` is whatever the CLI wrote to stdout between spawn
/// and EOF (e.g. a stream-json log line sequence).
///
/// `clear_env`: when `true`, the child sees ONLY `env_vars` (plus the
/// `NO_COLOR`/`TERM` defaults `oneshot_pty_invoke` always injects) instead
/// of the gateway's full ambient environment layered under it. WP-8B
/// (credentials doctrine P3): callers spawning a security-sensitive CLI pass
/// `true` with an allowlist-seeded `env_vars` so the child never sees the
/// gateway's vendor `*_API_KEY`s.
pub async fn invoke_oneshot(
    program: impl Into<String>,
    args: Vec<String>,
    env_vars: HashMap<String, String>,
    work_dir: Option<PathBuf>,
    deadline: Duration,
    clear_env: bool,
) -> Result<OneshotOutput, PtyError> {
    let mut inv = OneshotInvocation::new(program)
        .args(args)
        .envs(env_vars)
        .deadline(deadline)
        .clear_env(clear_env);
    if let Some(cwd) = work_dir {
        inv = inv.cwd(cwd);
    }
    oneshot_pty_invoke(inv).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spawns a real PTY and reads child echo — unreliable on headless Windows CI
    // (ConPTY). Covered on Unix.
    #[cfg_attr(
        windows,
        ignore = "ConPTY oneshot echo is flaky on headless Windows CI"
    )]
    #[tokio::test]
    async fn invoke_oneshot_runs_echo() {
        #[cfg(unix)]
        let (program, args) = ("echo".to_string(), vec!["pty-runtime-smoke".to_string()]);
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "echo".to_string(),
                "pty-runtime-smoke".to_string(),
            ],
        );
        let result = invoke_oneshot(
            program,
            args,
            HashMap::new(),
            None,
            Duration::from_secs(5),
            false,
        )
        .await
        .expect("oneshot ok");
        assert!(result.stdout.contains("pty-runtime-smoke"));
    }

    #[test]
    fn transport_errors_are_distinguished_from_account_errors() {
        use duduclaw_cli_runtime::PtyError;

        assert!(is_pty_transport_error(&PtyError::Closed.to_string()));
        assert!(is_pty_transport_error(
            &PtyError::ReadTimeout(Duration::from_secs(120)).to_string()
        ));
        assert!(is_pty_transport_error(
            &PtyError::WriteTimeout(Duration::from_secs(5)).to_string()
        ));
        assert!(is_pty_transport_error(
            &PtyError::OpenPty("no ptys available".into()).to_string()
        ));
        assert!(is_pty_transport_error(
            &PtyError::TaskPanicked("reader".into()).to_string()
        ));
        // Even nested inside a rotation summary.
        assert!(is_pty_transport_error(
            "All accounts exhausted. Last error: PTY closed unexpectedly"
        ));

        // Genuine account-level failures must still be booked against the
        // account — otherwise rotation would never cool a bad account down.
        assert!(!is_pty_transport_error("rate limit exceeded"));
        assert!(!is_pty_transport_error("credit balance is too low"));
        assert!(!is_pty_transport_error("Not logged in · Please run /login"));
        assert!(!is_pty_transport_error("claude CLI not found in PATH"));
    }

    /// Convention #2 — no unanchored substring checks for routing decisions.
    #[test]
    fn transport_matching_is_anchored_to_the_whole_phrase() {
        for benign in [
            "使用者問 HTTP read timeout 該設多久",
            "the pty device file lives under /dev/pts",
            "closed the ticket unexpectedly early",
        ] {
            assert!(
                !is_pty_transport_error(benign),
                "prose must not be treated as a transport failure: {benign}"
            );
        }
    }

    #[test]
    fn scaffold_leak_detector_flags_echoed_prompts_only() {
        assert!(answer_leaks_prompt_scaffold(
            "junk <conversation_history>\n<user>hi</user>"
        ));
        assert!(answer_leaks_prompt_scaffold(
            "<current_message>\nhello\n</current_message>"
        ));
        // Normal replies — including ones that TALK about history — pass.
        assert!(!answer_leaks_prompt_scaffold(
            "好的，我已把退貨規則記到知識庫。"
        ));
        assert!(!answer_leaks_prompt_scaffold(
            "Based on our conversation history, the answer is 42."
        ));
    }

    #[test]
    fn retry_kill_switch_recognises_truthy_values_only() {
        // SAFETY: test-only; no other test in this module touches this var.
        for v in ["1", "true", "TRUE", "yes", "YES"] {
            unsafe { std::env::set_var("DUDUCLAW_PTY_DISABLE_RETRY", v) };
            assert!(is_pty_retry_disabled(), "value {v:?} should disable retry");
        }
        for v in ["0", "false", "no", "off", "", "garbage"] {
            unsafe { std::env::set_var("DUDUCLAW_PTY_DISABLE_RETRY", v) };
            assert!(
                !is_pty_retry_disabled(),
                "value {v:?} should NOT disable retry"
            );
        }
        unsafe { std::env::remove_var("DUDUCLAW_PTY_DISABLE_RETRY") };
    }
}
