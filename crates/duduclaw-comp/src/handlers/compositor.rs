// Adapted from smithay's `smallvil` example
// (`smallvil/src/handlers/compositor.rs`), MIT License. See `main.rs` for
// the full attribution note.

use crate::{grabs::resize_grab, state::ClientState, DuduclawComp};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_surface::WlSurface},
        Client,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, CompositorClientState, CompositorHandler, CompositorState,
        },
        shm::{ShmHandler, ShmState},
    },
};

use super::xdg_shell;

impl CompositorHandler for DuduclawComp {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // A4 live-fire fix (CP-1, 2026-08-30): the spawned XWayland server
        // ITSELF connects to this compositor as an ordinary Wayland client,
        // but smithay's `XWayland::spawn` inserts ITS client data as
        // `XWaylandClientData` — never this crate's `ClientState`. The old
        // bare `unwrap()` therefore panicked the whole compositor ~15s
        // after boot, the instant Xwayland's connection issued its first
        // commit (live QEMU journal signature: "panicked at
        // src/handlers/compositor.rs:30", duduclaw-kiosk NRestarts=6
        // crash-loop). Two-branch shape is smithay's own anvil reference
        // (`anvil/src/handlers/compositor.rs`), not an invention.
        if let Some(data) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &data.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // A4-1: the single most important damage source — every pixel a
        // client ever changes arrives through here. See
        // `DuduclawComp::queue_redraw`.
        self.queue_redraw();
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            // A4 (CP-1, XWayland): fires on every commit, including an X11
            // client's continuous buffer commits once its window is mapped.
            // `self.toplevel_window_for` (`window_policy.rs`) only ever
            // matches an xdg toplevel by design, so `window.on_commit()`
            // below (which refreshes `Window`'s Wayland-only bbox cache — an
            // X11 window's `bbox()` reads from `X11Surface` directly instead,
            // see `desktop/wayland/window.rs`) is never reached for one.
            if let Some(window) = self.toplevel_window_for(&root) {
                window.on_commit();
            }
        };

        // WM-3: a layer surface is never also a toplevel, so this short-circuits
        // rather than running both paths. It owns its own arrangement +
        // initial-configure sequence (`LayerMap::arrange` deliberately refuses
        // to send the initial configure itself — see that function).
        if crate::layer_shell::handle_commit(self, surface) {
            return;
        }

        // WM-1: takes the whole state now (was `&mut popups, &space`) — the
        // initial-configure branch applies the window layout policy, which
        // moves the element and reads the session-shell identity.
        xdg_shell::handle_commit(self, surface);
        if resize_grab::handle_commit(&mut self.space, surface) {
            // WM-3: a TOP/LEFT resize moved the element's origin, and it did so
            // AFTER `xdg_shell::handle_commit` already ran its own
            // `decor_sync_frame`. Without this second sync the remembered
            // floating frame keeps the pre-resize rectangle, so an output-mode
            // change would snap the window back to where it was before the drag.
            if let Some(window) = self.toplevel_window_for(surface) {
                self.decor_sync_frame(&window);
            }
        }
    }
}

impl BufferHandler for DuduclawComp {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for DuduclawComp {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(DuduclawComp);
delegate_shm!(DuduclawComp);
