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
//! - **No interactive move/resize** (`resize_request`/`move_request` below
//!   are logged, not wired to a grab) and **no maximize/fullscreen/minimize**
//!   (the trait's default no-op bodies are kept as-is). `window_policy`'s
//!   WM-1/WM-2 reserved-band/cascade/maximize machinery is xdg-toplevel-only
//!   by construction (`apply_window_policy`'s own early return on a `Window`
//!   with no `toplevel()`, this round) — X11 windows get ONE placement,
//!   at map time, from [`DuduclawComp::x11_placement`] below, and are not
//!   reflowed on later output-resize (`window_policy::reapply_window_policy_
//!   all` skips them the same way).
//! - **No clipboard/selection forwarding** — `allow_selection_access` is left
//!   at the trait's default (`false`), so `send_selection`/`new_selection`/
//!   `cleared_selection` are never reached.
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

use std::process::Stdio;

use smithay::{
    desktop::{space::RenderZindex, Window},
    reexports::{
        calloop::EventLoop,
        wayland_server::DisplayHandle,
    },
    utils::{Logical, Rectangle, Size},
    wayland::{
        seat::WaylandFocus,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge, X11Window, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::{state::DuduclawComp, CalloopData};

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
        let existing = self
            .space
            .elements()
            .find(|w| w.x11_surface().is_some_and(|s| s == &window))
            .cloned();
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
        let Some(elem) = self
            .space
            .elements()
            .find(|w| w.x11_surface().is_some_and(|s| s == &window))
            .cloned()
        else {
            return;
        };
        self.space.map_element(elem, geometry.loc, false);
        self.queue_redraw();
    }

    /// Known limitation (this module's own doc): no interactive X11 move in
    /// this pass. Logged rather than silently dropped, so a live-run trace
    /// shows the request was seen and deliberately not actioned rather than
    /// looking like the request never arrived.
    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        tracing::debug!(
            window_id = ?window.window_id(),
            "xwayland: move_request — interactive X11 move is not implemented in this pass, ignoring"
        );
    }

    /// Known limitation: see [`Self::move_request`] — same reasoning, resize.
    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, _resize_edge: ResizeEdge) {
        tracing::debug!(
            window_id = ?window.window_id(),
            "xwayland: resize_request — interactive X11 resize is not implemented in this pass, ignoring"
        );
    }

    // maximize_request / unmaximize_request / fullscreen_request /
    // unfullscreen_request / minimize_request / unminimize_request /
    // allow_selection_access / send_selection / new_selection /
    // cleared_selection / randr_primary_output_change / disconnected are all
    // left at the trait's default bodies — every one of them is a documented
    // known limitation in this module's own doc, not an oversight.
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
    // Every OTHER `XwmHandler` method (maximize/fullscreen/minimize/
    // selection/randr/disconnected) is left at the trait's own default body
    // here too — `DuduclawComp`'s impl above does not override them either,
    // so there is nothing to forward.
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
}
