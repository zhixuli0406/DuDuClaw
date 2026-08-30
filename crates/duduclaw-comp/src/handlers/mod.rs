// Adapted from smithay's `smallvil` example (`smallvil/src/handlers/mod.rs`),
// MIT License. See `main.rs` for the full attribution note.

mod compositor;
mod xdg_shell;

use crate::DuduclawComp;

//
// Wl Seat
//

use smithay::input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{delegate_cursor_shape, delegate_data_device, delegate_output, delegate_primary_selection};
use std::os::unix::io::OwnedFd;

impl SeatHandler for DuduclawComp {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<DuduclawComp> {
        &mut self.seat_state
    }

    /// CUR-1 (2026-08-22). This was an empty function until now, which meant
    /// every `wl_pointer.set_cursor` request a client ever sent was silently
    /// discarded: no I-beam over a text field, no hand over a link, no resize
    /// arrows on a window edge — every application got the identical
    /// compositor-drawn shape no matter what it asked for.
    ///
    /// Two protocols funnel in here, and both matter:
    /// * legacy `wl_pointer.set_cursor` → `CursorImageStatus::Surface`
    ///   (`wayland/seat/pointer.rs`), the path `duduclaw-shell`'s gpui
    ///   backend takes when no `cursor-shape-v1` global is advertised;
    /// * `wp_cursor_shape_device_v1.set_shape` →
    ///   `CursorImageStatus::Named` (`wayland/cursor_shape.rs`), which gpui
    ///   prefers when the global IS advertised, and which `state.rs` now
    ///   advertises.
    ///
    /// The AGENT seat is deliberately excluded. Its pointer is a
    /// compositor-owned amber cross whose whole job is to be visually
    /// unmistakable (DESIGN-codrive-desktop-2026-08.md §3.3.2, "與人游標明確
    /// 異形異色"; it also turns dark red while frozen). Letting a focused
    /// application restyle it — an application the agent may itself be
    /// driving — would hand away exactly the signal a human relies on to see
    /// that something else is at the controls.
    fn cursor_image(&mut self, seat: &Seat<Self>, image: CursorImageStatus) {
        if seat == &self.agent_seat {
            return;
        }
        self.set_human_cursor_image(image);
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        // CP-1 X11 clipboard follow-up: the ordinary clipboard's focus
        // tracking (above) has always run here; PRIMARY selection needs the
        // identical "which client is now focused on this seat" update or a
        // middle-click paste target would never learn about a focus change
        // that happened after `primary_selection_state`'s global was added.
        set_primary_focus(dh, seat, client);
    }
}

// NOTE (D3-c, 2026-08-23): `delegate_seat!(DuduclawComp)` used to be here.
// It is now written out by hand in `crate::ime::seat_filter` — the four
// `Dispatch` halves delegated verbatim, the `GlobalDispatch` half hand-rolled
// so `can_view` can hide the agent seat from input-method clients. `bind`
// still forwards to smithay's own impl, so binding behaviour is unchanged.
// See that module's doc for why an input method must not see the agent seat.

//
// Cursor shape v1 (CUR-1)
//

/// Required by `delegate_cursor_shape!` — the protocol also covers tablet
/// tools, so smithay's dispatch bounds ask for this trait. Every method has a
/// default; this compositor advertises no tablet globals
/// (`wayland::tablet_manager` is never initialised), so no tablet request can
/// reach it and there is nothing to override.
impl TabletSeatHandler for DuduclawComp {}

// Advertising `wp_cursor_shape_manager_v1` is what lets the compositor own
// cursor artwork for every modern client at once: they name a shape, we
// resolve it against ONE theme. Without it each client loads its own theme
// (`foot`, chromium and GTK apps each with their own loader and their own
// idea of `XCURSOR_THEME`), which is both inconsistent on screen and, for the
// brand-cursor seam in `crate::cursor::source`, impossible to override
// centrally.
delegate_cursor_shape!(DuduclawComp);

//
// Wl Data Device
//

/// CP-1 X11 clipboard follow-up (this round): the two-way bridge between
/// this compositor's OWN clipboard/primary-selection state (this trait) and
/// Xwayland's X11-side selection ownership (`crate::xwayland`'s `XwmHandler`
/// selection methods — `allow_selection_access`/`send_selection`/
/// `new_selection`/`cleared_selection`). The two directions are:
///
/// * **Wayland → X11**: a genuine Wayland client claims the clipboard or
///   primary selection → [`Self::new_selection`] below fires → forwarded to
///   `X11Wm::new_selection`, which makes Xwayland claim the matching X11
///   selection on the Wayland side's behalf.
/// * **X11 → Wayland**: `crate::xwayland`'s `XwmHandler::new_selection`
///   (X11 claimed a selection) calls `set_data_device_selection`/
///   `set_primary_selection`, which makes THIS compositor the announced
///   Wayland-side owner. When some Wayland client then tries to read it,
///   [`Self::send_selection`] below fires — and reads the actual bytes back
///   FROM Xwayland via `X11Wm::send_selection` (the "X11 → Wayland" fetch;
///   see [`crate::state::DuduclawComp::loop_handle`]'s own doc for why a
///   loop handle is needed here).
///
/// Neither direction can feed back into itself: `set_data_device_selection`/
/// `set_primary_selection` (the X11 → Wayland setter, called from
/// `crate::xwayland`) does NOT re-invoke [`Self::new_selection`] — verified
/// against smithay 0.7.0's own `wayland::selection::seat_data::SeatData::
/// set_clipboard_selection`, which updates internal state and offers to the
/// focused client directly, never through this trait's callback — so there
/// is no X11 → Wayland → X11 → … loop.
impl SelectionHandler for DuduclawComp {
    type SelectionUserData = ();

    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        let mime_types = source.map(|s| s.mime_types());
        let Some(wm) = self.xwm_mut() else {
            // Xwayland not spawned, or not yet attached as a WM — nothing to
            // forward to. Fail-open to Wayland-only, same posture as every
            // other `crate::xwayland` integration point.
            return;
        };
        if let Err(err) = wm.new_selection(ty, mime_types) {
            tracing::warn!(error = %err, ?ty, "xwayland: failed to forward a Wayland selection to Xwayland");
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        // Cloned before the `xwm_mut()` borrow starts: `LoopHandle` is a
        // cheap handle clone (see the field's own doc), and borrowing
        // `self.loop_handle` and `self.xwayland` at once through two
        // separate field accesses would otherwise need splitting anyway.
        let loop_handle = self.loop_handle.clone();
        let Some(wm) = self.xwm_mut() else {
            return;
        };
        if let Err(err) = wm.send_selection(ty, mime_type, fd, loop_handle) {
            tracing::warn!(error = %err, ?ty, "xwayland: failed to read Xwayland's selection for a Wayland client");
        }
    }
}

impl DataDeviceHandler for DuduclawComp {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for DuduclawComp {}
impl ServerDndGrabHandler for DuduclawComp {}

delegate_data_device!(DuduclawComp);

//
// Wl Primary Selection (CP-1 X11 clipboard follow-up)
//

impl PrimarySelectionHandler for DuduclawComp {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

delegate_primary_selection!(DuduclawComp);

//
// Wl Output & Xdg Output
//

impl OutputHandler for DuduclawComp {}
delegate_output!(DuduclawComp);
