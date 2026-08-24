// Adapted from smithay's `smallvil` example (`smallvil/src/input.rs`), MIT
// License. See `main.rs` for the full attribution note.
//
// Translates smithay's backend-agnostic `InputEvent<I>` (fed here by the
// winit backend in `winit_backend.rs`) into the seat/pointer/keyboard calls
// that update focus and forward input to the focused client surface.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Device, DeviceCapability, Event,
        InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        PointerMotionEvent,
    },
    desktop::Window,
    input::{
        keyboard::{keysyms, FilterResult, Keysym},
        pointer::{AxisFrame, ButtonEvent, Focus, GrabStartData as PointerGrabStartData, MotionEvent},
    },
    reexports::wayland_server::Resource,
    utils::{Logical, Point, Rectangle, Serial, SERIAL_COUNTER},
};

use crate::{
    decor::{FrameEdge, FrameHit},
    grabs::{MoveClamp, MoveSurfaceGrab, ResizeClamp, ResizeSurfaceGrab},
    state::DuduclawComp,
};

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
                // WM-3: Alt-Tab needs to know when the modifier is RELEASED,
                // which is the one thing a per-press binding cannot observe.
                // Captured here for the same reason `logo_held_now` already is.
                let mut alt_held_now = false;

                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    key_state,
                    serial,
                    time,
                    |data, modifiers, handle| {
                        logo_held_now = modifiers.logo;
                        alt_held_now = modifiers.alt;
                        // D9-bug3/D9-bug4 (2026-08-24): a locked session has
                        // its own, much smaller keyboard policy, and it has
                        // to run BEFORE every arm below — not as one more
                        // `else if` at the end — because the whole point is
                        // that nothing else applies. `locked_key_filter` owns
                        // it (including the Super+Esc emergency stop, which is
                        // the one gesture that still fires while locked); see
                        // `crate::session_lock`'s module doc for why the key
                        // has to be delivered from inside THIS closure rather
                        // than forwarded normally.
                        if data.session_locked() {
                            return data.locked_key_filter(
                                modifiers, handle, key_state, serial, time,
                            );
                        }
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
                            && (modifiers.logo || modifiers.alt)
                            && is_switcher_keysym(handle.modified_sym())
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
                            // there.
                            //
                            // **WM-3 replaced `cycle_focus`** with a real MRU
                            // switcher, and widened the binding to Alt+Tab —
                            // Super+Tab stays as a synonym, per the task
                            // brief. Intercepted rather than forwarded: a
                            // stray Tab arriving in the focused client in the
                            // middle of a window switch is exactly the kind of
                            // "my form jumped a field" bug nobody traces back
                            // to the compositor. See the NOTE at the end of
                            // `state.rs`'s `impl DuduclawComp` for why MRU
                            // replaced z-order rotation.
                            if data.switcher_press(modifiers.shift) {
                                return FilterResult::Intercept(());
                            }
                        } else if key_state == KeyState::Pressed
                            && !modifiers.logo
                            && handle.modified_sym() == Keysym::new(keysyms::KEY_Escape)
                            && data.switcher.session.is_some()
                        {
                            // WM-3: Escape abandons an open switcher, changing
                            // nothing. Guarded on `!modifiers.logo` so it can
                            // never shadow the Super+Esc emergency stop above —
                            // that binding wins unconditionally, which is the
                            // entire point of an emergency stop.
                            data.switcher_cancel();
                            return FilterResult::Intercept(());
                        } else if key_state == KeyState::Pressed
                            && modifiers.logo
                            && is_close_window_keysym(handle.modified_sym())
                        {
                            // WM-1 (2026-08-23): Super+Q — politely ask the
                            // focused window to close. Live report: a Chromium
                            // window on the appliance could not be closed at
                            // all (comp advertised no decoration protocol, so
                            // the client drew no close button, and the dock was
                            // covered). The decoration protocol is the primary
                            // fix; this is the compositor-level guarantee that
                            // works even for a client that draws nothing.
                            //
                            // Same human-only keyboard filter closure as
                            // Super+Esc / Super+Enter / Super+Tab above, so an
                            // agent-injected key event structurally cannot
                            // forge it. `is_system_gesture_tail` below already
                            // exempts any key held with Logo from re-freezing
                            // the seat, so Q's chord tail needs no changes
                            // there either. The session shell is refused — see
                            // `DuduclawComp::close_focused_window`
                            // (`window_policy.rs`).
                            data.close_focused_window();
                        } else if key_state == KeyState::Pressed
                            && modifiers.logo
                            && is_task_bar_keysym(handle.modified_sym())
                        {
                            // A1: Super+K — global "open the交辦欄" gesture,
                            // reachable from on top of ANY app, not just the
                            // shell's own window. The compositor is the only
                            // thing that can see this: once a third-party
                            // client holds keyboard focus, the shell's own
                            // window never receives another key event at all
                            // (standard wlroots-ecosystem division of labour
                            // — the COMPOSITOR owns global hotkeys, a client
                            // only ever sees keys while it has focus). So the
                            // trigger has to live here, exactly like Super+Q/
                            // Super+Esc/Super+Enter/Alt-Tab above, and for the
                            // identical structural reason those are safe from
                            // agent forgery: this is the human seat's OWN
                            // keyboard filter closure, and an agent-injected
                            // key event is applied through a completely
                            // separate path (`codrive::handle_agent_inject` →
                            // `DuduclawComp::agent_key`, `codrive/mod.rs`),
                            // whose own filter closure is an unconditional
                            // `FilterResult::Forward` that never runs any of
                            // this matching at all — see that method's own
                            // doc comment. `is_system_gesture_tail` below
                            // already exempts any key held with Logo from
                            // re-freezing the seat, so K's chord tail needs no
                            // changes there either.
                            //
                            // Intercepted (never forwarded to the focused
                            // client) and queued rather than acted on
                            // directly: this compositor does not own the
                            // task-bar UI itself — `duduclaw-shell` does, as
                            // an Overlay-layer layer-shell surface — so all
                            // comp can honestly do is record that the gesture
                            // happened and let the shell's short poll
                            // (`take_shell_intents`) pick it up.
                            data.push_shell_intent(crate::shell_control::ShellIntent::GlobalTaskBar);
                            return FilterResult::Intercept(());
                        } else if key_state == KeyState::Released
                            && data.switcher.session.is_some()
                            && is_switcher_keysym(handle.modified_sym())
                        {
                            // WM-3: the matching RELEASE for a Tab press this
                            // closure intercepted. Forwarding it would hand the
                            // focused client a key-up with no key-down — an
                            // unbalanced pair that most toolkits tolerate but
                            // none should have to. Only while a session is open,
                            // so an ordinary Tab is completely unaffected.
                            return FilterResult::Intercept(());
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

                // WM-3: hold-to-preview, release-to-commit. The filter closure
                // above sees presses; only this — running after every keyboard
                // event, with the post-event modifier state — can see the
                // moment BOTH modifiers are gone. `switcher_commit` is a no-op
                // when no session is open, so this costs one boolean per key.
                if !(logo_held_now || alt_held_now) {
                    self.switcher_commit();
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
                // D3-f2: from here on the compositor's own pointer location is
                // authoritative — see `pointer_motion_seen`'s doc comment.
                self.pointer_motion_seen = true;
                let pointer = self.seat.get_pointer().unwrap();
                let pos = self.clamp_pointer(pointer.current_location() + event.delta());
                // WM-2: the close button lights up on hover, and the title bar
                // is not a surface, so nothing downstream of `pointer.motion`
                // would ever notice the pointer entering it.
                self.update_close_hover(pos);
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

                // D3-f2: see the identical line in the relative-motion arm.
                self.pointer_motion_seen = true;
                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();
                // WM-2: see the identical call in the relative-motion arm.
                self.update_close_hover(pos);
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

                // D3-f: before ANY of the routing below — a press must never
                // reach a pointer that has never entered a surface. See
                // `ensure_pointer_focus` for the failure this closes.
                //
                // D3-f2: and it must not enter it at the WRONG PLACE either,
                // which is what shipped. The device that produced this press
                // is the only thing that knows where the press happened when
                // no motion preceded it, so it is passed down rather than
                // left to `PointerHandle`'s `(0, 0)` default.
                let device_sysname = Device::id(&event.device());
                self.ensure_pointer_focus(event.time_msec(), &device_sysname);

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
                    let pos = pointer.current_location();

                    // D9-bug4 (2026-08-24): a locked session routes presses to
                    // LAYER SURFACES ONLY — the shell's lock screen (drawn on
                    // its `Background` surface) still takes its own clicks, and
                    // nothing else is reachable: no window focus, no title-bar
                    // drag, no close/minimize button, no resize ring. The
                    // decoration branch below is already dead here because
                    // `frame_hit_at` answers `None` while locked, but the
                    // window branches are not, which is why this arm comes
                    // first rather than relying on that. See
                    // `crate::session_lock`.
                    if self.session_locked {
                        let layer = self
                            .layer_under_pointer(pos, true)
                            .or_else(|| self.layer_under_pointer(pos, false))
                            .filter(|l| l.can_receive_keyboard_focus());
                        if let Some(layer) = layer {
                            let surface = layer.wl_surface().clone();
                            self.focus_layer_surface(&surface);
                        }
                    }
                    // WM-3: a layer surface on the `overlay`/`top` layers is
                    // drawn above every window, so it must also take the click
                    // that visibly lands on it — before the decoration hit test
                    // below, or a panel over a title bar would start a window
                    // drag. The press itself is still forwarded to the client
                    // through the ordinary `pointer.button` call further down;
                    // only keyboard focus is handled here, and only for a
                    // surface that said it wants it.
                    else if let Some(layer) = self.layer_under_pointer(pos, true) {
                        if layer.can_receive_keyboard_focus() {
                            let surface = layer.wl_surface().clone();
                            tracing::debug!(
                                namespace = %layer.namespace(),
                                "input: press on a layer surface — moving keyboard focus to it"
                            );
                            self.focus_layer_surface(&surface);
                        }
                    } else if let Some((window, hit)) = self.frame_hit_at(pos) {
                        // WM-2: the compositor's own decoration gets first
                        // refusal on a press. It has to, because a title bar is
                        // not a surface and `Space::element_under` cannot see
                        // it — see `crate::decor`'s module doc on the geometry
                        // model. `frame_hit_at` returns `None` for a press in a
                        // window's CONTENT area, which is what makes this an
                        // interception rather than a replacement of the
                        // ordinary routing below.
                        //
                        // Clicking any part of the decoration raises and
                        // focuses the window first — including the close
                        // button, so a mis-click still leaves the window you
                        // aimed at in front rather than doing nothing.
                        let seat = self.seat.clone();
                        self.focus_window(&seat, Some(&window), serial);
                        match hit {
                            FrameHit::Close => {
                                self.last_titlebar_click = None;
                                self.close_window_politely(&window, "titlebar_close_button");
                            }
                            FrameHit::Minimize => {
                                self.last_titlebar_click = None;
                                self.minimize_window(&window, "titlebar_minimize_button");
                            }
                            FrameHit::Edge(edge) => {
                                self.last_titlebar_click = None;
                                self.begin_edge_resize(&window, edge, pos, serial, button);
                            }
                            FrameHit::TitleBar => {
                                // WM-3: second click in time and place on the
                                // same bar toggles maximize instead of starting
                                // a second drag.
                                if self.take_titlebar_double_click(&window, pos) {
                                    self.toggle_maximized(&window, "titlebar_double_click");
                                } else {
                                    self.begin_titlebar_move(&window, pos, serial, button);
                                }
                            }
                        }
                        // Deliberately NOT forwarded to any client: the press
                        // landed on compositor-owned pixels. (It would be
                        // harmless — the pointer has no surface focus there —
                        // but "the compositor consumed this" should be
                        // explicit rather than incidental.)
                        return;
                    } else {
                        let window = self.space.element_under(pos).map(|(w, _)| w.clone());
                        // WM-3: with no window under the pointer, a `bottom`/
                        // `background` layer surface may still want the click
                        // (a desktop-icon layer, say). Checked only here, after
                        // windows, which is exactly where it sits in the
                        // z-order.
                        let below = window
                            .is_none()
                            .then(|| self.layer_under_pointer(pos, false))
                            .flatten()
                            .filter(|l| l.can_receive_keyboard_focus());
                        match below {
                            Some(layer) => {
                                let surface = layer.wl_surface().clone();
                                self.focus_layer_surface(&surface);
                            }
                            None => {
                                let seat = self.seat.clone();
                                self.focus_window(&seat, window.as_ref(), serial);
                            }
                        }
                    }
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
            // D3-f2. Deliberately NOT `on_human_input`: a device appearing is
            // not somebody touching it, and treating it as such would freeze
            // the agent seat every time a keyboard is plugged in.
            //
            // The guard keeps this to pointers. A touchscreen also carries
            // ABS_X/ABS_Y, but those hold the last TOUCH point, which is not
            // where any cursor should be — and this compositor has no touch
            // arm at all yet, so believing them would invent a position out
            // of nothing. Non-pointer devices fall through to `_ => {}`,
            // exactly as they did before this arm existed.
            InputEvent::DeviceAdded { device } if device.has_capability(DeviceCapability::Pointer) => {
                let sysname = Device::id(&device);
                self.seed_absolute_pointer_position(&sysname);
            }
            _ => {}
        }
    }

    /// WM-2: which window's **server-side decoration** is under `pos`, and
    /// what part of it.
    ///
    /// Walks the stack top-down and stops at the first window whose frame
    /// contains the point — including when that window answers "not my
    /// decoration". Stopping there is the whole correctness argument: a
    /// press inside window A's content area must never fall through to
    /// window B's title bar just because B happens to be underneath A.
    ///
    /// Returns `None` for a point on no window, on an undecorated window, or
    /// inside a decorated window's content area. All three cases mean the same
    /// thing to the caller: "carry on with the ordinary surface routing".
    ///
    /// D9-bug4 (2026-08-24): and `None` for **every** point while
    /// [`crate::state::DuduclawComp::session_locked`]. Decorations belong to
    /// windows that are not being painted at all on a locked screen, so a
    /// press there must not start a drag or hit an invisible close button —
    /// and the hover highlight in [`Self::update_close_hover`], which is this
    /// function's other caller, must not light up either.
    pub(crate) fn frame_hit_at(&self, pos: Point<f64, Logical>) -> Option<(Window, FrameHit)> {
        if self.session_locked {
            return None;
        }
        // WM-3: the resize ring lives OUTSIDE the frame, so it must be clipped
        // to the work area or a window near the top of it would put an 8 px
        // resize strip over the shell's menu bar. See `decor::edges`.
        let work = self.layout_work_area();
        for window in self.space.elements().rev() {
            let insets = self.window_insets(window);
            let Some(content) = self.space.element_geometry(window) else {
                continue;
            };
            let frame = crate::decor::frame_rect(content, insets);
            if frame.to_f64().contains(pos) {
                return crate::decor::hit_frame(frame, insets, pos).map(|hit| (window.clone(), hit));
            }
            // WM-3: not inside the frame — but possibly on this window's resize
            // ring. Unlike the frame test above, a miss here falls through to
            // the next window down rather than ending the walk: the ring is
            // mostly empty space, and stopping at it would make every window
            // shadow an 8 px dead zone over whatever is beneath it.
            if let Some(edge) = crate::decor::hit_frame_edge_in_work(frame, insets, work, pos) {
                return Some((window.clone(), FrameHit::Edge(edge)));
            }
        }
        None
    }

    /// WM-2: keeps the close button's hover highlight in step with the human
    /// pointer.
    ///
    /// Only repaints on an actual transition. Pointer motion is the highest
    /// frequency event this compositor sees, and marking the frame dirty on
    /// every one of them would defeat the udev backend's "no damage ⇒ no page
    /// flip" idle behaviour for the entire time a pointer is moving over a
    /// title bar.
    pub(crate) fn update_close_hover(&mut self, pos: Point<f64, Logical>) {
        let hit = self.frame_hit_at(pos);
        let button_id = |want: FrameHit| {
            hit.as_ref().and_then(|(window, got)| {
                (*got == want)
                    .then(|| window.toplevel().map(|t| t.wl_surface().id()))
                    .flatten()
            })
        };
        let hovered_close = button_id(FrameHit::Close);
        // WM-3: the minimize button lights up the same way.
        let hovered_minimize = button_id(FrameHit::Minimize);
        if self.decor.hovered_close != hovered_close
            || self.decor.hovered_minimize != hovered_minimize
        {
            self.decor.hovered_close = hovered_close;
            self.decor.hovered_minimize = hovered_minimize;
            self.queue_redraw();
        }
    }

    /// WM-3: is this title-bar press the second half of a double click?
    ///
    /// Consumes the remembered press either way — so a **third** rapid click
    /// starts a fresh pair rather than toggling maximize again, which is what
    /// every desktop does and what stops a drumroll on the title bar from
    /// flapping a window between states.
    fn take_titlebar_double_click(&mut self, window: &Window, pos: Point<f64, Logical>) -> bool {
        let Some(toplevel) = window.toplevel() else {
            return false;
        };
        let id = toplevel.wl_surface().id();
        let now = self.start_time.elapsed();
        let previous = self
            .last_titlebar_click
            .take()
            .and_then(|(prev_id, when, at)| (prev_id == id).then_some((when, at)));
        if crate::decor::is_double_click(previous, now, pos) {
            self.last_titlebar_click = None;
            true
        } else {
            self.last_titlebar_click = Some((id, now, pos));
            false
        }
    }

    /// WM-3: starts a compositor-driven, **clamped** resize from a press on the
    /// window's own resize ring.
    ///
    /// The grab is the same `ResizeSurfaceGrab` a client-initiated
    /// `xdg_toplevel.resize` uses (`handlers/xdg_shell.rs`); the difference is
    /// the [`ResizeClamp`], which keeps the resulting frame inside the work
    /// area — the "縮放結果 clamp 不得讓標題列離開工作區" half of this work
    /// package's third item — and applies the 320×240 floor.
    ///
    /// `start_data.focus` is `None` for the same reason
    /// [`Self::begin_titlebar_move`] uses `None`: the press landed on
    /// compositor-owned pixels, so no client surface can honestly be named as
    /// the grab's origin.
    fn begin_edge_resize(
        &mut self,
        window: &Window,
        edge: FrameEdge,
        pos: Point<f64, Logical>,
        serial: Serial,
        button: u32,
    ) {
        let Some(content) = self.space.element_geometry(window) else {
            return;
        };
        let insets = self.window_insets(window);
        let clamp = self
            .layout_work_area()
            .map(|work| ResizeClamp { work, insets });

        let grab = ResizeSurfaceGrab::start(
            PointerGrabStartData {
                focus: None,
                button,
                location: pos,
            },
            window.clone(),
            edge.into(),
            content,
            clamp,
        );
        tracing::info!(
            surface_id = ?window.toplevel().map(|t| t.wl_surface().id()),
            edge = edge.as_str(),
            initial = ?(content.loc.x, content.loc.y, content.size.w, content.size.h),
            clamped = clamp.is_some(),
            // A drag on TOP/LEFT moves the window's ORIGIN as well as its size
            // (`resize_grab::handle_commit` compensates on the following
            // commit). Which of the two shapes a live drag took is the first
            // thing worth knowing when a window walks sideways, and it is
            // otherwise invisible in the log.
            moves_origin = edge.moves_top() || edge.moves_left(),
            "input: resize ring pressed — resize grab armed"
        );
        let pointer = self.seat.get_pointer().unwrap();
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// WM-2: starts a compositor-driven, **clamped** move grab from a title
    /// bar press.
    ///
    /// The grab itself is the same `MoveSurfaceGrab` a client-initiated
    /// `xdg_toplevel.move` uses (`handlers/xdg_shell.rs`); the only difference
    /// is the [`MoveClamp`], which exists because this path is the human
    /// dragging a window and there is no way to recover one that has been
    /// thrown off the screen.
    ///
    /// `start_data.focus` is `None` on purpose: the press landed on
    /// compositor-owned pixels, so there is no client surface the grab could
    /// legitimately name as its origin.
    fn begin_titlebar_move(
        &mut self,
        window: &Window,
        pos: Point<f64, Logical>,
        serial: Serial,
        button: u32,
    ) {
        let Some(initial_window_location) = self.space.element_location(window) else {
            return;
        };
        let insets = self.window_insets(window);
        let frame_size = self
            .space
            .element_geometry(window)
            .map(|content| crate::decor::frame_rect(content, insets).size);
        let clamp = match (self.layout_work_area(), frame_size) {
            (Some(work), Some(frame_size)) => Some(MoveClamp {
                work,
                frame_size,
                insets,
            }),
            // No real output mapped yet: better an unclamped drag than a drag
            // clamped against a rectangle we invented.
            _ => None,
        };

        let grab = MoveSurfaceGrab {
            start_data: PointerGrabStartData {
                focus: None,
                button,
                location: pos,
            },
            window: window.clone(),
            initial_window_location,
            clamp,
        };
        tracing::info!(
            surface_id = ?window.toplevel().map(|t| t.wl_surface().id()),
            ?initial_window_location,
            clamped = clamp.is_some(),
            "input: title bar pressed — move grab armed"
        );
        let pointer = self.seat.get_pointer().unwrap();
        pointer.set_grab(self, grab, serial, Focus::Clear);
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

    /// D3-f: make sure the human pointer has a focused surface before a
    /// button is forwarded to it.
    ///
    /// ## The bug this fixes
    ///
    /// A `wl_pointer` client only ever learns where the pointer is from an
    /// `enter`, and smithay only emits one from [`PointerHandle::motion`].
    /// Nothing in this compositor called `motion` at startup — the two
    /// `InputEvent::PointerMotion*` arms were the only call sites — so
    /// between comp coming up and the first time the pointer physically
    /// MOVED, `PointerHandle` had no focus at all and
    /// `PointerHandle::button` had nowhere to deliver a press. The click was
    /// swallowed whole: comp itself still ran its click-to-focus path (the
    /// `focus: activation set` line appears in the journal), so from the
    /// outside the compositor looked healthy while the shell never saw a
    /// thing.
    ///
    /// Reproduced in the D3-f VM round after `systemctl restart
    /// duduclaw-kiosk`: the Home 交辦欄 took clicks that did nothing and the
    /// Launcher never opened, recovering only once the pointer was dragged
    /// across the screen. It is not a VM artefact — an absolute-positioning
    /// device (a touchscreen, a KVM, QEMU's `usb-tablet`) reports no motion
    /// at all when the tap lands where the pointer already is, so the first
    /// tap after every restart is dead by construction; a relative mouse
    /// merely hides it behind the jitter of picking the mouse up.
    ///
    /// Cheap and idempotent: one comparison per press on the healthy path,
    /// which is why it sits at the top of the button arm rather than behind
    /// a "have we started yet" flag that would go stale the first time
    /// something else cleared pointer focus.
    ///
    /// ## D3-f2: the enter has to carry the RIGHT coordinates
    ///
    /// The version above shipped, and clicks still missed every target. The
    /// synthesised `enter` was built from `PointerHandle::current_location()`
    /// — which, on the very path this function exists to rescue, is the
    /// untouched `(0, 0)` default, because "no motion has ever arrived" is
    /// the precondition. Measured on the appliance VM: tablet parked at
    /// (639, 226), `wl_pointer.enter(…, 0.0000, 0.0000)`, shell reports
    /// `mouse_down at Point { x: 0px, y: 0px }`. Every press landed on the
    /// top-left corner, so the press *arrived* (D3-f's fix was real) and hit
    /// nothing (D3-f's verification only checked arrival).
    ///
    /// `device_sysname` is libinput's name for the device that produced the
    /// press (`"event1"` for QEMU's tablet). If that device is absolute, the
    /// kernel still holds its current axis values and
    /// [`crate::abs_pointer`] reads them — no event needed. Anything else
    /// (relative mouse, unreadable fd, winit backend, degenerate axis range)
    /// falls through to the pre-D3-f2 behaviour unchanged.
    fn ensure_pointer_focus(&mut self, time: u32, device_sysname: &str) {
        let pointer = self.seat.get_pointer().expect("human seat always has a pointer");
        if pointer.current_focus().is_some() {
            return;
        }
        let (pos, source) = match self.absolute_device_position(device_sysname) {
            Some(p) => (self.clamp_pointer(p), "device"),
            None => (self.clamp_pointer(pointer.current_location()), "compositor"),
        };
        let under = self.surface_under(pos);
        if under.is_none() {
            // Nothing under the cursor to enter. Sending a focus-less motion
            // would be a no-op, and pretending otherwise would just hide the
            // fact that the press really did land on empty space.
            //
            // D3-f2: the position is still worth committing when it came from
            // the device — the human cursor is drawn at
            // `PointerHandle::current_location()` (`cursor/mod.rs`), so
            // leaving it at the origin would draw a cursor that lies about
            // where the next click will go.
            if source == "device" {
                let serial = SERIAL_COUNTER.next_serial();
                self.update_close_hover(pos);
                pointer.motion(self, None, &MotionEvent { location: pos, serial, time });
                pointer.frame(self);
                self.queue_redraw();
            }
            return;
        }
        tracing::debug!(
            ?pos,
            source,
            device = device_sysname,
            "input: pointer had no focused surface — synthesising an enter before the press (D3-f/D3-f2)"
        );
        self.update_close_hover(pos);
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time });
        pointer.frame(self);
    }

    /// D3-f2: where `device_sysname` says it is, in this compositor's logical
    /// coordinate space — or `None` if it is not an absolute device, is not
    /// one libinput opened through us, or has no usable axis range.
    ///
    /// Deliberately reads the kernel on every call rather than caching: the
    /// value is one `ioctl` on an already-open fd, it is only ever consulted
    /// on a press that found no pointer focus (i.e. almost never), and a
    /// cache would be exactly the sort of thing that goes stale in the one
    /// situation this exists to handle.
    pub(crate) fn absolute_device_position(&self, device_sysname: &str) -> Option<Point<f64, Logical>> {
        let (nx, ny) = self.abs_pointer.normalized_position(device_sysname)?;
        let output = self.primary_output()?;
        let geo = self.space.output_geometry(output)?;
        Some(crate::abs_pointer::map_to_output(nx, ny, geo))
    }

    /// D3-f2: place the pointer where an absolute device says it is, the
    /// moment libinput tells us that device exists.
    ///
    /// `ensure_pointer_focus` above already makes the first *click* land
    /// correctly. This makes the first *frame* correct too: without it the
    /// compositor draws its cursor at the origin until something is pressed,
    /// which reads as "the mouse is broken" and then teleports on click.
    ///
    /// Runs once, and only while the pointer has never genuinely moved
    /// ([`crate::state::DuduclawComp::pointer_motion_seen`]) — a device
    /// hot-plugged into a session already in use must not drag the cursor
    /// somewhere the user did not put it. `DeviceAdded` is NOT human input:
    /// it must not call `on_human_input`, or merely plugging in a keyboard
    /// would freeze the agent seat.
    fn seed_absolute_pointer_position(&mut self, device_sysname: &str) {
        if self.pointer_motion_seen {
            return;
        }
        let Some(pos) = self.absolute_device_position(device_sysname) else {
            return;
        };
        let pos = self.clamp_pointer(pos);
        let pointer = self.seat.get_pointer().expect("human seat always has a pointer");
        if pointer.current_location() == pos {
            return;
        }
        tracing::debug!(
            ?pos,
            device = device_sysname,
            "input: seeding the pointer from an absolute device's current axis values (D3-f2)"
        );
        self.update_close_hover(pos);
        let under = self.surface_under(pos);
        let serial = SERIAL_COUNTER.next_serial();
        // `time` is a client-visible event timestamp in the same
        // milliseconds base every other pointer event uses; there is no
        // libinput timestamp on `DeviceAdded`, so it comes from the
        // compositor's own clock (`start_time`) — monotonic, no wall clock.
        let time = self.start_time.elapsed().as_millis() as u32;
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time });
        pointer.frame(self);
        self.queue_redraw();
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

/// WM-1: does this keysym mean "Q" for the Super+Q close-window binding?
///
/// Both cases are accepted. `modified_sym()` is the keysym *after* modifiers
/// are applied, so a user with Caps Lock on — or one who happens to hold Shift
/// while reaching for Super — reports `Q` rather than `q`. Refusing the
/// uppercase form would make the only compositor-level close gesture
/// intermittently dead, which is the failure mode this binding exists to fix.
/// Pure and unit-testable, like the two decision functions above.
pub(crate) fn is_close_window_keysym(sym: Keysym) -> bool {
    sym == Keysym::new(keysyms::KEY_q) || sym == Keysym::new(keysyms::KEY_Q)
}

/// WM-3: does this keysym mean "Tab" for the Alt-Tab / Super-Tab switcher?
///
/// `ISO_Left_Tab` is the second half of the answer and the part that is easy
/// to miss: xkb maps **Shift+Tab** to `ISO_Left_Tab`, not to `Tab`, and
/// `modified_sym()` reports the keysym *after* modifiers are applied. Matching
/// only `Tab` would leave the backwards direction silently dead — a bug that
/// looks like "Shift+Tab does nothing" and is invisible in any test that only
/// exercises the forward direction. Pure and unit-testable, like the two
/// decision functions above.
pub(crate) fn is_switcher_keysym(sym: Keysym) -> bool {
    sym == Keysym::new(keysyms::KEY_Tab) || sym == Keysym::new(keysyms::KEY_ISO_Left_Tab)
}

/// A1: does this keysym mean "K" for the Super+K global task-bar binding?
///
/// Both cases are accepted, for the identical reason [`is_close_window_keysym`]
/// accepts both `Q`/`q`: `modified_sym()` is the keysym *after* modifiers are
/// applied, so a user with Caps Lock on — or one who happens to hold Shift
/// while reaching for Super — reports `K` rather than `k`. Refusing the
/// uppercase form would make the compositor's global task-bar gesture
/// intermittently dead. Pure and unit-testable, like the decision functions
/// above.
pub(crate) fn is_task_bar_keysym(sym: Keysym) -> bool {
    sym == Keysym::new(keysyms::KEY_k) || sym == Keysym::new(keysyms::KEY_K)
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_to, is_close_window_keysym, is_switcher_keysym, is_system_gesture_tail,
        is_task_bar_keysym,
    };
    use smithay::input::keyboard::{keysyms, Keysym};
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

    #[test]
    fn super_q_accepts_both_cases_of_q() {
        assert!(is_close_window_keysym(Keysym::new(keysyms::KEY_q)));
        assert!(is_close_window_keysym(Keysym::new(keysyms::KEY_Q)));
    }

    #[test]
    fn super_q_does_not_fire_on_neighbouring_or_similar_keys() {
        for other in [
            keysyms::KEY_a,
            keysyms::KEY_w,
            keysyms::KEY_Tab,
            keysyms::KEY_Escape,
            keysyms::KEY_Return,
        ] {
            assert!(
                !is_close_window_keysym(Keysym::new(other)),
                "keysym {other:#x} must not be treated as the close binding"
            );
        }
    }

    #[test]
    fn the_switcher_binding_accepts_both_tab_and_shift_tab() {
        // xkb reports Shift+Tab as ISO_Left_Tab; matching only Tab would leave
        // the backwards direction silently dead.
        assert!(is_switcher_keysym(Keysym::new(keysyms::KEY_Tab)));
        assert!(is_switcher_keysym(Keysym::new(keysyms::KEY_ISO_Left_Tab)));
    }

    #[test]
    fn the_switcher_binding_does_not_fire_on_other_keys() {
        for other in [
            keysyms::KEY_q,
            keysyms::KEY_Escape,
            keysyms::KEY_Return,
            keysyms::KEY_space,
            keysyms::KEY_grave,
        ] {
            assert!(
                !is_switcher_keysym(Keysym::new(other)),
                "keysym {other:#x} must not open the switcher"
            );
        }
    }

    #[test]
    fn super_k_accepts_both_cases_of_k() {
        assert!(is_task_bar_keysym(Keysym::new(keysyms::KEY_k)));
        assert!(is_task_bar_keysym(Keysym::new(keysyms::KEY_K)));
    }

    #[test]
    fn super_k_does_not_fire_on_neighbouring_or_similar_keys() {
        for other in [
            keysyms::KEY_j,
            keysyms::KEY_l,
            keysyms::KEY_q,
            keysyms::KEY_Tab,
            keysyms::KEY_Escape,
            keysyms::KEY_Return,
        ] {
            assert!(
                !is_task_bar_keysym(Keysym::new(other)),
                "keysym {other:#x} must not be treated as the global task-bar binding"
            );
        }
    }
}
