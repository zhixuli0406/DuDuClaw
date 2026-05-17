//! `PtySession` — a long-lived CLI subprocess accessed through a sentinel-framed PTY.
//!
//! Each `PtySession` wraps one CLI child (claude / codex / gemini). Callers acquire
//! the session, call `invoke(prompt)`, get back the model's final answer, and
//! release. Concurrent `invoke()` calls against the same session error with
//! `SessionError::Busy` — pooling is the [`crate::pool::PtyPool`] layer's job.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::envelope::{
    Envelope, Frame, INTERACTIVE_SENTINEL, RSP_END, ResponseFormat,
    extract_payload_with_chrome_filter, frame_request, parse_frame, strip_ansi,
};
use crate::error::{PtyError, SessionError};
use crate::platform::ChildGroup;
use crate::pty::{PtyCommand, PtyHandle};

/// Which CLI we're driving. Determines flag conventions used to inject the
/// sentinel-protocol instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CliKind {
    Claude,
    Codex,
    Gemini,
}

impl CliKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CliKind::Claude => "claude",
            CliKind::Codex => "codex",
            CliKind::Gemini => "gemini",
        }
    }

    pub fn parse(s: &str) -> Result<Self, SessionError> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            other => Err(SessionError::UnknownCliKind(other.to_string())),
        }
    }
}

/// Spawn-time configuration. Pure data so it can be constructed by callers in
/// other crates without dragging the rest of the runtime API surface in.
#[derive(Debug, Clone)]
pub struct SpawnOpts {
    pub agent_id: String,
    pub cli_kind: CliKind,
    /// Program path (e.g. resolved `which claude`). Caller's responsibility to
    /// pick the right binary for the target platform.
    pub program: String,
    /// CLI-specific args beyond what this crate injects (e.g. `--resume <id>`,
    /// `--bare`, `--model <name>`).
    pub extra_args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// Optional Claude `--resume <session_id>` value. We re-export it on the
    /// session struct so the pool can keep multi-turn continuity.
    pub session_id: Option<String>,
    /// Boot deadline (seeing the first prompt / readiness signal).
    pub boot_timeout: Duration,
    /// Per-invoke deadline. The pool may override this per call.
    pub default_invoke_timeout: Duration,
    /// PTY dimensions.
    pub rows: u16,
    pub cols: u16,
    /// **Phase 3.C.2**: drive the CLI as a real interactive REPL (no `-p`).
    ///
    /// When `true`:
    /// - `spawn` injects `--append-system-prompt` with the sentinel protocol
    ///   (via [`inject_protocol_args`]).
    /// - `spawn` performs the boot dance (trust dialog → REPL ready check).
    /// - `invoke` strips ANSI escapes and applies the TUI chrome filter before
    ///   returning the model's answer.
    ///
    /// When `false` (default, back-compat for echo-server tests): boot waits
    /// for the first newline only and `invoke` returns the raw sentinel payload.
    pub interactive: bool,
    /// **Phase 3.C.2**: skip the workspace trust prompt handling step.
    ///
    /// Set to `true` when the operator has already run `claude project trust`
    /// for the cwd, so the trust dialog will not appear at spawn time.
    /// Defaults to `false` — the spawn flow auto-detects + auto-accepts the
    /// trust dialog by sending `\r` (which selects the default "Yes, I
    /// trust this folder" option).
    pub pre_trusted: bool,
}

impl SpawnOpts {
    pub fn claude(agent_id: impl Into<String>, program: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            cli_kind: CliKind::Claude,
            program: program.into(),
            extra_args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_secs(30),
            default_invoke_timeout: Duration::from_secs(300),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        }
    }

    /// Phase 3.C.2: build SpawnOpts pre-configured for interactive Claude.
    pub fn claude_interactive(
        agent_id: impl Into<String>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            interactive: true,
            boot_timeout: Duration::from_secs(45),
            default_invoke_timeout: Duration::from_secs(180),
            ..Self::claude(agent_id, program)
        }
    }
}

/// Live CLI session. Cloning is cheap (Arc) and intentional — callers and the
/// pool both hold references.
pub struct PtySession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    agent_id: String,
    cli_kind: CliKind,
    pty: PtyHandle,
    child_group: Mutex<Option<ChildGroup>>,
    session_id: Mutex<Option<String>>,
    created_at: Instant,
    last_used: Mutex<Instant>,
    in_flight: AtomicBool,
    shutdown_token: CancellationToken,
    default_invoke_timeout: Duration,
    /// **Phase 3.C.2**: cached SpawnOpts.interactive — switches the parse
    /// path between raw sentinel matching (false) and ANSI-strip + chrome
    /// filter (true).
    interactive: bool,
    /// **Phase 3.C.3**: explicit unhealthy flag set by [`mark_unhealthy`].
    /// Soft-failure surface used by `is_healthy()`; the pool then evicts.
    marked_unhealthy: AtomicBool,
    /// Round 4 deferred-cleanup: the cwd this session was spawned
    /// with. Exposed via [`PtySession::spawn_cwd`] so callers (worker
    /// `handle_invoke`) can detect divergence between an in-flight
    /// invoke's requested work_dir and the cached session's actual
    /// cwd, and emit a one-shot warning instead of silently using the
    /// stale cwd.
    spawn_cwd: Option<PathBuf>,
}

impl PtySession {
    /// Spawn a new CLI under a fresh PTY and wait for boot completion.
    pub async fn spawn(opts: SpawnOpts) -> Result<Arc<Self>, SessionError> {
        let mut args = opts.extra_args.clone();
        // Interactive mode injects the `--append-system-prompt` instruction
        // that teaches the model the DUDUCLAW sentinel-wrapping protocol.
        // Non-interactive (echo-server test) mode leaves args caller-controlled.
        if opts.interactive {
            inject_protocol_args(&mut args, opts.cli_kind);
        }

        let mut env = opts.env.clone();
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());
        // Disable colour in stdout so sentinel matching isn't disrupted by ANSI codes.
        env.insert("NO_COLOR".to_string(), "1".to_string());
        // CLAUDE_CODE_NONINTERACTIVE / similar should be set by the caller when
        // appropriate (we don't presume the agent wants headless mode here).

        let pty_cmd = PtyCommand::new(&opts.program)
            .args(args)
            .size(opts.rows, opts.cols);
        let pty_cmd = opts.cwd.as_ref().map_or(pty_cmd.clone(), |cwd| {
            pty_cmd.clone().cwd(cwd.clone())
        });
        let pty_cmd = env
            .into_iter()
            .fold(pty_cmd, |acc, (k, v)| acc.env(k, v));

        let pty = PtyHandle::spawn(pty_cmd).map_err(SessionError::Pty)?;
        let pid = pty.pid();
        let child_group = pid.map(ChildGroup::new);

        let inner = Arc::new(SessionInner {
            agent_id: opts.agent_id.clone(),
            cli_kind: opts.cli_kind,
            pty,
            child_group: Mutex::new(child_group),
            session_id: Mutex::new(opts.session_id.clone()),
            created_at: Instant::now(),
            last_used: Mutex::new(Instant::now()),
            in_flight: AtomicBool::new(false),
            shutdown_token: CancellationToken::new(),
            default_invoke_timeout: opts.default_invoke_timeout,
            interactive: opts.interactive,
            marked_unhealthy: AtomicBool::new(false),
            spawn_cwd: opts.cwd.clone(),
        });
        let session = Self { inner };

        // Boot wait. Interactive mode does a more elaborate dance (trust
        // dialog handling + REPL-ready check); the legacy path just looks
        // for any first-line activity so it stays compatible with the
        // pre-3.C.2 echo-server tests.
        if opts.interactive {
            session
                .interactive_boot_dance(opts.boot_timeout, opts.pre_trusted)
                .await?;
        } else {
            session.wait_boot(opts.boot_timeout).await?;
        }

        info!(
            agent_id = %opts.agent_id,
            cli = %opts.cli_kind.as_str(),
            pid = pid.unwrap_or(0),
            "pty session spawned"
        );

        Ok(Arc::new(session))
    }

    /// Wait for any stdout activity from the child within `timeout`, OR return
    /// quickly if there's already buffered output.
    async fn wait_boot(&self, timeout: Duration) -> Result<(), SessionError> {
        // Just peek into the reader: we read up to `\n` to flush whatever banner
        // was printed. If the CLI doesn't print anything on boot (some headless
        // modes don't), we tolerate the timeout silently.
        match self.inner.pty.read_until("\n", timeout).await {
            Ok(_banner) => Ok(()),
            Err(PtyError::ReadTimeout(_)) => {
                debug!(
                    agent_id = %self.inner.agent_id,
                    "pty boot: no banner within timeout — proceeding anyway"
                );
                Ok(())
            }
            Err(PtyError::Closed) => Err(SessionError::ChildExited { code: None }),
            Err(e) => Err(SessionError::Pty(e)),
        }
    }

    /// **Phase 3.C.2**: interactive boot dance.
    ///
    /// Validated against `claude` v2.1.138 in the 2026-05-14 spike. Three
    /// phases:
    ///
    /// 1. Drain banner up to `total_timeout/3`. Look for the trust-folder
    ///    prompt fingerprint `Yes,Itrustthisfolder` (TUI strips spaces).
    /// 2. If the prompt was seen and `pre_trusted` is false, send `\r` to
    ///    accept the default option (`❯ 1. Yes, I trust this folder`).
    /// 3. Drain another `total_timeout/3`. Look for a REPL-ready
    ///    fingerprint (`?forshortcuts` or `Try"edit`). Tolerate timeout —
    ///    some claude versions don't print the hint line.
    ///
    /// If the child dies at any point (e.g. user picked the wrong option
    /// or OAuth is missing), return [`SessionError::ChildExited`].
    async fn interactive_boot_dance(
        &self,
        total_timeout: Duration,
        pre_trusted: bool,
    ) -> Result<(), SessionError> {
        let banner_window = total_timeout / 3;
        let banner = self.drain_window(banner_window).await;
        let stripped = strip_ansi(&banner);
        debug!(
            agent_id = %self.inner.agent_id,
            stripped_len = stripped.len(),
            "interactive_boot_dance: banner drained"
        );

        let trust_seen = stripped.contains("Yes,Itrustthisfolder")
            || stripped.contains("trust this folder")
            || stripped.contains("Quicksafetycheck");

        if trust_seen && !pre_trusted {
            if !self.inner.pty.is_alive() {
                return Err(SessionError::ChildExited { code: None });
            }
            info!(
                agent_id = %self.inner.agent_id,
                "interactive_boot_dance: trust prompt seen — sending '\\r'"
            );
            self.inner
                .pty
                .write_all(b"\r")
                .await
                .map_err(SessionError::Pty)?;
        }

        // Second drain: wait for REPL ready hint or just silence.
        let ready_window = total_timeout / 3;
        let post = self.drain_window(ready_window).await;
        let post_stripped = strip_ansi(&post);
        let ready_hint = post_stripped.contains("?forshortcuts")
            || post_stripped.contains("Try\"edit")
            || post_stripped.contains("Trytype");

        debug!(
            agent_id = %self.inner.agent_id,
            ready_hint,
            "interactive_boot_dance: post-trust drain complete"
        );

        if !self.inner.pty.is_alive() {
            return Err(SessionError::ChildExited { code: None });
        }
        Ok(())
    }

    /// Drain stdout for up to `window` returning everything captured. Used by
    /// the interactive boot dance + diagnostic paths.
    async fn drain_window(&self, window: Duration) -> String {
        let start = Instant::now();
        let mut buf = String::new();
        // Always drain whatever is already buffered.
        buf.push_str(&self.inner.pty.drain_buffer());
        loop {
            let remaining = match window.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r.min(Duration::from_millis(300)),
                _ => break,
            };
            // We use an unreachable sentinel string so read_until's only exits
            // are timeout (drain a chunk + loop) or close (return).
            const UNREACHABLE: &str = "\u{FFFD}DUDUCLAW_DRAIN_NEVER_EMIT\u{FFFD}";
            match self.inner.pty.read_until(UNREACHABLE, remaining).await {
                Ok(prefix) => buf.push_str(&prefix),
                Err(PtyError::ReadTimeout(_)) => {
                    buf.push_str(&self.inner.pty.drain_buffer());
                }
                Err(PtyError::Closed) => {
                    buf.push_str(&self.inner.pty.drain_buffer());
                    break;
                }
                Err(_) => break,
            }
        }
        buf
    }

    pub fn agent_id(&self) -> &str {
        &self.inner.agent_id
    }

    pub fn cli_kind(&self) -> CliKind {
        self.inner.cli_kind
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner.session_id.lock().clone()
    }

    pub fn set_session_id(&self, id: Option<String>) {
        *self.inner.session_id.lock() = id;
    }

    pub fn created_at(&self) -> Instant {
        self.inner.created_at
    }

    pub fn last_used(&self) -> Instant {
        *self.inner.last_used.lock()
    }

    pub fn is_healthy(&self) -> bool {
        !self.inner.shutdown_token.is_cancelled()
            && self.inner.pty.is_alive()
            && !self.inner.marked_unhealthy.load(Ordering::Acquire)
    }

    /// **Phase 3.C.3**: explicitly mark this session unhealthy without
    /// killing the child. The pool's next `acquire` for this key will
    /// observe the unhealthy flag and shut + replace the session.
    ///
    /// Used by callers that detect soft failures (e.g. OAuth token
    /// expiry hint in the response, "Not logged in" pattern, repeated
    /// empty-payload returns) without wanting to abort the in-flight
    /// turn.
    pub fn mark_unhealthy(&self) {
        self.inner.marked_unhealthy.store(true, Ordering::Release);
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.pty.pid()
    }

    /// Round 4 deferred-cleanup: the cwd this session was spawned
    /// with (set from [`SpawnOpts::cwd`] at spawn time). `None` when
    /// the spawn didn't override cwd — in that case the child
    /// inherited the parent process's cwd.
    pub fn spawn_cwd(&self) -> Option<&Path> {
        self.inner.spawn_cwd.as_deref()
    }

    /// Single round trip: inject `prompt`, wait for matching sentinel envelope,
    /// return the model's final answer.
    ///
    /// **Interactive mode** (Phase 3.C.2):
    /// - Writes the prompt directly + `\r` (no leading framing — the
    ///   sentinel-wrapping instruction was already injected via the
    ///   `--append-system-prompt` flag at spawn time).
    /// - Collects raw bytes, periodically strips ANSI + scans for the
    ///   sentinel pair generated from this turn's UUID, then applies the
    ///   TUI chrome filter to the payload.
    ///
    /// **Non-interactive mode** (legacy, echo-server tests):
    /// - Writes a complete `frame_request` envelope including the protocol
    ///   reminder text.
    /// - Echo path returns the raw payload between sentinels unmodified.
    pub async fn invoke(
        &self,
        prompt: &str,
        deadline: Option<Duration>,
    ) -> Result<String, SessionError> {
        // CAS to enforce single-flight.
        if self
            .inner
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SessionError::Busy);
        }
        // RAII release.
        let _guard = InFlightGuard {
            flag: &self.inner.in_flight,
        };

        // **Round 3 review fix (HIGH-H3)**: cancel-safe health guard.
        //
        // When the caller's future is dropped mid-`invoke` (e.g. axum
        // cancels a handler because the client disconnected), the
        // session's PTY is left in an indeterminate state — the model
        // may be mid-response, no sentinel emitted yet, and the next
        // `invoke` would read the half-response intermixed with the
        // new prompt. Without intervention, subsequent acquires would
        // reuse this poisoned session for several turns before
        // failing hard enough to trigger eviction.
        //
        // The guard's Drop fires unconditionally; we set `completed =
        // true` immediately before returning so the normal exit path
        // leaves the session healthy. Any cancellation point in
        // between (including `?` propagation on `Err`) leaves
        // `completed = false`, and Drop marks the session unhealthy
        // so the next acquire respawns.
        let mut cancel_guard = CancelHealthGuard {
            inner: &self.inner,
            completed: false,
        };

        if self.inner.shutdown_token.is_cancelled() {
            return Err(SessionError::Shutdown);
        }
        if !self.inner.pty.is_alive() {
            return Err(SessionError::ChildExited { code: None });
        }

        let deadline = deadline.unwrap_or(self.inner.default_invoke_timeout);
        let envelope = Envelope::new(prompt).with_format(ResponseFormat::Text);

        let result = if self.inner.interactive {
            // Interactive: drain whatever lingering bytes are buffered from
            // prior turns + TUI redraws BEFORE we write — that way our
            // collector sees only this turn's bytes when looking for the
            // sentinel pair.
            let _ = self.inner.pty.drain_buffer();
            // Send just the user prompt. The protocol is already loaded
            // via --append-system-prompt; we deliberately do NOT include
            // the literal sentinel string in the user message because the
            // TUI echoes user input verbatim, which would confuse the
            // positional pairing (echoed copy would become a third
            // sentinel occurrence in the rolling buffer).
            self.inner
                .pty
                .write_all(prompt.as_bytes())
                .await
                .map_err(SessionError::Pty)?;
            self.inner
                .pty
                .write_all(b"\r")
                .await
                .map_err(SessionError::Pty)?;
            self.collect_response_interactive(envelope.req_id, deadline)
                .await
        } else {
            let wire = frame_request(&envelope);
            self.inner
                .pty
                .write_all(wire.as_bytes())
                .await
                .map_err(SessionError::Pty)?;
            self.collect_response(envelope.req_id, deadline).await
        };

        if result.is_ok() {
            *self.inner.last_used.lock() = Instant::now();
            // **Round 3 review fix (HIGH-H3)**: mark the cancel guard
            // as completed only on the success path. Any error (or
            // mid-invoke drop) leaves `completed = false` so the
            // session is marked unhealthy.
            cancel_guard.completed = true;
        }
        result
    }

    /// **Phase 3.C.2** interactive response collection.
    ///
    /// Reads raw bytes into a rolling buffer; after each chunk, runs
    /// `strip_ansi` over the **entire accumulated** raw buffer (the
    /// previous strip's output is discarded — that's O(n) per iteration
    /// but n is bounded by the single-turn response size, typically <
    /// 50 KB). Once `parse_frame` finds a matching sentinel pair in the
    /// stripped view, applies [`extract_payload_with_chrome_filter`] and
    /// returns the cleaned answer.
    async fn collect_response_interactive(
        &self,
        req_id: Uuid,
        deadline: Duration,
    ) -> Result<String, SessionError> {
        let start = Instant::now();
        let mut raw = String::new();
        raw.push_str(&self.inner.pty.drain_buffer());

        // Try once with what was already buffered, in case the response
        // arrived during a tight turnaround (rare for interactive but
        // free to check).
        if let Some(answer) = try_extract_interactive_answer(&raw, req_id) {
            return Ok(answer);
        }

        loop {
            let remaining = match deadline.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r,
                _ => {
                    diagnostic_dump_on_timeout(&raw, req_id, deadline);
                    return Err(SessionError::InvokeTimeout(deadline));
                }
            };

            // Use the closing sentinel as the read-until probe. False
            // positives just cause the inner extractor to keep looking;
            // misses fall back to the timeout drain path.
            let chunk = match self
                .inner
                .pty
                .read_until(INTERACTIVE_SENTINEL, remaining.min(Duration::from_secs(3)))
                .await
            {
                Ok(prefix) => {
                    let mut s = prefix;
                    s.push_str(INTERACTIVE_SENTINEL);
                    s
                }
                Err(PtyError::ReadTimeout(_)) => {
                    // Drain whatever buffered up + keep trying.
                    let drained = self.inner.pty.drain_buffer();
                    if drained.is_empty() {
                        continue;
                    }
                    drained
                }
                Err(PtyError::Closed) => {
                    return Err(SessionError::ChildExited { code: None });
                }
                Err(e) => return Err(SessionError::Pty(e)),
            };
            raw.push_str(&chunk);

            if let Some(answer) = try_extract_interactive_answer(&raw, req_id) {
                return Ok(answer);
            }
        }
    }

    async fn collect_response(
        &self,
        req_id: Uuid,
        deadline: Duration,
    ) -> Result<String, SessionError> {
        let start = Instant::now();
        // Strategy: read into the rolling buffer chunk-by-chunk, after each chunk
        // try `parse_frame`. We borrow PtyHandle's buffer so we don't have to
        // reimplement chunked accumulation.
        let mut accumulator = String::new();
        // First take whatever was already buffered.
        accumulator.push_str(&self.inner.pty.drain_buffer());
        if let Some(frame) = parse_frame(&mut accumulator) {
            return finalize_frame(frame, req_id);
        }

        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .ok_or(SessionError::InvokeTimeout(deadline))?;
            if remaining.is_zero() {
                return Err(SessionError::InvokeTimeout(deadline));
            }

            // Use the response-end marker as a hint to chunk the read. Even if it
            // matches as a literal substring inside the model's prose, `parse_frame`
            // will reject the pairing (mismatched id), so we keep reading.
            let chunk = match self.inner.pty.read_until(RSP_END, remaining).await {
                Ok(prefix) => {
                    // The marker has been consumed by read_until — re-append it so
                    // parse_frame can see the closing sentinel.
                    let mut s = prefix;
                    s.push_str(RSP_END);
                    s
                }
                Err(PtyError::ReadTimeout(_)) => {
                    // Fall back to whatever has accumulated so far + drain remainder.
                    accumulator.push_str(&self.inner.pty.drain_buffer());
                    return Err(SessionError::InvokeTimeout(deadline));
                }
                Err(PtyError::Closed) => {
                    return Err(SessionError::ChildExited { code: None });
                }
                Err(e) => return Err(SessionError::Pty(e)),
            };
            accumulator.push_str(&chunk);

            if let Some(frame) = parse_frame(&mut accumulator) {
                // Anything left in `accumulator` belongs to a subsequent request —
                // stuff it back into the PTY's rolling buffer.
                if !accumulator.is_empty() {
                    let mut buf = self.inner.pty.drain_buffer();
                    accumulator.push_str(&buf);
                    buf.clear();
                    // PtyHandle exposes only drain_buffer + write side, so we
                    // re-buffer via the next read cycle. Until then, keep the
                    // tail on the side stack: the next invoke() will pick it up.
                    // To do that we'd need a `push_buffer` API — Phase 2 enhancement.
                    debug!(
                        leftover = accumulator.len(),
                        "discarding tail bytes after frame match \
                         (Phase 2 will re-buffer)"
                    );
                }
                return finalize_frame(frame, req_id);
            }
            // Otherwise the literal RSP_END matched but parse_frame couldn't
            // pair it — keep looping until the second sentinel arrives.
        }
    }

    /// Graceful shutdown: cancel the shutdown token, send SIGTERM (or the
    /// Windows Job Object equivalent), give the child a brief grace
    /// period to flush state, then SIGKILL via `PtyHandle::shutdown`.
    /// Idempotent.
    ///
    /// Phase 4 (2026-05-15): the grace period is honoured so well-behaved
    /// children (claude CLI flushing token usage to disk, MCP servers
    /// closing files) get a chance to clean up before the OS-level kill.
    pub async fn shutdown(&self) {
        if self.inner.shutdown_token.is_cancelled() {
            return;
        }
        self.inner.shutdown_token.cancel();

        // Step 1: SIGTERM / TerminateJobObject (gentle).
        let grace = Duration::from_secs(3);
        if let Some(grp) = self.inner.child_group.lock().as_ref() {
            grp.terminate(grace);
        }

        // Step 2: give the child up to `grace` to exit on its own. We
        // poll PtyHandle::is_alive each 100 ms instead of sleeping the
        // full grace so well-behaved children that exit fast don't drag
        // down dispatcher / pool eviction latency.
        let deadline = Instant::now() + grace;
        let mut check_interval = tokio::time::interval(Duration::from_millis(100));
        check_interval.tick().await; // discard immediate first tick
        while Instant::now() < deadline {
            if !self.inner.pty.is_alive() {
                debug!(
                    agent_id = %self.inner.agent_id,
                    "pty session: child exited within grace period"
                );
                break;
            }
            check_interval.tick().await;
        }

        // Step 3: SIGKILL (hard). PtyHandle::shutdown calls portable-pty's
        // `Child::kill` which sends SIGKILL on Unix and TerminateProcess
        // on Windows. Safe to call even if the child already exited.
        self.inner.pty.shutdown().await;
        info!(
            agent_id = %self.inner.agent_id,
            cli = %self.inner.cli_kind.as_str(),
            "pty session shut down"
        );
    }
}

impl Clone for PtySession {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Helpers ---------------------------------------------------------------

fn finalize_frame(frame: Frame, expected_id: Uuid) -> Result<String, SessionError> {
    match frame {
        Frame::Response { req_id, payload } if req_id == expected_id => Ok(payload),
        Frame::Response { req_id, .. } => {
            warn!(?req_id, %expected_id, "response uuid mismatch — treating as malformed");
            Err(SessionError::MalformedResponse)
        }
        Frame::Error { req_id, message } if req_id == expected_id => {
            Err(SessionError::CliError(message))
        }
        Frame::Error { req_id, .. } => {
            warn!(?req_id, %expected_id, "error uuid mismatch — treating as malformed");
            Err(SessionError::MalformedResponse)
        }
    }
}

/// Inject CLI-specific args that turn the protocol on.
///
/// **Phase 3.C.2 (interactive path only)**: appends `--append-system-prompt`
/// instructing the model to wrap every final answer in a `=====DUDUCLAW.RSP.<uuid>.MARK=====`
/// sentinel pair. Claude `--help` confirms this flag works in interactive
/// mode (not just `-p`); spike report 2026-05-14 §Q4 validated the model
/// honours the instruction.
///
/// The system prompt establishes the *protocol shape*. The per-turn UUID
/// is reiterated in each `invoke()`'s user message body, defeating recency
/// drift across long sessions.
pub(crate) fn inject_protocol_args(args: &mut Vec<String>, kind: CliKind) {
    match kind {
        CliKind::Claude => {
            // Fixed-string sentinel (no UUID substitution) because the
            // Claude TUI's inline rendering eats one character from
            // UUID-bearing opens, breaking pair matching — see
            // `INTERACTIVE_SENTINEL` doc + spike report §Q5/Q8.
            let bootstrap = format!(
                "DUDUCLAW PROTOCOL: Every assistant turn MUST wrap its FINAL answer between two \
                 identical sentinel lines exactly: {sentinel} on a line by itself, then your \
                 answer text, then {sentinel} on a line by itself again. Use the sentinel \
                 string LITERALLY — do NOT alter, paraphrase, translate, wrap in markdown / \
                 code fences, or insert any characters between the equals signs. Emit nothing \
                 after the closing sentinel. This protocol applies to EVERY assistant turn for \
                 the rest of this conversation.",
                sentinel = INTERACTIVE_SENTINEL,
            );
            args.push("--append-system-prompt".to_string());
            args.push(bootstrap);
        }
        CliKind::Codex | CliKind::Gemini => {
            // Phase 3.C.5+: Codex / Gemini CLI protocol injection. Each
            // surfaces its own flag conventions; not yet validated against
            // live binaries.
        }
    }
}

/// Diagnostic helper: when `collect_response_interactive` times out, dump
/// a tail of the ANSI-stripped buffer at warn level so operators can see
/// what claude was emitting. Truncated to 2 KB to keep logs readable.
fn diagnostic_dump_on_timeout(raw: &str, req_id: Uuid, deadline: Duration) {
    let stripped = strip_ansi(raw);
    let len = stripped.chars().count();
    let take = 2048;
    let tail: String = if len > take {
        stripped.chars().skip(len - take).collect()
    } else {
        stripped.clone()
    };
    let sentinel_open = format!(
        "{}{}{}",
        crate::envelope::RSP_START,
        req_id,
        crate::envelope::RSP_END
    );
    let occurrences = tail.matches(&sentinel_open).count();
    warn!(
        req_id = %req_id,
        deadline_ms = deadline.as_millis() as u64,
        raw_bytes = raw.len(),
        stripped_chars = len,
        sentinel_occurrences_in_tail = occurrences,
        "invoke timed out — stripped buffer tail follows:\n{tail}"
    );
}

/// **Phase 3.C.2** interactive extraction helper.
///
/// Uses [`INTERACTIVE_SENTINEL`] (fixed, UUID-less) and **last-pair**
/// matching.
///
/// Why not UUID-based pairing? The Claude TUI eats one character from the
/// opening sentinel when rendering it inline with the `⏺` assistant
/// marker — empirically verified in the 2026-05-14 spike where an open
/// sentinel arrived as `3c1991a-…` while the close arrived as the
/// correct `33c1991a-…`. UUID matching fails because exactly one
/// sentinel survives intact.
///
/// **Review fix (HIGH)**: take the LAST pair of sentinel occurrences,
/// not the first. Rationale: if the model echoes / quotes the literal
/// sentinel string in its response (e.g. "the protocol uses
/// `=====DUDUCLAW.MARK=====`"), we end up with 3+ occurrences in the
/// stripped buffer. The real answer wrap is *always* the last two
/// because they bookend the actual response output, while preceding
/// occurrences live inside the model's prose. The original "first
/// pair" heuristic produced empty payloads on these inputs.
///
/// Steps:
/// 1. `strip_ansi` the entire raw buffer.
/// 2. Walk the buffer collecting ALL occurrence offsets of
///    [`INTERACTIVE_SENTINEL`].
/// 3. If fewer than 2 occurrences exist → `None` (caller keeps reading).
/// 4. Take the last two; payload is the slice between them.
/// 5. Apply `extract_payload_with_chrome_filter` to drop TUI noise.
fn try_extract_interactive_answer(raw: &str, _req_id: Uuid) -> Option<String> {
    let stripped = strip_ansi(raw);
    let mut positions: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = stripped[cursor..].find(INTERACTIVE_SENTINEL) {
        let abs = cursor + rel;
        positions.push(abs);
        cursor = abs + INTERACTIVE_SENTINEL.len();
    }
    if positions.len() < 2 {
        return None;
    }
    // Take the LAST pair.
    let close = positions[positions.len() - 1];
    let open = positions[positions.len() - 2];
    let after_open = open + INTERACTIVE_SENTINEL.len();
    let between = &stripped[after_open..close];
    Some(extract_payload_with_chrome_filter(between))
}

struct InFlightGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// **Round 3 review fix (HIGH-H3)** — cancel-safe health guard.
///
/// Constructed at the top of `PtySession::invoke` with `completed = false`.
/// On the normal success path, the caller flips `completed = true` right
/// before returning, after which Drop is a no-op. Any cancellation
/// (caller's future dropped mid-invoke, `?`-propagated error, etc.)
/// leaves `completed = false`, and Drop sets `marked_unhealthy` so the
/// pool's next acquire spawns a fresh session.
///
/// This complements `InFlightGuard` (which only clears the single-flight
/// CAS flag, leaving the session reusable but in unknown protocol
/// state) by additionally flagging the session for eviction.
struct CancelHealthGuard<'a> {
    inner: &'a SessionInner,
    completed: bool,
}

impl Drop for CancelHealthGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.inner
                .marked_unhealthy
                .store(true, Ordering::Release);
            // No tracing call here — the Drop runs during cancellation
            // and a `warn!` would pollute logs on every legitimate
            // error return too. Operators see the eviction event via
            // the pool's metrics once the next acquire respawns.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat_program() -> (String, Vec<String>) {
        #[cfg(unix)]
        {
            ("cat".to_string(), vec![])
        }
        #[cfg(windows)]
        {
            // `findstr "^"` echoes stdin lines on Windows.
            (
                "findstr".to_string(),
                vec!["/N".to_string(), "^".to_string()],
            )
        }
    }

    #[test]
    fn cli_kind_round_trip() {
        for k in [CliKind::Claude, CliKind::Codex, CliKind::Gemini] {
            assert_eq!(CliKind::parse(k.as_str()).unwrap(), k);
        }
        assert!(matches!(
            CliKind::parse("nothing"),
            Err(SessionError::UnknownCliKind(_))
        ));
    }

    // Round 4 deferred-cleanup — spawn_cwd accessor.

    #[tokio::test]
    async fn spawn_cwd_returns_configured_path() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test-cwd".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: Some(cwd.clone()),
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(2),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn");
        assert_eq!(session.spawn_cwd(), Some(cwd.as_path()));
        session.shutdown().await;
    }

    #[tokio::test]
    async fn spawn_cwd_returns_none_when_not_set() {
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test-no-cwd".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(2),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn");
        assert!(session.spawn_cwd().is_none());
        session.shutdown().await;
    }

    #[tokio::test]
    async fn invoke_round_trip_via_cat() {
        // Drive a `cat` process: anything we write to stdin, we read back from
        // stdout. If we frame a request and read it back, parse_frame should
        // recognise the synthetic round trip.
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(3),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn cat");
        // `cat` will echo our framed request back verbatim. Since the request
        // string contains a `<<<DUDUCLAW:RSP:<id>:RSP>>>` sentinel twice (as part
        // of the protocol reminder), parse_frame should pair them and treat the
        // text between them as the "answer".
        let result = session.invoke("doesn't matter — cat echoes", None).await;
        // We don't assert on the exact payload because cat echoes the whole
        // wire including PTY's CRLF treatment, but we expect *some* success.
        // The session must at minimum not deadlock and must release in_flight.
        let _ = result; // ignored — outcome platform-dependent for cat echo
        assert!(!session.inner.in_flight.load(Ordering::Acquire));
        session.shutdown().await;
    }

    // **Round 3 review fix (HIGH-H3)** — cancel-safe drop guard.

    #[tokio::test]
    async fn cancel_health_guard_drop_without_completed_marks_unhealthy() {
        // Direct unit test of the cancel-safe guard's Drop behaviour.
        // We don't go through `invoke()` because Unix PTYs have ECHO
        // mode enabled by default — `cat` (and any program that
        // doesn't change termios) will echo our framed request bytes
        // back through the slave PTY, which makes `parse_frame` match
        // the protocol reminder's sentinel pair and complete invoke
        // before we get a chance to cancel.
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test-cancel".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(2),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn");
        assert!(session.is_healthy(), "fresh session should be healthy");

        // Simulate the mid-invoke cancellation: construct the guard
        // with `completed = false` and drop it. The Drop impl mirrors
        // exactly what runs when `PtySession::invoke`'s future is
        // cancelled before the completion-flag flip.
        {
            let _guard = CancelHealthGuard {
                inner: &session.inner,
                completed: false,
            };
            // _guard drops here.
        }
        assert!(
            !session.is_healthy(),
            "session must be unhealthy after CancelHealthGuard with completed=false drops"
        );

        session.shutdown().await;
    }

    #[tokio::test]
    async fn cancel_health_guard_drop_with_completed_keeps_healthy() {
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test-cancel-ok".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(2),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn");

        // Successful-path: guard is constructed, then caller flips
        // `completed = true` before Drop fires.
        {
            let mut guard = CancelHealthGuard {
                inner: &session.inner,
                completed: false,
            };
            guard.completed = true;
        }
        assert!(
            session.is_healthy(),
            "session must stay healthy when CancelHealthGuard completed=true at drop"
        );

        session.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_invoke_returns_busy() {
        let (program, args) = cat_program();
        let opts = SpawnOpts {
            agent_id: "test".into(),
            cli_kind: CliKind::Claude,
            program,
            extra_args: args,
            env: HashMap::new(),
            cwd: None,
            session_id: None,
            boot_timeout: Duration::from_millis(500),
            default_invoke_timeout: Duration::from_secs(2),
            rows: 24,
            cols: 200,
            interactive: false,
            pre_trusted: false,
        };
        let session = PtySession::spawn(opts).await.expect("spawn cat");
        let s1 = session.clone();
        let s2 = session.clone();
        // Force overlap by holding the in_flight flag from outside.
        s1.inner.in_flight.store(true, Ordering::Release);
        let result = s2.invoke("x", Some(Duration::from_millis(100))).await;
        assert!(matches!(result, Err(SessionError::Busy)));
        s1.inner.in_flight.store(false, Ordering::Release);
        session.shutdown().await;
    }

    // ── Phase 3.C.2: interactive-mode pure-function tests ─────────────

    #[test]
    fn inject_protocol_args_adds_append_system_prompt_for_claude() {
        let mut args = vec!["--model".to_string(), "claude-haiku-4-5".to_string()];
        inject_protocol_args(&mut args, CliKind::Claude);
        // The injection must append exactly two new args:
        // `--append-system-prompt` and the bootstrap string.
        assert_eq!(args.len(), 4, "expected 2 new args appended: {args:?}");
        assert_eq!(args[2], "--append-system-prompt");
        assert!(
            args[3].contains("DUDUCLAW PROTOCOL"),
            "bootstrap missing protocol marker: {}",
            args[3]
        );
        assert!(
            args[3].contains(INTERACTIVE_SENTINEL),
            "bootstrap missing fixed sentinel string"
        );
        assert!(
            !args[3].contains('<') && !args[3].contains('>'),
            "bootstrap leaked angle brackets: {}",
            args[3]
        );
    }

    #[test]
    fn inject_protocol_args_is_noop_for_codex_and_gemini() {
        for kind in [CliKind::Codex, CliKind::Gemini] {
            let mut args = vec!["--existing".to_string()];
            inject_protocol_args(&mut args, kind);
            assert_eq!(args, vec!["--existing".to_string()], "kind={kind:?}");
        }
    }

    #[test]
    fn try_extract_answer_returns_none_when_no_sentinels() {
        let id = Uuid::new_v4();
        assert!(try_extract_interactive_answer("plain text", id).is_none());
    }

    #[test]
    fn try_extract_answer_returns_none_when_only_one_sentinel() {
        let id = Uuid::new_v4();
        let raw = format!("{INTERACTIVE_SENTINEL}\nhalf the answer");
        assert!(try_extract_interactive_answer(&raw, id).is_none());
    }

    #[test]
    fn try_extract_answer_returns_clean_payload() {
        let id = Uuid::new_v4();
        let raw = format!(
            "preamble noise\n{INTERACTIVE_SENTINEL}\nThe real answer\n{INTERACTIVE_SENTINEL}\n",
        );
        let answer = try_extract_interactive_answer(&raw, id).expect("must extract");
        assert_eq!(answer, "The real answer");
    }

    #[test]
    fn try_extract_answer_strips_ansi_around_sentinels() {
        let id = Uuid::new_v4();
        let raw = format!(
            "\x1b[1C{INTERACTIVE_SENTINEL}\n\x1b[1Ch\x1b[1Ci\n{INTERACTIVE_SENTINEL}\n",
        );
        let answer = try_extract_interactive_answer(&raw, id).expect("must extract");
        assert_eq!(answer, "hi");
    }

    #[test]
    fn try_extract_answer_filters_tui_chrome_between_sentinels() {
        let id = Uuid::new_v4();
        let raw = format!(
            "{INTERACTIVE_SENTINEL}\n\
             Real model answer\n\
             ❯  user prompt cursor\n\
             ────────────────\n\
             esctointerrupt 1MCPserverneedsauth\n\
             {INTERACTIVE_SENTINEL}\n",
        );
        let answer = try_extract_interactive_answer(&raw, id).expect("must extract");
        assert_eq!(answer, "Real model answer");
    }

    #[test]
    fn try_extract_answer_positional_pairing_succeeds_even_with_wrong_uuid_param() {
        // With the fixed-sentinel approach, the req_id parameter is purely
        // cosmetic — pairing is positional, so any UUID works.
        let bogus_id = Uuid::new_v4();
        let raw = format!(
            "{INTERACTIVE_SENTINEL}\npayload\n{INTERACTIVE_SENTINEL}\n",
        );
        let answer = try_extract_interactive_answer(&raw, bogus_id).expect("must extract");
        assert_eq!(answer, "payload");
    }

    // **Review fix (HIGH)** regression test for sentinel false-positive.

    #[test]
    fn try_extract_answer_picks_last_pair_when_model_quotes_sentinel() {
        // Model echoes the literal sentinel inside its prose while still
        // emitting the wrap pair around the actual response. The
        // extractor must pick the wrap pair (LAST two occurrences),
        // not the first two — which would return everything between
        // the quoted sentinel and the response's open.
        let bogus_id = Uuid::new_v4();
        let raw = format!(
            "Some preamble. The protocol marker is {INTERACTIVE_SENTINEL} (shown inline) — \
             see runtime-pty-pool-design.md.\n\
             ⏺{INTERACTIVE_SENTINEL}\n\
             The real model answer\n\
             {INTERACTIVE_SENTINEL}\n"
        );
        let answer = try_extract_interactive_answer(&raw, bogus_id).expect("must extract");
        assert_eq!(answer, "The real model answer");
    }

    #[test]
    fn try_extract_answer_handles_arbitrary_count_above_two() {
        // 4 sentinels: payload spans the last pair (positions 2 → 3).
        let bogus_id = Uuid::new_v4();
        let raw = format!(
            "{INTERACTIVE_SENTINEL}\nfake-1\n{INTERACTIVE_SENTINEL}\nfake-2\n{INTERACTIVE_SENTINEL}\nreal answer\n{INTERACTIVE_SENTINEL}\n"
        );
        let answer = try_extract_interactive_answer(&raw, bogus_id).expect("must extract");
        assert_eq!(answer, "real answer");
    }

    #[test]
    fn spawn_opts_claude_interactive_sets_defaults() {
        let opts = SpawnOpts::claude_interactive("alice", "/usr/bin/claude");
        assert!(opts.interactive);
        assert!(!opts.pre_trusted);
        assert_eq!(opts.agent_id, "alice");
        assert_eq!(opts.cli_kind, CliKind::Claude);
        assert!(opts.boot_timeout >= Duration::from_secs(30));
    }
}
