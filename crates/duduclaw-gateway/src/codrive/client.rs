//! Long-lived Unix-socket client for the `duduclaw-comp` co-drive
//! injection protocol.
//!
//! Wire contract (comp side, `duduclaw-comp/src/codrive/`; mirrored here
//! by hand since the gateway cannot depend on that Linux-only, detached
//! workspace crate — see the module docs on [`super`]): one persistent
//! connection, JSON lines, one ack per command. The FIRST line sent after
//! connecting must be `{"op":"auth","token":"<hex>"}`; every command after
//! that gets exactly one ack line back, with `{"event": "..."}` lines
//! (frozen/resumed/emergency_stop) interleaved asynchronously in the
//! response stream.
//!
//! This client deliberately keeps ONE connection open for the whole script
//! run ([`super::driver::run_script`]) — reconnecting mid-session would
//! look like a brand new co-drive session to comp (CD-0 §9 fixed exactly
//! this bug: a reconnect used to silently clear the human-frozen flag).

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Wire types ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodriveButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodriveButtonState {
    Press,
    Release,
}

/// One command sent to comp. `#[serde(tag = "op")]` renders exactly the
/// wire shape the protocol contract specifies — see the `wire_shape_*`
/// tests at the bottom of this file.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CodriveCmd {
    Auth { token: String },
    Move { x: f64, y: f64 },
    Button { btn: CodriveButton, state: CodriveButtonState },
    Text { s: String },
    KeyName { name: String, state: CodriveButtonState },
    Highlight { x: f64, y: f64, w: f64, h: f64, ms: u32 },
    Status,
    Resume,
    /// CD-3 (DESIGN §5's "接手/交還＋watch mode" row): agent-initiated
    /// hand-off to the human — comp freezes the seat AND disables the
    /// shadow-bypass exception (see comp's `codrive/takeover.rs`). Ends the
    /// same way any freeze ends, via the human's Super+Enter.
    TakeOver { reason: String },
    /// CD-3: toggles idle-based auto-pause supervision for the rest of this
    /// session (comp's `codrive/watch.rs`).
    Watch { enable: bool },
}

/// Every ack shape the protocol can return, deserialized permissively
/// (every field but `ok` is optional) since the shape varies by op —
/// `{"ok":true,"authenticated":true}` for auth, `{"ok":true,"frozen":bool}`
/// for an injection op, `{"ok":true,"frozen":bool,"terminated":bool}` for
/// `status`, `{"ok":false,"error":"..."}` for a rejection, `{"ok":false,
/// "frozen":true,"reason":"agent_seat_frozen"}` for a frozen-dropped
/// injection.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CodriveAck {
    pub ok: bool,
    #[serde(default)]
    pub authenticated: Option<bool>,
    #[serde(default)]
    pub frozen: Option<bool>,
    #[serde(default)]
    pub terminated: Option<bool>,
    /// CD-3: distinguishes an agent-initiated takeover from an ordinary
    /// human-triggered freeze — both read `frozen:true`, only this flips
    /// for a takeover. Present on every `status` ack.
    #[serde(default)]
    pub takeover: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// An unsolicited `{"event": "..."}` line, interleaved with acks in the
/// read stream (`frozen` / `resumed` / `emergency_stop`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CodriveEvent {
    pub event: String,
}

/// Client-side error classification.
#[derive(Debug, Error)]
pub enum CodriveClientError {
    #[error("could not connect to comp socket at {path}: {source}")]
    Connect { path: String, #[source] source: std::io::Error },
    #[error("comp auth failed: {0}")]
    Auth(String),
    #[error("comp transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("comp returned malformed response: {0}")]
    Decode(String),
    #[error("comp call timed out after {0:?}")]
    Timeout(Duration),
    /// The action was NOT applied — comp dropped it because the agent seat
    /// is currently frozen by human input (design §6 red line 3: human
    /// input always wins, no exceptions). Not a transport failure; the
    /// caller is expected to poll `status` and retry.
    #[error("agent seat is frozen — action was dropped, not applied")]
    Frozen,
    /// The co-drive session ended (emergency stop, or comp closed the
    /// connection) — this socket can no longer be used.
    #[error("co-drive session terminated")]
    Terminated,
}

#[cfg(unix)]
mod unix_impl {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::UnixStream;

    use super::*;

    /// One persistent connection to the comp co-drive socket. Not `Clone` —
    /// a co-drive session holds exactly one connection for its whole
    /// lifetime (see the module docs on [`super`]).
    pub struct CodriveClient {
        reader: BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
        pending_events: VecDeque<CodriveEvent>,
        op_timeout: Duration,
    }

    impl CodriveClient {
        /// Connect and authenticate. Reads `token` verbatim (already
        /// resolved/trimmed by the caller) and sends it as the mandatory
        /// first line. `Err(Auth)` on rejection; comp closes the
        /// connection on auth failure per the wire contract, so no retry
        /// is attempted here.
        pub async fn connect(
            socket_path: &Path,
            token: &str,
            op_timeout: Duration,
        ) -> Result<Self, CodriveClientError> {
            let connect = tokio::time::timeout(op_timeout, UnixStream::connect(socket_path))
                .await
                .map_err(|_| CodriveClientError::Timeout(op_timeout))?;
            let stream = connect.map_err(|e| CodriveClientError::Connect {
                path: socket_path.display().to_string(),
                source: e,
            })?;
            let (read_half, write_half) = stream.into_split();
            let mut client = Self {
                reader: BufReader::new(read_half),
                writer: write_half,
                pending_events: VecDeque::new(),
                op_timeout,
            };
            let ack = client
                .write_and_read_ack(&CodriveCmd::Auth { token: token.to_string() })
                .await?;
            if !ack.ok || ack.authenticated != Some(true) {
                return Err(CodriveClientError::Auth(ack.error.unwrap_or_else(|| "auth_failed".to_string())));
            }
            Ok(client)
        }

        /// Send one command and return its ack. Frozen/terminated protocol
        /// states are surfaced as [`CodriveClientError::Frozen`] /
        /// [`CodriveClientError::Terminated`] rather than `Ok(ack)` so
        /// callers don't have to re-derive the same two checks at every
        /// call site — every other `ok:false` ack (e.g. `resume`'s
        /// `resume_is_human_only`) is returned as `Ok` for the caller to
        /// interpret on its own.
        pub async fn send(&mut self, cmd: &CodriveCmd) -> Result<CodriveAck, CodriveClientError> {
            let ack = self.write_and_read_ack(cmd).await?;
            if !ack.ok {
                if ack.frozen == Some(true) {
                    return Err(CodriveClientError::Frozen);
                }
                if ack.error.as_deref() == Some("session_terminated") {
                    return Err(CodriveClientError::Terminated);
                }
            }
            Ok(ack)
        }

        /// Drain and return every event line observed since the last call
        /// (comp interleaves `frozen`/`resumed`/`emergency_stop` events
        /// into the ack stream at any point).
        pub fn drain_events(&mut self) -> Vec<CodriveEvent> {
            self.pending_events.drain(..).collect()
        }

        async fn write_and_read_ack(&mut self, cmd: &CodriveCmd) -> Result<CodriveAck, CodriveClientError> {
            let mut line = serde_json::to_string(cmd)
                .map_err(|e| CodriveClientError::Decode(format!("encode command: {e}")))?;
            line.push('\n');
            let timeout_dur = self.op_timeout;
            tokio::time::timeout(timeout_dur, self.writer.write_all(line.as_bytes()))
                .await
                .map_err(|_| CodriveClientError::Timeout(timeout_dur))??;
            self.read_ack().await
        }

        /// Read lines until an ack (a line carrying `ok`) arrives, stashing
        /// any interleaved `{"event": ...}` lines along the way. EOF (comp
        /// closed the connection, e.g. right after an emergency stop) maps
        /// to [`CodriveClientError::Terminated`].
        async fn read_ack(&mut self) -> Result<CodriveAck, CodriveClientError> {
            loop {
                let mut buf = String::new();
                let timeout_dur = self.op_timeout;
                let n = tokio::time::timeout(timeout_dur, self.reader.read_line(&mut buf))
                    .await
                    .map_err(|_| CodriveClientError::Timeout(timeout_dur))??;
                if n == 0 {
                    return Err(CodriveClientError::Terminated);
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: serde_json::Value = serde_json::from_str(trimmed)
                    .map_err(|e| CodriveClientError::Decode(format!("{e}: {trimmed}")))?;
                if value.get("event").is_some() {
                    let event: CodriveEvent = serde_json::from_value(value)
                        .map_err(|e| CodriveClientError::Decode(format!("event: {e}")))?;
                    self.pending_events.push_back(event);
                    continue;
                }
                let ack: CodriveAck = serde_json::from_value(value)
                    .map_err(|e| CodriveClientError::Decode(format!("ack: {e}")))?;
                return Ok(ack);
            }
        }
    }
}

#[cfg(unix)]
pub use unix_impl::CodriveClient;

/// Non-unix stub (e.g. Windows CI target): the co-drive socket only exists
/// on the Linux appliance image, so every call fails closed with the same
/// `Connect` shape the "socket missing" path produces on unix. Mirrors
/// `duduclaw_sysd::client::SysdClient`'s platform split — the exact
/// pattern that broke Windows release CI once already when a unix-only
/// transport compiled ungated.
#[cfg(not(unix))]
pub struct CodriveClient;

#[cfg(not(unix))]
impl CodriveClient {
    pub async fn connect(
        socket_path: &Path,
        _token: &str,
        _op_timeout: Duration,
    ) -> Result<Self, CodriveClientError> {
        Err(CodriveClientError::Connect {
            path: socket_path.display().to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "co-drive requires Unix domain sockets — it only runs on the Linux appliance image",
            ),
        })
    }

    pub async fn send(&mut self, _cmd: &CodriveCmd) -> Result<CodriveAck, CodriveClientError> {
        Err(CodriveClientError::Terminated)
    }

    pub fn drain_events(&mut self) -> Vec<CodriveEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Wire shape lock: every op serializes to exactly the JSON the
    // protocol contract specifies. ─────────────────────────────────────

    #[test]
    fn wire_shape_auth() {
        let cmd = CodriveCmd::Auth { token: "deadbeef".into() };
        assert_eq!(serde_json::to_value(&cmd).unwrap(), serde_json::json!({"op": "auth", "token": "deadbeef"}));
    }

    #[test]
    fn wire_shape_move() {
        let cmd = CodriveCmd::Move { x: 10.5, y: 20.0 };
        assert_eq!(serde_json::to_value(&cmd).unwrap(), serde_json::json!({"op": "move", "x": 10.5, "y": 20.0}));
    }

    #[test]
    fn wire_shape_button() {
        let cmd = CodriveCmd::Button { btn: CodriveButton::Left, state: CodriveButtonState::Press };
        assert_eq!(
            serde_json::to_value(&cmd).unwrap(),
            serde_json::json!({"op": "button", "btn": "left", "state": "press"})
        );
    }

    #[test]
    fn wire_shape_text() {
        let cmd = CodriveCmd::Text { s: "echo done".into() };
        assert_eq!(serde_json::to_value(&cmd).unwrap(), serde_json::json!({"op": "text", "s": "echo done"}));
    }

    #[test]
    fn wire_shape_key_name() {
        let cmd = CodriveCmd::KeyName { name: "enter".into(), state: CodriveButtonState::Release };
        assert_eq!(
            serde_json::to_value(&cmd).unwrap(),
            serde_json::json!({"op": "key_name", "name": "enter", "state": "release"})
        );
    }

    #[test]
    fn wire_shape_highlight() {
        let cmd = CodriveCmd::Highlight { x: 10.0, y: 10.0, w: 200.0, h: 80.0, ms: 200 };
        assert_eq!(
            serde_json::to_value(&cmd).unwrap(),
            serde_json::json!({"op": "highlight", "x": 10.0, "y": 10.0, "w": 200.0, "h": 80.0, "ms": 200})
        );
    }

    #[test]
    fn wire_shape_status_and_resume() {
        assert_eq!(serde_json::to_value(&CodriveCmd::Status).unwrap(), serde_json::json!({"op": "status"}));
        assert_eq!(serde_json::to_value(&CodriveCmd::Resume).unwrap(), serde_json::json!({"op": "resume"}));
    }

    #[test]
    fn wire_shape_take_over() {
        let cmd = CodriveCmd::TakeOver { reason: "login page needs a human".into() };
        assert_eq!(
            serde_json::to_value(&cmd).unwrap(),
            serde_json::json!({"op": "take_over", "reason": "login page needs a human"})
        );
    }

    #[test]
    fn wire_shape_watch() {
        assert_eq!(
            serde_json::to_value(&CodriveCmd::Watch { enable: true }).unwrap(),
            serde_json::json!({"op": "watch", "enable": true})
        );
        assert_eq!(
            serde_json::to_value(&CodriveCmd::Watch { enable: false }).unwrap(),
            serde_json::json!({"op": "watch", "enable": false})
        );
    }

    // ── Ack/event deserialization: every documented server shape parses. ─

    #[test]
    fn ack_auth_success_and_failure() {
        let ok: CodriveAck = serde_json::from_value(serde_json::json!({"ok": true, "authenticated": true})).unwrap();
        assert!(ok.ok);
        assert_eq!(ok.authenticated, Some(true));

        let fail: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "auth_failed"})).unwrap();
        assert!(!fail.ok);
        assert_eq!(fail.error.as_deref(), Some("auth_failed"));
    }

    #[test]
    fn ack_injection_success_and_frozen_drop() {
        let ok: CodriveAck = serde_json::from_value(serde_json::json!({"ok": true, "frozen": false})).unwrap();
        assert!(ok.ok);
        assert_eq!(ok.frozen, Some(false));

        let dropped: CodriveAck = serde_json::from_value(
            serde_json::json!({"ok": false, "frozen": true, "reason": "agent_seat_frozen"}),
        )
        .unwrap();
        assert!(!dropped.ok);
        assert_eq!(dropped.frozen, Some(true));
        assert_eq!(dropped.reason.as_deref(), Some("agent_seat_frozen"));
    }

    #[test]
    fn ack_status() {
        let ack: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": true, "frozen": true, "terminated": false})).unwrap();
        assert!(ack.ok);
        assert_eq!(ack.frozen, Some(true));
        assert_eq!(ack.terminated, Some(false));
        assert_eq!(ack.takeover, None, "an ack from before CD-3 (no takeover field) must parse fine");
    }

    #[test]
    fn ack_status_with_takeover_field() {
        let ack: CodriveAck = serde_json::from_value(
            serde_json::json!({"ok": true, "frozen": true, "terminated": false, "takeover": true}),
        )
        .unwrap();
        assert_eq!(ack.takeover, Some(true));
    }

    #[test]
    fn ack_resume_and_session_terminated() {
        let resume: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "resume_is_human_only"})).unwrap();
        assert_eq!(resume.error.as_deref(), Some("resume_is_human_only"));

        let terminated: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "session_terminated"})).unwrap();
        assert_eq!(terminated.error.as_deref(), Some("session_terminated"));
    }

    #[test]
    fn event_shapes() {
        for name in ["frozen", "resumed", "emergency_stop"] {
            let ev: CodriveEvent = serde_json::from_value(serde_json::json!({"event": name})).unwrap();
            assert_eq!(ev.event, name);
        }
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn non_unix_stub_fails_closed() {
        let err = CodriveClient::connect(Path::new("/tmp/x.sock"), "tok", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(err, CodriveClientError::Connect { .. }));
    }
}
