//! A4 (CP-1, 2026-08-30): XWayland support.
//!
//! Design: `commercial/docs/DESIGN-app-compat-layer-2026-08.md` R-P7 ("image
//! 帶 XWayland; Bottles/Wine 路硬前置"). Motivating evidence: the 2026-08-30
//! QEMU live-run found an X11-only client (`chromium --ozone-platform=x11`)
//! simply could not start at all — this compositor advertised no XWayland,
//! so there was nowhere for it to connect. This module is what makes an
//! X11-only client render at all: it spawns the `Xwayland` binary, plays the
//! role of its window manager (`XwmHandler`), and pairs each `X11Surface`
//! with the `wl_surface` XWayland creates for it (`XWaylandShellHandler`) so
//! that surface can be mapped into [`crate::state::DuduclawComp::space`] and
//! composited by the SAME code path an ordinary xdg toplevel already uses
//! (`crate::decor::paint::build_output_elements`'s per-window loop, `Window::
//! send_frame`, `Space`'s own damage tracking — none of that is X11-aware,
//! and none of it needed to become so; see `decor/paint.rs`'s `toplevel_
//! elements` for the one place content rendering DID need an X11 branch).
//!
//! ## Why startup, not lazy
//! Task brief: "選 startup 直起（kiosk 情境無省資源需求且簡化狀態機）". A
//! lazy-spawn-on-first-X11-client design needs an extra state machine (spawn
//! pending / ready / failed) threaded through whatever first asks for it;
//! this appliance's kiosk session has no battery/resource-budget reason to
//! defer the ~few-MB idle cost of a not-yet-used `Xwayland` process, so
//! `spawn` below runs unconditionally from [`crate::state::DuduclawComp::new`],
//! the same place `codrive::init`/`shell_control::init` (this crate's other
//! "extra IPC/protocol surface" constructors) already run from.
//!
//! ## Two `XwmHandler`/`XWaylandShellHandler` impls, on purpose
//! `Dispatch<XwaylandShellV1, _, D> for XWaylandShellState` (smithay's own
//! impl, reached through [`delegate_xwayland_shell!`]) requires `D:
//! XWaylandShellHandler + XwmHandler` with `D` fixed to whatever type
//! `wayland_server::Display<D>` was created with — `DuduclawComp`
//! (`state.rs`). So `DuduclawComp` itself MUST implement both traits, with
//! the real logic.
//!
//! Separately, `X11Wm::start_wm::<D>` needs a `calloop::LoopHandle<'static,
//! D>`, and internally registers an `X11Source` whose calloop callback
//! receives `&mut D` — but THIS crate's event loop is `EventLoop<CalloopData>`
//! (see `main.rs`), not `EventLoop<DuduclawComp>`, because `CalloopData` also
//! carries the udev backend's optional state (A4-1) and the display handle.
//! `event_loop.handle()` therefore only ever yields `LoopHandle<'static,
//! CalloopData>` — a DIFFERENT type from `LoopHandle<'static, DuduclawComp>`,
//! and calloop cannot mix the two within one loop. So `CalloopData` must ALSO
//! implement both traits for `X11Wm::start_wm::<CalloopData>` to typecheck
//! against this crate's real loop handle. The `CalloopData` impls below are
//! pure forwarding to `self.state`'s real ones (mirrors the existing
//! `udev_backend::dispatch_render(data: &mut CalloopData)` "thin routing
//! wrapper" shape already used elsewhere in this crate) — no logic is
//! duplicated, and nothing here changes `CalloopData`'s existing private-field
//! visibility (both fields it touches, `state`/`display_handle`, are already
//! reached the same way by `udev_backend.rs`/`state.rs`).
//!
//! ## What "same treatment as an xdg toplevel" means here, and what it does not
//! Reached via `map_window_request`/`mapped_override_redirect_window`: an
//! X11 window becomes a `Window::new_x11_window(..)` mapped into
//! `self.space`, exactly like `handlers/xdg_shell.rs::new_toplevel` maps a
//! `Window::new_wayland_window(..)`. From there it is:
//! - **rendered** — `decor/paint.rs`'s per-window loop iterates `self.space.
//!   elements()` with no Wayland-only assumption once `toplevel_elements`'s
//!   X11 branch (this round) is in place;
//! - **focusable / raisable / closable-by-the-user's-own-app** — every path
//!   through `DuduclawComp::focus_window` (`state.rs`) now tolerates a
//!   `Window` whose `.toplevel()` is `None` (this round's other fix), and
//!   `Window::set_activated`/`Space::element_under`/`Window::surface_under`
//!   are already generic over both window kinds inside smithay itself — no
//!   crate code needed to change for click-to-focus or pointer hit-testing
//!   to reach an X11 window;
//! - **listed by name** — `codrive::window_target::window_identity` (this
//!   round) reads `X11Surface::class()`/`title()` the same way it reads an
//!   xdg toplevel's `app_id`/`title`, so `shell_control`'s dock and codrive's
//!   `activate_window` see X11 windows too.
//!
//! What it explicitly does NOT get, in this pass (documented rather than
//! silently missing):
//! - **No server-side decoration.** `decor::paint::window_uses_ssd` already
//!   returns `false` for any `Window` with no `toplevel()` — an X11 window
//!   was never routed through the xdg-decoration negotiation that flag
//!   guards, so this was true before this round too and needed no change.
//!   `configure_request` below never lets a fully undecorated content
//!   rectangle look like anything other than what it is.
//! - ~~No interactive move/resize~~ **CP-1/A4 follow-up (this round):
//!   interactive move/resize now work.** `move_request`/`resize_request`
//!   below arm the SAME [`crate::grabs::MoveSurfaceGrab`]/[`crate::grabs::
//!   ResizeSurfaceGrab`] a client-initiated `xdg_toplevel.move`/`.resize`
//!   already uses — `MoveSurfaceGrab` needed no change (it was already
//!   generic over `Window`, never assumed an xdg toplevel);
//!   `ResizeSurfaceGrab` was generalized to branch on `Window::
//!   underlying_surface()` (Wayland vs. X11) at every point that used to
//!   assume `.toplevel().unwrap()` — see that type's own doc in
//!   `grabs/resize_grab.rs`. `window_policy`'s WM-1/WM-2 reserved-band/
//!   cascade machinery remains xdg-toplevel-only by construction
//!   (`apply_window_policy`'s own early return on a `Window` with no
//!   `toplevel()`) — X11 windows still get ONE placement at map time, from
//!   [`DuduclawComp::x11_placement`] below, and are still not reflowed on
//!   later output-resize (`window_policy::reapply_window_policy_all` skips
//!   them the same way) — an interactively moved/resized X11 window IS
//!   remembered by `Space` itself (same as any other mapped element), just
//!   not by the floating-frame restore bookkeeping (`DecorState::frames`)
//!   xdg toplevels get.
//! - ~~No maximize/fullscreen/minimize~~ **maximize and fullscreen now
//!   work; minimize remains a known limitation.** `maximize_request`/
//!   `unmaximize_request` target [`DuduclawComp::layout_work_area`] — the
//!   same "the work area, not the whole output" rule
//!   `handlers/xdg_shell.rs::set_maximized` already applies to xdg
//!   toplevels. `fullscreen_request`/`unfullscreen_request` target the raw
//!   output (`DuduclawComp::layout_output_geometry`) — fullscreen means
//!   edge-to-edge, covering the shell's own chrome, by definition. Restore
//!   geometry is kept in `X11Surface::user_data()` (one slot, shared between
//!   the maximize and fullscreen paths — see [`X11RestoreGeometry`]'s own
//!   doc for the resulting, anvil-inherited edge case: maximize →
//!   fullscreen → unfullscreen → unmaximize does not recover the ORIGINAL
//!   floating frame). **`minimize_request`/`unminimize_request` are still
//!   left at the trait's default no-op bodies** — this crate's minimize
//!   machinery (`crate::minimize`, the `self.minimized: Vec<Window>` parking
//!   lot) is wired to the compositor's OWN title-bar minimize button
//!   (xdg-toplevel-only UI, see `decor::minus`) and was out of this round's
//!   scope; an X11 client's own minimize request (or its own minimize
//!   button, drawn client-side) is still silently ignored — logged rather
//!   than dropped without a trace would be the natural next step, not done
//!   this round since the trait's default body already IS a documented,
//!   intentional no-op rather than an oversight.
//! - **Clipboard/PRIMARY-selection forwarding now works, both directions.**
//!   `allow_selection_access` grants access when an X11 window belonging to
//!   this Xwayland instance holds keyboard focus on EITHER seat (human or
//!   agent); `send_selection`/`new_selection`/`cleared_selection` below
//!   complete the X11 → Wayland half, and `handlers/mod.rs`'s
//!   `SelectionHandler` impl completes the Wayland → X11 half. See this
//!   module's `impl XwmHandler for DuduclawComp` selection methods and
//!   `handlers/mod.rs`'s `SelectionHandler` doc for the full two-way wiring
//!   and why the two directions cannot feed back into each other.
//! - **No default X11 cursor image** (`X11Wm::set_cursor`, which anvil calls
//!   at startup) — an X11 app that sets its own cursor is unaffected; one
//!   that relies on the WM to supply a fallback sees whatever XWayland's own
//!   built-in default is instead of this compositor's brand cursor.
//! - **DISPLAY propagation to app-launch is a downstream integration point,
//!   not solved here.** This compositor does not spawn applications — that is
//!   `kiosk-launch`'s job, running as a child of the same session (same user,
//!   same `$XDG_RUNTIME_DIR`, so the `Xwayland` socket under
//!   `/tmp/.X11-unix` and the abstract socket are already reachable) — so
//!   the only thing this module owes that integration is an honest,
//!   greppable log line carrying the display number, which [`spawn`] emits
//!   the moment `XWaylandEvent::Ready` fires. Wiring `DISPLAY=:N` into
//!   whatever launches X11 apps is the next wave's job.

use std::{cell::RefCell, os::unix::io::OwnedFd, process::Stdio};

use smithay::{
    desktop::{space::RenderZindex, Window},
    input::{
        pointer::{Focus, GrabStartData as PointerGrabStartData},
        Seat,
    },
    reexports::{
        calloop::EventLoop,
        wayland_server::{DisplayHandle, Resource},
    },
    utils::{Logical, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
            SelectionTarget,
        },
        seat::WaylandFocus,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge, X11Window, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::{
    grabs::{MoveSurfaceGrab, ResizeSurfaceGrab},
    state::DuduclawComp,
    CalloopData,
};

/// Startup-only bookkeeping: the negotiated X11 window manager connection
/// (`None` until `XWaylandEvent::Ready` fires — spawning the `Xwayland`
/// process and this compositor negotiating the WM role over its X11 socket
/// both happen asynchronously off the calloop main loop) and the display
/// number, kept for logging / the DISPLAY-propagation integration point this
/// module's doc describes.
///
/// The `XWayland` handle itself (returned by [`XWayland::spawn`]) is
/// deliberately NOT stored here — `calloop::LoopHandle::insert_source` takes
/// ownership of it and keeps it alive inside calloop's own registry for as
/// long as it stays registered (dropping it is what its own doc says
/// shuts the instance down), so a second copy here would be redundant and
/// would raise the question of which one is authoritative.
#[derive(Default)]
pub struct XWaylandState {
    wm: Option<X11Wm>,
    /// Set once, from the `Ready` event; read back only for logging today.
    #[allow(dead_code)]
    display_number: Option<u32>,
}

/// Spawns `Xwayland` and arms the calloop source that starts the X11 window
/// manager once it reports readiness. Called once, from
/// [`DuduclawComp::new`], alongside `codrive::init`/`shell_control::init`.
///
/// **Failure resilience (task requirement):** a spawn failure (binary not on
/// `PATH`, sockets exhausted, …) is logged as a `warn!` and this function
/// simply returns — it never panics, and it never touches `dh`/`event_loop`
/// again on that path. Every other backend (winit/udev) and every other
/// protocol this compositor speaks is entirely unaffected: XWayland is one
/// more optional Wayland CLIENT as far as the rest of this crate is
/// concerned, not a dependency anything else here waits on. A headless/
/// degraded run (no `Xwayland` binary installed, e.g. a minimal appliance
/// image build) keeps running pure-Wayland exactly as it did before this
/// round.
pub fn spawn(dh: &DisplayHandle, event_loop: &mut EventLoop<'static, CalloopData>) {
    let (xwayland, client) = match XWayland::spawn(
        dh,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    ) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "xwayland: failed to spawn — running Wayland-only (X11-only clients, e.g. \
                 chromium --ozone-platform=x11, will not be able to connect); is the \
                 `Xwayland` binary on PATH?"
            );
            return;
        }
    };

    let loop_handle = event_loop.handle();
    let wm_loop_handle = loop_handle.clone();
    let insert_result = loop_handle.insert_source(xwayland, move |event, _, data: &mut CalloopData| {
        match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                match X11Wm::start_wm(wm_loop_handle.clone(), x11_socket, client.clone()) {
                    Ok(wm) => {
                        data.state.xwayland.wm = Some(wm);
                        data.state.xwayland.display_number = Some(display_number);
                        tracing::info!(
                            display_number,
                            display = format!(":{display_number}"),
                            "xwayland: ready — X11 window manager attached; DISPLAY propagation \
                             to app-launch is the kiosk-launch integration point (see \
                             crate::xwayland's module doc), not this compositor"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "xwayland: X11 window manager attach failed — X11 clients cannot be \
                             managed even though Xwayland itself started"
                        );
                    }
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("xwayland: Xwayland exited unexpectedly during startup");
            }
        }
    });
    if let Err(err) = insert_result {
        tracing::warn!(error = %err, "xwayland: failed to register the Xwayland event source");
    }
}

impl XWaylandShellHandler for DuduclawComp {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    // `surface_associated` is left at the trait's default no-op: the pairing
    // it announces is already reflected in `X11Surface::wl_surface()`/
    // `Window::wl_surface()` (`WaylandFocus`) by the time anything in this
    // crate next asks, so there is nothing extra to record.
}

impl XWaylandShellHandler for CalloopData {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        self.state.xwayland_shell_state()
    }

    fn surface_associated(
        &mut self,
        xwm: XwmId,
        wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        surface: X11Surface,
    ) {
        self.state.surface_associated(xwm, wl_surface, surface)
    }
}

smithay::delegate_xwayland_shell!(DuduclawComp);

impl XwmHandler for DuduclawComp {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwayland
            .wm
            .as_mut()
            .expect("XwmHandler callbacks only ever fire after X11Wm::start_wm succeeded and was stored")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!(
            window_id = ?window.window_id(),
            class = %window.class(),
            title = %window.title(),
            "xwayland: new X11 window (not mapped yet)"
        );
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!(
            window_id = ?window.window_id(),
            "xwayland: new override-redirect X11 window (not mapped yet)"
        );
    }

    /// "To grant the wish you have to call `X11Surface::set_mapped(true)` for
    /// the window to become visible" (`XwmHandler::map_window_request`'s own
    /// doc). This is the ONE placement an X11 toplevel gets in this pass —
    /// see [`DuduclawComp::x11_placement`] and this module's own doc for why
    /// there is no reflow/reserved-band/maximize machinery layered on top.
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(err) = window.set_mapped(true) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: failed to grant map request");
            return;
        }

        let rect = self.x11_placement(&window);
        let win = Window::new_x11_window(window.clone());
        // `false`: same "don't auto-steal focus on map" convention
        // `handlers/xdg_shell.rs::new_toplevel` already uses for a fresh xdg
        // toplevel — a newly mapped window becomes focusable, not
        // automatically focused. `map_element` on a never-before-mapped
        // element inserts it at the top of its z-index group regardless of
        // this flag (`state.rs::focus_window`'s own doc has the upstream
        // source reference for that), so this does not affect stacking.
        self.space.map_element(win.clone(), rect.loc, false);
        let bbox = self.space.element_bbox(&win).unwrap_or(rect);
        if let Err(err) = window.configure(Some(bbox)) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: post-map configure failed");
        }

        tracing::info!(
            window_id = ?window.window_id(),
            class = %window.class(),
            title = %window.title(),
            geometry = ?(bbox.loc.x, bbox.loc.y, bbox.size.w, bbox.size.h),
            "xwayland: X11 window mapped"
        );
        self.queue_redraw();
    }

    /// "It is best to replicate their state in smithay as faithfully as
    /// possible (e.g. positioning) and don't touch their state in any way"
    /// (`XwmHandler::new_override_redirect_window`'s own doc) — unlike
    /// `map_window_request` above, an override-redirect window (menus,
    /// tooltips) is not asking permission and gets no WM placement pass: its
    /// own X11-side geometry is authoritative, verbatim.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let loc = window.geometry().loc;
        let win = Window::new_x11_window(window.clone());
        // Stacked above ordinary toplevels, same as smithay's own
        // `impl SpaceElement for X11Surface::z_index` (which this crate's
        // `Window` wrapper does not inherit automatically — `Window::
        // new_x11_window` hardcodes `RenderZindex::Shell` for every X11
        // window regardless of override-redirect, see `desktop/wayland/
        // window.rs`) — a dropdown/tooltip rendered BELOW its parent window
        // would be worse than not decorating it at all.
        win.override_z_index(RenderZindex::Overlay as u8);
        self.space.map_element(win, loc, false);
        tracing::info!(
            window_id = ?window.window_id(),
            loc = ?(loc.x, loc.y),
            "xwayland: override-redirect window mapped"
        );
        self.queue_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let existing = self.x11_window_for(&window);
        if let Some(elem) = existing {
            self.space.unmap_elem(&elem);
            // Same close-time handoff `XdgShellHandler::toplevel_destroyed`
            // uses: if this window held either seat's keyboard focus, hand it
            // to the new topmost survivor rather than leaving a seat's focus
            // pointing at a now-unmapped surface.
            if let Some(surface) = elem.wl_surface() {
                self.reassign_focus_on_window_removed(&surface);
            }
        }
        // Per `XwmHandler::unmapped_window`'s own doc / anvil's reference
        // implementation: an override-redirect window's mapped state is
        // owned entirely by XWayland's own X11-side tracking, never told
        // back through `set_mapped`.
        if !window.is_override_redirect() {
            if let Err(err) = window.set_mapped(false) {
                tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: set_mapped(false) failed");
            }
        }
        tracing::info!(window_id = ?window.window_id(), "xwayland: X11 window unmapped");
        self.queue_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // A window can be destroyed without ever having been mapped (e.g. a
        // client that creates and immediately withdraws a window) — in that
        // case `unmapped_window` never ran and there is nothing in `space` to
        // remove; the common "unmap then destroy" order already cleaned up
        // in `unmapped_window` above.
        tracing::debug!(window_id = ?window.window_id(), "xwayland: X11 window destroyed");
    }

    /// "we just set the new size, but don't let windows move themselves
    /// around freely" — same rule anvil's reference `XwmHandler` impl uses,
    /// and the same rule this crate's own `window_policy` already applies to
    /// xdg toplevels (the compositor owns placement, not the client).
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let _ = (x, y);
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        if let Err(err) = window.configure(geo) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: configure_request failed");
        }
    }

    /// Reposition/resize notifications a client issued on its own — in
    /// practice almost always an override-redirect popup/tooltip
    /// repositioning itself; an ordinary WM-placed toplevel rarely reaches
    /// this for its own location. Mirrors `layer_shell`'s
    /// `LayerMap`-arranged-surfaces pattern of trusting the client's own
    /// reported geometry for anything not WM-owned.
    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
        let Some(elem) = self.x11_window_for(&window) else {
            return;
        };
        self.space.map_element(elem, geometry.loc, false);
        self.queue_redraw();
    }

    /// CP-1/A4 follow-up (this round): interactive X11 move. Arms the SAME
    /// [`MoveSurfaceGrab`] a client-initiated `xdg_toplevel.move` uses (see
    /// `handlers/xdg_shell.rs::move_request` — that type never assumed an
    /// xdg toplevel, so no change was needed to it, only to how it is
    /// constructed here).
    ///
    /// Unlike `xdg_toplevel.move` (which names its `wl_seat` explicitly),
    /// this X11 WM request carries no seat identity — see
    /// [`DuduclawComp::x11_grab_seat_and_start_data`] for why looking one up
    /// is necessary and how.
    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        let Some(win) = self.x11_window_for(&window) else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: move_request for an unmapped/unknown window — ignoring");
            return;
        };
        let Some((seat, start_data)) = self.x11_grab_seat_and_start_data(&window) else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: move_request with no active pointer grab on either seat — ignoring");
            return;
        };
        let Some(initial_window_location) = self.space.element_location(&win) else {
            return;
        };

        let grab = MoveSurfaceGrab {
            start_data,
            window: win,
            initial_window_location,
            // Same "unclamped — this is a CLIENT asking to be moved"
            // convention `handlers/xdg_shell.rs::move_request` documents for
            // `xdg_toplevel.move`.
            clamp: None,
        };

        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        tracing::info!(
            window_id = ?window.window_id(),
            ?initial_window_location,
            "xwayland: move_request — move grab armed"
        );
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }

    /// CP-1/A4 follow-up (this round): interactive X11 resize. Arms the SAME
    /// [`ResizeSurfaceGrab`] a client-initiated `xdg_toplevel.resize` uses —
    /// generalized (see `grabs/resize_grab.rs`'s own doc) to branch on
    /// `Window::underlying_surface()` at every point that used to assume an
    /// xdg toplevel. Edge mapping is [`crate::grabs::resize_grab::ResizeEdge`]'s
    /// `From<smithay::xwayland::xwm::ResizeEdge>` impl, verified against
    /// anvil's own table (`anvil/src/shell/grabs.rs`, MIT, `v0.7.0` tag).
    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, resize_edge: ResizeEdge) {
        let Some(win) = self.x11_window_for(&window) else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: resize_request for an unmapped/unknown window — ignoring");
            return;
        };
        let Some((seat, start_data)) = self.x11_grab_seat_and_start_data(&window) else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: resize_request with no active pointer grab on either seat — ignoring");
            return;
        };
        let Some(initial_window_location) = self.space.element_location(&win) else {
            return;
        };
        let initial_window_size = window.geometry().size;

        let grab = ResizeSurfaceGrab::start(
            start_data,
            win,
            resize_edge.into(),
            Rectangle::new(initial_window_location, initial_window_size),
            // Same "unclamped — this is a CLIENT asking to be resized"
            // convention `handlers/xdg_shell.rs::resize_request` documents.
            None,
        );

        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        tracing::info!(
            window_id = ?window.window_id(),
            ?initial_window_location,
            ?initial_window_size,
            "xwayland: resize_request — resize grab armed"
        );
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }

    /// CP-1/A4 follow-up (this round): "maximize" for an X11 window means
    /// the WORK AREA, not the raw output — the identical rule
    /// `handlers/xdg_shell.rs::set_maximized` applies to xdg toplevels
    /// ("WM-1: 'maximize' means the work area, not the whole output"). X11
    /// windows are never server-decorated in this crate (`window_insets`
    /// always answers [`crate::decor::DecorInsets::NONE`] for one — see this
    /// module's own doc), so frame == content and there is no inset to
    /// subtract; the work-area rule is still the reason to target
    /// [`DuduclawComp::layout_work_area`] rather than the raw output.
    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(win) = self.x11_window_for(&window) else {
            return;
        };
        let Some(work) = self.layout_work_area() else {
            // No real output yet: nothing to maximize against. A synthetic
            // configure still goes out so the client is not left waiting.
            if let Err(err) = window.configure(None) {
                tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: maximize_request synthetic configure failed");
            }
            return;
        };
        let old = self.space.element_bbox(&win).unwrap_or_else(|| window.geometry());
        window.user_data().insert_if_missing(X11RestoreGeometry::default);
        if let Some(restore) = window.user_data().get::<X11RestoreGeometry>() {
            restore.save(old);
        }

        if let Err(err) = window.set_maximized(true) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: set_maximized(true) failed");
        }
        if let Err(err) = window.configure(Some(work)) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: maximize configure failed");
        }
        self.space.map_element(win, work.loc, false);
        tracing::info!(
            window_id = ?window.window_id(),
            rect = ?(work.loc.x, work.loc.y, work.size.w, work.size.h),
            "xwayland: X11 window maximized (work area)"
        );
        self.queue_redraw();
    }

    /// CP-1/A4 follow-up: restores the frame [`Self::maximize_request`]
    /// saved, refitted into the CURRENT work area (`crate::decor::
    /// refit_frame`, the same helper `handlers/xdg_shell.rs::unmaximize_
    /// request` uses) in case the output changed size while this window was
    /// maximized. A window with no remembered frame (never floated before —
    /// e.g. mapped straight into a maximized state) is left exactly where it
    /// is, matching anvil's own reference behaviour.
    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(win) = self.x11_window_for(&window) else {
            return;
        };
        if let Err(err) = window.set_maximized(false) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: set_maximized(false) failed");
        }
        let Some(old) = window
            .user_data()
            .get::<X11RestoreGeometry>()
            .and_then(X11RestoreGeometry::take)
        else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: unmaximize_request with no remembered frame — leaving geometry untouched");
            self.queue_redraw();
            return;
        };
        let target = self
            .layout_work_area()
            .map(|work| crate::decor::refit_frame(old, work, crate::decor::DecorInsets::NONE))
            .unwrap_or(old);
        if let Err(err) = window.configure(Some(target)) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: unmaximize configure failed");
        }
        self.space.map_element(win, target.loc, false);
        tracing::info!(
            window_id = ?window.window_id(),
            rect = ?(target.loc.x, target.loc.y, target.size.w, target.size.h),
            "xwayland: X11 window unmaximized (restored)"
        );
        self.queue_redraw();
    }

    /// CP-1/A4 follow-up: fullscreen means the RAW output — edge-to-edge,
    /// covering the shell's own chrome — unlike maximize above. This is the
    /// first fullscreen implementation in this crate (xdg toplevels do not
    /// implement `fullscreen_request` at all yet), so there is no existing
    /// xdg convention to mirror for the target rectangle; edge-to-edge is
    /// the universal desktop convention and matches anvil's own
    /// `fullscreen_request`.
    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(win) = self.x11_window_for(&window) else {
            return;
        };
        let Some(output_geo) = self.layout_output_geometry() else {
            if let Err(err) = window.configure(None) {
                tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: fullscreen_request synthetic configure failed");
            }
            return;
        };
        let old = self.space.element_bbox(&win).unwrap_or_else(|| window.geometry());
        window.user_data().insert_if_missing(X11RestoreGeometry::default);
        if let Some(restore) = window.user_data().get::<X11RestoreGeometry>() {
            restore.save(old);
        }

        if let Err(err) = window.set_fullscreen(true) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: set_fullscreen(true) failed");
        }
        if let Err(err) = window.configure(Some(output_geo)) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: fullscreen configure failed");
        }
        self.space.map_element(win, output_geo.loc, false);
        tracing::info!(
            window_id = ?window.window_id(),
            rect = ?(output_geo.loc.x, output_geo.loc.y, output_geo.size.w, output_geo.size.h),
            "xwayland: X11 window fullscreened (whole output)"
        );
        self.queue_redraw();
    }

    /// CP-1/A4 follow-up: restores the frame [`Self::fullscreen_request`]
    /// saved, refitted into the CURRENT work area — an un-fullscreened
    /// window becomes an ordinary floating window again, which belongs
    /// inside the work area (not under the shell's own chrome), same target
    /// [`Self::unmaximize_request`] uses.
    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(win) = self.x11_window_for(&window) else {
            return;
        };
        if let Err(err) = window.set_fullscreen(false) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: set_fullscreen(false) failed");
        }
        let Some(old) = window
            .user_data()
            .get::<X11RestoreGeometry>()
            .and_then(X11RestoreGeometry::take)
        else {
            tracing::debug!(window_id = ?window.window_id(), "xwayland: unfullscreen_request with no remembered frame — leaving geometry untouched");
            self.queue_redraw();
            return;
        };
        let target = self
            .layout_work_area()
            .map(|work| crate::decor::refit_frame(old, work, crate::decor::DecorInsets::NONE))
            .unwrap_or(old);
        if let Err(err) = window.configure(Some(target)) {
            tracing::warn!(error = %err, window_id = ?window.window_id(), "xwayland: unfullscreen configure failed");
        }
        self.space.map_element(win, target.loc, false);
        tracing::info!(window_id = ?window.window_id(), "xwayland: X11 window unfullscreened (restored)");
        self.queue_redraw();
    }

    // minimize_request / unminimize_request remain at the trait's default
    // no-op bodies — documented known limitation, not an oversight; see
    // this module's own doc.

    /// CP-1 X11 clipboard follow-up: grants selection access (both
    /// CLIPBOARD and PRIMARY — `_selection` is deliberately unused, the rule
    /// is the same for either target) only while an X11 window belonging to
    /// THIS Xwayland instance holds keyboard focus, on EITHER seat this
    /// crate has (human or agent) — see [`DuduclawComp::x11_focused_seat`].
    /// Anvil's own reference checks only its one seat's keyboard focus; this
    /// crate has two.
    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        self.x11_focused_seat(xwm).is_some()
    }

    /// The given selection is being read by an X client — fetch the bytes
    /// FROM the real Wayland client that owns them (via `wl_data_device`/
    /// `zwp_primary_selection_device_v1`) and write them into `fd`. Gated by
    /// [`Self::allow_selection_access`] above (the trait's own doc: this
    /// panics if `allow_selection_access` ever returns `true` with no
    /// `send_selection` override — it is overridden here, so that can't
    /// happen).
    fn send_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let Some(seat) = self.x11_focused_seat(xwm) else {
            tracing::warn!(
                ?selection,
                "xwayland: send_selection with no X11-focused seat — dropping (allow_selection_access should have gated this)"
            );
            return;
        };
        // `data_device`'s and `primary_selection`'s own `SelectionRequestError`
        // types are two DISTINCT types (each module defines its own,
        // identically-named error) — handled per-arm rather than unified
        // into one `Result` binding, which is the only reason this isn't a
        // one-liner.
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&seat, mime_type, fd) {
                    tracing::warn!(error = %err, ?selection, "xwayland: failed to read the Wayland clipboard for Xwayland");
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&seat, mime_type, fd) {
                    tracing::warn!(error = %err, ?selection, "xwayland: failed to read the Wayland primary selection for Xwayland");
                }
            }
        }
    }

    /// An X11 client claimed a selection — mirror it onto BOTH Wayland
    /// seats (X11's clipboard/primary selection is a single, seat-agnostic
    /// piece of X server state; broadcasting to both is what lets a
    /// Wayland-native client on EITHER seat paste it, matching how
    /// [`Self::allow_selection_access`]/[`Self::send_selection`] above also
    /// consider both seats). See `handlers/mod.rs`'s `SelectionHandler` doc
    /// for why this direction cannot loop back into itself.
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        for seat in [self.seat.clone(), self.agent_seat.clone()] {
            match selection {
                SelectionTarget::Clipboard => {
                    set_data_device_selection(&self.display_handle, &seat, mime_types.clone(), ())
                }
                SelectionTarget::Primary => {
                    set_primary_selection(&self.display_handle, &seat, mime_types.clone(), ())
                }
            }
        }
        tracing::debug!(
            ?selection,
            mime_types_count = mime_types.len(),
            "xwayland: X11 claimed a selection — mirrored to both Wayland seats"
        );
    }

    /// The X11 selection [`Self::new_selection`] mirrored was cleared —
    /// clear the SAME compositor-owned copy on both seats. Guarded by
    /// `current_*_selection_userdata` (matching anvil's own reference): only
    /// clears a selection this compositor itself owns, never a genuine
    /// Wayland client's own currently-active one.
    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        for seat in [self.seat.clone(), self.agent_seat.clone()] {
            match selection {
                SelectionTarget::Clipboard => {
                    if current_data_device_selection_userdata(&seat).is_some() {
                        clear_data_device_selection(&self.display_handle, &seat);
                    }
                }
                SelectionTarget::Primary => {
                    if current_primary_selection_userdata(&seat).is_some() {
                        clear_primary_selection(&self.display_handle, &seat);
                    }
                }
            }
        }
    }

    // randr_primary_output_change / disconnected remain at the trait's
    // default bodies — out of this round's scope, not needed for move/
    // resize/maximize/fullscreen/clipboard.
}

impl XwmHandler for CalloopData {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        self.state.xwm_state(xwm)
    }
    fn new_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.new_window(xwm, window)
    }
    fn new_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.new_override_redirect_window(xwm, window)
    }
    fn map_window_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.map_window_request(xwm, window)
    }
    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.mapped_override_redirect_window(xwm, window)
    }
    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmapped_window(xwm, window)
    }
    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.destroyed_window(xwm, window)
    }
    fn configure_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        reorder: Option<Reorder>,
    ) {
        self.state.configure_request(xwm, window, x, y, w, h, reorder)
    }
    fn configure_notify(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<X11Window>,
    ) {
        self.state.configure_notify(xwm, window, geometry, above)
    }
    fn resize_request(&mut self, xwm: XwmId, window: X11Surface, button: u32, resize_edge: ResizeEdge) {
        self.state.resize_request(xwm, window, button, resize_edge)
    }
    fn move_request(&mut self, xwm: XwmId, window: X11Surface, button: u32) {
        self.state.move_request(xwm, window, button)
    }
    // CP-1/A4 follow-up (this round): `X11Wm::start_wm::<CalloopData>` (see
    // `spawn` above) means every `XwmHandler` callback is actually
    // DISPATCHED against `CalloopData`, not `DuduclawComp` directly — so
    // every method `DuduclawComp`'s own impl above now overrides MUST also
    // be forwarded here, or the trait's default (no-op) body on
    // `CalloopData` would silently run instead and the real implementation
    // above would be dead code, reachable only from direct unit-test calls.
    // This is exactly the same trap the module doc's "Two `XwmHandler`/
    // `XWaylandShellHandler` impls, on purpose" section warns about.
    fn maximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.maximize_request(xwm, window)
    }
    fn unmaximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmaximize_request(xwm, window)
    }
    fn fullscreen_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.fullscreen_request(xwm, window)
    }
    fn unfullscreen_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unfullscreen_request(xwm, window)
    }
    fn allow_selection_access(&mut self, xwm: XwmId, selection: SelectionTarget) -> bool {
        self.state.allow_selection_access(xwm, selection)
    }
    fn send_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        self.state.send_selection(xwm, selection, mime_type, fd)
    }
    fn new_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        self.state.new_selection(xwm, selection, mime_types)
    }
    fn cleared_selection(&mut self, xwm: XwmId, selection: SelectionTarget) {
        self.state.cleared_selection(xwm, selection)
    }
    // minimize_request / unminimize_request / randr_primary_output_change /
    // disconnected are left at the trait's own default body here too —
    // `DuduclawComp`'s impl above does not override them either, so there is
    // nothing to forward.
}

impl DuduclawComp {
    /// Where a newly-mapped, non-override-redirect X11 window should land.
    ///
    /// X11 has no reserved-band/decoration concept of its own —
    /// `window_policy`'s WM-1/WM-2 machinery is xdg-toplevel-only (see
    /// `apply_window_policy`'s own early return for a `Window` with no
    /// `toplevel()`) — so this reuses just the CASCADE step of that same
    /// machinery (`decor::cascade_frame_rect`, undecorated insets) against
    /// the live work area, sharing `DecorState::cascade_next` with floating
    /// xdg windows so an X11 app and a Wayland app opened back-to-back don't
    /// land exactly on top of each other. The SIZE is the client's own
    /// requested geometry when it offered one (clamped to fit the work area
    /// — a client-requested size is a preference, not a licence to cover the
    /// whole output), falling back to the same 80%-of-work-area default a
    /// brand new xdg toplevel with no remembered frame gets.
    fn x11_placement(&mut self, window: &X11Surface) -> Rectangle<i32, Logical> {
        let Some(work) = self.layout_work_area() else {
            // No real output yet: honour whatever the client itself asked
            // for rather than guessing against a geometry that doesn't exist.
            return window.geometry();
        };
        let index = self.decor.cascade_next;
        self.decor.cascade_next = self.decor.cascade_next.wrapping_add(1);
        let cascade = crate::decor::cascade_frame_rect(work, crate::decor::DecorInsets::NONE, index);

        let requested = window.geometry().size;
        let size = if requested.w > 0 && requested.h > 0 {
            Size::from((
                requested.w.min(work.size.w).max(1),
                requested.h.min(work.size.h).max(1),
            ))
        } else {
            cascade.size
        };
        Rectangle::new(cascade.loc, size)
    }

    /// The mapped `Window` wrapping `window`, or `None` if it is not
    /// (yet, or any longer) in `self.space`. The single lookup every
    /// `XwmHandler` method that needs "which `Window` is this `X11Surface`"
    /// now shares — `unmapped_window`/`configure_notify`/`move_request`/
    /// `resize_request`/`maximize_request`/`unmaximize_request`/
    /// `fullscreen_request`/`unfullscreen_request` all used to open-code
    /// this same `.find(...)` (or, before this round, didn't need it at
    /// all).
    fn x11_window_for(&self, window: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.x11_surface().is_some_and(|s| s == window))
            .cloned()
    }

    /// CP-1/A4 follow-up: which seat + pointer-grab start data should drive
    /// an interactive move/resize of `window`.
    ///
    /// Unlike `xdg_toplevel.move`/`.resize` (which name their `wl_seat`
    /// explicitly — see `handlers/xdg_shell.rs::move_request`'s
    /// `Seat::from_resource(&seat)`), `XwmHandler::move_request`/
    /// `resize_request` carry no seat identity at all, only a button code.
    /// The X11 client that sent `_NET_WM_MOVERESIZE` did so WHILE still
    /// holding the button that triggered it, on whichever seat's pointer was
    /// actually focused on it — human or agent, this compositor has two —
    /// so this asks each seat in turn whether IT currently has an active
    /// pointer grab (`PointerHandle::grab_start_data()` is only ever
    /// `Some` while at least one button is held: pressing a button installs
    /// smithay's own internal `ClickGrab` via its `DefaultGrab::button`
    /// handler — verified against smithay 0.7.0's own `input/pointer/
    /// grab.rs` — which is exactly the grab this reads back out).
    ///
    /// Anvil's own reference (`anvil/src/shell/x11.rs`) trusts a bare
    /// `self.pointer.grab_start_data().unwrap()` — safe for anvil because it
    /// has exactly one seat, so "a grab is active" and "it is the grab that
    /// led to this request" are the same fact. This crate has two, so that
    /// is not enough: if the OTHER seat happened to have an unrelated grab
    /// active on some unrelated window at the exact same moment, a bare
    /// `.is_some()` check could attribute this window's move/resize to the
    /// wrong seat. `same_client_as` narrows this to "the held grab's focus
    /// is a surface belonging to Xwayland's own client connection" — the
    /// same client-level granularity `handlers/xdg_shell.rs::check_grab`'s
    /// `focus.id().same_client_as(&surface.id())` already uses for the xdg
    /// path, and correct here because every X11 window shares that ONE
    /// client connection, and a `PointerHandle` can only ever hold one
    /// active grab per seat at a time.
    fn x11_grab_seat_and_start_data(&self, window: &X11Surface) -> Option<(Seat<Self>, PointerGrabStartData<Self>)> {
        let target_id = window.wl_surface()?.id();
        for seat in [self.seat.clone(), self.agent_seat.clone()] {
            let Some(pointer) = seat.get_pointer() else {
                continue;
            };
            let Some(start_data) = pointer.grab_start_data() else {
                continue;
            };
            let matches = start_data
                .focus
                .as_ref()
                .is_some_and(|(focus, _)| focus.id().same_client_as(&target_id));
            if matches {
                return Some((seat, start_data));
            }
        }
        None
    }

    /// CP-1 X11 clipboard follow-up: which seat (human or agent), if any,
    /// currently has its keyboard focused on an X11 window belonging to
    /// `xwm`. Shared by [`Self::allow_selection_access`]/[`Self::
    /// send_selection`] (via the `impl XwmHandler for DuduclawComp` block
    /// above) — selection access is granted, and read from, exactly the
    /// seat that is "looking at" an X11 window right now.
    fn x11_focused_seat(&self, xwm: XwmId) -> Option<Seat<Self>> {
        for seat in [self.seat.clone(), self.agent_seat.clone()] {
            let Some(keyboard) = seat.get_keyboard() else {
                continue;
            };
            let Some(focus) = keyboard.current_focus() else {
                continue;
            };
            let is_x11_focus = self.space.elements().any(|w| {
                w.x11_surface().is_some_and(|s| s.xwm_id() == Some(xwm)) && w.wl_surface().as_deref() == Some(&focus)
            });
            if is_x11_focus {
                return Some(seat);
            }
        }
        None
    }

    /// CP-1 X11 clipboard follow-up: the negotiated X11 window manager
    /// connection, if Xwayland has finished starting up — `None` before
    /// `XWaylandEvent::Ready` fires (or if it never does, e.g. no
    /// `Xwayland` binary on `PATH`). Used by `handlers/mod.rs`'s
    /// `SelectionHandler` impl, which lives in a different module and so
    /// cannot reach [`XWaylandState`]'s private `wm` field directly — this
    /// is the one accessor that lets it, mirroring the same "funnel access
    /// through `crate::xwayland`" shape [`Self::xwm_state`] (the
    /// `XwmHandler`-only, panics-if-called-too-early accessor) already
    /// established. Unlike that one, this returns `Option` rather than
    /// panicking: `SelectionHandler::new_selection`/`send_selection` can
    /// fire at ANY time a Wayland client touches the clipboard, including
    /// long before (or after a failed) Xwayland startup, and must fail open
    /// to "nothing to forward to" rather than panic the whole compositor.
    pub(crate) fn xwm_mut(&mut self) -> Option<&mut X11Wm> {
        self.xwayland.wm.as_mut()
    }
}

/// CP-1/A4 follow-up (this round): the frame an X11 window occupied just
/// before [`DuduclawComp::maximize_request`]/[`DuduclawComp::
/// fullscreen_request`] changed it, so the matching `unmaximize_request`/
/// `unfullscreen_request` can put it back.
///
/// Stored in [`X11Surface::user_data()`] rather than a `DecorState`-style
/// `ObjectId`-keyed map (the way xdg toplevels' restore geometry lives in
/// `DecorState::frames`) for two reasons: (a) `DecorState::frames` is keyed
/// by a Wayland `ObjectId`, which an X11 window is not guaranteed to have
/// paired yet at the exact moment a maximize/fullscreen request could
/// arrive, and (b) `user_data()` lives and dies with the `X11Surface`
/// itself, so — unlike `DecorState`'s maps, which need `forget_window_decor`
/// to evict entries by hand — there is no separate cleanup to add anywhere.
/// Same shape as anvil's own `OldGeometry` (`anvil/src/shell/x11.rs`, MIT,
/// verified against the `v0.7.0` tag).
///
/// **Known limitation, inherited from anvil's own reference implementation**:
/// one slot, shared between the maximize and fullscreen paths. A window that
/// goes maximized → fullscreened → unfullscreened → unmaximized does not
/// recover its ORIGINAL floating frame — the fullscreen request overwrites
/// the slot with the (already-maximized) frame it saw at that moment. Rare
/// in practice (most X11 apps use one state or the other, not both at once
/// on the same window) and not something anvil's own reference solves
/// either.
#[derive(Default)]
struct X11RestoreGeometry(RefCell<Option<Rectangle<i32, Logical>>>);

impl X11RestoreGeometry {
    fn save(&self, geo: Rectangle<i32, Logical>) {
        *self.0.borrow_mut() = Some(geo);
    }

    /// Takes (and clears) the remembered geometry — a restore is single-shot,
    /// the same "consume, don't peek" contract `grabs::resize_grab::
    /// ResizeSurfaceState::commit` already established for the xdg side.
    fn take(&self) -> Option<Rectangle<i32, Logical>> {
        self.0.borrow_mut().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_freshly_created_restore_slot_has_nothing_to_restore() {
        let slot = X11RestoreGeometry::default();
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn save_then_take_round_trips_exactly() {
        let slot = X11RestoreGeometry::default();
        let geo = rect(10, 20, 800, 600);
        slot.save(geo);
        assert_eq!(slot.take(), Some(geo));
    }

    #[test]
    fn take_is_single_shot_a_second_take_finds_nothing() {
        // The exact property `unmaximize_request`/`unfullscreen_request`
        // rely on: restoring twice in a row (e.g. a duplicate/racing
        // request) must not re-apply a stale geometry the second time.
        let slot = X11RestoreGeometry::default();
        slot.save(rect(0, 0, 100, 100));
        assert!(slot.take().is_some());
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn a_later_save_overwrites_an_earlier_unconsumed_one() {
        // The documented "one slot, shared between maximize and fullscreen"
        // limitation: saving twice before a take() only ever remembers the
        // MOST RECENT geometry, matching anvil's own reference behaviour.
        let slot = X11RestoreGeometry::default();
        slot.save(rect(0, 0, 100, 100));
        slot.save(rect(50, 50, 200, 200));
        assert_eq!(slot.take(), Some(rect(50, 50, 200, 200)));
    }

    #[test]
    fn every_x11_resize_edge_maps_onto_the_matching_bitflags_edge() {
        // Cross-check against `grabs::resize_grab`'s own, more detailed
        // table-driven tests for this conversion — this one just confirms
        // the specific 8 variants `XwmHandler::resize_request` can hand us
        // all convert to something, exercised from THIS module's call site
        // shape (`resize_edge.into()`).
        use crate::grabs::resize_grab::ResizeEdge as GrabResizeEdge;
        for edge in [
            ResizeEdge::Top,
            ResizeEdge::Bottom,
            ResizeEdge::Left,
            ResizeEdge::Right,
            ResizeEdge::TopLeft,
            ResizeEdge::TopRight,
            ResizeEdge::BottomLeft,
            ResizeEdge::BottomRight,
        ] {
            let _: GrabResizeEdge = edge.into();
        }
    }
}
