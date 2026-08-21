// Adapted from smithay's `smallvil` example (`smallvil/src/state.rs`), MIT
// License. See `main.rs` for the full attribution note.

use std::{ffi::OsString, sync::Arc};

use smithay::{
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState},
    output::Output,
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, LoopSignal, Mode, PostAction},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Display, DisplayHandle,
        },
    },
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::{codrive, CalloopData};

/// Top-level compositor state. One instance per compositor process — this
/// is the `D` type parameter smithay's `delegate_*!` macros generate
/// `Dispatch` impls against, so every protocol handler in `handlers/`
/// borrows from this struct.
pub struct DuduclawComp {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    /// The mapped windows, in stacking order. `Space` is smithay's 2D
    /// arrangement primitive — windows and outputs both get positioned on
    /// it, and it does the geometry math for "what's under this point".
    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay protocol state — one struct per Wayland global/interface this
    // compositor advertises.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<DuduclawComp>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,

    /// The real human seat — every hardware/winit-forwarded input event
    /// goes here (see `input.rs`).
    pub seat: Seat<Self>,

    /// CD-0 codrive spike (DESIGN-codrive-desktop-2026-08.md §3.3.1): the
    /// agent-only `wl_seat`. Injected commands from `codrive::listener` are
    /// applied through this seat's keyboard/pointer handles, never through
    /// `seat` above — that separation is what makes per-seat event
    /// attribution (audit, freeze) trivial instead of needing a tag on
    /// every event.
    pub agent_seat: Seat<Self>,
    /// Cross-thread freeze/terminated/audit state shared with the
    /// injection-socket thread. See `codrive/mod.rs` module doc.
    pub codrive: std::sync::Arc<codrive::CodriveShared>,
    /// Set the instant the agent seat most recently transitioned
    /// not-frozen → frozen; used to log freeze-to-next-command latency in
    /// `codrive::handle_agent_inject`. `None` until the first human input
    /// of this process's lifetime.
    pub codrive_freeze_set_at: Option<std::time::Instant>,
    /// CD-1 (DESIGN §3.3.2(b) target highlight box, task brief req 5): the
    /// currently active highlight rectangle and its expiry instant, or
    /// `None` if no highlight is active. Set by `codrive::handle_agent_
    /// inject`'s `Highlight` arm; read (and cleared on expiry) by
    /// `codrive::highlight::DuduclawComp::codrive_highlight_elements`,
    /// called once per redraw from `winit_backend.rs`.
    pub codrive_highlight: Option<(Rectangle<f64, Logical>, std::time::Instant)>,
    /// CD-2 shadow workspace (WP-CD2-shadow, DESIGN §3.3.4): the headless
    /// second `Output`, mapped into `space` at `codrive::SHADOW_ORIGIN`.
    /// Never bound to any real display backend — `winit_backend.rs` only
    /// ever renders it offscreen (`DuduclawComp::codrive_render_pip`,
    /// `codrive/shadow.rs`) for the PiP preview, never directly to the host
    /// window. Created here (not in `winit_backend.rs`, unlike the main
    /// "winit" output) because it needs no real backend/window-size
    /// dependency — see `codrive::create_shadow_output`.
    pub shadow_output: Output,
    /// True while a shadow-workspace session is active for this agent seat
    /// (set by `{"op":"shadow","enable":true}` — `codrive::shadow`). Plain
    /// `bool`, not an atomic: only ever touched on this calloop main
    /// thread, same as `codrive_highlight`/`codrive_freeze_set_at` above —
    /// the `{"op":"shadow",…}` command reaches the seat-owning main thread
    /// via the same `InjectCmd` channel every other seat-touching op uses
    /// (see `codrive::handle_agent_inject`'s `Shadow` arm), unlike
    /// `status`/`resume`/`rotate_token`, which the socket thread answers
    /// synchronously and never need main-thread state at all.
    pub codrive_shadow_active: bool,
    /// CD-2 VM round bugfix: true if the human keyboard's Logo (Super)
    /// modifier was held during the *previous* keyboard event on the human
    /// seat. See `input.rs::is_system_gesture_tail`'s doc comment for why
    /// this exists — real-hardware verification found that the trailing
    /// key-release events of a physical Super+Enter/Super+Esc chord (which
    /// themselves count as "human touched input") immediately re-froze the
    /// seat the same chord had just un-frozen, making Super+Enter unable to
    /// durably hand control back on real hardware. Plain `bool`, not an
    /// atomic — only ever touched on this calloop main thread, from the
    /// human ("winit") seat's own keyboard arm.
    pub codrive_logo_held_prev: bool,
}

impl DuduclawComp {
    pub fn new(event_loop: &mut EventLoop<CalloopData>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();

        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let popups = PopupManager::default();

        // A seat is a group of keyboard/pointer/touch devices. This spike
        // assumes a single always-present keyboard+pointer (real hotplug
        // tracking is out of scope for a winit-nested spike — there's no
        // real hardware to plug into).
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();

        // CD-0 codrive spike: agent seat + injection socket + audit log.
        // Must happen while we still hold `&mut seat_state` locally (before
        // it moves into `Self` below) and while `event_loop` is available
        // to register the injection channel's calloop source.
        let (agent_seat, codrive) = codrive::init(&mut seat_state, &dh, event_loop);

        let mut space = Space::default();

        // CD-2 shadow workspace (WP-CD2-shadow): the headless second
        // output is created here (needs only `&DisplayHandle`, no real
        // backend/window-size dependency, unlike the "winit" output that
        // `winit_backend::init_winit` creates once `backend.window_size()`
        // is available) and mapped into the SAME space the main output
        // will later join — see `codrive::SHADOW_ORIGIN`'s doc for why
        // that location gives the two outputs structural isolation.
        let shadow_output = codrive::create_shadow_output(&dh);
        space.map_output(&shadow_output, codrive::SHADOW_ORIGIN);

        let socket_name = Self::init_wayland_listener(display, event_loop);
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            socket_name,

            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups,
            seat,

            agent_seat,
            codrive,
            codrive_freeze_set_at: None,
            codrive_highlight: None,
            shadow_output,
            codrive_shadow_active: false,
            codrive_logo_held_prev: false,
        }
    }

    fn init_wayland_listener(
        display: Display<DuduclawComp>,
        event_loop: &mut EventLoop<CalloopData>,
    ) -> OsString {
        // Auto-picks the next free `wayland-N` socket name under
        // $XDG_RUNTIME_DIR; clients connect to this via $WAYLAND_DISPLAY.
        let listening_socket = ListeningSocketSource::new_auto().unwrap();
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        // The display itself also needs to be pumped by the event loop so
        // client requests actually get dispatched.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display.
                    unsafe {
                        display.get_mut().dispatch_clients(&mut state.state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + location).to_f64()))
        })
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        // Live-run evidence (Shell-S0 nested headless round, 2026-08-19/20):
        // proves a real wl client reached this compositor's socket, not just
        // that the socket exists. See BUILD.md "Nested headless live-run".
        tracing::info!(?client_id, "xdg client connected");
    }
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::info!(?client_id, ?reason, "xdg client disconnected");
    }
}
