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

use super::mode::{CodriveDrivingMode, CodriveHandoverReason};

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
    /// WP-CD4b-fix (B3): READ-ONLY query for where a client's *visible*
    /// window sits in comp's global logical coordinate space — the missing
    /// half of AT-SPI's `CoordType::Window` offsets (see
    /// [`super::atspi_locate`]'s module doc for why `CoordType::Screen` is
    /// unusable on GTK4). Answered by comp outside the frozen gate (it is a
    /// query, not an action); at least one of `app_id`/`pid` must be set or
    /// comp rejects the line. Both fields are omitted from the wire when
    /// `None` so an unused key never reaches comp's parser.
    WindowGeometry {
        #[serde(skip_serializing_if = "Option::is_none")]
        app_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
}

/// The `window` object comp returns on a successful `window_geometry`
/// query (comp's `codrive/window_geometry.rs::WindowGeometryInfo`).
///
/// `origin_*`/`width`/`height` are REQUIRED (no `#[serde(default)]`): a
/// half-populated reply must fail to parse into a
/// [`CodriveClientError::Decode`] rather than silently deserializing to
/// zeros — zeros here are exactly the bug class this whole round exists to
/// kill. `shadow_*`/`matched_via` are diagnostics and may be absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodriveWindowGeometry {
    /// Top-left of the VISIBLE window (CSD shadow excluded) in comp global
    /// logical coordinates — the origin AT-SPI `CoordType::Window` offsets
    /// are relative to.
    pub origin_x: i32,
    pub origin_y: i32,
    /// The visible window's size, used to bound-check a converted point.
    pub width: i32,
    pub height: i32,
    /// The client-side-decoration shadow inset, diagnostics only.
    /// Legitimately `0` when maximized/fullscreen.
    #[serde(default)]
    pub shadow_dx: i32,
    #[serde(default)]
    pub shadow_dy: i32,
    /// Which criterion comp used to resolve the query (`pid`,
    /// `pid+app_id`, `title_prefix`, …).
    #[serde(default)]
    pub matched_via: Option<String>,
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
    /// WP-CD4b-fix (B3): present only on a successful `window_geometry`
    /// ack. Absent on every other op (and on a comp too old to know the op
    /// at all, which answers a plain `{"ok":true,"frozen":false}` — hence
    /// `None` here MUST be treated as "unsupported / unusable", never as
    /// "origin (0,0)"; see [`super::atspi_locate::frame_from_ack`]).
    #[serde(default)]
    pub window: Option<CodriveWindowGeometry>,
    /// WP-CD4b-fix (B3): how many toplevels an `ambiguous_window` refusal
    /// matched — diagnostics for the audit trail.
    #[serde(default)]
    pub candidates: Option<usize>,

    // ── A2 driving-mode block (§3.1) ────────────────────────────────────
    // Present on a `status` ack from a comp that speaks A2. EVERY field
    // here is `#[serde(default)]` and that is load-bearing, not tidiness:
    // the gateway and comp are separately deployed binaries (gateway runs
    // as `duduclaw`, comp as `duduclaw-kiosk` on the appliance) and their
    // versions WILL skew. A comp that predates A2 answers the old
    // three-field shape `{"ok":true,"frozen":…,"terminated":…}`; that must
    // keep parsing into `None`s, never become a hard `Decode` error that
    // takes down `wait_for_resume`'s poll loop with it.
    //
    // `mode`/`handover_reason` are parsed into this crate's own closed
    // enums; an unrecognized token becomes `Unknown(<token>)` rather than a
    // decode failure or a silent fallback to `Human` — see [`super::mode`].
    /// Which seat is driving (A2 §1). `None` = comp did not report one.
    #[serde(default)]
    pub mode: Option<CodriveDrivingMode>,
    /// Why the seat is in `handover`; comp sends `null` in every other
    /// mode, which lands here as `None` (A2 §2).
    #[serde(default)]
    pub handover_reason: Option<CodriveHandoverReason>,
    /// Whether the agent is working in a shadow output. Deliberately NOT
    /// folded into `mode` — see [`super::mode`]'s module doc.
    #[serde(default)]
    pub shadow: Option<bool>,
    /// Whether idle-based watch supervision is armed for this session.
    #[serde(default)]
    pub watch_active: Option<bool>,
    /// Whether watch supervision has currently auto-paused the seat.
    #[serde(default)]
    pub watch_paused: Option<bool>,
}

/// An unsolicited `{"event": "..."}` line, interleaved with acks in the
/// read stream (`frozen` / `resumed` / `emergency_stop`).
///
/// A2 §3.2 adds one ADDITIVE event on top of those three —
/// `{"event":"driving_mode","mode":"codrive","reason":null}` — pushed only
/// when the mode genuinely changes. The two payload fields are
/// `#[serde(default)]` for the same version-skew reason [`CodriveAck`]'s
/// are, and they are the reason this client can report a driving mode
/// without sending a single extra `status` query: the push arrives on the
/// connection the session already holds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CodriveEvent {
    pub event: String,
    /// A2 §3.2: set on a `driving_mode` event, absent on every other.
    #[serde(default)]
    pub mode: Option<CodriveDrivingMode>,
    /// A2 §3.2: the handover reason on a `driving_mode` event; `null`
    /// whenever the new mode is not `handover`.
    #[serde(default)]
    pub reason: Option<CodriveHandoverReason>,
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
        /// A2: the most recent driving mode comp reported on THIS
        /// connection, from either source — a `status` ack's mode block
        /// (§3.1) or a pushed `driving_mode` event (§3.2). Updated where
        /// the line is parsed, never where it is consumed, so
        /// [`Self::drain_events`] taking the events away (as
        /// `step::wait_for_resume` does every poll) cannot erase it.
        last_mode: Option<CodriveDrivingMode>,
        last_handover_reason: Option<CodriveHandoverReason>,
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
                last_mode: None,
                last_handover_reason: None,
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

        /// A2: the last driving mode comp reported on this connection, or
        /// `None` if it never reported one (a pre-A2 comp, or a session so
        /// short nothing carrying a mode came back yet). Deliberately NOT
        /// defaulted to `Human` — see [`super::mode`]'s module doc.
        ///
        /// This is a passive observation, not a query: reading it sends
        /// nothing. Every source that populates it (`status` acks,
        /// `driving_mode` pushes) is traffic the session was already going
        /// to carry, which is why the run report can carry a mode without
        /// adding a single wire op to any existing script path.
        pub fn last_driving_mode(&self) -> Option<CodriveDrivingMode> {
            self.last_mode.clone()
        }

        /// The handover reason that accompanied [`Self::last_driving_mode`].
        pub fn last_handover_reason(&self) -> Option<CodriveHandoverReason> {
            self.last_handover_reason.clone()
        }

        /// Record a mode observation. A line that carries no mode leaves
        /// the cache untouched (absence is not evidence of `Human`); a line
        /// that does carry one replaces both fields together, so the reason
        /// can never outlive the mode it belonged to.
        fn observe_mode(
            &mut self,
            mode: Option<&CodriveDrivingMode>,
            reason: Option<&CodriveHandoverReason>,
        ) {
            let Some(mode) = mode else {
                return;
            };
            self.last_mode = Some(mode.clone());
            self.last_handover_reason = reason.cloned();
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
                    self.observe_mode(event.mode.as_ref(), event.reason.as_ref());
                    self.pending_events.push_back(event);
                    continue;
                }
                let ack: CodriveAck = serde_json::from_value(value)
                    .map_err(|e| CodriveClientError::Decode(format!("ack: {e}")))?;
                self.observe_mode(ack.mode.as_ref(), ack.handover_reason.as_ref());
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

    /// Always `None` — this target can never hold a co-drive session, so
    /// it has never observed a mode. Honest absence, not a guessed `Human`.
    pub fn last_driving_mode(&self) -> Option<CodriveDrivingMode> {
        None
    }

    pub fn last_handover_reason(&self) -> Option<CodriveHandoverReason> {
        None
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

    // ── A2 driving-mode block (§3.1/§3.2) ───────────────────────────────
    // These tests ARE the lock against the two binaries drifting apart:
    // the exact JSON in the A2 contract must parse into the exact fields,
    // and a comp on either side of the version line must never produce a
    // decode error.

    #[test]
    fn ack_status_with_full_a2_block_parses_verbatim() {
        // The contract's own example line (A2 §3.1), byte for byte.
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true, "frozen": false, "terminated": false, "takeover": false,
            "mode": "human", "handover_reason": null,
            "shadow": false, "watch_active": false, "watch_paused": false
        }))
        .unwrap();
        assert_eq!(ack.mode, Some(CodriveDrivingMode::Human));
        assert_eq!(ack.handover_reason, None, "an explicit null is None, not a variant");
        assert_eq!(ack.shadow, Some(false));
        assert_eq!(ack.watch_active, Some(false));
        assert_eq!(ack.watch_paused, Some(false));
    }

    #[test]
    fn ack_status_handover_carries_its_reason() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true, "frozen": true, "terminated": false, "takeover": false,
            "mode": "handover", "handover_reason": "shell_take_wheel",
            "shadow": false, "watch_active": true, "watch_paused": false
        }))
        .unwrap();
        assert_eq!(ack.mode, Some(CodriveDrivingMode::Handover));
        assert_eq!(
            ack.handover_reason,
            Some(CodriveHandoverReason::ShellTakeWheel)
        );
        assert_eq!(ack.watch_active, Some(true));
    }

    /// Shadow is NOT a fourth mode (A2 §1): the shared desktop's seat is
    /// still the human's while the agent works in a shadow output.
    #[test]
    fn ack_status_shadow_is_reported_beside_human_mode_not_folded_into_it() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true, "frozen": false, "terminated": false,
            "mode": "human", "shadow": true
        }))
        .unwrap();
        assert_eq!(ack.mode, Some(CodriveDrivingMode::Human));
        assert_eq!(ack.shadow, Some(true));
    }

    /// Version skew, gateway ahead of comp: a pre-A2 status ack must parse
    /// into `None`s, never a hard error.
    #[test]
    fn pre_a2_status_ack_leaves_every_new_field_none() {
        let ack: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": true, "frozen": true, "terminated": false}))
                .unwrap();
        assert_eq!(ack.mode, None);
        assert_eq!(ack.handover_reason, None);
        assert_eq!(ack.shadow, None);
        assert_eq!(ack.watch_active, None);
        assert_eq!(ack.watch_paused, None);
    }

    /// Version skew the other way, comp ahead of gateway: an unrecognized
    /// token must NOT fail the whole ack (which would break the frozen
    /// poll loop) and must NOT be rounded down to `human`.
    #[test]
    fn ack_with_an_unknown_mode_token_still_parses_and_is_not_human() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true, "frozen": false, "terminated": false,
            "mode": "teleop", "handover_reason": "meteor"
        }))
        .unwrap();
        assert_eq!(ack.mode, Some(CodriveDrivingMode::Unknown("teleop".into())));
        assert_eq!(
            ack.handover_reason,
            Some(CodriveHandoverReason::Unknown("meteor".into()))
        );
    }

    #[test]
    fn driving_mode_event_parses_with_and_without_a_reason() {
        let ev: CodriveEvent = serde_json::from_value(
            serde_json::json!({"event": "driving_mode", "mode": "codrive", "reason": null}),
        )
        .unwrap();
        assert_eq!(ev.event, "driving_mode");
        assert_eq!(ev.mode, Some(CodriveDrivingMode::CoDrive));
        assert_eq!(ev.reason, None);

        let handover: CodriveEvent = serde_json::from_value(
            serde_json::json!({"event": "driving_mode", "mode": "handover", "reason": "human_input"}),
        )
        .unwrap();
        assert_eq!(handover.mode, Some(CodriveDrivingMode::Handover));
        assert_eq!(handover.reason, Some(CodriveHandoverReason::HumanInput));
    }

    /// The three pre-A2 events are untouched — they carry no mode payload
    /// and must still parse (this is the "additive, nothing replaced" half
    /// of A2 §3.2).
    #[test]
    fn legacy_events_still_parse_and_carry_no_mode() {
        for name in ["frozen", "resumed", "emergency_stop"] {
            let ev: CodriveEvent = serde_json::from_value(serde_json::json!({"event": name})).unwrap();
            assert_eq!(ev.event, name);
            assert_eq!(ev.mode, None);
            assert_eq!(ev.reason, None);
        }
    }

    // The `CodriveDrivingState` projection of these acks lives with its
    // only consumer, the read-only status query — see `super::status`.

    #[test]
    fn ack_resume_and_session_terminated() {
        let resume: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "resume_is_human_only"})).unwrap();
        assert_eq!(resume.error.as_deref(), Some("resume_is_human_only"));

        let terminated: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "session_terminated"})).unwrap();
        assert_eq!(terminated.error.as_deref(), Some("session_terminated"));
    }

    // ── WP-CD4b-fix (B3): window_geometry query ─────────────────────────

    #[test]
    fn wire_shape_window_geometry_omits_absent_fields() {
        let pid_only = CodriveCmd::WindowGeometry { app_id: None, pid: Some(1234) };
        assert_eq!(
            serde_json::to_value(&pid_only).unwrap(),
            serde_json::json!({"op": "window_geometry", "pid": 1234}),
            "an absent app_id must not be serialized as an explicit null"
        );

        let both = CodriveCmd::WindowGeometry { app_id: Some("foot-A".into()), pid: Some(7) };
        assert_eq!(
            serde_json::to_value(&both).unwrap(),
            serde_json::json!({"op": "window_geometry", "app_id": "foot-A", "pid": 7})
        );
    }

    #[test]
    fn ack_window_geometry_success_parses() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true,
            "window": {
                "origin_x": 10, "origin_y": 20,
                "width": 800, "height": 600,
                "shadow_dx": 26, "shadow_dy": 23,
                "matched_via": "pid"
            }
        }))
        .unwrap();
        let w = ack.window.expect("window object must parse");
        assert_eq!((w.origin_x, w.origin_y, w.width, w.height), (10, 20, 800, 600));
        assert_eq!(w.matched_via.as_deref(), Some("pid"));
    }

    #[test]
    fn ack_window_geometry_refusals_parse() {
        let not_found: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "window_not_found"})).unwrap();
        assert!(not_found.window.is_none());
        assert_eq!(not_found.error.as_deref(), Some("window_not_found"));

        let ambiguous: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": false, "error": "ambiguous_window", "candidates": 3}))
                .unwrap();
        assert_eq!(ambiguous.candidates, Some(3));
    }

    /// A comp that predates this op answers a plain injection ack. That must
    /// parse cleanly and leave `window` as `None` — the caller then refuses
    /// (see `atspi_locate::frame_from_ack`), rather than reading zeros.
    #[test]
    fn ack_from_a_comp_without_the_op_leaves_window_none() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({"ok": true, "frozen": false})).unwrap();
        assert!(ack.window.is_none());
        assert!(ack.ok);
    }

    /// A half-populated `window` object is a DECODE failure, not a
    /// silently-zeroed geometry.
    #[test]
    fn ack_window_geometry_missing_required_field_fails_to_parse() {
        let res: Result<CodriveAck, _> = serde_json::from_value(serde_json::json!({
            "ok": true,
            "window": {"origin_x": 10, "origin_y": 20, "width": 800}
        }));
        assert!(res.is_err(), "a window object missing `height` must not deserialize");
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
