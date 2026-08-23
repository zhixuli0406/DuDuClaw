//! E1a-1a option (b) — synthesise agent input through the **human** seat when
//! the target client cannot see the agent seat.
//!
//! ## Why this exists
//!
//! E1a-1 (`crate::ime::seat_filter`) hides the agent seat from every client
//! except the session shell, which fixed the Chromium input blackout. The
//! price, measured rather than assumed, is that smithay delivers seat events
//! only through the client's **own** `wl_keyboard`/`wl_pointer` objects
//! (`for_each_focused_kbds` → `known_kbds`, `smithay-0.7.0/src/wayland/seat/
//! keyboard.rs:143`), so a client that never bound the agent seat receives
//! nothing at all. Co-drive could therefore drive no third-party app, and
//! `handle_agent_inject` honestly dropped those commands
//! (`inject_dropped / unreachable_client`).
//!
//! 2026-08-24 decision (DESIGN-codrive-desktop-2026-08.md §6.1): when the
//! target cannot see the agent seat, mirror the event onto the human seat.
//! Every client can see that one.
//!
//! ## What the design review (DESIGN §6.1) required this module to guarantee
//!
//! The red line being touched is §3.3.1 "事件源頭天然歸因：稽核、凍結、可視化
//! 全部以 seat 為單位". Four structural obligations came out of the review:
//!
//! 1. **Never let a synthesised event look like human input.** `on_human_
//!    input` (`super::on_human_input`) is the single writer of both the freeze
//!    flag and `codrive_last_human_activity`. If a synthesised event reached
//!    it, the agent would either freeze *itself* on every injection (a live
//!    lock: inject → freeze → drop) or forge "a human is present" and disarm
//!    watch mode's idle auto-pause (DESIGN §3.4). Two defences: the emission
//!    helpers below call `Seat`'s API directly and never go through
//!    `input.rs::process_input_event`, and `DuduclawComp::codrive_
//!    synthesizing` is a re-entrancy flag `on_human_input` checks first, so a
//!    future refactor that *does* route synthesis through the backend path
//!    fails loudly instead of silently live-locking.
//!
//! 2. **Never let the agent reach the compositor's global gestures.** The
//!    human keyboard's filter closure in `input.rs` is where Super+Esc
//!    (emergency stop), Super+Enter (hand-back), Super+Q, Super+K and Alt-Tab
//!    are matched. [`DuduclawComp::codrive_human_seat_key`] passes its own
//!    unconditional `FilterResult::Forward` closure, so none of that matching
//!    runs — the same structural property `super::agent_key` already has.
//!
//!    **The review found one real hole here.** `KeyboardHandle::input` updates
//!    the *seat's* xkb modifier state, so an agent that synthesised a bare
//!    Logo-down on the human seat would leave `modifiers.logo == true` for the
//!    next **genuine** human key — a plain Escape would then trigger the
//!    emergency stop, a plain Enter the hand-back. That is remote control of a
//!    human-only gesture by modifier residue, i.e. §6 red line 3 ("急停鍵永遠
//!    有效，agent 不可攔截") defeated indirectly. Hence
//!    [`is_gesture_modifier_keycode`]: Logo and Alt are refused outright on
//!    the synthesis path. `key_name` never maps to them (see
//!    `keymap_ascii::key_name_to_xkb`'s table) and `text` only ever uses
//!    Shift, so only a raw `key` op can carry one.
//!
//! 3. **Never claim agent-seat delivery for a human-seat event.** Synthesised
//!    commands record their own audit kind (`inject_via_human_seat`) rather
//!    than the generic `inject_applied`, so the existing counts keep their
//!    meaning and the trail says which instrument actually moved.
//!
//! 4. **Refuse rather than mix.** The primary defence against human/agent
//!    event interleaving is the existing freeze (any human input stops the
//!    agent). The review enumerated the two gaps where the freeze does *not*
//!    hold — a watch-idle pause being lifted by human input (`codrive_try_
//!    watch_resume` returns before setting `frozen`) and the Super+Enter
//!    gesture tail (`input.rs::is_system_gesture_tail`) — and closes them with
//!    [`HUMAN_ACTIVE_WINDOW`], a synthesis-only quiet period. The agent-seat
//!    path is not subject to it and is byte-identical to before.
//!
//! ## Deliberately NOT solved here
//!
//! * **Client-side attribution.** A client receives a synthesised
//!   `wl_keyboard.key` that is bit-for-bit what a human would have produced,
//!   so an app's own log will attribute it to the user. That is irreducible
//!   for option (b) (the same property RDP/`xdotool`/`wtype` have) and is
//!   disclosed in DESIGN §6.1.1 rather than papered over.
//! * **IME-safe text injection.** With fcitx5 holding the human seat's
//!   keyboard grab, a synthesised keystroke vanishes into a composition. The
//!   honest refusal ([`RefuseReason::ImeGrabHumanSeat`]) ships; routing text
//!   through `zwp_text_input_v3` as a commit string would make comp the
//!   text-input *server* peer, the opposite role from D3-a, and is its own
//!   round.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithay::{
    backend::input::KeyState,
    input::{
        keyboard::FilterResult,
        pointer::{ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};
use smithay::input::keyboard::Keycode;

use super::InjectCmd;
use crate::state::DuduclawComp;

/// Kill switch, following `DUDUCLAW_COMP_SEAT_FILTER`'s convention: anything
/// that is not an explicit `off` leaves the feature ON, so an operator typo
/// lands on the shipped behaviour rather than silently disabling it.
pub const HUMAN_SEAT_SYNTH_ENV: &str = "DUDUCLAW_COMP_CODRIVE_HUMAN_SEAT_SYNTH";

/// How long after a real human input event synthesis stays refused.
///
/// **Reused, not invented** (DESIGN §6.1.2): this crate has no existing
/// "freeze window" constant — `codrive_freeze_set_at` is a timestamp, and
/// DESIGN §5's `<50ms` is a latency *target*. `watch::MIN_WATCH_IDLE_SECS` is
/// the nearest existing semantic: the shortest silence this codebase is
/// willing to call "nobody is there". Borrowing it as "the shortest silence
/// we are willing to call 'the human is not touching this'" keeps one number
/// with one meaning.
///
/// Two accepted consequences, both recorded in DESIGN §6.1.2: no synthesis in
/// the first 5s after boot (`codrive_last_human_activity` starts at startup),
/// and none in the 5s after `watch enable` (which resets that same clock on
/// purpose, see `watch::codrive_set_watch`).
pub const HUMAN_ACTIVE_WINDOW: Duration = Duration::from_secs(super::watch::MIN_WATCH_IDLE_SECS);

/// XKB keycodes for the modifiers that arm a compositor global gesture.
///
/// evdev code + `keymap_ascii::EVDEV_TO_XKB_OFFSET` (8): `KEY_LEFTALT` 56,
/// `KEY_RIGHTALT` 100, `KEY_LEFTMETA` 125, `KEY_RIGHTMETA` 126. Kept as
/// explicit values (not a computed table) so the list is greppable next to
/// the `input.rs` bindings it mirrors: Super+Esc, Super+Enter, Super+Q,
/// Super+K, Alt-Tab / Super-Tab.
const GESTURE_MODIFIER_XKB: [u32; 4] = [56 + 8, 100 + 8, 125 + 8, 126 + 8];

/// See [`GESTURE_MODIFIER_XKB`] and this module's doc item 2.
pub fn is_gesture_modifier_keycode(xkb_keycode: u32) -> bool {
    GESTURE_MODIFIER_XKB.contains(&xkb_keycode)
}

/// Read once per process, like `watch::watch_idle_threshold` and
/// `DUDUCLAW_COMP_DEBUG_STDIN` before it.
pub fn human_seat_synthesis_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let raw = std::env::var(HUMAN_SEAT_SYNTH_ENV).unwrap_or_default();
        parse_synthesis_enabled(&raw)
    })
}

/// Pure half of [`human_seat_synthesis_enabled`], testable without touching
/// the environment.
fn parse_synthesis_enabled(raw: &str) -> bool {
    !raw.trim().eq_ignore_ascii_case("off")
}

/// What an [`super::InjectCmd`] needs from the seat layer, reduced to the
/// axes the routing decision actually turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// `highlight` / `shadow` / `watch` / `take_over` / `activate_window` and
    /// the socket-answered ops. Nothing is delivered to a client, so seat
    /// visibility is irrelevant and routing never applies.
    CompositorOnly,
    /// `move`. Never dropped — the compositor-drawn agent cursor moving is a
    /// real effect even when no client hears it (E1a-1's standing exemption).
    PointerMove,
    /// `button`.
    PointerButton,
    /// `key` / `key_name` / `text`. `gesture_modifier` is true only for a raw
    /// `key` carrying a Logo/Alt keycode (see [`is_gesture_modifier_keycode`]).
    Keyboard { gesture_modifier: bool },
}

/// Live seat/session facts the routing decision reads. Gathered by
/// [`DuduclawComp::codrive_synthesis_env`] so [`route_inject`] stays pure.
#[derive(Debug, Clone, Copy)]
pub struct SynthesisEnv {
    /// [`human_seat_synthesis_enabled`].
    pub enabled: bool,
    /// `now - codrive_last_human_activity`.
    pub human_idle: Duration,
    /// Does an input method hold the HUMAN seat's keyboard grab? (Normal and
    /// wanted for fcitx5 — it is only a problem for synthesis.)
    pub human_ime_grabbed: bool,
    /// Does the human seat's keyboard have a focused surface to deliver to?
    pub human_has_kbd_focus: bool,
    /// Is a shadow-workspace session active? (`codrive_shadow_active`.)
    ///
    /// Synthesis and the shadow workspace are mutually exclusive by
    /// construction: shadow's whole premise (DESIGN §3.1 rule 2) is that the
    /// agent works on a separate output through a separate input channel,
    /// "與人的桌面零交集". The human seat's pointer and keyboard focus live on
    /// the MAIN output, so mirroring a shadow-confined command onto them would
    /// deliver the agent's keystrokes into whatever window the human is
    /// actually using, and drag the human's cursor toward the shadow origin
    /// (0, 100000) — i.e. off-screen. Refused rather than made precise: a
    /// per-command "is this target really on the shadow output" test would
    /// have to fail *open* to be useful here, and failing open is the wrong
    /// direction for a cross-domain leak.
    pub shadow_active: bool,
}

/// Why human-seat synthesis was refused. Each variant is also its audit
/// `detail` prefix — see [`RefuseReason::audit_detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    /// The kill switch is set to `off`: fall back to E1a-1's honest drop.
    SynthDisabled,
    /// A shadow-workspace session is active — see [`SynthesisEnv::shadow_active`].
    ShadowActive,
    /// A real human touched input less than [`HUMAN_ACTIVE_WINDOW`] ago.
    HumanActive,
    /// An input method holds the human seat's keyboard grab, so the keystroke
    /// would vanish into a composition (D3-c's failure mode, other seat).
    ImeGrabHumanSeat,
    /// The human seat's keyboard has no focus — the key would reach nobody,
    /// which is the exact failure E1a-1 exists to stop reporting as success.
    NoHumanFocus,
    /// A raw `key` carrying Logo/Alt: refused so modifier residue can never
    /// arm a compositor global gesture for the next genuine human key.
    GestureModifier,
}

impl RefuseReason {
    /// Stable, greppable audit `detail`. `target` is the refused client's
    /// `/proc/<pid>/comm`, kept in the same shape E1a-1 established for
    /// `unreachable_client`.
    pub fn audit_detail(self, target: &str) -> String {
        let (tag, why) = match self {
            RefuseReason::SynthDisabled => (
                "unreachable_client_synth_disabled",
                "human-seat synthesis is switched off",
            ),
            RefuseReason::ShadowActive => (
                "shadow_active",
                "a shadow-workspace session must never borrow the human's seat",
            ),
            RefuseReason::HumanActive => (
                "human_active",
                "a human touched input inside the synthesis quiet window",
            ),
            RefuseReason::ImeGrabHumanSeat => (
                "paused_by_ime_human_seat",
                "an input method holds the human seat's keyboard grab",
            ),
            RefuseReason::NoHumanFocus => (
                "no_human_focus",
                "the human seat's keyboard has no focused surface",
            ),
            RefuseReason::GestureModifier => (
                "gesture_modifier",
                "Logo/Alt may never be synthesised on the human seat",
            ),
        };
        format!("{tag}: {target} does not see the agent seat and {why} (E1a-1a)")
    }
}

/// The routing decision for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectRouting {
    /// Also emit this event on the human seat after the agent-seat path runs.
    pub mirror_to_human_seat: bool,
    /// Drop the command instead of running it, with this reason.
    pub drop_with: Option<RefuseReason>,
}

impl InjectRouting {
    /// The pre-E1a-1a behaviour: run the agent-seat path, mirror nothing,
    /// drop nothing.
    const UNCHANGED: Self = InjectRouting { mirror_to_human_seat: false, drop_with: None };
}

/// **The whole policy, as one pure function** (same shape as
/// `seat_filter::agent_seat_visible_to` and `shadow::freeze_bypass_decision`).
///
/// `target_hidden` is `Some(true)` when the seat filter hides the agent seat
/// from the client this command would deliver to, `Some(false)` when it does
/// not, and `None` when the target could not be resolved (no focus, nothing
/// under the pointer, client already gone) — which keeps E1a-1's fail-open.
///
/// The freeze gate runs *before* this, in `handle_agent_inject`, and is
/// untouched.
pub fn route_inject(kind: OpKind, target_hidden: Option<bool>, env: &SynthesisEnv) -> InjectRouting {
    if kind == OpKind::CompositorOnly || target_hidden != Some(true) {
        return InjectRouting::UNCHANGED;
    }
    match synthesis_blocker(kind, env) {
        None => InjectRouting { mirror_to_human_seat: true, drop_with: None },
        // `move` keeps E1a-1's exemption: the agent cursor still moves, we
        // just do not drag the human's pointer along. Dropping it would be a
        // regression against today's behaviour, where `move` is never dropped.
        Some(_) if kind == OpKind::PointerMove => InjectRouting::UNCHANGED,
        Some(reason) => InjectRouting { mirror_to_human_seat: false, drop_with: Some(reason) },
    }
}

/// First blocking condition, in refusal order. Ordered cheapest-and-most-
/// absolute first so the audit reason names the *governing* condition rather
/// than whichever check happened to run.
fn synthesis_blocker(kind: OpKind, env: &SynthesisEnv) -> Option<RefuseReason> {
    if !env.enabled {
        return Some(RefuseReason::SynthDisabled);
    }
    if env.shadow_active {
        return Some(RefuseReason::ShadowActive);
    }
    if let OpKind::Keyboard { gesture_modifier: true } = kind {
        return Some(RefuseReason::GestureModifier);
    }
    if env.human_idle < HUMAN_ACTIVE_WINDOW {
        return Some(RefuseReason::HumanActive);
    }
    if matches!(kind, OpKind::Keyboard { .. }) {
        if env.human_ime_grabbed {
            return Some(RefuseReason::ImeGrabHumanSeat);
        }
        if !env.human_has_kbd_focus {
            return Some(RefuseReason::NoHumanFocus);
        }
    }
    None
}

/// Classifies a command for [`route_inject`].
///
/// `gesture_modifier` can only be true for a raw `key`: `key_name`'s table
/// (`keymap_ascii::key_name_to_xkb`) contains no modifier at all, and `text`
/// only ever synthesises Shift.
pub fn op_kind_of(cmd: &InjectCmd) -> OpKind {
    match cmd {
        InjectCmd::Move { .. } => OpKind::PointerMove,
        InjectCmd::Button { .. } => OpKind::PointerButton,
        InjectCmd::Key { keycode, .. } => {
            OpKind::Keyboard { gesture_modifier: is_gesture_modifier_keycode(*keycode) }
        }
        InjectCmd::KeyName { .. } | InjectCmd::Text { .. } => {
            OpKind::Keyboard { gesture_modifier: false }
        }
        // Everything else changes compositor state or is answered by the
        // socket thread; nothing is delivered to a client. Matched
        // exhaustively (no `_` arm) so a new op has to make this choice
        // consciously rather than inheriting "compositor-only" by default.
        InjectCmd::Resume
        | InjectCmd::Status
        | InjectCmd::Highlight { .. }
        | InjectCmd::RotateToken
        | InjectCmd::Shadow { .. }
        | InjectCmd::TakeOver { .. }
        | InjectCmd::Watch { .. }
        | InjectCmd::ActivateWindow { .. }
        | InjectCmd::WindowGeometry { .. } => OpKind::CompositorOnly,
    }
}

impl DuduclawComp {
    /// Emits the human-seat half of a command whose routing said to mirror.
    ///
    /// Runs AFTER the agent-seat path in `handle_agent_inject`, so the agent's
    /// own cursor/focus bookkeeping and audit lines stay exactly as they were.
    /// Re-derives the payload defensively for the same reason the agent-seat
    /// arms do — this module never trusts an upstream parse for anything that
    /// would otherwise panic.
    pub(super) fn codrive_mirror_to_human_seat(&mut self, cmd: &InjectCmd) {
        // The single wrap site for the whole mirror. The individual emission
        // helpers wrap too (each is safe to call on its own, and
        // `codrive_while_synthesizing` saves/restores so nesting is fine), but
        // having it here means the flag also covers `focus_window` and the
        // per-character loop in the `Text` arm, i.e. everything a mirror does.
        self.codrive_while_synthesizing(|this| this.codrive_mirror_inner(cmd))
    }

    fn codrive_mirror_inner(&mut self, cmd: &InjectCmd) {
        match cmd {
            InjectCmd::Move { x, y } => self.codrive_human_seat_motion(*x, *y),
            InjectCmd::Button { btn, state } => {
                let (Ok(button), Ok(pressed)) =
                    (super::parse_button_code(btn), super::parse_press_state(state))
                else {
                    tracing::error!(btn, state, "codrive: invalid button reached human-seat synthesis — dropping the mirror");
                    return;
                };
                self.codrive_human_seat_button(button, pressed);
            }
            InjectCmd::Key { keycode, state } => {
                let Ok(pressed) = super::parse_press_state(state) else {
                    tracing::error!(state, "codrive: invalid key state reached human-seat synthesis — dropping the mirror");
                    return;
                };
                // Belt and braces: `route_inject` already refused this, but a
                // gesture modifier reaching the human seat is the one failure
                // in this module that hands the agent a human-only gesture.
                if is_gesture_modifier_keycode(*keycode) {
                    tracing::error!(keycode, "codrive: a gesture modifier reached human-seat synthesis — refused (see human_seat.rs module doc item 2)");
                    return;
                }
                self.codrive_human_seat_key(*keycode, pressed);
            }
            InjectCmd::KeyName { name, state } => {
                let Ok(pressed) = super::parse_press_state(state) else {
                    tracing::error!(state, "codrive: invalid key_name state reached human-seat synthesis — dropping the mirror");
                    return;
                };
                let Some(xkb) = super::key_name_to_xkb(name) else {
                    tracing::error!(name, "codrive: invalid key_name reached human-seat synthesis — dropping the mirror");
                    return;
                };
                self.codrive_human_seat_key(xkb, pressed);
            }
            InjectCmd::Text { s } => {
                // Same ASCII-only table and shift pairing as the agent-seat
                // `Text` arm, so the two paths cannot disagree about what a
                // string means.
                for c in s.chars() {
                    let Some((xkb, shift)) = super::ascii_to_xkb(c) else {
                        continue; // already warned by the agent-seat arm
                    };
                    if shift {
                        self.codrive_human_seat_key(super::SHIFT_XKB_KEYCODE, true);
                    }
                    self.codrive_human_seat_key(xkb, true);
                    self.codrive_human_seat_key(xkb, false);
                    if shift {
                        self.codrive_human_seat_key(super::SHIFT_XKB_KEYCODE, false);
                    }
                }
            }
            // `route_inject` classifies these `CompositorOnly` and therefore
            // never asks for a mirror. Matched (not `unreachable!`) so a future
            // change fails quietly rather than panicking a compositor.
            InjectCmd::Resume
            | InjectCmd::Status
            | InjectCmd::Highlight { .. }
            | InjectCmd::RotateToken
            | InjectCmd::Shadow { .. }
            | InjectCmd::TakeOver { .. }
            | InjectCmd::Watch { .. }
            | InjectCmd::ActivateWindow { .. }
            | InjectCmd::WindowGeometry { .. } => {
                tracing::warn!("codrive: a compositor-only op reached human-seat synthesis — no-op");
            }
        }
    }

    /// Gathers [`SynthesisEnv`] from live seat state. Cheap: two atomic-ish
    /// reads and an `Instant` subtraction.
    pub(super) fn codrive_synthesis_env(&self) -> SynthesisEnv {
        SynthesisEnv {
            enabled: human_seat_synthesis_enabled(),
            human_idle: Instant::now().saturating_duration_since(self.codrive_last_human_activity),
            human_ime_grabbed: self.human_ime_grab_active(),
            human_has_kbd_focus: self
                .seat
                .get_keyboard()
                .map(|k| k.current_focus().is_some())
                .unwrap_or(false),
            shadow_active: self.codrive_shadow_active,
        }
    }

    /// Runs `f` with [`DuduclawComp::codrive_synthesizing`] set, then clears
    /// it unconditionally.
    ///
    /// This is obligation 1 of the module doc made executable: while the flag
    /// is up, `super::on_human_input` refuses to treat anything as human
    /// input. Nothing in the current call graph can re-enter it (the emission
    /// helpers call `Seat` APIs directly, never `process_input_event`), so
    /// today the flag is pure defence in depth — which is precisely why it is
    /// here rather than in a comment.
    fn codrive_while_synthesizing<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = std::mem::replace(&mut self.codrive_synthesizing, true);
        let out = f(self);
        self.codrive_synthesizing = previous;
        out
    }

    /// Human-seat pointer motion, mirroring the agent-seat `Move` arm.
    ///
    /// Moving the human's visible pointer is a real, disclosed behaviour
    /// change (DESIGN §6.1.2): without it a synthesised click would land
    /// wherever the human last left the cursor, and hover-driven UI (menus,
    /// tooltips) would never respond.
    pub(super) fn codrive_human_seat_motion(&mut self, x: f64, y: f64) {
        self.codrive_while_synthesizing(|this| {
            let pos = Point::<f64, Logical>::from((x, y));
            let serial = SERIAL_COUNTER.next_serial();
            let time = this.start_time.elapsed().as_millis() as u32;
            let under = this.surface_under(pos);
            let Some(pointer) = this.seat.get_pointer() else {
                return;
            };
            pointer.motion(this, under, &MotionEvent { location: pos, serial, time });
            pointer.frame(this);
        });
    }

    /// Human-seat button, mirroring the agent-seat `Button` arm — including
    /// its click-to-focus, deliberately NOT `input.rs`'s much larger human
    /// button arm.
    ///
    /// Two reasons for mirroring the agent arm rather than the human one.
    /// First, `input.rs` hands the compositor's own decoration (title bar,
    /// close button, resize edges) first refusal on a press; routing agent
    /// input through that would hand the agent the close button and window
    /// drags, which the agent seat has never had. Second, the agent arm's
    /// click-to-focus is exactly the piece a following `text` op needs, since
    /// keyboard focus is per-seat.
    pub(super) fn codrive_human_seat_button(
        &mut self,
        button: u32,
        pressed: bool,
    ) {
        self.codrive_while_synthesizing(|this| {
            let serial = SERIAL_COUNTER.next_serial();
            let time = this.start_time.elapsed().as_millis() as u32;
            let Some(pointer) = this.seat.get_pointer() else {
                return;
            };
            if pressed && !pointer.is_grabbed() {
                let pos = pointer.current_location();
                let window = this.space.element_under(pos).map(|(w, _)| w.clone());
                let seat = this.seat.clone();
                this.focus_window(&seat, window.as_ref(), serial);
            }
            let Some(pointer) = this.seat.get_pointer() else {
                return;
            };
            pointer.button(
                this,
                &ButtonEvent {
                    button,
                    state: if pressed {
                        smithay::backend::input::ButtonState::Pressed
                    } else {
                        smithay::backend::input::ButtonState::Released
                    },
                    serial,
                    time,
                },
            );
            pointer.frame(this);
        });
    }

    /// Human-seat key, mirroring `super::agent_key`.
    ///
    /// The `FilterResult::Forward` closure is load-bearing (module doc item
    /// 2): it is what keeps Super+Esc / Super+Enter / Super+Q / Super+K /
    /// Alt-Tab matching — which lives in `input.rs`'s own closure — out of
    /// reach of an injected event, even though the event now travels on the
    /// human seat.
    pub(super) fn codrive_human_seat_key(&mut self, xkb_code: u32, pressed: bool) {
        self.codrive_while_synthesizing(|this| {
            let serial = SERIAL_COUNTER.next_serial();
            let time = this.start_time.elapsed().as_millis() as u32;
            let state = if pressed { KeyState::Pressed } else { KeyState::Released };
            let Some(keyboard) = this.seat.get_keyboard() else {
                return;
            };
            keyboard.input::<(), _>(this, Keycode::new(xkb_code), state, serial, time, |_, _, _| {
                FilterResult::Forward
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_env() -> SynthesisEnv {
        SynthesisEnv {
            enabled: true,
            human_idle: HUMAN_ACTIVE_WINDOW + Duration::from_secs(1),
            human_ime_grabbed: false,
            human_has_kbd_focus: true,
            shadow_active: false,
        }
    }

    // ---- the "nothing changed" half of the decision table (DESIGN §6.1.3) --

    #[test]
    fn a_visible_target_is_routed_exactly_as_before() {
        for kind in [
            OpKind::PointerMove,
            OpKind::PointerButton,
            OpKind::Keyboard { gesture_modifier: false },
        ] {
            assert_eq!(route_inject(kind, Some(false), &quiet_env()), InjectRouting::UNCHANGED);
        }
    }

    #[test]
    fn an_unresolvable_target_fails_open_exactly_as_before() {
        // E1a-1's fail-open: "cannot tell" must never become "drop".
        for kind in [
            OpKind::PointerMove,
            OpKind::PointerButton,
            OpKind::Keyboard { gesture_modifier: false },
        ] {
            assert_eq!(route_inject(kind, None, &quiet_env()), InjectRouting::UNCHANGED);
        }
    }

    #[test]
    fn compositor_only_ops_are_never_routed_even_against_a_hidden_target() {
        let mut hostile = quiet_env();
        hostile.enabled = false;
        hostile.human_idle = Duration::ZERO;
        assert_eq!(
            route_inject(OpKind::CompositorOnly, Some(true), &hostile),
            InjectRouting::UNCHANGED
        );
    }

    // ---- the synthesis half -------------------------------------------------

    #[test]
    fn a_hidden_target_in_a_quiet_session_is_mirrored_to_the_human_seat() {
        for kind in [
            OpKind::PointerMove,
            OpKind::PointerButton,
            OpKind::Keyboard { gesture_modifier: false },
        ] {
            assert_eq!(
                route_inject(kind, Some(true), &quiet_env()),
                InjectRouting { mirror_to_human_seat: true, drop_with: None },
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_recent_human_touch_refuses_synthesis() {
        let mut env = quiet_env();
        env.human_idle = HUMAN_ACTIVE_WINDOW - Duration::from_millis(1);
        assert_eq!(
            route_inject(OpKind::PointerButton, Some(true), &env),
            InjectRouting { mirror_to_human_seat: false, drop_with: Some(RefuseReason::HumanActive) }
        );
    }

    #[test]
    fn the_quiet_window_boundary_is_inclusive_of_exactly_the_window() {
        let mut env = quiet_env();
        env.human_idle = HUMAN_ACTIVE_WINDOW;
        assert!(route_inject(OpKind::PointerButton, Some(true), &env).mirror_to_human_seat);
    }

    #[test]
    fn an_ime_grab_on_the_human_seat_refuses_keyboard_ops_only() {
        let mut env = quiet_env();
        env.human_ime_grabbed = true;
        assert_eq!(
            route_inject(OpKind::Keyboard { gesture_modifier: false }, Some(true), &env).drop_with,
            Some(RefuseReason::ImeGrabHumanSeat)
        );
        // A pointer op is unaffected — a composition swallows keys, not clicks.
        assert!(route_inject(OpKind::PointerButton, Some(true), &env).mirror_to_human_seat);
    }

    #[test]
    fn no_human_keyboard_focus_refuses_keyboard_ops_only() {
        let mut env = quiet_env();
        env.human_has_kbd_focus = false;
        assert_eq!(
            route_inject(OpKind::Keyboard { gesture_modifier: false }, Some(true), &env).drop_with,
            Some(RefuseReason::NoHumanFocus)
        );
        assert!(route_inject(OpKind::PointerButton, Some(true), &env).mirror_to_human_seat);
    }

    #[test]
    fn a_gesture_modifier_is_refused_even_in_a_perfectly_quiet_session() {
        // Module doc item 2: this is the modifier-residue hole, and it must
        // not be reachable by simply waiting out the quiet window.
        assert_eq!(
            route_inject(OpKind::Keyboard { gesture_modifier: true }, Some(true), &quiet_env())
                .drop_with,
            Some(RefuseReason::GestureModifier)
        );
    }

    #[test]
    fn the_kill_switch_restores_the_e1a_1_drop() {
        let mut env = quiet_env();
        env.enabled = false;
        assert_eq!(
            route_inject(OpKind::PointerButton, Some(true), &env).drop_with,
            Some(RefuseReason::SynthDisabled)
        );
    }

    // ---- `move` never drops -------------------------------------------------

    #[test]
    fn move_is_never_dropped_whatever_blocks_synthesis() {
        for env in [
            SynthesisEnv { enabled: false, ..quiet_env() },
            SynthesisEnv { human_idle: Duration::ZERO, ..quiet_env() },
            SynthesisEnv { human_ime_grabbed: true, ..quiet_env() },
            SynthesisEnv { human_has_kbd_focus: false, ..quiet_env() },
        ] {
            assert_eq!(
                route_inject(OpKind::PointerMove, Some(true), &env).drop_with,
                None,
                "move must keep the agent cursor moving: {env:?}"
            );
        }
    }

    #[test]
    fn a_keyboard_only_blocker_still_lets_a_move_be_mirrored() {
        // An IME composition swallows keys, not pointer motion, and keyboard
        // focus is irrelevant to a pointer. Refusing the mirror here would
        // strand the human pointer away from the target and make the next
        // synthesised click land somewhere else entirely.
        for env in [
            SynthesisEnv { human_ime_grabbed: true, ..quiet_env() },
            SynthesisEnv { human_has_kbd_focus: false, ..quiet_env() },
        ] {
            assert!(route_inject(OpKind::PointerMove, Some(true), &env).mirror_to_human_seat, "{env:?}");
        }
    }

    #[test]
    fn an_active_shadow_session_never_borrows_the_human_seat() {
        // The cross-domain leak this closes: shadow work is defined as "zero
        // intersection with the human's desktop", and the human seat's focus
        // and pointer are on the MAIN output.
        let env = SynthesisEnv { shadow_active: true, ..quiet_env() };
        for kind in [OpKind::PointerButton, OpKind::Keyboard { gesture_modifier: false }] {
            assert_eq!(
                route_inject(kind, Some(true), &env).drop_with,
                Some(RefuseReason::ShadowActive),
                "{kind:?}"
            );
        }
        // …and a `move` is not dragged toward the shadow origin either.
        assert_eq!(route_inject(OpKind::PointerMove, Some(true), &env), InjectRouting::UNCHANGED);
    }

    #[test]
    fn a_session_wide_blocker_stops_the_move_mirror_too() {
        // The kill switch, a live human and an active shadow session must all
        // stop the human pointer being driven, even though the command itself
        // is never dropped.
        for env in [
            SynthesisEnv { enabled: false, ..quiet_env() },
            SynthesisEnv { human_idle: Duration::ZERO, ..quiet_env() },
            SynthesisEnv { shadow_active: true, ..quiet_env() },
        ] {
            assert_eq!(
                route_inject(OpKind::PointerMove, Some(true), &env),
                InjectRouting::UNCHANGED,
                "{env:?}"
            );
        }
    }

    // ---- gesture-modifier table --------------------------------------------

    #[test]
    fn gesture_modifier_table_covers_both_alt_and_both_logo_keys() {
        for evdev in [56u32, 100, 125, 126] {
            assert!(is_gesture_modifier_keycode(evdev + 8), "evdev {evdev}");
        }
    }

    #[test]
    fn ordinary_keys_and_shift_are_not_gesture_modifiers() {
        // Shift (evdev 42) is what `text` uses for uppercase; refusing it
        // would kill capital letters for every synthesised string.
        assert!(!is_gesture_modifier_keycode(super::super::SHIFT_XKB_KEYCODE));
        // Ctrl (evdev 29) is not a compositor gesture modifier, so Ctrl+T etc.
        // stay drivable.
        assert!(!is_gesture_modifier_keycode(29 + 8));
        assert!(!is_gesture_modifier_keycode(30 + 8)); // 'a'
    }

    // ---- op classification --------------------------------------------------

    #[test]
    fn op_kind_classifies_every_delivery_op() {
        assert_eq!(op_kind_of(&InjectCmd::Move { x: 1.0, y: 2.0 }), OpKind::PointerMove);
        assert_eq!(
            op_kind_of(&InjectCmd::Button { btn: "left".into(), state: "press".into() }),
            OpKind::PointerButton
        );
        assert_eq!(
            op_kind_of(&InjectCmd::Text { s: "hi".into() }),
            OpKind::Keyboard { gesture_modifier: false }
        );
        assert_eq!(
            op_kind_of(&InjectCmd::KeyName { name: "enter".into(), state: "press".into() }),
            OpKind::Keyboard { gesture_modifier: false }
        );
    }

    #[test]
    fn op_kind_flags_a_raw_logo_or_alt_key_as_a_gesture_modifier() {
        for evdev in [56u32, 100, 125, 126] {
            assert_eq!(
                op_kind_of(&InjectCmd::Key { keycode: evdev + 8, state: "press".into() }),
                OpKind::Keyboard { gesture_modifier: true },
                "evdev {evdev}"
            );
        }
        assert_eq!(
            op_kind_of(&InjectCmd::Key { keycode: 30 + 8, state: "press".into() }),
            OpKind::Keyboard { gesture_modifier: false }
        );
    }

    #[test]
    fn every_key_name_in_the_table_is_free_of_gesture_modifiers() {
        // The module doc claims `key_name` can never carry Logo/Alt. Pin it,
        // so adding "super" to that table has to face this test.
        for name in [
            "enter", "tab", "backspace", "escape", "delete", "space", "up", "down", "left",
            "right", "home", "end", "pageup", "pagedown",
        ] {
            let xkb = super::super::key_name_to_xkb(name).expect(name);
            assert!(!is_gesture_modifier_keycode(xkb), "{name}");
        }
    }

    #[test]
    fn compositor_only_ops_classify_as_compositor_only() {
        for cmd in [
            InjectCmd::Highlight { x: 0.0, y: 0.0, w: 1.0, h: 1.0, ms: None },
            InjectCmd::Shadow { enable: true },
            InjectCmd::Watch { enable: true },
            InjectCmd::TakeOver { reason: "x".into() },
            InjectCmd::ActivateWindow { app_id: "x".into() },
            InjectCmd::Status,
            InjectCmd::Resume,
            InjectCmd::RotateToken,
            InjectCmd::WindowGeometry { app_id: None, pid: None },
        ] {
            assert_eq!(op_kind_of(&cmd), OpKind::CompositorOnly, "{cmd:?}");
        }
    }

    // ---- kill-switch parser -------------------------------------------------

    #[test]
    fn only_an_explicit_off_disables_synthesis() {
        assert!(!parse_synthesis_enabled("off"));
        assert!(!parse_synthesis_enabled("  OFF  "));
        assert!(parse_synthesis_enabled(""));
        assert!(parse_synthesis_enabled("on"));
        assert!(parse_synthesis_enabled("offf")); // typo lands on the shipped side
    }

    // ---- self-freeze guard (DESIGN §6.1.2 M3/M4) ---------------------------
    //
    // These two are source-structure tests, deliberately. The guarantee they
    // pin IS structural — "no code path from a synthesised event into the
    // human-input observer" — so there is no value to assert on, and the
    // runtime alternative would need a live compositor with a GL context,
    // which this crate's suite does not build (see BUILD.md: every live check
    // here is a nested-container run, not a `#[test]`).

    #[test]
    fn the_synthesis_module_never_reaches_the_human_input_path() {
        // `on_human_input` (freeze + watch-presence) and `process_input_event`
        // (the backend door that calls it) are the two ways a synthesised
        // event could be mistaken for a human one. Neither may appear in this
        // module's *code* — the doc comments discuss both at length, so
        // comment lines are stripped first.
        let code: String = include_str!("human_seat.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // This test's own body names them; cut the test module off first.
        let code = code.split("mod tests").next().unwrap_or(&code).to_string();
        assert!(
            !code.contains("on_human_input"),
            "human_seat.rs must never call on_human_input — a synthesised event that freezes the \
             agent live-locks it (DESIGN §6.1.2 M3)"
        );
        assert!(
            !code.contains("process_input_event"),
            "human_seat.rs must never route through the backend input path — that path calls \
             on_human_input for every event (DESIGN §6.1.2 M3/M4)"
        );
        assert!(
            !code.contains("codrive_last_human_activity ="),
            "human_seat.rs must never refresh the human-presence clock — that forges 'a human is \
             here' and disarms watch-mode idle auto-pause (DESIGN §6.1.2 M4)"
        );
    }

    #[test]
    fn the_mirror_entry_point_is_wrapped_in_the_synthesis_guard() {
        let src = include_str!("human_seat.rs");
        let body = src
            .split("pub(super) fn codrive_mirror_to_human_seat")
            .nth(1)
            .expect("the mirror entry point was renamed — re-point this invariant");
        let wrap = body
            .find("codrive_while_synthesizing")
            .expect("the mirror entry point is no longer wrapped in the self-freeze guard");
        let dispatch = body
            .find("codrive_mirror_inner")
            .expect("the mirror entry point no longer dispatches through codrive_mirror_inner");
        assert!(wrap < dispatch, "the guard must be established before anything is emitted");
    }

    #[test]
    fn on_human_input_consults_the_guard_before_recording_human_activity() {
        let src = include_str!("mod.rs");
        let body = src
            .split("pub fn on_human_input")
            .nth(1)
            .expect("on_human_input was renamed — re-point this invariant");
        let guard = body
            .find("self.codrive_synthesizing")
            .expect("the E1a-1a self-freeze guard is missing from on_human_input");
        let activity = body
            .find("self.codrive_last_human_activity =")
            .expect("on_human_input no longer records human activity — re-point this invariant");
        let freeze = body.find("frozen.swap(true").expect("on_human_input no longer freezes");
        assert!(
            guard < activity && guard < freeze,
            "the guard must run before BOTH the presence clock and the freeze, or a synthesised \
             event could still forge presence or freeze the agent (DESIGN §6.1.2 M3/M4)"
        );
    }

    // ---- audit detail -------------------------------------------------------

    #[test]
    fn every_refusal_reason_names_itself_and_the_target_in_the_audit_detail() {
        for (reason, tag) in [
            (RefuseReason::SynthDisabled, "unreachable_client_synth_disabled"),
            (RefuseReason::ShadowActive, "shadow_active"),
            (RefuseReason::HumanActive, "human_active"),
            (RefuseReason::ImeGrabHumanSeat, "paused_by_ime_human_seat"),
            (RefuseReason::NoHumanFocus, "no_human_focus"),
            (RefuseReason::GestureModifier, "gesture_modifier"),
        ] {
            let detail = reason.audit_detail("chromium");
            assert!(detail.starts_with(tag), "{detail}");
            assert!(detail.contains("chromium"), "{detail}");
        }
    }
}
