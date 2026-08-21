//! CD-0/CD-1 codrive spike — human/agent co-drive core loop.
//!
//! Implements DESIGN-codrive-desktop-2026-08.md §5's CD-0 and CD-1 slices:
//! an agent-only `wl_seat` ("duduclaw-agent"), a token-authenticated
//! private injection socket that drives it, compositor-enforced
//! freeze-on-human-input, human-side resume (Super+Enter), a Super+Esc
//! emergency stop, a target highlight box, and a JSONL audit trail. See
//! BUILD.md's "CD-0 codrive spike verification" and "CD-1 comp-side
//! additions" sections for how this was exercised.
//!
//! State machine this module implements (DESIGN §3.1, scoped down to what
//! CD-0/CD-1 actually need — the fuller Shadow/Watch/PENDING state machine
//! is CD-2+):
//!
//! ```text
//!   [live]  --human input (any)-->  [frozen]
//!   [frozen] --Super+Enter (human)->  [live]   (CD-1: the ONLY way to
//!                                               clear frozen — see
//!                                               `human_resume` below)
//!   [live/frozen] --Super+Esc----->  [terminated]  (connection force-closed)
//!   [terminated] --new connection->  [live]  (a fresh connection IS a fresh
//!                                             session — see listener.rs;
//!                                             note a fresh connection does
//!                                             NOT clear `frozen`, only
//!                                             `terminated` — §6 red line 3)
//! ```
//!
//! Authority for the freeze gate lives in `handle_agent_inject` (the main
//! calloop thread), not in the socket thread's optimistic pre-check in
//! `listener.rs`: because human-input processing (`on_human_input`,
//! called from `input.rs`) and agent-command execution both run on the
//! *same* single-threaded calloop event loop, non-preemptively, the instant
//! `frozen` flips true on that thread, every agent command whose turn comes
//! up afterward — even ones already sitting in the channel queue — sees it.
//! That's what makes the freeze latency effectively "one calloop dispatch",
//! not something that needs a lock or a rendezvous.
//!
//! CD-1 additions (task brief): (1) socket auth — every connection's first
//! line must be `{"op":"auth","token":"<hex>"}`, checked against a fresh
//! per-run token (see `init`'s token generation and
//! `CodriveShared::check_token`); the CD-0-era "clear on connect" ordering
//! bug that let a reconnect bypass an active freeze is now structurally
//! prevented by moving ALL session bookkeeping behind the auth gate (see
//! `listener.rs::handle_conn`). (2) resume moves to the human side —
//! `human_resume` (Super+Enter, `input.rs`), while the socket `resume` op
//! is now always denied (`listener.rs`). (3) a `status` query. (4) named
//! functional keys (`key_name`). (5) a target highlight box
//! (`highlight.rs`).

mod audit;
mod cursor;
mod debug_sim;
mod highlight;
mod keymap_ascii;
mod listener;
mod protocol;

pub use cursor::build_cursor_elements;
pub use debug_sim::maybe_init_stdin_simulator;
pub use protocol::InjectCmd;

use std::{
    io::Write,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use smithay::{
    backend::input::KeyState,
    input::{
        keyboard::{FilterResult, Keycode, XkbConfig},
        pointer::{ButtonEvent, MotionEvent},
        SeatState,
    },
    reexports::{
        calloop::{self, EventLoop},
        wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle},
    },
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
};
pub use smithay::input::Seat;

use crate::{state::DuduclawComp, CalloopData};
use audit::AuditLog;
use highlight::clamp_highlight_ms;
use keymap_ascii::{ascii_to_xkb, key_name_to_xkb, SHIFT_XKB_KEYCODE};
use protocol::{parse_button_code, parse_press_state};

/// Cross-thread state shared between the calloop main thread and the
/// injection-socket thread (`listener.rs`). See that module's doc comment
/// for the "why a plain `Mutex`, not `duduclaw_core::with_file_lock`" note.
pub struct CodriveShared {
    /// True while the agent seat is frozen (human input observed). Set on
    /// any human-seat event; cleared ONLY by human-side resume
    /// (`DuduclawComp::human_resume`, Super+Enter) — never by connection
    /// lifecycle and never by a socket `resume` op (CD-1: DESIGN-codrive-
    /// desktop §6 red line 3 + §3.1 "交還是明確動作"). Gate checked
    /// authoritatively in `DuduclawComp::handle_agent_inject`.
    pub frozen: AtomicBool,
    /// True after a Super+Esc emergency stop, until a *new* connection is
    /// accepted (see `listener.rs::handle_conn`).
    pub terminated: AtomicBool,
    /// The currently-connected agent's stream, kept so `emergency_stop`
    /// can force-close it and so state-transition events (`frozen`/
    /// `resumed`) can be pushed to it — both from the main thread.
    active_conn: Mutex<Option<UnixStream>>,
    audit: Option<AuditLog>,
    /// This run's hex-encoded 32-byte socket-auth token (CD-1, DESIGN
    /// §3.3.1 "EIS 界線"). `None` means token generation/write failed at
    /// startup — see `init` — in which case `check_token` always returns
    /// `false` (fail-closed: no listener is even spawned in that case, but
    /// the field stays `None` rather than some sentinel value so there's
    /// no "empty token" to accidentally match against).
    auth_token: Option<String>,
}

impl CodriveShared {
    fn disabled() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            audit: None,
            auth_token: None,
        }
    }

    /// Same "no listener will ever run" shape as `disabled()`, but keeps an
    /// audit log that was already successfully opened before the failure
    /// forcing this fallback (token generation/write failure) — so
    /// freeze/emergency-stop events from human input alone (which need no
    /// socket at all) still get audited.
    fn disabled_keep_audit(audit: Option<AuditLog>) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            audit,
            auth_token: None,
        }
    }

    /// No-ops (besides a log line, on first use) when the audit log failed
    /// to open — a broken audit trail must never become a reason to block
    /// or crash the injection path (fail-open on *logging*, fail-closed on
    /// *authorization*, per repo security convention #4 — those are two
    /// different gates).
    pub fn record(&self, kind: &'static str, op: Option<&'static str>, x: Option<f64>, y: Option<f64>, detail: Option<String>) {
        if let Some(audit) = &self.audit {
            audit.record(kind, op, x, y, detail, self.frozen.load(Ordering::SeqCst));
        }
    }

    /// Convenience read of the freeze flag for callers outside this module
    /// (e.g. `winit_backend.rs`'s redraw path, which needs it to pick the
    /// agent cursor's frozen-vs-live color — see `cursor.rs`).
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
    }

    /// Best-effort constant-time-*ish* comparison against this run's
    /// socket-auth token (CD-1, DESIGN §3.3.1 "EIS 界線"). Folds every byte
    /// position of both operands with XOR rather than returning as soon as
    /// a mismatch is found, so a naive `==`'s "how many leading bytes
    /// matched" timing signal isn't there to lean on. Not a
    /// cryptographic-grade constant-time primitive (no SIMD/compiler-
    /// barrier guarantees, and `.get(i)` bounds checks branch on length) —
    /// sized to this channel's actual threat model (a same-host Unix
    /// socket, not a network timing-attack surface), not claimed to be
    /// more than that. `None` (token generation/write failed at startup)
    /// always fails: with no token durably shared with a legitimate
    /// caller, nothing should ever authenticate.
    pub(crate) fn check_token(&self, presented: &str) -> bool {
        let Some(expected) = self.auth_token.as_deref() else {
            return false;
        };
        let a = expected.as_bytes();
        let b = presented.as_bytes();
        let max_len = a.len().max(b.len());
        let mut diff: u8 = (a.len() != b.len()) as u8;
        for i in 0..max_len {
            diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
        }
        diff == 0
    }

    /// Best-effort push of a one-line JSON event to the currently connected
    /// agent client (task brief req 3; mirrors `emergency_stop`'s existing
    /// push of `{"event":"emergency_stop"}`). Silently does nothing if
    /// there's no active connection or the write fails — this is a
    /// courtesy notification, not a reliable channel; the client can
    /// always poll via `{"op":"status"}`. Clones the stream (rather than
    /// writing through the `&UnixStream` borrow directly) to reuse the
    /// exact write pattern `emergency_stop` already had proven to work,
    /// without introducing a second borrow-through-Mutex shape.
    fn push_event(&self, event_line: &str) {
        if let Ok(guard) = self.active_conn.lock() {
            if let Some(stream) = guard.as_ref() {
                if let Ok(clone) = stream.try_clone() {
                    let _ = writeln!(&clone, "{event_line}");
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(auth_token: Option<String>) -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            audit: None,
            auth_token,
        }
    }
}

/// Reads exactly 32 bytes from `/dev/urandom` for this run's socket-auth
/// token (CD-1, DESIGN §3.3.1's "EIS 界線" — the injection socket now
/// requires a caller-presented secret, not just filesystem permissions).
/// `/dev/urandom` directly rather than a `rand`-crate dependency: this
/// crate is already Linux-only (see Cargo.toml's workspace-detach
/// comment), so reading the kernel CSPRNG device needs no new dependency
/// and no portability concern.
fn generate_token_bytes() -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    let mut buf = [0u8; 32];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Writes the hex token to `path` with mode 0600 set atomically at create
/// time (`OpenOptionsExt::mode`, not a chmod-after-the-fact like
/// `audit.rs`'s belt-and-suspenders approach) — this file holds an actual
/// bearer secret, so there must be no window where it's briefly readable
/// at default permissions. A stale token file from a previous run is
/// removed first (mirrors `listener.rs`'s stale-socket handling), so a new
/// token is guaranteed correct perms every run regardless of a prior run's
/// file state.
fn write_token_file(path: &Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(token.as_bytes())?;
    Ok(())
}

/// Creates the agent seat and starts the injection listener + audit log.
/// Called from `DuduclawComp::new`, which already owns the `SeatState` and
/// `EventLoop` this needs.
pub fn init(
    seat_state: &mut SeatState<DuduclawComp>,
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<CalloopData>,
) -> (Seat<DuduclawComp>, Arc<CodriveShared>) {
    let mut agent_seat: Seat<DuduclawComp> = seat_state.new_wl_seat(dh, "duduclaw-agent");
    agent_seat
        .add_keyboard(XkbConfig::default(), 200, 25)
        .expect("codrive: failed to initialize agent seat keyboard");
    agent_seat.add_pointer();

    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        tracing::error!(
            "codrive: XDG_RUNTIME_DIR is not set — the agent injection socket and audit log \
             are disabled for this run (the agent seat still exists, but nothing can drive it)"
        );
        return (agent_seat, Arc::new(CodriveShared::disabled()));
    };

    let sock_path = PathBuf::from(&runtime_dir).join("duduclaw-codrive.sock");
    let audit_path = PathBuf::from(&runtime_dir).join("duduclaw-codrive-audit.jsonl");
    let token_path = PathBuf::from(&runtime_dir).join("duduclaw-codrive.token");

    let audit = match AuditLog::open(&audit_path) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::error!(error = %e, path = %audit_path.display(), "codrive: failed to open audit log — continuing without one");
            None
        }
    };

    // CD-1 socket auth (DESIGN §3.3.1 "EIS 界線" / §6 red line 2): a fresh
    // 32-byte token every process start. Fail-closed on either step: with
    // no durable token, nobody could ever legitimately authenticate anyway,
    // so — unlike a failed audit-log open, which is fine to degrade past —
    // this disables the listener entirely rather than starting a socket
    // that (say) falls back to "accept anyone."
    let auth_token = match generate_token_bytes() {
        Ok(bytes) => hex_encode(&bytes),
        Err(e) => {
            tracing::error!(error = %e, "codrive: failed to generate the injection-socket auth token — the agent injection socket is disabled for this run (fail-closed)");
            return (agent_seat, Arc::new(CodriveShared::disabled_keep_audit(audit)));
        }
    };
    if let Err(e) = write_token_file(&token_path, &auth_token) {
        tracing::error!(error = %e, path = %token_path.display(), "codrive: failed to write the injection-socket auth token file — the agent injection socket is disabled for this run (fail-closed)");
        return (agent_seat, Arc::new(CodriveShared::disabled_keep_audit(audit)));
    }

    let shared = Arc::new(CodriveShared {
        frozen: AtomicBool::new(false),
        terminated: AtomicBool::new(false),
        active_conn: Mutex::new(None),
        audit,
        auth_token: Some(auth_token),
    });

    let (tx, rx) = calloop::channel::channel::<InjectCmd>();

    if let Err(e) = listener::spawn(sock_path, Arc::clone(&shared), tx) {
        tracing::error!(error = %e, "codrive: failed to start the agent injection socket listener — agent seat will receive no events this run");
    }

    event_loop
        .handle()
        .insert_source(rx, |event, _, data: &mut CalloopData| {
            if let calloop::channel::Event::Msg(cmd) = event {
                data.state.handle_agent_inject(cmd);
            }
        })
        .expect("codrive: failed to insert the injection channel into the event loop");

    (agent_seat, shared)
}

impl DuduclawComp {
    /// Called from `input.rs` for every human (real "winit" seat) input
    /// event. Freezes the agent seat on the *first* such event since the
    /// last resume — DESIGN §3.1: "人輸入永遠優先…人一有事件，compositor
    /// 立即凍結 agent seat". Repeated human input while already frozen is a
    /// cheap no-op (the flag is already set; there's no "extend freeze"
    /// timer at CD-0/CD-1 — that's watch-mode territory, CD-2).
    pub fn on_human_input(&mut self, kind: &'static str) {
        let was_frozen = self.codrive.frozen.swap(true, Ordering::SeqCst);
        if !was_frozen {
            self.codrive_freeze_set_at = Some(std::time::Instant::now());
            tracing::info!(kind, "codrive: human input observed — freezing agent seat");
            self.codrive.record("freeze", Some(kind), None, None, None);
            // CD-1 req 3: push the state transition to the connected agent
            // client — one event per transition, not per human input event
            // while already frozen (hence gated on `!was_frozen`).
            self.codrive.push_event(r#"{"event":"frozen"}"#);
        }
    }

    /// Human-side "交還" (DESIGN §3.1: "『交還』是明確動作（按鈕/
    /// Super+Enter）"), CD-1's replacement for the CD-0 socket-`resume`
    /// stand-in (see `listener.rs`'s now-permanent `resume_is_human_only`
    /// denial). Reachable only from the human keyboard filter closure in
    /// `input.rs` (Super+Enter) and `debug_sim.rs`'s
    /// `simulate_super_enter` line — never from the agent injection
    /// socket, matching the same "agent structurally cannot reach this"
    /// property `emergency_stop` already has for Super+Esc. No-op (no
    /// state change, no audit line) if the seat wasn't frozen to begin
    /// with (task brief req 2: "frozen 本來就 false 時 no-op 不記 audit").
    pub fn human_resume(&mut self) {
        let was_frozen = self.codrive.frozen.swap(false, Ordering::SeqCst);
        if was_frozen {
            tracing::info!("codrive: human resume (Super+Enter) — un-freezing agent seat");
            self.codrive.record("resume", Some("human_super_enter"), None, None, None);
            self.codrive.push_event(r#"{"event":"resumed"}"#);
        }
    }

    /// Super+Esc (DESIGN §3.3.3 / §6.3): global emergency stop, not
    /// interceptable by the agent (it's detected in the human keyboard
    /// path, which the agent seat has no way to reach — there's no code
    /// path from an injected agent key event into this function).
    pub fn emergency_stop(&mut self, reason: &'static str) {
        self.codrive.frozen.store(true, Ordering::SeqCst);
        self.codrive.terminated.store(true, Ordering::SeqCst);
        tracing::warn!(reason, "codrive: EMERGENCY STOP — terminating the co-drive session");
        self.codrive.record("emergency_stop", None, None, None, Some(reason.to_string()));

        if let Ok(mut guard) = self.codrive.active_conn.lock() {
            if let Some(stream) = guard.take() {
                use std::io::Write;
                // Best-effort: tell the client why, then force-close. Either
                // step failing (e.g. the client already went away) is fine —
                // the connection is going down either way.
                let _ = writeln!(&stream, r#"{{"event":"emergency_stop"}}"#);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    /// Executes one already-validated (by `listener.rs`) agent command on
    /// the calloop main thread — the only thread allowed to touch the
    /// agent seat or `self.space`. See the module doc comment for why the
    /// freeze re-check here, not just the socket thread's, is the
    /// authoritative one.
    pub fn handle_agent_inject(&mut self, cmd: InjectCmd) {
        if self.codrive.frozen.load(Ordering::SeqCst) {
            let (op, x, y) = cmd.describe();
            let latency = self.codrive_freeze_set_at.map(|t| t.elapsed());
            tracing::debug!(
                op,
                latency_us = latency.map(|d| d.as_micros()),
                "codrive: dropping a queued agent command — seat frozen by the time its turn came up"
            );
            self.codrive.record(
                "inject_dropped",
                Some(op),
                x,
                y,
                Some(format!(
                    "frozen at execution time (queued-then-frozen race, latency_us={:?})",
                    latency.map(|d| d.as_micros())
                )),
            );
            return;
        }

        let (op, x, y) = cmd.describe();

        match cmd {
            InjectCmd::Move { x, y } => {
                let pos = Point::<f64, Logical>::from((x, y));
                let serial = SERIAL_COUNTER.next_serial();
                let time = self.start_time.elapsed().as_millis() as u32;
                let under = self.surface_under(pos);
                let pointer = self.agent_seat.get_pointer().expect("agent seat always has a pointer");
                pointer.motion(self, under, &MotionEvent { location: pos, serial, time });
                pointer.frame(self);
            }
            InjectCmd::Button { btn, state } => {
                // Re-derived defensively even though `listener.rs` already
                // validated this — `handle_agent_inject` never trusts an
                // upstream check alone for anything that would otherwise
                // panic (repo convention: security/validation gates fail
                // closed, not "trust the caller already checked").
                let (Ok(button), Ok(pressed)) = (parse_button_code(&btn), parse_press_state(&state)) else {
                    tracing::error!(btn, state, "codrive: invalid button command reached the main thread (should have been rejected by listener.rs) — dropping");
                    return;
                };
                let serial = SERIAL_COUNTER.next_serial();
                let time = self.start_time.elapsed().as_millis() as u32;
                let pointer = self.agent_seat.get_pointer().expect("agent seat always has a pointer");

                // Click-to-focus on PRESS: mirrors what `input.rs`'s human
                // PointerButton arm already does for the human seat (raise
                // + set keyboard focus to the window under the pointer) —
                // deliberately duplicated here rather than extracted into a
                // shared helper, so the already-VM-verified human path in
                // `input.rs` (see BUILD.md "VM cage real-seat input
                // verification") stays byte-for-byte untouched. Without
                // this, `InjectCmd::Text`/`InjectCmd::Key` would have
                // nowhere to route: each `wl_seat`'s keyboard focus is
                // independent (wl_seat spec), and nothing else ever sets
                // the agent seat's.
                if pressed && !pointer.is_grabbed() {
                    let pos = pointer.current_location();
                    if let Some((window, _loc)) = self.space.element_under(pos).map(|(w, l)| (w.clone(), l)) {
                        self.space.raise_element(&window, true);
                        if let Some(keyboard) = self.agent_seat.get_keyboard() {
                            keyboard.set_focus(self, Some(window.toplevel().unwrap().wl_surface().clone()), serial);
                        }
                        self.space.elements().for_each(|w| {
                            w.toplevel().unwrap().send_pending_configure();
                        });
                    } else if let Some(keyboard) = self.agent_seat.get_keyboard() {
                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                    }
                }

                let pointer = self.agent_seat.get_pointer().expect("agent seat always has a pointer");
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: if pressed { smithay::backend::input::ButtonState::Pressed } else { smithay::backend::input::ButtonState::Released },
                        serial,
                        time,
                    },
                );
                pointer.frame(self);
            }
            InjectCmd::Key { keycode, state } => {
                let Ok(pressed) = parse_press_state(&state) else {
                    tracing::error!(state, "codrive: invalid key state reached the main thread (should have been rejected by listener.rs) — dropping");
                    return;
                };
                self.agent_key(keycode, pressed);
            }
            InjectCmd::KeyName { name, state } => {
                let Ok(pressed) = parse_press_state(&state) else {
                    tracing::error!(state, "codrive: invalid key_name state reached the main thread (should have been rejected by listener.rs) — dropping");
                    return;
                };
                let Some(xkb) = key_name_to_xkb(&name) else {
                    tracing::error!(name, "codrive: invalid key_name reached the main thread (should have been rejected by listener.rs) — dropping");
                    return;
                };
                self.agent_key(xkb, pressed);
            }
            InjectCmd::Text { s } => {
                for c in s.chars() {
                    let Some((xkb, shift)) = ascii_to_xkb(c) else {
                        tracing::warn!(char = ?c, "codrive: text op — character outside the ASCII-only synthesis table, skipped (see keymap_ascii.rs)");
                        continue;
                    };
                    if shift {
                        self.agent_key(SHIFT_XKB_KEYCODE, true);
                    }
                    self.agent_key(xkb, true);
                    self.agent_key(xkb, false);
                    if shift {
                        self.agent_key(SHIFT_XKB_KEYCODE, false);
                    }
                }
            }
            InjectCmd::Highlight { x, y, w, h, ms } => {
                let ms = clamp_highlight_ms(ms);
                let rect = Rectangle::<f64, Logical>::new(
                    Point::from((x, y)),
                    Size::from((w, h)),
                );
                self.codrive_highlight =
                    Some((rect, std::time::Instant::now() + std::time::Duration::from_millis(ms)));
            }
            InjectCmd::Resume => {
                // Handled synchronously by the socket thread (listener.rs) —
                // and now (CD-1) always denied there, never forwarded here.
                // Kept as an arm (not `unreachable!`) so a future change
                // that starts forwarding it here fails safe instead of
                // panicking.
                tracing::warn!("codrive: Resume reached handle_agent_inject unexpectedly — no-op (see listener.rs)");
                return;
            }
            InjectCmd::Status => {
                // Handled synchronously by the socket thread (listener.rs) —
                // a pure atomic-read op needing no seat access, so it never
                // reaches this channel. Kept as an arm for the same
                // fail-safe reasoning as the Resume arm above.
                tracing::warn!("codrive: Status reached handle_agent_inject unexpectedly — no-op (see listener.rs)");
                return;
            }
        }

        self.codrive.record("inject_applied", Some(op), x, y, None);
    }

    fn agent_key(&mut self, xkb_code: u32, pressed: bool) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        let keyboard = self
            .agent_seat
            .get_keyboard()
            .expect("agent seat always has a keyboard");
        keyboard.input::<(), _>(self, Keycode::new(xkb_code), state, serial, time, |_, _, _| {
            FilterResult::Forward
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_token_accepts_matching_token() {
        let shared = CodriveShared::for_test(Some("abc123".to_string()));
        assert!(shared.check_token("abc123"));
    }

    #[test]
    fn check_token_rejects_wrong_or_partial_token() {
        let shared = CodriveShared::for_test(Some("abc123".to_string()));
        assert!(!shared.check_token("abc124"));
        assert!(!shared.check_token("abc12"));
        assert!(!shared.check_token("abc1234"));
        assert!(!shared.check_token(""));
    }

    #[test]
    fn check_token_rejects_everything_when_none() {
        let shared = CodriveShared::for_test(None);
        assert!(!shared.check_token(""));
        assert!(!shared.check_token("anything"));
    }

    #[test]
    fn generate_token_bytes_returns_32_fresh_random_bytes() {
        let a = generate_token_bytes().expect("failed to read /dev/urandom");
        let b = generate_token_bytes().expect("failed to read /dev/urandom");
        assert_eq!(a.len(), 32);
        assert_ne!(a, b, "two consecutive reads of /dev/urandom must not collide");
    }

    #[test]
    fn hex_encode_produces_lowercase_hex_of_expected_length() {
        let bytes = [0u8, 1, 255, 16];
        assert_eq!(hex_encode(&bytes), "0001ff10");
        let token = hex_encode(&[7u8; 32]);
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
