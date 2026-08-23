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
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{delegate_cursor_shape, delegate_data_device, delegate_output};

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
        set_data_device_focus(dh, seat, client);
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

impl SelectionHandler for DuduclawComp {
    type SelectionUserData = ();
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
// Wl Output & Xdg Output
//

impl OutputHandler for DuduclawComp {}
delegate_output!(DuduclawComp);
