//! Cross-platform PTY handle backed by the selected execution backend.
//!
//! LocalBackend owns portable-pty; protected Linux owns the daemon PTY.
//! Callers retain asynchronous streams and opaque lifecycle authority.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::CommandBuilder;
use duduclaw_core::execution_gate::{RunControl, PtySize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tracing::{debug, trace, warn};

use crate::error::PtyError;

/// Which PTY backend portable-pty chose. Useful for diagnostics + tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtySystemKind {
    /// Windows ConPTY (10 1809+).
    ConPty,
    /// Unix openpty / posix_openpt.
    Unix,
}

impl PtySystemKind {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            Self::ConPty
        }
        #[cfg(unix)]
        {
            Self::Unix
        }
    }
}

/// Spawn parameters for a PTY-backed child process.
#[derive(Debug, Clone)]
pub struct PtyCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub rows: u16,
    pub cols: u16,
    /// If false, the child inherits the parent's environment (then `env` is layered on top).
    /// If true, the child starts with an empty env and only sees `env`.
    pub clear_env: bool,
}

impl PtyCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            rows: 24,
            // Wide columns reduce the chance that the CLI's TUI wraps our sentinel
            // string across a line boundary (which would still parse correctly
            // because we look for `<<<...>>>` byte sequences, but keeps logs cleaner).
            cols: 200,
            clear_env: false,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
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

    pub fn cwd(mut self, p: PathBuf) -> Self {
        self.cwd = Some(p);
        self
    }

    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }
}

/// Live PTY handle. Cloning is cheap and intentionally shares the same master / child.
pub struct PtyHandle {
    inner: Arc<PtyInner>,
}

struct PtyInner {
    control: RunControl,
    writer_tx: mpsc::Sender<WriteCmd>,
    reader_rx: AsyncMutex<mpsc::Receiver<ReadEvent>>,
    /// Drained-yet-not-yet-consumed bytes from the reader pump (used to handle
    /// `read_until` partial sentinel matches).
    rx_buffer: Mutex<String>,
    pid: Option<u32>,
    system: PtySystemKind,
}

enum WriteCmd {
    Bytes {
        bytes: Vec<u8>,
        ack: oneshot::Sender<Result<(), PtyError>>,
    },
    Shutdown,
}

#[derive(Debug)]
enum ReadEvent {
    Chunk(String),
    Eof,
    Error(String),
}

impl PtyHandle {
    /// Spawn through the selected backend and retain its opaque lifecycle handle.
    pub async fn spawn(cmd: PtyCommand) -> Result<Self, PtyError> {
        Self::spawn_with_handoff(cmd, || {}).await
    }

    pub(crate) async fn spawn_with_handoff(
        cmd: PtyCommand, handed_off: impl FnOnce(),
    ) -> Result<Self, PtyError> {
        let builder = command_builder(&cmd);
        let mut child = duduclaw_core::execution_gate::spawn_pty(builder,
            PtySize { rows: cmd.rows, columns: cmd.cols },
            duduclaw_core::execution_gate::capture_class()).await
            .map_err(|source| PtyError::SpawnChild { program: cmd.program.clone(), source })?;
        handed_off();
        let pid = child.id();
        let control = child.control();
        let reader = child.output.take().ok_or(PtyError::Closed)?;
        let writer = child.input.take().ok_or(PtyError::Closed)?;
        let (reader_tx, reader_rx) = mpsc::channel::<ReadEvent>(64);
        let (writer_tx, writer_rx) = mpsc::channel::<WriteCmd>(64);
        spawn_reader_pump(reader, reader_tx);
        spawn_writer_pump(writer, writer_rx);
        // Keep the owning handle alive until termination. Cancellation is
        // independent of this wait and runs through the backend control handle.
        tokio::spawn(async move { let _ = child.wait().await; });
        Ok(Self { inner: Arc::new(PtyInner {
            control, writer_tx, reader_rx: AsyncMutex::new(reader_rx),
            rx_buffer: Mutex::new(String::new()), pid, system: PtySystemKind::current(),
        }) })
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    pub fn system(&self) -> PtySystemKind {
        self.inner.system
    }

    /// Write raw bytes into the PTY input. Awaits flush.
    pub async fn write_all(&self, bytes: &[u8]) -> Result<(), PtyError> {
        let (ack, rx) = oneshot::channel();
        self.inner
            .writer_tx
            .send(WriteCmd::Bytes {
                bytes: bytes.to_vec(),
                ack,
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    /// Resize the backend's real PTY (rows × columns).
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.inner.control.resize(PtySize { rows, columns: cols }).await.map_err(PtyError::Io)
    }

    /// Read until `marker` appears in the rolling buffer, OR until `deadline` elapses.
    ///
    /// On success returns the consumed prefix (excluding the marker bytes); the
    /// remaining tail stays in the rolling buffer for the next read.
    ///
    /// On timeout returns [`PtyError::ReadTimeout`] and the buffer is preserved.
    pub async fn read_until(&self, marker: &str, deadline: Duration) -> Result<String, PtyError> {
        let start = std::time::Instant::now();

        loop {
            // First check whether the marker is already buffered.
            {
                let mut buf = self.inner.rx_buffer.lock();
                if let Some(pos) = buf.find(marker) {
                    let prefix = buf[..pos].to_string();
                    let after = pos + marker.len();
                    buf.drain(..after);
                    return Ok(prefix);
                }
            }

            let remaining = match deadline.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r,
                _ => return Err(PtyError::ReadTimeout(deadline)),
            };

            // Wait for next chunk. tokio::Mutex is held across the await safely.
            let mut guard = self.inner.reader_rx.lock().await;
            let recv_result = tokio::time::timeout(remaining, guard.recv()).await;

            match recv_result {
                Ok(Some(ReadEvent::Chunk(c))) => {
                    trace!(bytes = c.len(), "pty: read chunk");
                    self.inner.rx_buffer.lock().push_str(&c);
                }
                Ok(Some(ReadEvent::Eof)) => {
                    debug!("pty: reader EOF");
                    return Err(PtyError::Closed);
                }
                Ok(Some(ReadEvent::Error(e))) => {
                    warn!(error = %e, "pty: reader error");
                    return Err(PtyError::Io(std::io::Error::other(e)));
                }
                Ok(None) => return Err(PtyError::Closed),
                Err(_elapsed) => return Err(PtyError::ReadTimeout(deadline)),
            }
        }
    }

    /// Read whatever bytes are currently buffered (non-blocking). Returns an empty
    /// string if nothing is available.
    ///
    /// **HC14 fix**: this only drains `rx_buffer` — bytes that the reader pump
    /// has produced but `read_until` has not yet pulled off the mpsc channel
    /// are NOT touched here. Callers that need to fully purge residual bytes
    /// from a previous turn (so they don't pollute the next response) must use
    /// [`PtyHandle::drain_residual`], which also non-blockingly drains the
    /// reader channel.
    pub fn drain_buffer(&self) -> String {
        let mut buf = self.inner.rx_buffer.lock();
        std::mem::take(&mut *buf)
    }

    /// **HC14 fix**: fully drain residual bytes left over from a prior turn —
    /// both the rolling `rx_buffer` AND any chunks the reader pump has already
    /// queued onto the mpsc channel but `read_until` never consumed.
    ///
    /// Without this, the reader channel can hold the previous turn's trailing
    /// bytes (e.g. a stray closing sentinel or TUI redraw), which the next
    /// `read_until` would then surface as the head of the new response,
    /// corrupting sentinel pairing.
    ///
    /// Non-blocking: uses `try_recv` in a tight loop so it never awaits. Drains
    /// the rolling buffer first, then everything currently sitting on the
    /// channel. Returns all drained bytes (the boot path / diagnostics may want
    /// to inspect them; the per-invoke path simply discards the result).
    pub fn drain_residual(&self) -> String {
        let mut out = self.drain_buffer();
        // Drain the reader channel without awaiting. We may not get the async
        // mutex if a `read_until` is concurrently in flight — but `invoke`
        // single-flights, so the per-invoke caller always has exclusive access.
        if let Ok(mut guard) = self.inner.reader_rx.try_lock() {
            loop {
                match guard.try_recv() {
                    Ok(ReadEvent::Chunk(c)) => out.push_str(&c),
                    // Stop on EOF / error / empty / disconnected — there's
                    // nothing more buffered to drain right now.
                    _ => break,
                }
            }
        }
        out
    }

    /// **M46 fix**: push bytes back onto the FRONT of the rolling buffer so the
    /// next `read_until` / `drain_buffer` sees them first.
    ///
    /// `collect_response` uses this when a frame's closing sentinel is followed
    /// by the start of the *next* response on a reused session: those trailing
    /// bytes must be re-buffered rather than discarded, or the next invoke loses
    /// its response head.
    pub fn push_buffer(&self, bytes: &str) {
        if bytes.is_empty() {
            return;
        }
        let mut buf = self.inner.rx_buffer.lock();
        // Prepend: existing buffered bytes are newer relative to `bytes` only if
        // `bytes` truly came earlier in the stream. In practice the caller hands
        // us the tail it over-read, and the buffer is empty (just drained), so a
        // prepend keeps stream order correct in every case.
        if buf.is_empty() {
            buf.push_str(bytes);
        } else {
            let existing = std::mem::take(&mut *buf);
            buf.push_str(bytes);
            buf.push_str(&existing);
        }
    }

    /// Check the backend's cached terminal state; a PID is never authority.
    pub fn is_alive(&self) -> bool {
        matches!(self.inner.control.try_wait(), Ok(None))
    }

    pub async fn shutdown(&self) {
        let _ = self.inner.writer_tx.send(WriteCmd::Shutdown).await;
        if let Err(error) = self.inner.control.kill().await {
            warn!(%error, "pty: backend termination failed");
        }
        let _ = self.inner.control.wait().await;
    }
}

pub async fn spawn_pty(cmd: PtyCommand) -> Result<PtyHandle, PtyError> {
    PtyHandle::spawn(cmd).await
}

fn command_builder(cmd: &PtyCommand) -> CommandBuilder {
    let mut builder = CommandBuilder::new(&cmd.program);
    for arg in &cmd.args { builder.arg(arg); }
    if cmd.clear_env { builder.env_clear(); }
    for (key, value) in &cmd.env { builder.env(OsString::from(key), OsString::from(value)); }
    if let Some(cwd) = &cmd.cwd { builder.cwd(cwd); }
    builder
}

impl Clone for PtyHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for PtyInner {
    fn drop(&mut self) {
        // Best-effort cleanup. Background pumps will see the channels close and exit.
        let _ = self.writer_tx.try_send(WriteCmd::Shutdown);
        let _ = self.control.start_kill();
    }
}

fn spawn_reader_pump(mut reader: duduclaw_core::execution_gate::ChildStdout, tx: mpsc::Sender<ReadEvent>) {
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        // Spill buffer for incomplete UTF-8 codepoints at chunk boundaries.
        let mut carry: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => {
                    let _ = tx.send(ReadEvent::Eof).await;
                    break;
                }
                Ok(n) => {
                    carry.extend_from_slice(&chunk[..n]);
                    // Decode as much complete UTF-8 as possible; keep trailing
                    // partial bytes for the next iteration.
                    match std::str::from_utf8(&carry) {
                        Ok(s) => {
                            let payload = s.to_string();
                            carry.clear();
                            if tx.send(ReadEvent::Chunk(payload)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let valid_up_to = e.valid_up_to();
                            if valid_up_to > 0 {
                                // The Utf8Error guarantees `carry[..valid_up_to]` is
                                // valid UTF-8; the safe checked call is free for
                                // already-validated bytes.
                                let payload = match std::str::from_utf8(&carry[..valid_up_to]) {
                                    Ok(s) => s.to_string(),
                                    Err(_) => String::new(),
                                };
                                let remainder = carry[valid_up_to..].to_vec();
                                carry = remainder;
                                if tx.send(ReadEvent::Chunk(payload)).await.is_err() {
                                    break;
                                }
                            } else if carry.len() > 8 {
                                // Pathological: 9+ bytes of garbage with no valid prefix
                                // — emit as lossy and drain so we don't loop forever.
                                let payload = String::from_utf8_lossy(&carry).into_owned();
                                carry.clear();
                                if tx.send(ReadEvent::Chunk(payload)).await.is_err() {
                                    break;
                                }
                            }
                            // Otherwise wait for more bytes to complete the codepoint.
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(ReadEvent::Error(e.to_string())).await;
                    break;
                }
            }
        }
    });
}

fn spawn_writer_pump(
    mut writer: duduclaw_core::execution_gate::ChildStdin,
    mut rx: mpsc::Receiver<WriteCmd>,
) {
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                WriteCmd::Bytes { bytes, ack } => {
                    let result = async {
                        writer.write_all(&bytes).await?;
                        writer.flush().await
                    }.await;
                    let _ = ack.send(result.map_err(PtyError::Io));
                }
                WriteCmd::Shutdown => break,
            }
        }
        let _ = writer.shutdown().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scheduled_grok_pty_keeps_typed_policy_across_task_boundary() {
        use duduclaw_core::execution_gate::{self, ExecutionClass, RuntimeKind, CapturedLaunch};
        let captured = execution_gate::with_class(ExecutionClass::Scheduled,
            execution_gate::with_runtime(RuntimeKind::Grok, async { CapturedLaunch::capture() })).await;
        tokio::spawn(async move {
            captured.scope(async {
                // These are the task-scoped values consumed by spawn_with_handoff.
                assert_eq!(execution_gate::capture_class(), ExecutionClass::Scheduled);
                assert_eq!(execution_gate::capture_runtime(), RuntimeKind::Grok);
                let command = PtyCommand::new("grok-fixture").args(["-p", "繁體中文 $HOME;\nnext"]);
                let builder = command_builder(&command);
                let before = execution_gate::pty_snapshot(&builder);
                assert_eq!(execution_gate::local_pty_roundtrip(builder,
                    PtySize { rows: command.rows, columns: command.cols }).await.unwrap(), before);
            }).await;
        }).await.unwrap();
    }


    fn echo_program() -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "echo".to_string(), "hello-pty".to_string()],
            )
        }
        #[cfg(unix)]
        {
            ("echo".to_string(), vec!["hello-pty".to_string()])
        }
    }

    // ConPTY round-trip is unreliable on headless Windows CI runners (the reader
    // pump doesn't observe the child's echo within the deadline). Covered on Unix.
    #[cfg_attr(windows, ignore = "ConPTY echo round-trip is flaky on headless Windows CI")]
    #[tokio::test]
    async fn echo_round_trip() {
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).await.expect("spawn echo");
        let out = pty
            .read_until("hello-pty", Duration::from_secs(5))
            .await
            .expect("should see echo");
        // Output may include leading data, but `read_until` returns the prefix
        // before the marker — so the original "hello-pty" itself has been consumed.
        assert!(!out.contains("hello-pty"), "marker should have been consumed");
        pty.shutdown().await;
    }

    #[tokio::test]
    async fn read_until_times_out_or_closes_when_marker_absent() {
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).await.expect("spawn echo");
        // After `echo` finishes, the reader eventually sees EOF, which surfaces
        // as PtyError::Closed; before EOF arrives, the marker absence yields
        // PtyError::ReadTimeout. Both outcomes prove `read_until` doesn't
        // synthesise a false success when the marker never appears.
        let result = pty
            .read_until("zzz-never-emitted-zzz", Duration::from_millis(300))
            .await;
        assert!(
            matches!(result, Err(PtyError::ReadTimeout(_)) | Err(PtyError::Closed)),
            "expected timeout or close, got {result:?}"
        );
        pty.shutdown().await;
    }

    #[test]
    fn pty_system_kind_matches_platform() {
        let kind = PtySystemKind::current();
        #[cfg(unix)]
        assert_eq!(kind, PtySystemKind::Unix);
        #[cfg(windows)]
        assert_eq!(kind, PtySystemKind::ConPty);
    }

    // ── M46: push_buffer re-buffering ─────────────────────────────────

    #[tokio::test]
    async fn push_buffer_round_trips_via_drain() {
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).await.expect("spawn echo");
        // Push synthetic tail bytes; drain_buffer must return them verbatim.
        pty.push_buffer("tail-bytes-from-prev-turn");
        let drained = pty.drain_buffer();
        assert!(
            drained.starts_with("tail-bytes-from-prev-turn"),
            "push_buffer content must be drained first: {drained:?}"
        );
        pty.shutdown().await;
    }

    #[tokio::test]
    async fn push_buffer_preserves_stream_order_when_buffer_nonempty() {
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).await.expect("spawn echo");
        // Simulate: buffer already holds "newer" bytes, then we push back the
        // "older" tail we over-read. Older must come out first.
        pty.push_buffer("NEWER");
        pty.push_buffer("OLDER");
        // After two pushes onto a (logically) growing front, the second push is
        // prepended ahead of the first.
        let drained = pty.drain_buffer();
        assert_eq!(drained, "OLDERNEWER", "second push must precede first");
        pty.shutdown().await;
    }

    // ── HC14: drain_residual drains the reader channel too ────────────

    // Depends on observing real ConPTY echo bytes on the reader channel —
    // unreliable on headless Windows CI. Covered on Unix.
    #[cfg_attr(windows, ignore = "ConPTY reader-channel drain is flaky on headless Windows CI")]
    #[tokio::test]
    async fn drain_residual_purges_reader_channel() {
        // `echo hello-pty` emits bytes that the reader pump queues onto the
        // mpsc channel. Without consuming via read_until, drain_buffer alone
        // would miss them (they're on the channel, not in rx_buffer).
        // drain_residual must surface them.
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).await.expect("spawn echo");

        // Give the reader pump time to push the echo onto the channel.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Peek: if drain_residual already sees the marker, stop early.
            let snapshot = {
                // Non-destructive-ish: we just check via a residual drain.
                pty.drain_residual()
            };
            if snapshot.contains("hello-pty") {
                // Success: the channel-queued bytes were drained.
                pty.shutdown().await;
                return;
            }
            // Re-buffer what we drained so the next iteration can still find it.
            pty.push_buffer(&snapshot);
        }
        panic!("drain_residual never surfaced reader-channel bytes");
    }
}

#[cfg(test)]
mod execution_backend_equivalence_tests {
    use super::*;

    #[tokio::test]
    async fn grok_pty_backend_command_matches_legacy_argv_env_cwd() {
        for clear_env in [false, true] {
            let mut command = PtyCommand::new("grok").args(["--prompt", "繁體中文 $HOME;\nnext", ""])
                .cwd(PathBuf::from("pty workspace")).env("PTY_FIXTURE", "literal value").size(31, 123);
            command.clear_env = clear_env;
            let actual = command_builder(&command);
            let mut legacy = CommandBuilder::new(&command.program);
            for arg in &command.args { legacy.arg(arg); }
            if command.clear_env { legacy.env_clear(); }
            for (key, value) in &command.env { legacy.env(OsString::from(key), OsString::from(value)); }
            legacy.cwd(command.cwd.as_ref().unwrap());
            assert_eq!(duduclaw_core::execution_gate::local_pty_roundtrip(actual,
                PtySize { rows: command.rows, columns: command.cols }).await.unwrap(),
                duduclaw_core::execution_gate::pty_snapshot(&legacy));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_pty_resize_changes_real_terminal_dimensions() {
        let pty = PtyHandle::spawn(PtyCommand::new("sh")
            .args(["-c", "read value; stty size"])).await.unwrap();
        pty.resize(37, 93).await.unwrap();
        pty.write_all(b"go\n").await.unwrap();
        pty.read_until("37 93", Duration::from_secs(5)).await.unwrap();
        pty.shutdown().await;
        assert!(!pty.is_alive());
    }
}
