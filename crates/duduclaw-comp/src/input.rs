// Adapted from smithay's `smallvil` example (`smallvil/src/input.rs`), MIT
// License. See `main.rs` for the full attribution note.
//
// Translates smithay's backend-agnostic `InputEvent<I>` (fed here by the
// winit backend in `winit_backend.rs`) into the seat/pointer/keyboard calls
// that update focus and forward input to the focused client surface.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{keysyms, FilterResult, Keysym},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
};

use crate::state::DuduclawComp;

impl DuduclawComp {
    /// Every arm below runs exclusively on the real human ("winit") seat —
    /// the agent seat's own events are applied through a completely
    /// separate path (`codrive::handle_agent_inject`) that never calls
    /// into this function. That separation is what makes `on_human_input`
    /// safe to call unconditionally at the top of every arm here: nothing
    /// coming through `process_input_event` can ever be agent-originated
    /// input freezing itself.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        // A4-1: any human input can move the human cursor overlay, change
        // focus, or drag a window under an active grab. Marking dirty here
        // (once, for every arm) is what lets the udev backend stay blocked
        // in `epoll` the rest of the time. No-op for the winit backend,
        // which drives its own unconditional redraw loop.
        self.queue_redraw();
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let key_state = event.state();
                let mut logo_held_now = false;

                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    key_state,
                    serial,
                    time,
                    |data, modifiers, handle| {
                        logo_held_now = modifiers.logo;
                        // Super+Esc global emergency stop (DESIGN
                        // §3.3.3/§6.3): the human keyboard's filter
                        // closure is the only code path that can ever
                        // observe this combo — there is no route from an
                        // injected agent key event into this closure, so
                        // the agent structurally cannot trigger or
                        // intercept it. NOT hardware-verified by this
                        // round's container live-run (headless weston has
                        // no keyboard device at all — see
                        // `codrive/debug_sim.rs` module doc); the debug
                        // stdin path exercises the resulting state machine
                        // instead.
                        if key_state == KeyState::Pressed
                            && modifiers.logo
                            && handle.modified_sym() == Keysym::new(keysyms::KEY_Escape)
                        {
                            data.emergency_stop("super+esc");
                        } else if key_state == KeyState::Pressed
                            && modifiers.logo
                            && handle.modified_sym() == Keysym::new(keysyms::KEY_Return)
                        {
                            // CD-1 human-side "交還" (DESIGN §3.1: "『交還』
                            // 是明確動作（按鈕/Super+Enter）", task brief
                            // req 2). Same structural guarantee as
                            // Super+Esc above: only the human keyboard
                            // path can ever reach this, so the agent
                            // cannot self-resume by forging the combo.
                            // Same container-vs-VM verification split as
                            // Super+Esc too — see `codrive/debug_sim.rs`'s
                            // `simulate_super_enter` line for the
                            // container-level state-machine coverage.
                            data.human_resume();
                        } else if key_state == KeyState::Pressed
                            && modifiers.logo
                            && handle.modified_sym() == Keysym::new(keysyms::KEY_Tab)
                        {
                            // WP-A1 multi-window round (task brief req 3):
                            // window cycling, same human-only keyboard
                            // filter closure as Super+Esc/Super+Enter above
                            // — structurally unreachable from agent-
                            // injected key events for the identical reason
                            // those two are. `is_system_gesture_tail`
                            // below already exempts ANY key while Logo is
                            // (or was just) held from re-freezing the
                            // seat, so Tab's chord tail needed no changes
                            // there. See `DuduclawComp::cycle_focus`'s doc
                            // comment (`state.rs`) for the rotation
                            // strategy.
                            data.cycle_focus();
                        }
                        FilterResult::Forward
                    },
                );

                // CD-2 VM round fix: real-hardware verification found that
                // sending a genuine Super+Enter chord (four discrete key
                // events — Logo down, Return down, Return up, Logo up, the
                // way a physical keyboard actually reports a held-modifier
                // chord) left the seat FROZEN right after `human_resume()`
                // un-froze it, because `on_human_input` used to run
                // unconditionally for every keyboard event including the
                // chord's own trailing release events — the "hand back
                // control" gesture was immediately re-observed as "human
                // touched input" and re-froze itself, making Super+Enter
                // unable to durably resume on real hardware (the container
                // round's debug-stdin simulator called `human_resume()`
                // directly and could never have caught this — it has no
                // release-event tail at all). See
                // `is_system_gesture_tail`'s doc comment for the exemption
                // rule; ordinary keys (not part of an active Logo chord)
                // are completely unaffected.
                let system_gesture = is_system_gesture_tail(logo_held_now, self.codrive_logo_held_prev);
                self.codrive_logo_held_prev = logo_held_now;
                if !system_gesture {
                    self.on_human_input("keyboard");
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                self.on_human_input("pointer_motion");

                // A4-1: this arm used to be a bare `on_human_input` call and
                // nothing else. That was harmless on the winit backend —
                // smithay's winit backend only ever emits
                // `PointerMotionAbsolute` (see `backend/winit/mod.rs`), so
                // relative motion never arrived. libinput emits exactly the
                // opposite for an ordinary mouse/trackpad, so on real
                // hardware an unimplemented arm here means "the pointer
                // never moves at all". Implemented as
                // accumulate-delta-then-clamp, the standard shape for a
                // compositor with no pointer-constraint protocol.
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                let pointer = self.seat.get_pointer().unwrap();
                let pos = self.clamp_pointer(pointer.current_location() + event.delta());
                let under = self.surface_under(pos);
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_human_input("pointer_motion_absolute");

                // A4-1 bug fix: this used to be `self.space.outputs().next()`,
                // which since the CD-2 shadow workspace landed has returned
                // the HEADLESS shadow output (mapped first, in
                // `DuduclawComp::new`, at `codrive::SHADOW_ORIGIN` =
                // `(0, 100_000)`), not the real one. Absolute pointer
                // positions were therefore being mapped 100 000 px below
                // every real window. See `DuduclawComp::primary_output`.
                let Some(output) = self.primary_output().cloned() else {
                    return;
                };
                let Some(output_geo) = self.space.output_geometry(&output) else {
                    return;
                };

                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();
                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                self.on_human_input("pointer_button");

                let pointer = self.seat.get_pointer().unwrap();

                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let button_state = event.state();

                // WP-A1 multi-window round: routed through
                // `DuduclawComp::focus_window` (`state.rs`) instead of the
                // hand-rolled raise+focus this arm used to carry. Same
                // raise/keyboard-focus *behavior* as before (this is the
                // path BUILD.md's "VM cage real-seat input verification"
                // already exercised on real hardware) — the fix is that
                // the old code never called `Window::set_activated(true)`
                // on the window it just focused, only `set_activated(false)`
                // on the click-on-empty-space path, so a selected window's
                // xdg-shell `activated` state (and client-side active/
                // inactive titlebar styling keyed off it) never lit up.
                // `focus_window` sets it for every window on every call.
                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    let window = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, _)| w.clone());
                    let seat = self.seat.clone();
                    self.focus_window(&seat, window.as_ref(), serial);
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                self.on_human_input("pointer_axis");

                let source = event.source();

                let horizontal_amount = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
                let vertical_amount = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    /// A4-1: keeps a relative-motion pointer inside the union of the REAL
    /// outputs (the CD-2 shadow output at `codrive::SHADOW_ORIGIN` is
    /// excluded via `primary_output`-style filtering, otherwise the union
    /// would stretch 100 000 px down and the cursor could wander off the
    /// visible screen into the shadow workspace).
    ///
    /// With no real output mapped yet the position is returned unchanged —
    /// clamping to an empty region would pin the cursor at the origin.
    fn clamp_pointer(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let mut bounds: Option<Rectangle<i32, Logical>> = None;
        for output in self.space.outputs() {
            if output == &self.shadow_output {
                continue;
            }
            if let Some(geo) = self.space.output_geometry(output) {
                bounds = Some(match bounds {
                    Some(b) => b.merge(geo),
                    None => geo,
                });
            }
        }
        let Some(b) = bounds else {
            return pos;
        };
        clamp_to(pos, b)
    }
}

/// Pure clamp, split out of [`DuduclawComp::clamp_pointer`] so the geometry
/// rule is unit-testable without a `Space`/`Output` (this crate's standing
/// constraint — see `is_system_gesture_tail` below).
///
/// The upper bound is exclusive-ish: a pointer exactly on `loc + size` would
/// be one pixel past the last addressable pixel and `surface_under` would
/// find nothing there, so it is pulled back by a hair.
pub(crate) fn clamp_to(pos: Point<f64, Logical>, bounds: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    const EPS: f64 = 1.0;
    let min_x = bounds.loc.x as f64;
    let min_y = bounds.loc.y as f64;
    let max_x = (bounds.loc.x + bounds.size.w) as f64 - EPS;
    let max_y = (bounds.loc.y + bounds.size.h) as f64 - EPS;
    Point::from((
        pos.x.clamp(min_x, min_x.max(max_x)),
        pos.y.clamp(min_y, min_y.max(max_y)),
    ))
}

/// Pure decision function, kept unit-testable without a full `DuduclawComp`
/// (this crate's usual constraint — see `duduclaw-comp/BUILD.md`'s many
/// "Honest stub" notes on why anything touching real seat/space state is
/// live/container-verified instead). `logo_held_now` is the Logo (Super)
/// modifier's state reported by the keyboard filter closure for THIS
/// keyboard event; `logo_held_prev` is the same value captured on the
/// human seat's immediately preceding keyboard event.
///
/// True whenever this event is plausibly part of an in-progress Super+Enter
/// / Super+Esc chord — Logo held now (covers Logo-down, Return/Escape-down,
/// and Return/Escape-up, since Logo is still held for all three) OR Logo
/// was held on the previous event but not this one (covers Logo's own
/// release, whose reported `modifiers.logo` may already read `false` for
/// the very event that clears it). This compositor has no other binding on
/// the Logo key, so exempting the whole chord — not just the exact
/// Return/Escape keysyms — from re-freezing the seat is intentional, not an
/// overly broad approximation: any keyboard event where Logo is or was just
/// involved is, by construction, chord activity, never ordinary desktop
/// typing.
pub(crate) fn is_system_gesture_tail(logo_held_now: bool, logo_held_prev: bool) -> bool {
    logo_held_now || logo_held_prev
}

#[cfg(test)]
mod tests {
    use super::{clamp_to, is_system_gesture_tail};
    use smithay::utils::{Logical, Point, Rectangle, Size};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn a_pointer_inside_the_bounds_is_untouched() {
        let p = clamp_to(Point::from((640.0, 400.0)), rect(0, 0, 1280, 800));
        assert_eq!((p.x, p.y), (640.0, 400.0));
    }

    #[test]
    fn a_pointer_off_the_left_or_top_is_pulled_back_to_the_origin() {
        let p = clamp_to(Point::from((-50.0, -9.0)), rect(0, 0, 1280, 800));
        assert_eq!((p.x, p.y), (0.0, 0.0));
    }

    #[test]
    fn a_pointer_off_the_right_or_bottom_stays_on_an_addressable_pixel() {
        let p = clamp_to(Point::from((99_999.0, 99_999.0)), rect(0, 0, 1280, 800));
        assert_eq!((p.x, p.y), (1279.0, 799.0));
    }

    #[test]
    fn a_non_zero_origin_is_respected_in_both_directions() {
        // Second monitor in a left-to-right layout.
        let b = rect(1280, 0, 1920, 1080);
        assert_eq!(clamp_to(Point::from((0.0, 0.0)), b).x, 1280.0);
        assert_eq!(clamp_to(Point::from((99_999.0, 0.0)), b).x, 3199.0);
    }

    #[test]
    fn a_degenerate_one_pixel_output_does_not_invert_the_clamp_range() {
        // `min.max(max)` guards `clamp`'s "min > max" panic for a 1px (or
        // 0px) output — a real possibility for a connector that reports a
        // nonsense mode.
        let p = clamp_to(Point::from((50.0, 50.0)), rect(10, 10, 1, 1));
        assert_eq!((p.x, p.y), (10.0, 10.0));
        let p = clamp_to(Point::from((50.0, 50.0)), rect(10, 10, 0, 0));
        assert_eq!((p.x, p.y), (10.0, 10.0));
    }

    #[test]
    fn logo_currently_held_is_always_a_gesture_tail() {
        assert!(is_system_gesture_tail(true, true));
        assert!(is_system_gesture_tail(true, false));
    }

    #[test]
    fn logos_own_release_is_still_a_gesture_tail() {
        // logo_held_now = false (this event IS the Logo release), but it
        // was held on the previous event — must still be exempted, this is
        // the exact case the CD-2 VM round found broken.
        assert!(is_system_gesture_tail(false, true));
    }

    #[test]
    fn ordinary_key_with_no_recent_logo_activity_is_not_a_gesture_tail() {
        assert!(!is_system_gesture_tail(false, false));
    }
}
