//! CD-0 codrive spike — human/agent co-drive core loop.
//!
//! Implements the CD-0 slice of DESIGN-codrive-desktop-2026-08.md §5:
//! an agent-only `wl_seat` ("duduclaw-agent"), a private injection socket
//! that drives it, compositor-enforced freeze-on-human-input, a Super+Esc
//! emergency stop, and a JSONL audit trail. See BUILD.md's "CD-0 codrive
//! spike verification" section for how this was exercised.
//!
//! State machine this module implements (DESIGN §3.1, scoped down to what
//! CD-0 actually needs — the fuller Shadow/Watch/PENDING state machine is
//! CD-1+):
//!
//! ```text
//!   [live]  --human input (any)-->  [frozen]
//!   [frozen] --"resume" op-------->  [live]
//!   [live/frozen] --Super+Esc----->  [terminated]  (connection force-closed)
//!   [terminated] --new connection->  [live]  (a fresh connection IS a fresh
//!                                             session — see listener.rs)
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

mod audit;
mod cursor;
mod debug_sim;
mod keymap_ascii;
mod listener;
mod protocol;

pub use cursor::build_cursor_elements;
pub use debug_sim::maybe_init_stdin_simulator;
pub use protocol::InjectCmd;

use std::{
    os::unix::net::UnixStream,
    path::PathBuf,
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
    utils::{Logical, Point, SERIAL_COUNTER},
};
pub use smithay::input::Seat;

use crate::{state::DuduclawComp, CalloopData};
use audit::AuditLog;
use keymap_ascii::{ascii_to_xkb, SHIFT_XKB_KEYCODE};
use protocol::{parse_button_code, parse_press_state};

/// Cross-thread state shared between the calloop main thread and the
/// injection-socket thread (`listener.rs`). See that module's doc comment
/// for the "why a plain `Mutex`, not `duduclaw_core::with_file_lock`" note.
pub struct CodriveShared {
    /// True while the agent seat is frozen (human input observed). Set on
    /// any human-seat event; cleared ONLY by an explicit `resume` op — never
    /// by connection lifecycle (a reconnect must not bypass an active human
    /// freeze: DESIGN-codrive-desktop §6 red line 3). Gate checked
    /// authoritatively in `DuduclawComp::handle_agent_inject`.
    pub frozen: AtomicBool,
    /// True after a Super+Esc emergency stop, until a *new* connection is
    /// accepted (see `listener.rs::accept_loop`).
    pub terminated: AtomicBool,
    /// The currently-connected agent's stream, kept so `emergency_stop` can
    /// force-close it from the main thread.
    active_conn: Mutex<Option<UnixStream>>,
    audit: Option<AuditLog>,
}

impl CodriveShared {
    fn disabled() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            active_conn: Mutex::new(None),
            audit: None,
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

    let audit = match AuditLog::open(&audit_path) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::error!(error = %e, path = %audit_path.display(), "codrive: failed to open audit log — continuing without one");
            None
        }
    };

    let shared = Arc::new(CodriveShared {
        frozen: AtomicBool::new(false),
        terminated: AtomicBool::new(false),
        active_conn: Mutex::new(None),
        audit,
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
    /// timer at CD-0 — that's watch-mode territory, CD-2).
    pub fn on_human_input(&mut self, kind: &'static str) {
        let was_frozen = self.codrive.frozen.swap(true, Ordering::SeqCst);
        if !was_frozen {
            self.codrive_freeze_set_at = Some(std::time::Instant::now());
            tracing::info!(kind, "codrive: human input observed — freezing agent seat");
            self.codrive.record("freeze", Some(kind), None, None, None);
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
            InjectCmd::Resume => {
                // Handled synchronously by the socket thread (listener.rs) —
                // it's a pure flag flip needing no seat access, so it never
                // reaches this channel. Kept as an arm (not `unreachable!`)
                // so a future change that starts forwarding it here fails
                // safe instead of panicking.
                tracing::warn!("codrive: Resume reached handle_agent_inject unexpectedly — no-op (see listener.rs)");
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
