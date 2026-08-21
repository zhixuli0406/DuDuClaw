// Adapted from smithay's `smallvil` example (`smallvil/src/input.rs`), MIT
// License. See `main.rs` for the full attribution note.
//
// Translates smithay's backend-agnostic `InputEvent<I>` (fed here by the
// winit backend in `winit_backend.rs`) into the seat/pointer/keyboard calls
// that update focus and forward input to the focused client surface.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::{keysyms, FilterResult, Keysym},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
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
            InputEvent::PointerMotion { .. } => {
                self.on_human_input("pointer_motion");
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_human_input("pointer_motion_absolute");

                let output = self.space.outputs().next().unwrap();
                let output_geo = self.space.output_geometry(output).unwrap();

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
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    if let Some((window, _loc)) = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
                    {
                        self.space.raise_element(&window, true);
                        keyboard.set_focus(
                            self,
                            Some(window.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                        self.space.elements().for_each(|window| {
                            window.toplevel().unwrap().send_pending_configure();
                        });
                    } else {
                        self.space.elements().for_each(|window| {
                            window.set_activated(false);
                            window.toplevel().unwrap().send_pending_configure();
                        });
                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                    }
                };

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
    use super::is_system_gesture_tail;

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
