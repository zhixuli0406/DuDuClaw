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
//! CD-1 additions (task brief, detail in `listener.rs`/`highlight.rs`): (1)
//! socket auth (`{"op":"auth","token":"<hex>"}` vs. `CodriveShared::
//! check_token`; the CD-0 reconnect-bypasses-freeze bug is fixed by moving
//! ALL session bookkeeping behind the auth gate). (2) resume moves to the
//! human side (`human_resume`, Super+Enter, `input.rs`) — the socket
//! `resume` op is now always denied. (3) a `status` query. (4) named
//! functional keys (`key_name`). (5) a target highlight box.
//!
//! CD-2 additions (full detail in `rotation.rs`/`shadow.rs` — this file only
//! wires them into the state machine below): (6) socket-auth token rotation
//! WITHOUT restart (`{"op":"rotate_token"}` or `SIGHUP` → `CodriveShared::
//! rotate_token`; old token invalid for new connections immediately,
//! already-authenticated ones unaffected). (7) `{"op":"shadow","enable":…}`
//! moves the agent's focused window to/from a headless second `Output`
//! ("duduclaw-shadow-0") with a PiP preview on the main output. (8)
//! WP-CD2-freeze-scope: a command may bypass an active freeze iff `shadow::
//! is_freeze_bypass_eligible` confirms it's confined to the shadow output
//! (never the `Shadow` toggle itself) — `listener.rs` mirrors `shadow_active`
//! for an optimistic pre-check.
//!
//! CD-3 (DESIGN §5's "接手/交還＋watch mode" row; full detail in
//! `codrive/takeover.rs`/`codrive/watch.rs`): (9) `take_over` — agent-
//! initiated hand-off; freezes like human input but ALSO kills the shadow-
//! bypass exception (`takeover_active`) — zero exceptions for a credential
//! window. (10) `watch` — idle-based auto-pause; `on_human_input` auto-lifts
//! it with no explicit resume, since the input itself IS "still watching".
//!
//! WP-CD4a-COMP (B-line CD-4a, multi-window targeting; full detail in
//! `codrive/window_target.rs`): (11) `activate_window` — raises/focuses a
//! toplevel by xdg-shell app_id (exact match, priority) or a title-prefix
//! fallback, reusing the WP-A1 `DuduclawComp::focus_window` helper. Never
//! shadow-bypass-eligible (`shadow::freeze_bypass_decision`), so it's
//! denied outright while frozen/under takeover like `Shadow`/`Watch`. A
//! query matching nothing is answered honestly (`activate_window_failed`
//! audit line), never a silent no-op.
//!
//! ## Relationship to `crate::shell_control` (WP-comp-shell-ipc, 2026-08-22)
//! `crate::shell_control` is a SEPARATE Unix socket, wire protocol, and
//! audit trail — not a 12th item in the state machine above. It exists so
//! `duduclaw-shell` (a human clicking the dock) can list/switch windows
//! without going anywhere near this module's agent-injection channel: the
//! socket here is token-authenticated and every command through it is
//! attributed to the AGENT (codrive audit `kind`s, agent-seat focus) —
//! reusing it for a human dock click would misattribute human action as
//! agent action in the audit trail and, worse, would mean anything that
//! can read the codrive token file (see `write_token_file` below) could
//! drive the agent seat by pretending to be the shell. `shell_control`'s
//! own module doc has the full design (same-uid `SO_PEERCRED` auth instead
//! of a bearer token, no freeze gate — a human can always operate their
//! own desktop, independent audit log). The only code shared between the
//! two is the pure window-matching/focus logic in `window_target.rs`
//! (widened to `pub(crate)` this round for exactly that reuse) — no shared
//! socket, no shared auth, no shared audit trail.

mod audit;
mod cursor;
mod debug_sim;
mod highlight;
mod human_seat;
mod keymap_ascii;
mod listener;
// A2 共駕復活 (2026-08-24): the driving-mode state machine and its
// screen-edge indicator — see `mode.rs`'s module doc.
mod mode;
mod mode_indicator;
mod protocol;
mod rotation;
mod shadow;
mod shared;
mod takeover;
#[cfg(test)]
mod tests_listener;
#[cfg(test)]
mod tests_takeover;
// A2: this file's own former `#[cfg(test)] mod tests` block moved out
// verbatim — `mod.rs` is already over the 800-line cap and A2 must add
// transition calls to it. Same split `tests_listener.rs` already used.
#[cfg(test)]
mod tests_token;
mod watch;
// WP-CD4b-fix (B3): the READ-ONLY `window_geometry` query — see that file's
// module doc for the GTK4 `CoordType::Screen`-returns-zeros defect it
// exists to close and the smithay coordinate semantics it depends on.
mod window_geometry;
// WP-comp-shell-ipc (2026-08-22): widened module-private -> `pub(crate)` so
// `crate::shell_control` can reuse `find_target_window`/`window_identity`
// (see `window_target.rs`'s own module doc "WP-comp-shell-ipc reuse"
// section) — no logic moved or duplicated, only visibility.
pub(crate) mod window_target;

pub use cursor::build_agent_cursor_elements;
pub use debug_sim::maybe_init_stdin_simulator;
// A2. `DrivingMode` is what both backends pass to the two element builders;
// `status_snapshot` is what `shell_control::codrive_ops` answers the human
// side from — the SAME derivation the agent side uses, so the two channels
// can never disagree about one desktop.
pub use mode::{CodriveModeCache, DrivingMode, HandoverReason};
pub use mode_indicator::build_mode_indicator_elements;
pub(crate) use mode::{status_snapshot, CodriveStatusSnapshot};
pub use protocol::InjectCmd;
pub use shadow::{create_shadow_output, SHADOW_ORIGIN};
// WP-A1 multi-window round: `CodriveShared` itself moved to `shared.rs`
// (see that file's module doc for why — `mod.rs` was at the crate's
// 800-line file-size cap). Re-exported here so every existing external
// reference (`codrive::CodriveShared` from `state.rs`, `super::
// CodriveShared` from sibling submodules) keeps working unchanged.
pub use shared::CodriveShared;

/// The agent seat's `wl_seat` name.
///
/// A named constant rather than a literal because D3-c's per-client seat
/// filter (`crate::ime::seat_filter`) identifies the agent seat *by this
/// name*, and a silent divergence between the two would disarm the filter
/// at startup — which is exactly the sort of thing a constant prevents and
/// a duplicated string literal invites.
pub const AGENT_SEAT_NAME: &str = "duduclaw-agent";

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
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
        wayland_server::{self, DisplayHandle},
    },
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
};
pub use smithay::input::Seat;

use crate::{state::DuduclawComp, CalloopData};
use audit::AuditLog;
use highlight::clamp_highlight_ms;
use keymap_ascii::{ascii_to_xkb, key_name_to_xkb, SHIFT_XKB_KEYCODE};
use protocol::{parse_button_code, parse_press_state};

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

// `block_sighup_on_current_thread` and `spawn_sighup_rotation_thread` (CD-2
// task item 1) live in the `rotation` submodule, not here — see that
// module's doc comment for the full "why", and the file-size note on
// `rotate_token`'s removal above for why they moved.

/// Creates the agent seat and starts the injection listener + audit log.
/// Called from `DuduclawComp::new`, which already owns the `SeatState` and
/// `EventLoop` this needs.
pub fn init(
    seat_state: &mut SeatState<DuduclawComp>,
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<CalloopData>,
) -> (Seat<DuduclawComp>, Arc<CodriveShared>) {
    // CD-2 (task item 1): must be the very first statement in this
    // function — see `rotation::block_sighup_on_current_thread`'s doc for
    // why.
    let sighup_blocked = match rotation::block_sighup_on_current_thread() {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "codrive: failed to block SIGHUP on the main thread — SIGHUP-triggered token \
                 rotation will be unavailable this run (every other codrive feature, including \
                 socket-op rotation, is unaffected)"
            );
            false
        }
    };

    let mut agent_seat: Seat<DuduclawComp> = seat_state.new_wl_seat(dh, AGENT_SEAT_NAME);
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

    let shared = Arc::new(CodriveShared::new(audit, auth_token, token_path));

    let (tx, rx) = calloop::channel::channel::<InjectCmd>();

    // WP-CD4b-fix (B3): a SECOND, separate channel for the read-only
    // `window_geometry` query. Deliberately not folded into the `InjectCmd`
    // channel above: that one is fire-and-forget (the listener acks from
    // what it already knows), whereas a query genuinely needs the main
    // thread's computed answer routed back — the same request/response
    // shape `crate::shell_control` uses, and the reason its message type
    // carries a oneshot `reply_tx`. Installed on `shared` BEFORE the
    // listener is spawned so no connection can observe a half-built bridge.
    let (query_tx, query_rx) = calloop::channel::channel::<window_geometry::CodriveQuery>();
    shared.set_query_channel(query_tx);

    if let Err(e) = listener::spawn(sock_path, Arc::clone(&shared), tx) {
        tracing::error!(error = %e, "codrive: failed to start the agent injection socket listener — agent seat will receive no events this run");
    }

    // CD-2 (task item 1): only meaningful once the listener above actually
    // has a token file to rewrite — mirrors why the listener itself is
    // only spawned in this same success path.
    if sighup_blocked {
        rotation::spawn_sighup_rotation_thread(Arc::clone(&shared));
    }

    event_loop
        .handle()
        .insert_source(rx, |event, _, data: &mut CalloopData| {
            if let calloop::channel::Event::Msg(cmd) = event {
                data.state.handle_agent_inject(cmd);
            }
        })
        .expect("codrive: failed to insert the injection channel into the event loop");

    // WP-CD4b-fix (B3): the query bridge's main-thread end. Read-only —
    // `codrive_window_geometry` takes `&self`, queues no redraw, records no
    // audit row (see `window_geometry.rs`'s "Not an action" section). A
    // dropped receiver on the socket side (caller gave up / timed out) makes
    // `reply_tx.send` fail, which is fine to ignore: that caller already got
    // its own honest `timeout` answer.
    event_loop
        .handle()
        .insert_source(query_rx, |event, _, data: &mut CalloopData| {
            if let calloop::channel::Event::Msg(msg) = event {
                let reply = data.state.codrive_window_geometry(&msg.req);
                let _ = msg.reply_tx.send(reply);
            }
        })
        .expect("codrive: failed to insert the window-geometry query channel into the event loop");

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
        // E1a-1a self-freeze guard (DESIGN §6.1.1 item ②, §6.1.2 M3/M4).
        // Unreachable today — `human_seat.rs`'s emission helpers call `Seat`
        // APIs directly, and this function's only caller is `input.rs::
        // process_input_event` (plus `debug_sim.rs`) — so this is defence in
        // depth against a future refactor that routes synthesis through the
        // backend path. Without it that refactor would either live-lock the
        // agent (inject → freeze itself → drop the next inject) or forge "a
        // human is present" and permanently disarm watch mode's idle
        // auto-pause. Reported rather than silent, per the module's standing
        // "never a silent no-op" doctrine.
        if self.codrive_synthesizing {
            tracing::error!(
                kind,
                "codrive: a human-input event arrived while human-seat synthesis was in flight — \
                 IGNORED. Agent-synthesised events must never be observed as human input (see \
                 codrive/human_seat.rs). This means synthesis is now re-entering the backend \
                 input path, which it must not."
            );
            self.codrive.record(
                "synthesis_reentry_ignored",
                Some(kind),
                None,
                None,
                Some("a synthesised event re-entered on_human_input (E1a-1a guard)".into()),
            );
            return;
        }
        self.codrive_last_human_activity = std::time::Instant::now();
        if self.codrive_try_watch_resume() {
            return; // CD-3: this event itself IS the "still watching" signal.
        }
        // A2: the trigger, recorded before the freeze (see `mode.rs`).
        self.codrive_note_handover_reason(mode::HandoverReason::HumanInput);
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
        // A2: `codrive -> handover`, once per real transition (this is a
        // no-op on the already-frozen repeat path above).
        self.codrive_sync_mode();
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
        // CD-2 shadow workspace (WP-CD2-shadow, DESIGN §3.3.4 / task brief
        // item 4 "接手＝shadow 視窗搬回主 output"): unconditional, not
        // nested inside the `if was_frozen` branch above — a shadow session
        // is separate state from the freeze flag (see `shadow.rs` module
        // doc), so "交還" collects any active shadow session back to the
        // main output regardless of whether the real desktop ever actually
        // froze the seat.
        self.codrive_handback_shadow_if_active("human_super_enter");
        // CD-3: same "separate state, always check" — ends an active
        // takeover and/or a stale watch-idle-pause flag either way.
        self.codrive_end_takeover_if_active("human_super_enter");
        self.codrive_end_watch_pause("human_super_enter", false);
        // A2: `handover -> codrive` (the two helpers above already sync too;
        // this covers the path where neither had anything to undo).
        self.codrive_sync_mode();
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
        // CD-2 shadow workspace (WP-CD2-shadow, DESIGN §6 red line 3 "急停
        // 鍵永遠有效" / task brief item 4 "Super+Esc 急停一樣殺 shadow
        // session"): emergency stop is a global kill switch — it collects
        // any active shadow session back to the main output too, not just
        // the foreground Watch/Takeover case this function already handled
        // before this addition.
        self.codrive_handback_shadow_if_active(reason);
        // CD-3: gives takeover/watch-pause a clean, audited terminal
        // transition too (Super+Esc already reaches them unconditionally).
        self.codrive_end_takeover_if_active(reason);
        self.codrive_end_watch_pause(reason, false);

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
        // A2: cleared in lockstep with the `active_conn.take()` above, and
        // unconditionally — "no connection" and "connection just killed"
        // must both end at `false`.
        self.codrive.session_active.store(false, Ordering::SeqCst);
        // Then `-> human`: `terminated` already forces that, but the sync is
        // what turns it into ONE audited `driving_mode` line + push event.
        self.codrive_sync_mode();
    }

    /// Executes one already-validated (by `listener.rs`) agent command on
    /// the calloop main thread — the only thread allowed to touch the
    /// agent seat or `self.space`. See the module doc comment for why the
    /// freeze re-check here, not just the socket thread's, is the
    /// authoritative one.
    pub fn handle_agent_inject(&mut self, cmd: InjectCmd) {
        // D3-c backstop. Keeps the socket thread's mirror honest AND is the
        // authoritative read for the keyboard gate below.
        let ime_paused = self.codrive_refresh_ime_pause();
        if ime_paused && cmd.is_keyboard_op() {
            let (op, x, y) = cmd.describe();
            tracing::warn!(
                op,
                "codrive: dropping a keyboard command — an input method holds a keyboard \
                 grab on the agent seat, so the keystroke would vanish into a composition \
                 instead of reaching the focused window. See crate::ime::seat_filter"
            );
            self.codrive.record(
                "inject_dropped",
                Some(op),
                x,
                y,
                Some("paused_by_ime: an input method holds the agent seat's keyboard grab".into()),
            );
            return;
        }

        let frozen = self.codrive.frozen.load(Ordering::SeqCst);
        // WP-CD2-freeze-scope (module doc item 8): drops everything while
        // frozen UNLESS THIS command's actual target is confirmed confined
        // to the shadow output — see `shadow::is_freeze_bypass_eligible`.
        // CD-3 (module doc item 9): an active takeover forces a TOTAL
        // freeze — no shadow-bypass exception (`takeover.rs` module doc).
        let shadow_bypass =
            frozen && !self.codrive_takeover_active && shadow::is_freeze_bypass_eligible(self, &cmd);

        if frozen && !shadow_bypass {
            let (op, x, y) = cmd.describe();
            let latency = self.codrive_freeze_set_at.map(|t| t.elapsed());
            // Distinguish a plain queued-then-frozen race from a failed
            // shadow-scope check, so the audit trail tells them apart.
            let shadow_note = if self.codrive_shadow_active {
                " — shadow active but this op's target is not confirmed inside the shadow output (fail-closed)"
            } else {
                " (queued-then-frozen race)"
            };
            let detail = format!("frozen at execution time{shadow_note}, latency_us={:?}", latency.map(|d| d.as_micros()));
            tracing::debug!(
                op,
                latency_us = latency.map(|d| d.as_micros()),
                shadow_active = self.codrive_shadow_active,
                "codrive: dropping a queued agent command — seat frozen by the time its turn came up"
            );
            self.codrive.record("inject_dropped", Some(op), x, y, Some(detail));
            return;
        }

        // E1a-1 / E1a-1a routing. smithay routes seat events through the
        // client's OWN `wl_keyboard`/`wl_pointer` objects, which only exist if
        // the client bound that seat (`for_each_focused_kbds` /
        // `for_each_focused_pointer`) — so a command aimed at a client the
        // seat filter hides the agent seat from reaches nobody.
        //
        // E1a-1 shipped the honest half of that: report the drop, never let
        // `inject_applied` claim a keystroke that went nowhere. E1a-1a (option
        // (b), user decision 2026-08-24, reviewed in DESIGN §6.1) adds the
        // recovery: mirror the event onto the HUMAN seat, which every client
        // can see. `human_seat::route_inject` is the whole policy as one pure
        // function; the reasons it refuses, and the two red-line defences
        // behind them, are in that module's doc.
        let target = self.agent_delivery_target(&cmd);
        // Deliberately a closure, not an eagerly-built `String`: the unchanged
        // agent-seat path must stay allocation-free, and only the drop and
        // mirror branches ever need a name.
        let target_app = || {
            target
                .as_ref()
                .and_then(|c| c.get_data::<crate::state::ClientState>())
                .and_then(|d| d.comm().map(str::to_string))
                .unwrap_or_else(|| "<unknown>".to_string())
        };
        let routing = human_seat::route_inject(
            human_seat::op_kind_of(&cmd),
            target.as_ref().map(crate::ime::seat_filter::agent_seat_hidden_from),
            &self.codrive_synthesis_env(),
        );
        if let Some(reason) = routing.drop_with {
            let (op, x, y) = cmd.describe();
            let app = target_app();
            tracing::warn!(
                op,
                app = %app,
                ?reason,
                "codrive: dropping a command — the target client cannot see the agent seat, and \
                 human-seat synthesis is not available for it. Allow-list the process with {} to \
                 co-drive it on the agent seat instead (see crate::ime::seat_filter for the \
                 tradeoff)",
                crate::ime::seat_filter::AGENT_SEAT_PROCS_ENV
            );
            self.codrive.record("inject_dropped", Some(op), x, y, Some(reason.audit_detail(&app)));
            return;
        }

        let (op, x, y) = cmd.describe();
        // Kept for the human-seat mirror below: the agent-seat `match` takes
        // `cmd` by value. Injection commands are small (a `String` at worst),
        // and this clone only happens on the synthesis path.
        let mirror = routing.mirror_to_human_seat.then(|| cmd.clone());

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
                // PointerButton arm does for the human seat (raise + give
                // keyboard focus to the window under the pointer). Without
                // this, `InjectCmd::Text`/`InjectCmd::Key` would have
                // nowhere to route: each `wl_seat`'s keyboard focus is
                // independent (wl_seat spec), and nothing else ever sets
                // the agent seat's.
                //
                // WP-A1 multi-window round: now routed through
                // `DuduclawComp::focus_window` (`state.rs`) instead of the
                // hand-rolled raise+focus this arm used to carry — the
                // hand-rolled version (like `input.rs`'s own, see that
                // file's fix in the same round) never called
                // `Window::set_activated(true)` on the newly-focused
                // window, only `set_activated(false)` on the deselect path
                // (there wasn't one here at all — every window kept
                // whatever activation state it last had). `focus_window`
                // is a small shared helper, not a refactor of the
                // already-VM-verified human click-to-focus semantics in
                // `input.rs`: the raise/pointer/keyboard-focus *behavior*
                // is unchanged for either seat, only the "which windows are
                // marked active" bookkeeping that both call sites need
                // identically is now centralized.
                if pressed && !pointer.is_grabbed() {
                    let pos = pointer.current_location();
                    let window = self.space.element_under(pos).map(|(w, _)| w.clone());
                    let agent_seat = self.agent_seat.clone();
                    self.focus_window(&agent_seat, window.as_ref(), serial);
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
            InjectCmd::WindowGeometry { .. } => {
                // WP-CD4b-fix (B3): answered by the socket thread over its
                // own oneshot query bridge (listener.rs → `CodriveShared::
                // query_window_geometry` → this file's `init`-installed
                // query source → `codrive_window_geometry`), so it never
                // travels down THIS channel. Kept as an arm for the same
                // fail-safe reasoning as Resume/Status/RotateToken below —
                // and critically, returning here means it can never fall
                // through to the `queue_redraw()`/`inject_applied` tail, so
                // a read-only query stays read-only even if a future change
                // starts routing it here.
                tracing::warn!("codrive: WindowGeometry reached handle_agent_inject unexpectedly — no-op (see listener.rs)");
                return;
            }
            InjectCmd::RotateToken => {
                // Handled synchronously by the socket thread (listener.rs) —
                // touches only `CodriveShared`/the token file, never the
                // seat, so it never reaches this channel. Kept as an arm
                // for the same fail-safe reasoning as Resume/Status above.
                tracing::warn!("codrive: RotateToken reached handle_agent_inject unexpectedly — no-op (see listener.rs)");
                return;
            }
            InjectCmd::Shadow { enable } => {
                // CD-2 shadow workspace (WP-CD2-shadow) — full logic lives
                // in `shadow.rs`'s own `impl DuduclawComp` block (window
                // lookup/move + its own detailed audit lines); this arm
                // falls through to the generic `inject_applied` record
                // below like Move/Button/Key/Text/Highlight already do.
                self.codrive_set_shadow(enable);
            }
            // CD-3: full logic lives in `takeover.rs`/`watch.rs` — same
            // "falls through to the generic record below" shape as Shadow.
            InjectCmd::TakeOver { reason } => self.codrive_take_over(reason),
            InjectCmd::Watch { enable } => self.codrive_set_watch(enable),
            // WP-CD4a-COMP: full logic (matching + focus + its own
            // success/failure audit lines) lives in `window_target.rs` —
            // same "falls through to the generic record below" shape as
            // every other seat/space-touching op above.
            InjectCmd::ActivateWindow { app_id } => self.codrive_activate_window(app_id),
        }

        // E1a-1a: the agent-seat path above ran first and unchanged (it keeps
        // the amber cursor, the agent seat's own focus bookkeeping and every
        // existing audit line honest); this additionally puts the same event
        // on the human seat, which is the only seat the target can see.
        if let Some(cmd) = &mirror {
            self.codrive_mirror_to_human_seat(cmd);
        }

        // A4-1 damage source: every arm above that reaches this point moved
        // the agent cursor, clicked, typed into a focused surface, armed a
        // highlight box, or toggled the shadow workspace / takeover / watch
        // state — all of which change what the next composited frame should
        // look like (the agent cursor's colour alone flips on freeze). The
        // early-`return` arms (Resume/Status/RotateToken and the invalid-
        // input drops) deliberately skip this: they change no pixels.
        self.queue_redraw();

        // WP-CD2-freeze-scope: tag shadow-bypassed applies for the audit
        // trail; `None` (unchanged) for every non-bypass apply.
        //
        // E1a-1a: a synthesised command gets its OWN audit kind rather than a
        // tagged `inject_applied`, so existing `inject_applied` counts keep
        // meaning "delivered on the agent seat" (DESIGN §6.1.1 item ③).
        if mirror.is_some() {
            let app = target_app();
            self.codrive.record(
                "inject_via_human_seat",
                Some(op),
                x,
                y,
                Some(format!("synthesized_via=human_seat; target={app}")),
            );
        } else {
            self.codrive.record(
                "inject_applied",
                Some(op),
                x,
                y,
                if shadow_bypass { Some("scope:shadow".to_string()) } else { None },
            );
        }
    }

    /// E1a-1: which client, if any, would this command actually deliver to?
    ///
    /// Only the ops whose entire purpose is client delivery are answered.
    /// `Highlight` / `Shadow` / `Watch` / `TakeOver` / `ActivateWindow` are
    /// compositor-side by construction.
    ///
    /// `None` means "cannot tell" — no keyboard focus, nothing under the
    /// pointer, or a surface whose client already went away — and is treated
    /// as "do not drop", i.e. the check fails open. The command then behaves
    /// exactly as it did before this guard existed.
    ///
    /// **E1a-1a added the `Move` arm.** Under E1a-1 alone this function's only
    /// consumer was the drop decision, and `Move` had to be absent from it: an
    /// agent pointer motion no client hears still moves the compositor-drawn
    /// amber cursor, which is a real effect, not a failure. It is now also the
    /// input to the *mirror* decision, where `Move` matters — a synthesised
    /// click lands wherever the human pointer already is, so the motion has to
    /// be mirrored too. `human_seat::route_inject` keeps `Move`'s exemption
    /// from the drop half explicitly, so answering it here cannot make a
    /// motion droppable.
    fn agent_delivery_target(&self, cmd: &InjectCmd) -> Option<wayland_server::Client> {
        use wayland_server::Resource as _;
        match cmd {
            InjectCmd::Key { .. } | InjectCmd::KeyName { .. } | InjectCmd::Text { .. } => self
                .agent_seat
                .get_keyboard()?
                .current_focus()?
                .client(),
            InjectCmd::Button { .. } => {
                let pos = self.agent_seat.get_pointer()?.current_location();
                self.surface_under(pos).and_then(|(surface, _)| surface.client())
            }
            // The DESTINATION, not the current location: the question is which
            // client the pointer is about to be over.
            InjectCmd::Move { x, y } => self
                .surface_under(Point::<f64, Logical>::from((*x, *y)))
                .and_then(|(surface, _)| surface.client()),
            _ => None,
        }
    }

    /// D3-c backstop: is an input method holding a keyboard grab on the
    /// AGENT seat right now?
    ///
    /// This should never be true — `crate::ime::seat_filter` hides the agent
    /// seat from input-method clients precisely so no such grab can be
    /// established. It exists because that filter has one soft edge: it
    /// recognises an input method by its process name, so an input method
    /// running under an unexpected name (or a smithay upgrade that disarms
    /// the filter's self-check) would slip past. The failure that would then
    /// occur is the worst possible kind — `type_text` returning success while
    /// every keystroke vanishes into a composition nobody reads — so it gets
    /// an explicit, reported state rather than being left to chance.
    ///
    /// Reads smithay's own per-seat `InputMethodHandle`, which tracks whether
    /// a live `zwp_input_method_keyboard_grab_v2` exists for this seat.
    pub fn codrive_ime_grab_active(&self) -> bool {
        use smithay::wayland::input_method::InputMethodSeat;
        self.agent_seat.input_method().keyboard_grabbed()
    }

    /// The same read for the HUMAN seat, where an input-method grab is the
    /// normal, wanted state. Only used for the paired transition log in
    /// `DuduclawComp::ime_note_grab_state`.
    pub fn human_ime_grab_active(&self) -> bool {
        use smithay::wayland::input_method::InputMethodSeat;
        self.seat.input_method().keyboard_grabbed()
    }

    /// Re-reads [`Self::codrive_ime_grab_active`], publishes it to the socket
    /// thread's mirror, and returns it.
    ///
    /// Called from `handle_agent_inject` (so the authoritative check and the
    /// mirror can never disagree at the moment it matters) and once per
    /// housekeeping tick from both backends (so the mirror cannot latch
    /// `true` after the input method exits — a latched mirror would make
    /// `listener.rs` reject keyboard ops forever, with no injection left to
    /// clear it).
    pub fn codrive_refresh_ime_pause(&mut self) -> bool {
        let paused = self.codrive_ime_grab_active();
        // Piggy-backed here rather than given its own per-tick call site:
        // this method already runs exactly where and when that observation is
        // wanted, and both halves read the same pair of seats.
        let human = self.human_ime_grab_active();
        self.ime_note_grab_state(human, paused);
        let was = self.codrive.ime_paused.swap(paused, Ordering::SeqCst);
        if was != paused {
            tracing::warn!(
                paused,
                "codrive: agent-seat keyboard injection {} by an input method grab \
                 (D3-c backstop — the seat filter should have prevented this; see \
                 crate::ime::seat_filter)",
                if paused { "PAUSED" } else { "resumed" }
            );
            self.codrive.record(
                if paused { "ime_pause" } else { "ime_resume" },
                None,
                None,
                None,
                Some("input-method keyboard grab on the agent seat".into()),
            );
        }
        paused
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

// This file's own `#[cfg(test)] mod tests` block moved to
// `codrive/tests_token.rs` in the A2 round — see the `mod tests_token`
// declaration above for why. `check_token`'s tests had already moved to
// `shared.rs` (WP-A1) and `rotate_token`'s to `rotation.rs` (CD-2).
