// Adapted from smithay's `smallvil` example (`smallvil/src/handlers/mod.rs`),
// MIT License. See `main.rs` for the full attribution note.

mod compositor;
mod xdg_shell;

use crate::DuduclawComp;

//
// Wl Seat
//

use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::{delegate_data_device, delegate_output, delegate_seat};

impl SeatHandler for DuduclawComp {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<DuduclawComp> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

delegate_seat!(DuduclawComp);

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
