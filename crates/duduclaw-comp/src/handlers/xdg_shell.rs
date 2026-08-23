// Adapted from smithay's `smallvil` example
// (`smallvil/src/handlers/xdg_shell.rs`), MIT License. See `main.rs` for
// the full attribution note.

use smithay::{
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, PopupKeyboardGrab,
        PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
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
                // WM-2: deliberately unclamped. This is a CLIENT asking to be
                // moved (tear-off tab, drag-to-attach); the compositor's own
                // title-bar drag is the clamped one. See `grabs::MoveClamp`.
                clamp: None,
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
                // WM-3: deliberately unclamped. This is a CLIENT asking to be
                // resized (its own resize edges, its own toolkit); the
                // compositor's edge-ring drag is the clamped one. See
                // `grabs::resize_grab::ResizeClamp`.
                None,
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
        let root_is_toplevel = self
            .space
            .elements()
            .any(|w| w.toplevel().unwrap().wl_surface() == &root);
        // WM-3: a layer surface is a legitimate popup root now (a panel's own
        // menu). Before layer-shell existed this branch could only ever mean
        // "already closed", which is why the original comment said so.
        if !root_is_toplevel && !self.is_mapped_layer_surface(&root) {
            tracing::debug!(
                "xdg_shell: grab request whose root isn't a mapped toplevel or layer surface — ignoring"
            );
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
        // WM-3: a MINIMIZED window is not in the space, so the lookup above
        // misses it — but the switcher may well be pointing at it. Resolve
        // against both sets before anything is unmapped.
        let window_for_switcher = window_to_remove.clone().or_else(|| {
            self.minimized
                .iter()
                .find(|w| {
                    w.toplevel()
                        .map(|t| t.wl_surface() == &wl_surface)
                        .unwrap_or(false)
                })
                .cloned()
        });
        if let Some(window) = window_to_remove {
            self.space.unmap_elem(&window);
        }

        // WM-1: release the session-shell role if this was the shell, so a
        // restarted shell can claim it again (and so nothing keeps comparing
        // against a dead surface).
        self.forget_shell_window(&wl_surface);
        // WM-2: drop the decoration buffers, the negotiated mode, the restore
        // geometry and the hover state for this toplevel. `ObjectId`s are
        // never reused, so nothing else would ever evict these.
        self.forget_window_decor(&wl_surface.id());
        // WM-3: and everything else keyed on this window — the minimized park,
        // the MRU order the switcher reads, and an open switcher session that
        // may be pointing at it right now.
        self.forget_minimized(&wl_surface);
        crate::alt_tab::mru_forget(&mut self.focus_mru, &wl_surface.id());
        if let Some(window) = window_for_switcher {
            self.switcher_forget(&window);
        }

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
    /// WM-2 changed two things about it:
    ///
    /// 1. The work area is now the **frame**, not the content — a maximized
    ///    server-decorated window still has its 32 px title bar, so the client
    ///    is configured to the work area *minus* its decoration. Without this
    ///    the bottom of a maximized window would hang past the work area and
    ///    over the shell's dock by exactly the decoration's height.
    /// 2. Comp now remembers where the window was, so [`Self::
    ///    unmaximize_request`] has somewhere to go back to. That memory is
    ///    `DecorState::frames` — the same restore rectangle the floating
    ///    placement already maintains, not a second store.
    ///
    /// This is still the one place `xdg_toplevel.State::Maximized` is set. The
    /// initial configure deliberately still does not set it — see
    /// `handle_commit` below for why (it changes CSD for every GTK/Qt app we
    /// host, which is only ever appropriate when the client itself asked).
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // WM-3 moved the body into `DuduclawComp::set_maximized` so the
        // double-click-the-title-bar path (`input.rs`) drives exactly the same
        // code rather than a second, drifting copy.
        self.set_maximized(&surface, true, "maximize_request");
    }

    /// WM-1 counterpart to [`Self::maximize_request`], **rewritten in WM-2**.
    ///
    /// WM-1's version cleared the `Maximized` state and left the size where it
    /// was, with an explicit note that comp kept no restore geometry. WM-2
    /// keeps one (`DecorState::frames`, the floating placement's own
    /// rectangle), so this genuinely restores the window — refitted into the
    /// current work area, which may have changed while it was maximized.
    ///
    /// A client that opened maximized has no remembered frame; it falls back
    /// to a fresh cascade slot rather than to nothing at all.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.set_maximized(&surface, false, "unmaximize_request");
    }

    /// WM-2: the title bar draws `xdg_toplevel.title`, so a title change is
    /// now a visual change.
    ///
    /// Nothing is invalidated by hand: `decor::paint` keys its cached glyph
    /// raster on `(title, available width)`, so the next composite picks the
    /// new string up by itself. All this has to do is make sure there *is* a
    /// next composite — on the udev backend nothing else would schedule one,
    /// and a renamed tab would sit stale until some unrelated damage happened.
    /// Upstream's default is a no-op, so nothing was listening before.
    fn title_changed(&mut self, surface: ToplevelSurface) {
        tracing::debug!(
            surface_id = ?surface.wl_surface().id(),
            "xdg_shell: title changed — repainting the title bar"
        );
        self.queue_redraw();
    }
}

/// `zxdg_decoration_manager_v1` — **WM-2: negotiated per window**.
///
/// WM-1 answered a flat `ClientSide` for one honest reason: comp drew no
/// decorations, so claiming `ServerSide` would have given every window *no*
/// decoration at all. WM-2 draws them (`crate::decor`), so the preference
/// flips to `ServerSide` — but not unconditionally. The rule, its table, and
/// the first-hand Chromium observation behind it live in `decor::mode`; this
/// impl is only the protocol plumbing around
/// [`crate::decor::mode::answer_request`].
///
/// Effect on `duduclaw-shell`: still none. It creates its decoration object
/// before its first commit (gpui `gpui_linux/src/linux/wayland/window.rs:278`)
/// and would therefore be recorded as `ServerSide` here — but the shell role
/// is settled a moment later on that first commit, and
/// `window_policy::sync_decoration_mode` downgrades it to `ClientSide` on the
/// same configure that carries its size, so what the shell is *told* always
/// matches what comp actually *draws* for it (nothing).
impl XdgDecorationHandler for DuduclawComp {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // The client created the object but has not asked for anything: it
        // gets our preference.
        self.set_decoration_mode(&toplevel, crate::decor::mode::PREFERRED, "new_decoration");
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: zxdg_toplevel_decoration_v1::Mode) {
        self.set_decoration_mode(
            &toplevel,
            crate::decor::mode::answer_request(mode),
            "request_mode",
        );
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        // "I withdraw my preference" — back to ours.
        self.set_decoration_mode(&toplevel, crate::decor::mode::PREFERRED, "unset_mode");
    }
}

impl DuduclawComp {
    /// WM-3: the one implementation of maximize/restore.
    ///
    /// Three callers drive it: `maximize_request`, `unmaximize_request`, and
    /// the WM-3 double-click on the title bar (`input.rs`). Having them share
    /// this is not tidiness — a second copy would be a second place for the
    /// "the FRAME fills the work area, the client gets that minus its
    /// decoration" rule and the restore-geometry snapshot ordering to drift,
    /// and both are subtle enough that the drift would be silent.
    ///
    /// Maximizing:
    /// * the **frame** fills the work area, so a maximized server-decorated
    ///   window still shows its 32 px title bar and its bottom edge lands on
    ///   the work area's bottom rather than hanging over the dock;
    /// * where the window was is snapshotted **before** the `maximized` flag is
    ///   set, because `decor_sync_frame` deliberately does nothing once it is —
    ///   that is what stops the restore rectangle being overwritten with the
    ///   maximized one.
    ///
    /// Restoring goes back to the remembered floating frame, refitted into the
    /// work area as it is *now* (it may have changed while the window was
    /// maximized). A client that opened maximized has no remembered frame and
    /// gets a fresh cascade slot rather than nothing at all.
    pub(crate) fn set_maximized(
        &mut self,
        surface: &ToplevelSurface,
        maximized: bool,
        reason: &'static str,
    ) {
        let wl_surface = surface.wl_surface().clone();
        let id = wl_surface.id();
        let window = self.toplevel_window_for(&wl_surface);
        let Some(work) = self.layout_work_area() else {
            // No real output: nothing to maximize against. The configure still
            // goes out so the client is not left waiting on one.
            surface.send_configure();
            return;
        };
        let insets = window
            .as_ref()
            .map(|w| self.window_insets(w))
            .unwrap_or(crate::decor::DecorInsets::NONE);

        let frame = if maximized {
            if let Some(window) = window.as_ref() {
                self.decor_sync_frame(window);
            }
            self.decor.maximized.insert(id.clone());
            work
        } else {
            self.decor.maximized.remove(&id);
            let frame = match self.decor.frames.get(&id).copied() {
                Some(remembered) => crate::decor::refit_frame(remembered, work, insets),
                None => {
                    let index = self.decor.cascade_next;
                    self.decor.cascade_next = self.decor.cascade_next.wrapping_add(1);
                    crate::decor::cascade_frame_rect(work, insets, index)
                }
            };
            self.decor.frames.insert(id.clone(), frame);
            frame
        };
        let content = crate::decor::content_rect(frame, insets);

        surface.with_pending_state(|state| {
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
            state.size = Some(content.size);
        });
        if let Some(window) = window {
            if self.space.element_location(&window) != Some(content.loc) {
                self.space.map_element(window, content.loc, false);
            }
        }

        tracing::info!(
            surface_id = ?id,
            reason,
            maximized,
            frame = ?(frame.loc.x, frame.loc.y, frame.size.w, frame.size.h),
            content = ?(content.loc.x, content.loc.y, content.size.w, content.size.h),
            decorated = insets.is_decorated(),
            "xdg_shell: maximize state changed — the FRAME fills the work area; \
             the client gets that minus its decoration"
        );
        self.queue_redraw();
        surface.send_configure();
    }

    /// WM-3: toggles the maximize state of a mapped window, used by the
    /// double-click-the-title-bar path. No-op for a window with no toplevel.
    pub(crate) fn toggle_maximized(&mut self, window: &Window, reason: &'static str) {
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        let maximized = self.decor.maximized.contains(&toplevel.wl_surface().id());
        self.set_maximized(&toplevel, !maximized, reason);
    }

    fn set_decoration_mode(
        &mut self,
        toplevel: &ToplevelSurface,
        mode: crate::decor::DecorMode,
        reason: &'static str,
    ) {
        let surface = toplevel.wl_surface().clone();
        let previous = self.decor.modes.insert(surface.id(), mode);

        // What we ANSWER is what we will actually DRAW, which for an already
        // mapped window depends on its role as well as on its request (the
        // shell and shadow-workspace windows are never server-decorated). For
        // a window that has not mapped yet the role is not decided, so the
        // raw negotiated mode goes out and the initial configure's
        // `window_policy::sync_decoration_mode` corrects it a moment later.
        let effective = match self.toplevel_window_for(&surface) {
            Some(window) if self.window_uses_ssd(&window) => crate::decor::DecorMode::ServerSide,
            Some(_) => crate::decor::DecorMode::ClientSide,
            None => mode,
        };
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(effective.wire());
        });
        tracing::debug!(
            surface_id = ?surface.id(),
            reason,
            requested = mode.as_str(),
            answered = effective.as_str(),
            "xdg_decoration: negotiated"
        );

        // Sending a configure here BEFORE the initial one would be a
        // correctness bug, not just noise: `ToplevelSurface::send_configure`
        // sets `initial_configure_sent` (smithay 0.7.0
        // `wayland/shell/xdg/mod.rs`), so `handle_commit`'s initial-configure
        // branch — the only thing that gives a window its size and position —
        // would never run and the client would fall back to picking its own
        // geometry. Both gpui and Chromium create their decoration object
        // before their first commit, so that branch is the normal path.
        if !toplevel.is_initial_configure_sent() {
            return;
        }

        // WM-2: a mode change on an ALREADY MAPPED window changes the frame
        // insets, which changes how big the client may be inside a frame that
        // must stay put. Re-running the layout policy is what recomputes that;
        // it sends the configure itself.
        if previous != Some(mode) {
            if let Some(window) = self.toplevel_window_for(&surface) {
                self.apply_window_policy(&window);
                return;
            }
        }
        toplevel.send_pending_configure();
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
            //
            // WM-2: a client may answer a configure with a size it picked
            // itself, so the remembered floating frame is brought back in step
            // here. No-op for an unplaced, maximized or degenerate window —
            // see `DuduclawComp::decor_sync_frame`.
            state.decor_sync_frame(&window);
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
    /// WM-3 changed three things here, none of them cosmetic:
    ///
    /// 1. `pub(crate)` — `crate::layer_shell`'s `new_popup` calls it too.
    /// 2. **A layer surface can be a popup's root.** A panel's own menu is an
    ///    `xdg_popup` whose parent is a `zwlr_layer_surface_v1`, so the
    ///    toplevel-only lookup would have bailed and left the menu placed by
    ///    the client's raw positioner, i.e. free to run off the screen.
    /// 3. **`space.outputs().next()` was the CD-2 shadow-output bug** (see
    ///    `state::primary_output`'s note): it returns the headless shadow
    ///    output at `(0, 100_000)`, so every popup was being unconstrained
    ///    against a rectangle 100 000 px below the screen. Now it asks
    ///    `layout_output`. The `unwrap()`s went with it — a popup arriving
    ///    before any output is mapped is a real ordering, not a panic.
    pub(crate) fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(output) = self.layout_output() else {
            return;
        };
        let Some(output_geo) = self.space.output_geometry(&output) else {
            return;
        };

        // The parent's geometry, in GLOBAL coordinates — a layer map's own
        // geometry is output-local, hence the `+ output_geo.loc`.
        let parent_geo = match self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        {
            Some(window) => self.space.element_geometry(window),
            None => {
                let map = layer_map_for_output(&output);
                map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                    .and_then(|l| map.layer_geometry(l))
                    .map(|g| Rectangle::new(g.loc + output_geo.loc, g.size))
            }
        };
        let Some(parent_geo) = parent_geo else {
            return;
        };

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= parent_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    /// Is `surface` a currently-mapped layer surface? Used by the popup-grab
    /// gate, which must accept a panel's menu as readily as an application's.
    fn is_mapped_layer_surface(&self, surface: &WlSurface) -> bool {
        self.layout_output()
            .map(|output| {
                layer_map_for_output(&output)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .unwrap_or(false)
    }
}
