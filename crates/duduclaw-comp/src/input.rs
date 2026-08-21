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
                self.on_human_input("keyboard");

                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let key_state = event.state();

                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    key_state,
                    serial,
                    time,
                    |data, modifiers, handle| {
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
