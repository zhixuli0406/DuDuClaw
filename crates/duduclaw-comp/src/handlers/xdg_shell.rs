// Adapted from smithay's `smallvil` example
// (`smallvil/src/handlers/xdg_shell.rs`), MIT License. See `main.rs` for
// the full attribution note.

use smithay::{
    delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, PopupKeyboardGrab, PopupKind, PopupManager,
        PopupPointerGrab, PopupUngrabStrategy, Space, Window,
    },
    input::{
        pointer::{Focus, GrabStartData as PointerGrabStartData},
        Seat,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            protocol::{wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Rectangle, Serial},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
    },
};

use crate::{
    grabs::{MoveSurfaceGrab, ResizeSurfaceGrab},
    DuduclawComp,
};

impl XdgShellHandler for DuduclawComp {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // A4-1 damage source: a new window enters the stack.
        self.queue_redraw();
        // Live-run evidence (Shell-S0 nested headless round, 2026-08-19/20):
        // a real xdg_toplevel object was created by a connected client. This
        // fires before the client's first commit/configure ack, so
        // `handle_commit` below logs the actual "now visible" moment.
        tracing::info!(
            surface_id = ?surface.wl_surface().id(),
            "xdg_shell: new toplevel created, mapping into space"
        );
        let window = Window::new_wayland_window(surface);
        // CD-2 shadow workspace (WP-CD2-shadow, DESIGN §3.3.4): a toplevel
        // created while a shadow session is already active (e.g. the agent
        // launches a second client mid-session) maps straight into the
        // shadow region instead of the main output's `(0, 0)` — see
        // `codrive::SHADOW_ORIGIN`'s doc for the isolation this location
        // gives for free. A window that already existed BEFORE shadow mode
        // was enabled is instead moved by `DuduclawComp::codrive_set_shadow`
        // (`codrive/shadow.rs`), not here.
        if self.codrive_shadow_active {
            self.space.map_element(window, crate::codrive::SHADOW_ORIGIN, false);
            self.codrive.record(
                "shadow_window_moved",
                Some("shadow"),
                None,
                None,
                Some("to_shadow (mapped directly — shadow was already active at toplevel-creation time)".into()),
            );
        } else {
            self.space.map_element(window, (0, 0), false);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // A4-1 damage source: a popup enters the stack.
        self.queue_redraw();
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();

        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let window = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.space.element_location(&window).unwrap();

            let grab = MoveSurfaceGrab {
                start_data,
                window,
                initial_window_location,
            };

            // WP-A1 multi-window round: greppable evidence that a client's
            // own CSD drag handling actually reached the compositor and a
            // move grab was armed — this handler previously had no log
            // line at all, so the only prior evidence `grabs/move_grab.rs`
            // had was "it compiles" (BUILD.md's "Still unverified" list).
            tracing::info!(surface_id = ?wl_surface.id(), ?initial_window_location, "xdg_shell: move_request — move grab armed");
            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();

        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let window = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.space.element_location(&window).unwrap();
            let initial_window_size = window.geometry().size;

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });

            surface.send_pending_configure();

            let grab = ResizeSurfaceGrab::start(
                start_data,
                window,
                edges.into(),
                Rectangle::new(initial_window_location, initial_window_size),
            );

            // WP-A1 multi-window round: same "previously silent" gap as
            // `move_request` above.
            tracing::info!(surface_id = ?wl_surface.id(), ?initial_window_location, ?initial_window_size, "xdg_shell: resize_request — resize grab armed");
            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    /// WP-A1 multi-window round: popup grabs (right-click/dropdown menus
    /// getting exclusive input until dismissed). smallvil (this crate's
    /// original template — see `main.rs`'s attribution note) never
    /// implemented this; the *structure* here (which library types to
    /// call, in what order) is adapted from smithay's `anvil` example
    /// (`anvil/src/shell/xdg.rs`'s own `grab()`, same upstream repo, same
    /// MIT license — verified 2026-08-22 against the `v0.7.0` tag: repo-
    /// root `LICENSE.txt` covers `anvil/` too, no separate license file
    /// inside that directory) — per this round's task brief, the only
    /// permitted reference besides the Wayland protocol spec text itself.
    /// simplified for this crate's plainer surface model: anvil threads a
    /// `KeyboardFocusTarget` enum (Window | LayerSurface | Popup) through
    /// `Seat<AnvilState>::KeyboardFocus` because it also has layer-shell;
    /// this crate's `SeatHandler::KeyboardFocus = WlSurface` (see
    /// `handlers/mod.rs`) already matches what `PopupManager::grab_popup`
    /// wants directly, so there is no focus-target wrapper type to build,
    /// and the layer-shell fallback branch anvil's version has doesn't
    /// apply here (this crate has no layer-shell — see `main.rs`'s "what
    /// this spike deliberately does not carry over" list) — a popup's
    /// root here must be an already-mapped toplevel window, checked with
    /// the exact same `self.space.elements().find(...)` lookup
    /// `unconstrain_popup` below already uses. Also no touch-grab branch:
    /// this crate's seats never call `add_touch` (`state.rs`/`codrive/
    /// mod.rs` only ever add keyboard+pointer), so `seat.get_touch()`
    /// would always be `None` here.
    ///
    /// The actual grab mechanics — outside-click dismissal, nested-popup
    /// topmost-only enforcement, keyboard-event forwarding while grabbed —
    /// are NOT reimplemented here at all: `PopupManager::grab_popup` plus
    /// `PopupKeyboardGrab`/`PopupPointerGrab` are smithay LIBRARY types
    /// (`smithay::desktop`, the same crate this file already depends on
    /// via `Cargo.toml`, not application code), so this function's job is
    /// only to construct them correctly and hand them to the seat via the
    /// same `pointer.set_grab`/`keyboard.set_grab` calls `move_request`/
    /// `resize_request` above already use for the move/resize grabs. See
    /// `PopupPointerGrab::button`'s own doc comment (upstream, in
    /// smithay's `src/desktop/wayland/popup/grab.rs`) for exactly how the
    /// "click outside dismisses" behavior works: it compares the pointer's
    /// current focus's client against the grabbed popup's client on every
    /// press and ungrabs-all on a mismatch — this crate's existing
    /// `input.rs`/`codrive/mod.rs` pointer-motion/-button code already
    /// feeds `pointer.motion`/`pointer.button` unconditionally every
    /// event, which is all `PointerHandle` needs to route through whatever
    /// grab (move/resize/popup/none) is currently active — no changes
    /// were needed there for this to work.
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<DuduclawComp> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);

        let Ok(root) = find_popup_root_surface(&kind) else {
            tracing::debug!("xdg_shell: grab request for a popup with no resolvable root surface — ignoring");
            return;
        };
        if !self
            .space
            .elements()
            .any(|w| w.toplevel().unwrap().wl_surface() == &root)
        {
            // Root isn't a currently-mapped toplevel (already closed, or —
            // this crate has no layer-shell — some other kind of surface
            // entirely). Nothing to grab against.
            tracing::debug!("xdg_shell: grab request whose root isn't a mapped toplevel — ignoring");
            return;
        }

        let mut grab = match self.popups.grab_popup(root, kind, &seat, serial) {
            Ok(grab) => grab,
            Err(e) => {
                tracing::debug!(error = %e, "xdg_shell: popup grab denied by PopupManager");
                return;
            }
        };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                tracing::debug!("xdg_shell: popup grab denied — keyboard already held by an unrelated grab");
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                tracing::debug!("xdg_shell: popup grab denied — pointer already held by an unrelated grab");
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }

        tracing::info!("xdg_shell: popup grab established");
    }

    /// WP-A1 multi-window round (task brief req 3, "視窗關閉焦點轉移規則"):
    /// smithay calls this automatically (default no-op upstream — see
    /// `XdgShellHandler::toplevel_destroyed`'s doc in `smithay::wayland::
    /// shell::xdg`) whenever a client destroys an `xdg_toplevel`. Before
    /// this round nothing implemented it at all, so a closed window's
    /// `Window` lingered in `self.space` until the next frame's
    /// `state.space.refresh()` (`winit_backend.rs`'s redraw loop) happened
    /// to reap it — and even then, nothing ever reassigned keyboard focus
    /// away from the now-dead surface, so a client closing its focused
    /// window left both seats' keyboard focus pointing at a dead object
    /// until the next click. Two fixes here: unmap eagerly (don't wait for
    /// the next redraw) so `reassign_focus_on_window_removed`'s z-order
    /// lookup already reflects the removal, then hand focus to whatever's
    /// now on top — see that method's doc (`state.rs`) for why it's
    /// per-seat and conditional rather than unconditional.
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // A4-1 damage source: whatever the closed window was covering has to
        // be repainted. Set here rather than relying on
        // `reassign_focus_on_window_removed` → `focus_window`, because that
        // path deliberately does nothing when the destroyed window did not
        // hold focus — and a background window closing still leaves a hole.
        self.queue_redraw();
        let wl_surface = surface.wl_surface().clone();
        tracing::info!(surface_id = ?wl_surface.id(), "xdg_shell: toplevel destroyed, unmapping and reassigning focus");

        // Bound to a `let` first (not `if let self.space.elements()....`
        // directly) so the borrow of `self.space` inside `elements()` ends
        // at this statement's `;` — `if let`'s scrutinee temporaries are
        // otherwise kept alive for the whole `if let` block, which would
        // conflict with the `&mut self.space` `unmap_elem` call below.
        let window_to_remove = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &wl_surface)
            .cloned();
        if let Some(window) = window_to_remove {
            self.space.unmap_elem(&window);
        }

        self.reassign_focus_on_window_removed(&wl_surface);
    }
}

// Xdg Shell
delegate_xdg_shell!(DuduclawComp);

fn check_grab(
    seat: &Seat<DuduclawComp>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<PointerGrabStartData<DuduclawComp>> {
    let pointer = seat.get_pointer()?;

    // Check that this surface has a click grab.
    if !pointer.has_grab(serial) {
        return None;
    }

    let start_data = pointer.grab_start_data()?;

    let (focus, _) = start_data.focus.as_ref()?;
    // If the focus was for a different surface, ignore the request.
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }

    Some(start_data)
}

/// Should be called on `WlSurface::commit`
pub fn handle_commit(popups: &mut PopupManager, space: &Space<Window>, surface: &WlSurface) {
    // Handle toplevel commits.
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
        .cloned()
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        if !initial_configure_sent {
            tracing::info!(
                surface_id = ?surface.id(),
                "xdg_shell: sending initial configure to toplevel"
            );
            window.toplevel().unwrap().send_configure();
        } else {
            // Every later commit is the client redrawing/resizing an
            // already-mapped surface; the *first* commit after the initial
            // configure (the `else` branch on the very next call) is the
            // "client actually has a buffer up" moment we want in evidence.
            // WP-A1 multi-window round: geometry added at debug level —
            // previously the only way to find a window's negotiated size
            // (needed to target a CSD resize hotspot for a live multi-
            // client resize-grab test) was to guess blindly with no
            // screenshot available in this headless container.
            tracing::debug!(
                surface_id = ?surface.id(),
                geometry = ?window.geometry(),
                location = ?space.element_location(&window),
                "xdg_shell: toplevel commit (already configured)"
            );
        }
    }

    // Handle popup commits.
    popups.commit(surface);
    if let Some(popup) = popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    // NOTE: This should never fail as the initial configure is always
                    // allowed.
                    xdg.send_configure().expect("initial configure failed");
                }
            }
            PopupKind::InputMethod(ref _input_method) => {}
        }
    }
}

impl DuduclawComp {
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        else {
            return;
        };

        let output = self.space.outputs().next().unwrap();
        let output_geo = self.space.output_geometry(output).unwrap();
        let window_geo = self.space.element_geometry(window).unwrap();

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
