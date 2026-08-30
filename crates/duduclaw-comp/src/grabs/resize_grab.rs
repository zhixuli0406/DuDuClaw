// Adapted from smithay's `smallvil` example
// (`smallvil/src/grabs/resize_grab.rs`), MIT License. See `main.rs` for the
// full attribution note.

use crate::{
    decor::{DecorInsets, MIN_RESIZE_H, MIN_RESIZE_W},
    DuduclawComp,
};
use smithay::{
    desktop::{Space, Window, WindowSurface},
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
        PointerInnerHandle, RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor, shell::xdg::SurfaceCachedState},
    xwayland::xwm::ResizeEdge as X11ResizeEdge,
};
use std::cell::RefCell;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ResizeEdge: u32 {
        const TOP          = 0b0001;
        const BOTTOM       = 0b0010;
        const LEFT         = 0b0100;
        const RIGHT        = 0b1000;

        const TOP_LEFT     = Self::TOP.bits() | Self::LEFT.bits();
        const BOTTOM_LEFT  = Self::BOTTOM.bits() | Self::LEFT.bits();

        const TOP_RIGHT    = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits(x as u32).unwrap()
    }
}

/// CP-1/A4 follow-up (interactive X11 move/resize, this round): the X11
/// window manager protocol's own edge enum — `XwmHandler::resize_request`'s
/// `resize_edge` parameter — mapped onto the same bitflags
/// `xdg_toplevel::ResizeEdge`/`decor::FrameEdge` already convert into.
/// Verified against anvil's own table (`anvil/src/shell/grabs.rs`,
/// `impl From<X11ResizeEdge> for ResizeEdge`, MIT, `v0.7.0` tag) rather than
/// assumed: X11's `ResizeEdge` has no numeric representation of its own to
/// derive a mapping FROM (unlike `xdg_toplevel::ResizeEdge`, which is a wire
/// enum with explicit discriminants this crate's other `From` impl re-uses
/// via `from_bits`), so a hand-written per-variant match is the only option
/// and anvil's table is the one hand already proven correct against a real
/// X11 client.
impl From<X11ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(edge: X11ResizeEdge) -> Self {
        match edge {
            X11ResizeEdge::Top => Self::TOP,
            X11ResizeEdge::Bottom => Self::BOTTOM,
            X11ResizeEdge::Left => Self::LEFT,
            X11ResizeEdge::Right => Self::RIGHT,
            X11ResizeEdge::TopLeft => Self::TOP_LEFT,
            X11ResizeEdge::TopRight => Self::TOP_RIGHT,
            X11ResizeEdge::BottomLeft => Self::BOTTOM_LEFT,
            X11ResizeEdge::BottomRight => Self::BOTTOM_RIGHT,
        }
    }
}

/// WM-3: the compositor's own edge hit test (`decor::edges`) speaks a plain
/// enum; the grab speaks these bitflags. One conversion, in one place, so the
/// two vocabularies cannot drift.
impl From<crate::decor::FrameEdge> for ResizeEdge {
    #[inline]
    fn from(edge: crate::decor::FrameEdge) -> Self {
        use crate::decor::FrameEdge as E;
        match edge {
            E::Top => Self::TOP,
            E::Bottom => Self::BOTTOM,
            E::Left => Self::LEFT,
            E::Right => Self::RIGHT,
            E::TopLeft => Self::TOP_LEFT,
            E::TopRight => Self::TOP_RIGHT,
            E::BottomLeft => Self::BOTTOM_LEFT,
            E::BottomRight => Self::BOTTOM_RIGHT,
        }
    }
}

/// WM-3: the bounds a **compositor-driven** edge resize is kept inside.
///
/// `None` on a grab means "client limits only", which is what a client-
/// initiated `xdg_toplevel.resize` still gets: a client asking to be resized
/// has its own reasons, and this compositor's floors are not its business. The
/// title-bar-adjacent edge drag (`decor::edges`) is the clamped one, for the
/// same reason `grabs::MoveClamp` exists — that path is the human dragging, and
/// a window resized until its title bar is off the work area cannot be dragged
/// back.
#[derive(Debug, Clone, Copy)]
pub struct ResizeClamp {
    /// The work area the resulting **frame** must stay inside.
    pub work: Rectangle<i32, Logical>,
    /// This window's decoration insets — how to convert between the frame the
    /// human drags and the content rectangle the client is configured with.
    pub insets: DecorInsets,
}

/// The width limit imposed by the work area for this drag, or `None` when the
/// drag does not move a horizontal edge.
fn width_limit(
    initial_content: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    clamp: ResizeClamp,
) -> Option<i32> {
    let work_left = clamp.work.loc.x;
    let work_right = clamp.work.loc.x + clamp.work.size.w;
    if edges.intersects(ResizeEdge::LEFT) {
        // The right edge is pinned; the frame's LEFT edge moves left as the
        // content grows: frame.left = (initial.right - w) - insets.left.
        Some(initial_content.loc.x + initial_content.size.w - work_left - clamp.insets.left)
    } else if edges.intersects(ResizeEdge::RIGHT) {
        // The left edge is pinned; the frame's RIGHT edge moves right.
        Some(work_right - initial_content.loc.x - clamp.insets.right)
    } else {
        None
    }
}

/// The height limit imposed by the work area for this drag. The `TOP` arm is
/// the one that keeps the **title bar** on screen, which is the requirement
/// this whole clamp exists for.
fn height_limit(
    initial_content: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    clamp: ResizeClamp,
) -> Option<i32> {
    let work_top = clamp.work.loc.y;
    let work_bottom = clamp.work.loc.y + clamp.work.size.h;
    if edges.intersects(ResizeEdge::TOP) {
        // frame.top = (initial.bottom - h) - insets.top  >=  work.top
        Some(initial_content.loc.y + initial_content.size.h - work_top - clamp.insets.top)
    } else if edges.intersects(ResizeEdge::BOTTOM) {
        // frame.bottom = content.top + h + insets.bottom  <=  work.bottom
        Some(work_bottom - initial_content.loc.y - clamp.insets.bottom)
    } else {
        None
    }
}

/// Pure half of the resize: the size a drag is actually allowed to produce.
///
/// Split out for the usual reason in this crate — a `PointerGrab` cannot be
/// exercised in a unit test without a real `Seat`, but the arithmetic that
/// decides how big a dragged window may get is exactly the part worth pinning
/// down (see `grabs::move_grab::clamped_move_target` for the same split).
///
/// With `clamp = None` this is **byte-identical** to the pre-WM-3 expression
/// (`proposed.max(client_min).min(client_max)`), which is what keeps
/// client-initiated `xdg_toplevel.resize` behaving exactly as it did.
///
/// With `clamp = Some(..)`, two further rules apply, in this order:
/// 1. the [`MIN_RESIZE_W`]/[`MIN_RESIZE_H`] floor is layered on top of the
///    client's own minimum (the larger wins — a client that declares a bigger
///    minimum is never overridden);
/// 2. the resulting **frame** may not leave the work area on the edge being
///    dragged, and the floor from (1) then wins over that cap. On a work area
///    too small to hold even a minimum window the result is a window that
///    overhangs, which is recoverable; a 1 px window is not.
pub fn clamp_resize_size(
    initial_content: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    proposed: Size<i32, Logical>,
    client_min: Size<i32, Logical>,
    client_max: Size<i32, Logical>,
    clamp: Option<ResizeClamp>,
) -> Size<i32, Logical> {
    let min_w = client_min.w.max(1);
    let min_h = client_min.h.max(1);
    let max_w = if client_max.w == 0 { i32::MAX } else { client_max.w };
    let max_h = if client_max.h == 0 { i32::MAX } else { client_max.h };

    let mut w = proposed.w.max(min_w).min(max_w);
    let mut h = proposed.h.max(min_h).min(max_h);

    if let Some(c) = clamp {
        let min_w = min_w.max(MIN_RESIZE_W);
        let min_h = min_h.max(MIN_RESIZE_H);
        w = w.max(min_w).min(max_w.max(min_w));
        h = h.max(min_h).min(max_h.max(min_h));
        if let Some(limit) = width_limit(initial_content, edges, c) {
            w = w.min(limit).max(min_w);
        }
        if let Some(limit) = height_limit(initial_content, edges, c) {
            h = h.min(limit).max(min_h);
        }
    }

    Size::from((w, h))
}

/// CP-1/A4 follow-up (interactive X11 resize, this round): `window` may now
/// be EITHER an xdg-toplevel-backed or an X11-backed [`Window`] — this whole
/// `impl` block branches on [`Window::underlying_surface`] at every point
/// that used to assume `.toplevel().unwrap()` was safe (a real A4 invariant,
/// documented here until this round changed it — see this type's own
/// `start`/`motion`/`button` for the two branches).
///
/// Two call sites construct this grab: `XdgShellHandler::resize_request`
/// (`handlers/xdg_shell.rs`, always xdg — a client can only ever ask
/// `xdg_toplevel.resize` for its OWN toplevel) and `input.rs::
/// begin_edge_resize` (the compositor's own edge-drag, reached through
/// `frame_hit_at`'s decoration hit-testing — keyed off `DecorState::frames`,
/// a map an X11 window is never entered into, since `window_uses_ssd`
/// returns `false` for one — so THAT call site is still xdg-only in
/// practice). The THIRD call site, `XwmHandler::resize_request`
/// (`crate::xwayland`, this round), is the only one that can ever hand this
/// type an X11-backed `Window`.
///
/// The X11 branch settles synchronously, entirely inside this grab (in
/// `button()`, on release) rather than reusing [`ResizeSurfaceState`]'s
/// wl_surface-commit-driven "wait for the client to ack, then correct the
/// TOP/LEFT anchor" dance: `X11Surface::configure()` applies immediately —
/// there is no separate ack/commit round trip for the WM side to wait on the
/// way `xdg_toplevel`'s `ack_configure` + `wl_surface.commit` requires — so
/// there is nothing async to defer the correction until. Matches anvil's own
/// `PointerResizeSurfaceGrab` X11 branch (`anvil/src/shell/grabs.rs`, MIT,
/// `v0.7.0` tag), which settles at the identical point for the identical
/// reason.
pub struct ResizeSurfaceGrab {
    start_data: PointerGrabStartData<DuduclawComp>,
    window: Window,

    edges: ResizeEdge,

    initial_rect: Rectangle<i32, Logical>,
    last_window_size: Size<i32, Logical>,
    /// WM-3: see [`ResizeClamp`]. `None` reproduces the pre-WM-3 behaviour
    /// byte for byte.
    clamp: Option<ResizeClamp>,
}

impl ResizeSurfaceGrab {
    pub fn start(
        start_data: PointerGrabStartData<DuduclawComp>,
        window: Window,
        edges: ResizeEdge,
        initial_window_rect: Rectangle<i32, Logical>,
        clamp: Option<ResizeClamp>,
    ) -> Self {
        let initial_rect = initial_window_rect;

        // xdg only: arms the commit-driven "wait for the client to ack, then
        // correct the TOP/LEFT anchor" state machine `handle_commit` below
        // consumes. An X11-backed `window` has no `toplevel()` (`None`) and
        // needs none of this — see this type's own doc for why its resize
        // settles synchronously in `button()` instead.
        if let Some(toplevel) = window.toplevel() {
            ResizeSurfaceState::with(toplevel.wl_surface(), |state| {
                *state = ResizeSurfaceState::Resizing { edges, initial_rect };
            });
        }

        Self {
            start_data,
            window,
            edges,
            initial_rect,
            last_window_size: initial_rect.size,
            clamp,
        }
    }
}

impl PointerGrab<DuduclawComp> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);

        let mut delta = event.location - self.start_data.location;

        let mut new_window_width = self.initial_rect.size.w;
        let mut new_window_height = self.initial_rect.size.h;

        if self.edges.intersects(ResizeEdge::LEFT | ResizeEdge::RIGHT) {
            if self.edges.intersects(ResizeEdge::LEFT) {
                delta.x = -delta.x;
            }

            new_window_width = (self.initial_rect.size.w as f64 + delta.x) as i32;
        }

        if self.edges.intersects(ResizeEdge::TOP | ResizeEdge::BOTTOM) {
            if self.edges.intersects(ResizeEdge::TOP) {
                delta.y = -delta.y;
            }

            new_window_height = (self.initial_rect.size.h as f64 + delta.y) as i32;
        }

        // CP-1/A4 follow-up: min/max size comes from wherever this window
        // kind actually declares it — the xdg-shell cached surface state for
        // a Wayland toplevel, or the ICCCM `WM_NORMAL_HINTS` min/max for an
        // X11 window (`X11Surface::min_size`/`max_size`, already in the same
        // `Size<i32, Logical>` unit — the surface itself converts from
        // client-scale pixels). `None` (no hint declared) maps onto `(0, 0)`,
        // the same "0 means unset" convention xdg-shell's own `SurfaceCachedState`
        // already uses here.
        let (min_size, max_size) = match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => compositor::with_states(toplevel.wl_surface(), |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let data = guard.current();
                (data.min_size, data.max_size)
            }),
            WindowSurface::X11(x11) => (
                x11.min_size().unwrap_or(Size::from((0, 0))),
                x11.max_size().unwrap_or(Size::from((0, 0))),
            ),
        };

        // WM-3: the client limits, this compositor's 320x240 floor and the
        // work-area cap now live in one pure, tested function. With
        // `self.clamp == None` (a client-initiated `xdg_toplevel.resize`) the
        // result is exactly what this expression produced before.
        self.last_window_size = clamp_resize_size(
            self.initial_rect,
            self.edges,
            Size::from((new_window_width, new_window_height)),
            min_size,
            max_size,
            self.clamp,
        );

        match self.window.underlying_surface() {
            WindowSurface::Wayland(xdg) => {
                xdg.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Resizing);
                    state.size = Some(self.last_window_size);
                });
                xdg.send_pending_configure();
            }
            WindowSurface::X11(x11) => {
                // No ack/commit round trip to wait for — live-resize the X11
                // window right now. The location is left as wherever it
                // currently sits; a TOP/LEFT drag's anchor correction is
                // deferred to `button()` on release (this type's own doc
                // explains why), matching anvil's own X11 resize `motion()`.
                let loc = data
                    .space
                    .element_location(&self.window)
                    .unwrap_or(self.initial_rect.loc);
                let _ = x11.configure(Rectangle::new(loc, self.last_window_size));
            }
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The button is a button code as defined in the
        // Linux kernel's linux/input-event-codes.h header file, e.g. BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;

        if !handle.current_pressed().contains(&BTN_LEFT) {
            // No more buttons are pressed, release the grab.
            handle.unset_grab(self, data, event.serial, event.time, true);

            match self.window.underlying_surface() {
                WindowSurface::Wayland(xdg) => {
                    xdg.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Resizing);
                        state.size = Some(self.last_window_size);
                    });

                    xdg.send_pending_configure();

                    ResizeSurfaceState::with(xdg.wl_surface(), |state| {
                        *state = ResizeSurfaceState::WaitingForLastCommit {
                            edges: self.edges,
                            initial_rect: self.initial_rect,
                        };
                    });
                }
                WindowSurface::X11(x11) => {
                    // CP-1/A4 follow-up: no ack/commit round trip for X11 —
                    // settle the size AND, for a TOP/LEFT drag, the
                    // anchor-correcting location right here, synchronously.
                    // Uses `self.last_window_size` (already the final size
                    // `motion()` computed) rather than re-reading
                    // `self.window.geometry()` — `X11Surface::configure`
                    // applies synchronously, so by this point the two are
                    // equal anyway, and this avoids depending on that timing
                    // fact holding. Mirrors anvil's own X11 resize-release
                    // path (`anvil/src/shell/grabs.rs`, MIT, `v0.7.0` tag).
                    let mut loc = data
                        .space
                        .element_location(&self.window)
                        .unwrap_or(self.initial_rect.loc);
                    if self.edges.intersects(ResizeEdge::TOP_LEFT) {
                        if self.edges.intersects(ResizeEdge::LEFT) {
                            loc.x = self.initial_rect.loc.x
                                + (self.initial_rect.size.w - self.last_window_size.w);
                        }
                        if self.edges.intersects(ResizeEdge::TOP) {
                            loc.y = self.initial_rect.loc.y
                                + (self.initial_rect.size.h - self.last_window_size.h);
                        }
                    }
                    if let Err(err) = x11.configure(Rectangle::new(loc, self.last_window_size)) {
                        tracing::warn!(error = %err, "grabs::resize: X11 resize-release configure failed");
                    }
                    data.space.map_element(self.window.clone(), loc, false);
                }
            }
        }
    }

    fn axis(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut DuduclawComp, handle: &mut PointerInnerHandle<'_, DuduclawComp>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut DuduclawComp,
        handle: &mut PointerInnerHandle<'_, DuduclawComp>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<DuduclawComp> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut DuduclawComp) {}
}

/// State of the resize operation.
///
/// It is stored inside of WlSurface,
/// and can be accessed using [`ResizeSurfaceState::with`]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
enum ResizeSurfaceState {
    #[default]
    Idle,
    Resizing {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
    /// Resize is done, we are now waiting for last commit, to do the final move
    WaitingForLastCommit {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
}

impl ResizeSurfaceState {
    fn with<F, T>(surface: &WlSurface, cb: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing(RefCell::<Self>::default);
            let state = states.data_map.get::<RefCell<Self>>().unwrap();

            cb(&mut state.borrow_mut())
        })
    }

    fn commit(&mut self) -> Option<(ResizeEdge, Rectangle<i32, Logical>)> {
        match *self {
            Self::Resizing { edges, initial_rect } => Some((edges, initial_rect)),
            Self::WaitingForLastCommit { edges, initial_rect } => {
                // The resize is done, let's go back to idle
                *self = Self::Idle;

                Some((edges, initial_rect))
            }
            Self::Idle => None,
        }
    }
}

/// Should be called on `WlSurface::commit`.
///
/// WM-3 changed the return type from `Option<()>` (which was `Some` for any
/// commit of a mapped window, and therefore told the caller nothing) to a
/// straight "did this commit move the window". A `TOP`/`LEFT` resize moves the
/// element's origin here, *after* `xdg_shell::handle_commit` has already synced
/// the remembered floating frame — so the caller needs to know to sync again,
/// and only then.
pub fn handle_commit(space: &mut Space<Window>, surface: &WlSurface) -> bool {
    handle_commit_inner(space, surface).unwrap_or(false)
}

fn handle_commit_inner(space: &mut Space<Window>, surface: &WlSurface) -> Option<bool> {
    // A4 (CP-1, XWayland): this runs on EVERY `WlSurface::commit`
    // (`handlers/compositor.rs::commit` calls it unconditionally after
    // `xdg_shell::handle_commit`) — including an X11 client's continuous
    // buffer commits once its window is mapped. `.is_some_and` short-
    // circuits `false` for a `Window` with no `toplevel()` instead of
    // panicking while scanning past it (this resize-tracking state is
    // xdg-toplevel-specific — `ResizeSurfaceState`/`xdg_toplevel.resize` has
    // no X11 equivalent reached through this path).
    let window = space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let new_loc: Point<Option<i32>, Logical> = ResizeSurfaceState::with(surface, |state| {
        state
            .commit()
            .and_then(|(edges, initial_rect)| {
                // If the window is being resized by top or left, its location must be adjusted
                // accordingly.
                edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                    let new_x = edges
                        .intersects(ResizeEdge::LEFT)
                        .then_some(initial_rect.loc.x + (initial_rect.size.w - geometry.size.w));

                    let new_y = edges
                        .intersects(ResizeEdge::TOP)
                        .then_some(initial_rect.loc.y + (initial_rect.size.h - geometry.size.h));

                    (new_x, new_y).into()
                })
            })
            .unwrap_or_default()
    });

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        // If TOP or LEFT side of the window got resized, we have to move it
        space.map_element(window, window_loc, false);
        return Some(true);
    }

    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Point;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        Size::from((w, h))
    }

    /// No client limits declared — xdg-shell's "0 means unset".
    fn no_limits() -> (Size<i32, Logical>, Size<i32, Logical>) {
        (size(0, 0), size(0, 0))
    }

    /// The appliance's work area, and a window floating well inside it.
    fn work() -> Rectangle<i32, Logical> {
        rect(0, 30, 1280, 680)
    }

    fn clamp() -> ResizeClamp {
        ResizeClamp {
            work: work(),
            insets: DecorInsets::SSD,
        }
    }

    #[test]
    fn every_x11_resize_edge_maps_onto_the_matching_resize_edge_bits() {
        // CP-1/A4 follow-up: the mapping table verified against anvil's own
        // `impl From<X11ResizeEdge> for ResizeEdge` (`anvil/src/shell/
        // grabs.rs`, MIT, `v0.7.0` tag) — see this file's `impl
        // From<X11ResizeEdge> for ResizeEdge` for the full citation.
        use X11ResizeEdge as E;
        assert_eq!(ResizeEdge::from(E::Top), ResizeEdge::TOP);
        assert_eq!(ResizeEdge::from(E::Bottom), ResizeEdge::BOTTOM);
        assert_eq!(ResizeEdge::from(E::Left), ResizeEdge::LEFT);
        assert_eq!(ResizeEdge::from(E::Right), ResizeEdge::RIGHT);
        // Same "corners must carry BOTH bits" requirement as the xdg/decor
        // conversions below — `handle_commit`'s xdg path AND this grab's own
        // X11 button()-release path both branch on `ResizeEdge::TOP_LEFT`,
        // and a single-bit corner would only correct one axis.
        for (x11_edge, expected) in [
            (E::TopLeft, ResizeEdge::TOP_LEFT),
            (E::TopRight, ResizeEdge::TOP_RIGHT),
            (E::BottomLeft, ResizeEdge::BOTTOM_LEFT),
            (E::BottomRight, ResizeEdge::BOTTOM_RIGHT),
        ] {
            let got = ResizeEdge::from(x11_edge);
            assert_eq!(got, expected, "{x11_edge:?}");
            assert_eq!(got.iter().count(), 2, "{x11_edge:?} must set two bits");
        }
    }

    #[test]
    fn every_frame_edge_maps_onto_the_matching_resize_edge_bits() {
        use crate::decor::FrameEdge as E;
        assert_eq!(ResizeEdge::from(E::Top), ResizeEdge::TOP);
        assert_eq!(ResizeEdge::from(E::Bottom), ResizeEdge::BOTTOM);
        assert_eq!(ResizeEdge::from(E::Left), ResizeEdge::LEFT);
        assert_eq!(ResizeEdge::from(E::Right), ResizeEdge::RIGHT);
        // The corners must carry BOTH bits, or `handle_commit`'s origin
        // compensation only fires on one axis and the window walks sideways.
        for (frame_edge, expected) in [
            (E::TopLeft, ResizeEdge::TOP_LEFT),
            (E::TopRight, ResizeEdge::TOP_RIGHT),
            (E::BottomLeft, ResizeEdge::BOTTOM_LEFT),
            (E::BottomRight, ResizeEdge::BOTTOM_RIGHT),
        ] {
            let got = ResizeEdge::from(frame_edge);
            assert_eq!(got, expected, "{}", frame_edge.as_str());
            assert_eq!(got.iter().count(), 2, "{} must set two bits", frame_edge.as_str());
        }
    }

    #[test]
    fn the_origin_moving_edges_agree_between_the_two_vocabularies() {
        use crate::decor::FrameEdge as E;
        for e in [E::Top, E::Bottom, E::Left, E::Right, E::TopLeft, E::TopRight, E::BottomLeft, E::BottomRight] {
            let bits = ResizeEdge::from(e);
            assert_eq!(bits.intersects(ResizeEdge::TOP), e.moves_top(), "{}", e.as_str());
            assert_eq!(bits.intersects(ResizeEdge::LEFT), e.moves_left(), "{}", e.as_str());
        }
    }

    #[test]
    fn an_unclamped_resize_is_exactly_the_pre_wm3_expression() {
        // Client-initiated `xdg_toplevel.resize` must not change behaviour.
        let (min, max) = no_limits();
        let got = clamp_resize_size(rect(0, 0, 100, 100), ResizeEdge::BOTTOM_RIGHT, size(7, 3), min, max, None);
        assert_eq!((got.w, got.h), (7, 3), "no floor is applied without a clamp");
        // And a declared client minimum still wins, as before.
        let got = clamp_resize_size(
            rect(0, 0, 100, 100),
            ResizeEdge::BOTTOM_RIGHT,
            size(7, 3),
            size(400, 300),
            size(0, 0),
            None,
        );
        assert_eq!((got.w, got.h), (400, 300));
    }

    #[test]
    fn a_clamped_resize_never_goes_below_the_320x240_floor() {
        let (min, max) = no_limits();
        let got = clamp_resize_size(
            rect(200, 200, 800, 600),
            ResizeEdge::BOTTOM_RIGHT,
            size(10, 10),
            min,
            max,
            Some(clamp()),
        );
        assert_eq!((got.w, got.h), (MIN_RESIZE_W, MIN_RESIZE_H));
    }

    #[test]
    fn a_client_declaring_a_bigger_minimum_is_never_overridden() {
        let got = clamp_resize_size(
            rect(200, 200, 800, 600),
            ResizeEdge::BOTTOM_RIGHT,
            size(10, 10),
            size(500, 400),
            size(0, 0),
            Some(clamp()),
        );
        assert_eq!((got.w, got.h), (500, 400));
    }

    #[test]
    fn a_client_maximum_still_caps_a_clamped_resize() {
        let got = clamp_resize_size(
            rect(200, 200, 400, 300),
            ResizeEdge::BOTTOM_RIGHT,
            size(9_000, 9_000),
            size(0, 0),
            size(700, 500),
            Some(clamp()),
        );
        assert_eq!((got.w, got.h), (700, 500));
    }

    #[test]
    fn dragging_the_top_edge_cannot_push_the_title_bar_off_the_work_area() {
        // The requirement this clamp exists for. Content starts at y=200 and is
        // 600 tall, so its bottom edge is at 800; the frame top must stay at or
        // below the work area's top (30), and the frame top is `insets.top`
        // above the content top.
        let (min, max) = no_limits();
        let initial = rect(200, 200, 800, 600);
        let got = clamp_resize_size(initial, ResizeEdge::TOP, size(800, 9_000), min, max, Some(clamp()));
        assert_eq!(got.h, 800 - 30 - DecorInsets::SSD.top);
        // Cross-check by replaying `handle_commit`'s own move formula: the
        // resulting FRAME top must land exactly on the work area's top edge.
        let new_content_top = initial.loc.y + initial.size.h - got.h;
        assert_eq!(new_content_top - DecorInsets::SSD.top, work().loc.y);
    }

    #[test]
    fn dragging_the_bottom_edge_cannot_push_the_frame_past_the_dock() {
        let (min, max) = no_limits();
        let initial = rect(200, 200, 800, 300);
        let got = clamp_resize_size(initial, ResizeEdge::BOTTOM, size(800, 9_000), min, max, Some(clamp()));
        // frame.bottom == content.top + h + insets.bottom == work.bottom
        assert_eq!(initial.loc.y + got.h + DecorInsets::SSD.bottom, work().loc.y + work().size.h);
    }

    #[test]
    fn dragging_the_left_and_right_edges_cannot_leave_the_work_area() {
        let (min, max) = no_limits();
        let initial = rect(200, 200, 800, 300);
        let left = clamp_resize_size(initial, ResizeEdge::LEFT, size(9_000, 300), min, max, Some(clamp()));
        let new_content_left = initial.loc.x + initial.size.w - left.w;
        assert_eq!(new_content_left - DecorInsets::SSD.left, work().loc.x);
        let right = clamp_resize_size(initial, ResizeEdge::RIGHT, size(9_000, 300), min, max, Some(clamp()));
        assert_eq!(
            initial.loc.x + right.w + DecorInsets::SSD.right,
            work().loc.x + work().size.w
        );
    }

    #[test]
    fn a_corner_drag_clamps_both_axes_at_once() {
        let (min, max) = no_limits();
        let initial = rect(200, 200, 400, 300);
        let got = clamp_resize_size(
            initial,
            ResizeEdge::TOP_LEFT,
            size(9_000, 9_000),
            min,
            max,
            Some(clamp()),
        );
        assert_eq!(got.w, initial.loc.x + initial.size.w - work().loc.x - DecorInsets::SSD.left);
        assert_eq!(got.h, initial.loc.y + initial.size.h - work().loc.y - DecorInsets::SSD.top);
    }

    #[test]
    fn a_drag_that_stays_inside_the_work_area_is_left_alone() {
        let (min, max) = no_limits();
        let got = clamp_resize_size(
            rect(200, 200, 800, 600),
            ResizeEdge::BOTTOM_RIGHT,
            size(900, 400),
            min,
            max,
            Some(clamp()),
        );
        assert_eq!((got.w, got.h), (900, 400));
    }

    #[test]
    fn the_minimum_wins_over_the_work_area_cap_on_a_pathological_work_area() {
        // A work area smaller than the floor: a window overhanging the dock is
        // recoverable, a 1px window is not.
        let tiny = ResizeClamp {
            work: rect(0, 0, 200, 150),
            insets: DecorInsets::SSD,
        };
        let (min, max) = no_limits();
        let got = clamp_resize_size(rect(0, 0, 100, 100), ResizeEdge::BOTTOM_RIGHT, size(10, 10), min, max, Some(tiny));
        assert_eq!((got.w, got.h), (MIN_RESIZE_W, MIN_RESIZE_H));
    }

    #[test]
    fn an_undecorated_window_clamps_against_the_frame_being_the_content() {
        let clamp = ResizeClamp {
            work: work(),
            insets: DecorInsets::NONE,
        };
        let (min, max) = no_limits();
        let initial = rect(200, 200, 800, 300);
        let got = clamp_resize_size(initial, ResizeEdge::TOP, size(800, 9_000), min, max, Some(clamp));
        assert_eq!(got.h, initial.loc.y + initial.size.h - work().loc.y);
    }
}
