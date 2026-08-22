// Adapted from smithay's `smallvil` example
// (`smallvil/src/handlers/xdg_shell.rs`), MIT License. See `main.rs` for
// the full attribution note.

use smithay::{
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, PopupKeyboardGrab, PopupKind,
        PopupPointerGrab, PopupUngrabStrategy, Window,
    },
    input::{
        pointer::{Focus, GrabStartData as PointerGrabStartData},
        Seat,
    },
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1, shell::server::xdg_toplevel,
        },
        wayland_server::{
            protocol::{wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Rectangle, Serial},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            decoration::XdgDecorationHandler,
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
            // WM-1: still `(0, 0)` here on purpose. The real position comes
            // from `DuduclawComp::apply_window_policy` on this toplevel's
            // FIRST commit (the initial-configure branch of `handle_commit`
            // below), because that is the earliest moment the window has an
            // identity to classify against and the last moment before the
            // client attaches its first buffer — so nothing is ever drawn at
            // the provisional origin and there is no visible jump.
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

        // WM-1: release the session-shell role if this was the shell, so a
        // restarted shell can claim it again (and so nothing keeps comparing
        // against a dead surface).
        self.forget_shell_window(&wl_surface);

        self.reassign_focus_on_window_removed(&wl_surface);
    }

    /// WM-1: the moment the reserved-band policy has been waiting for.
    ///
    /// gpui sets `xdg_toplevel.app_id` **after** its first `wl_surface.commit`
    /// (see `window_policy::DuduclawComp::classify_shell_window`'s doc for the
    /// exact upstream line numbers), so the initial configure necessarily runs
    /// on an identity-less window. This handler is where the identity finally
    /// arrives; re-running the policy here either confirms the first-mapped
    /// guess (the normal boot, no configure sent — the size is unchanged) or
    /// corrects it (a window that is really the shell gets the whole output,
    /// and the provisional holder is demoted to the work area).
    ///
    /// Upstream's default is a no-op, so nothing was listening before.
    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface().clone();
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &wl_surface)
            .cloned();
        if let Some(window) = window {
            self.apply_window_policy(&window);
        }
    }

    /// WM-1: "maximize" means **the work area**, not the whole output — the
    /// same rule Windows' taskbar and the macOS menu bar enforce, and the
    /// reason the reserved bands are called a work area at all. Without this
    /// (upstream's default sends a configure carrying no state change) a
    /// Chromium/GTK maximize button was simply inert.
    ///
    /// This is the one place `xdg_toplevel.State::Maximized` is set. The
    /// initial configure deliberately still does not set it — see
    /// `handle_commit` below for why (it changes CSD for every GTK/Qt app we
    /// host, which is only ever appropriate when the client itself asked).
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let Some(output_geo) = self.layout_output_geometry() else {
            surface.send_configure();
            return;
        };
        let area = crate::window_policy::work_area(output_geo, self.reserved_bands);
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some(area.size);
        });
        let wl_surface = surface.wl_surface().clone();
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &wl_surface)
            .cloned();
        if let Some(window) = window {
            if self.space.element_location(&window) != Some(area.loc) {
                self.space.map_element(window, area.loc, false);
            }
        }
        tracing::info!(
            surface_id = ?wl_surface.id(),
            area = ?(area.loc.x, area.loc.y, area.size.w, area.size.h),
            "xdg_shell: maximize_request — configured to the work area (output minus the shell's reserved bands)"
        );
        self.queue_redraw();
        surface.send_configure();
    }

    /// WM-1 counterpart to [`Self::maximize_request`]. Comp keeps no restore
    /// geometry (that is window-management state A5 owns), so this clears the
    /// `Maximized` state — which is what the client needs to redraw its
    /// titlebar correctly — and leaves the size where it is rather than
    /// inventing a "previous" size the compositor never recorded.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
        });
        tracing::info!(
            surface_id = ?surface.wl_surface().id(),
            "xdg_shell: unmaximize_request — clearing the maximized state (comp keeps no restore geometry; A5 owns that)"
        );
        self.queue_redraw();
        surface.send_configure();
    }
}

/// WM-1: `zxdg_decoration_manager_v1`, answered **always** `ClientSide`.
///
/// The live report was that a Chromium window had no way to be closed. Comp
/// draws no server-side decorations and did not advertise this protocol at
/// all, so a client had no negotiated answer to "who draws the title bar" and
/// was free to draw none. Advertising the global and replying `ClientSide`
/// makes the contract explicit: the client owns its own title bar, close
/// button, and drag/resize affordances.
///
/// Why not `ServerSide`: comp has no decoration renderer, and inventing one is
/// explicitly A5's work package, not this transitional one. Claiming
/// `ServerSide` while drawing nothing would give every window *no* decoration
/// at all — the exact bug being fixed.
///
/// Effect on `duduclaw-shell`: none. gpui's Wayland backend initialises
/// `decorations: WindowDecorations::Client` regardless
/// (`gpui_linux/src/linux/wayland/window.rs:610`) and only leaves that state
/// on an explicit `ServerSide` configure, and `duduclaw-shell` never reads
/// `Window::window_decorations()` anyway — verified by grep over the shell
/// crate, not assumed. It creates the decoration object as soon as the global
/// exists (`window.rs:278`), which is *before* its first commit, so the
/// `ClientSide` mode rides along on the same initial configure that carries
/// the size.
impl XdgDecorationHandler for DuduclawComp {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_client_side_decoration(&toplevel, "new_decoration");
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: zxdg_toplevel_decoration_v1::Mode) {
        // A client asking for `ServerSide` gets `ClientSide` anyway — which
        // the protocol explicitly allows ("the compositor can decide not to
        // use the client's mode"), and which is the honest answer while comp
        // draws no decorations. Logged rather than silently overridden so a
        // "my title bar looks wrong" report is answerable from the log.
        if mode == zxdg_toplevel_decoration_v1::Mode::ServerSide {
            tracing::debug!(
                surface_id = ?toplevel.wl_surface().id(),
                "xdg_decoration: client asked for server-side decorations — answering client-side (comp draws none)"
            );
        }
        self.set_client_side_decoration(&toplevel, "request_mode");
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_client_side_decoration(&toplevel, "unset_mode");
    }
}

impl DuduclawComp {
    fn set_client_side_decoration(&mut self, toplevel: &ToplevelSurface, reason: &'static str) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ClientSide);
        });
        // Sending a configure here BEFORE the initial one would be a
        // correctness bug, not just noise: `ToplevelSurface::send_configure`
        // sets `initial_configure_sent` (smithay 0.7.0
        // `wayland/shell/xdg/mod.rs`), so `handle_commit`'s initial-configure
        // branch — the only thing that gives a window its size and position —
        // would never run and the client would fall back to picking its own
        // geometry. Both gpui and Chromium create their decoration object
        // before their first commit, so this branch is the normal path.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        tracing::debug!(
            surface_id = ?toplevel.wl_surface().id(),
            reason,
            "xdg_decoration: client-side decorations"
        );
    }
}

// Xdg Shell
delegate_xdg_shell!(DuduclawComp);
// WM-1: xdg-decoration (see `XdgDecorationHandler` above).
delegate_xdg_decoration!(DuduclawComp);

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
///
/// WM-1 changed this from `(&mut PopupManager, &Space<Window>, &WlSurface)` to
/// the whole state: the initial-configure branch now consults the window
/// layout policy (`crate::window_policy`), which needs to read the shell
/// identity and *move* the element, not just read the space.
pub fn handle_commit(state: &mut DuduclawComp, surface: &WlSurface) {
    // Handle toplevel commits.
    //
    // Bound to a `let` first rather than used directly as the `if let`
    // scrutinee: `if let`'s temporaries live for the whole block, so the
    // immutable borrow of `state.space` taken by `elements()` would still be
    // alive at the `state.apply_window_policy(&window)` call below. Same
    // pattern (and the same reason) as `toplevel_destroyed` above.
    let committed_window = state
        .space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
        .cloned();
    if let Some(window) = committed_window {
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
            // Tell the client HOW BIG to be. `send_configure()` on its own
            // sends a 0x0 size, which in xdg-shell means "you pick" — and a
            // client that picks freely picks something that has nothing to do
            // with this screen. Found live on the appliance (2026-08-22, first
            // cold boot with comp as the session compositor): `duduclaw-shell`
            // chose a window ~1280x957 against a 1280x800 output, so the OOBE
            // footer — Back / Skip / 下一步, i.e. the only way forward — was
            // simply below the bottom edge. It looked like a missing button,
            // not an oversized window. `cage` never showed this because a
            // kiosk compositor forces its single client to the output size;
            // that is exactly the behaviour comp has to keep now that it is
            // the one running the session.
            //
            // A4's scope note said "EVERY toplevel gets the full output …
            // when A5's multi-window desktop lands it owns the layout policy".
            // WM-1 (2026-08-23) is the transitional half of that: the shell
            // still gets the full output, every other toplevel gets the output
            // MINUS the bands the shell's own menu bar and dock occupy — the
            // "work area" rule every mainstream desktop applies to its own
            // chrome. Without it the first third-party window covered the
            // shell entirely and the session had no reachable navigation left.
            // `crate::window_policy` owns the rule, the numbers, and the
            // shell-identification logic; A5 still owns real window management.
            //
            // Still NOT marking the surface Maximized/Fullscreen here, for A4's
            // original reason: those states change CSD (shadows, rounded
            // corners, client-side resize edges) for every GTK/Qt app we host.
            // `maximize_request` above sets `Maximized` — but only when the
            // client itself asked for it.
            //
            // No real output yet → `apply_window_policy` leaves the 0x0
            // ("you pick") behaviour untouched rather than guessing a size,
            // exactly as the pre-WM-1 code did.
            state.apply_window_policy(&window);
            tracing::info!(
                surface_id = ?surface.id(),
                configured_size = ?window.toplevel().unwrap().with_pending_state(|s| s.size),
                location = ?state.space.element_location(&window),
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
                location = ?state.space.element_location(&window),
                "xdg_shell: toplevel commit (already configured)"
            );
        }
    }

    // Handle popup commits.
    state.popups.commit(surface);
    if let Some(popup) = state.popups.find_popup(surface) {
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
