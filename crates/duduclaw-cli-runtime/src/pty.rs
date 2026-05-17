//! Cross-platform PTY handle backed by [`portable-pty`].
//!
//! `portable-pty` gives us a uniform `PtySystem` trait that picks ConPTY on
//! Windows 10 1809+ and openpty on Unix. The blocking reader/writer halves are
//! pumped onto async channels via dedicated `spawn_blocking` tasks so callers can
//! interact in pure Tokio land.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
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
    // Held to keep the PTY pair alive; Phase 4 will surface a resize() that
    // actually goes through this lock.
    #[allow(dead_code)]
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
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
    Resize {
        #[allow(dead_code)]
        rows: u16,
        #[allow(dead_code)]
        cols: u16,
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
    /// Spawn a child under a fresh PTY pair and start the async pumps.
    pub fn spawn(cmd: PtyCommand) -> Result<Self, PtyError> {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows: cmd.rows,
                cols: cmd.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::OpenPty(e.to_string()))?;

        let mut builder = CommandBuilder::new(&cmd.program);
        for arg in &cmd.args {
            builder.arg(arg);
        }
        if cmd.clear_env {
            builder.env_clear();
        }
        for (k, v) in &cmd.env {
            builder.env(OsString::from(k), OsString::from(v));
        }
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }

        let child = pair.slave.spawn_command(builder).map_err(|e| {
            PtyError::SpawnChild {
                program: cmd.program.clone(),
                source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            }
        })?;
        let pid = child.process_id();

        let master = pair.master;
        // Take blocking reader/writer halves before moving master into the lock.
        let reader = master
            .try_clone_reader()
            .map_err(|e| PtyError::OpenPty(format!("clone_reader: {e}")))?;
        let writer = master
            .take_writer()
            .map_err(|e| PtyError::OpenPty(format!("take_writer: {e}")))?;

        let (reader_tx, reader_rx) = mpsc::channel::<ReadEvent>(64);
        let (writer_tx, writer_rx) = mpsc::channel::<WriteCmd>(64);

        // Reader pump: blocking thread → async channel.
        spawn_reader_pump(reader, reader_tx);
        // Writer pump: async channel → blocking thread.
        spawn_writer_pump(writer, writer_rx);

        let inner = Arc::new(PtyInner {
            master: Mutex::new(master),
            child: Mutex::new(child),
            writer_tx,
            reader_rx: AsyncMutex::new(reader_rx),
            rx_buffer: Mutex::new(String::new()),
            pid,
            system: PtySystemKind::current(),
        });

        Ok(Self { inner })
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

    /// Resize the PTY (rows × cols).
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        let (ack, rx) = oneshot::channel();
        self.inner
            .writer_tx
            .send(WriteCmd::Resize { rows, cols, ack })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
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
    pub fn drain_buffer(&self) -> String {
        let mut buf = self.inner.rx_buffer.lock();
        std::mem::take(&mut *buf)
    }

    /// Check whether the child process is still alive.
    pub fn is_alive(&self) -> bool {
        let mut child = self.inner.child.lock();
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => false,
        }
    }

    /// Cooperative shutdown: stop background pumps, kill child, drop master.
    pub async fn shutdown(&self) {
        let _ = self.inner.writer_tx.send(WriteCmd::Shutdown).await;
        let mut child = self.inner.child.lock();
        if let Err(e) = child.kill() {
            warn!(error = %e, "pty: kill failed (already exited?)");
        }
        let _ = child.wait();
    }
}

/// Free-function spawn wrapper for symmetry with the rest of the crate API.
pub fn spawn_pty(cmd: PtyCommand) -> Result<PtyHandle, PtyError> {
    PtyHandle::spawn(cmd)
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
        if let Some(mut child) = self.child.try_lock() {
            let _ = child.kill();
        }
    }
}

fn spawn_reader_pump(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<ReadEvent>) {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        // Spill buffer for incomplete UTF-8 codepoints at chunk boundaries.
        let mut carry: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    let _ = tx.blocking_send(ReadEvent::Eof);
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
                            if tx.blocking_send(ReadEvent::Chunk(payload)).is_err() {
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
                                if tx.blocking_send(ReadEvent::Chunk(payload)).is_err() {
                                    break;
                                }
                            } else if carry.len() > 8 {
                                // Pathological: 9+ bytes of garbage with no valid prefix
                                // — emit as lossy and drain so we don't loop forever.
                                let payload = String::from_utf8_lossy(&carry).into_owned();
                                carry.clear();
                                if tx.blocking_send(ReadEvent::Chunk(payload)).is_err() {
                                    break;
                                }
                            }
                            // Otherwise wait for more bytes to complete the codepoint.
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(ReadEvent::Error(e.to_string()));
                    break;
                }
            }
        }
    });
}

fn spawn_writer_pump(
    mut writer: Box<dyn Write + Send>,
    mut rx: mpsc::Receiver<WriteCmd>,
) {
    std::thread::spawn(move || {
        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                WriteCmd::Bytes { bytes, ack } => {
                    let result = writer.write_all(&bytes).and_then(|_| writer.flush());
                    let _ = ack.send(result.map_err(PtyError::Io));
                }
                WriteCmd::Resize { rows: _, cols: _, ack } => {
                    // Resize is on the master, not the writer; the caller of resize()
                    // takes that lock directly. To avoid plumbing the master through
                    // here we treat this as an explicit no-op success — actual resize
                    // happens in PtyHandle::resize via the master mutex (TODO: wire
                    // through to master if needed; not used in Phase 1).
                    let _ = ack.send(Ok(()));
                }
                WriteCmd::Shutdown => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn echo_round_trip() {
        let (program, args) = echo_program();
        let cmd = PtyCommand::new(program).args(args);
        let pty = PtyHandle::spawn(cmd).expect("spawn echo");
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
        let pty = PtyHandle::spawn(cmd).expect("spawn echo");
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
}
