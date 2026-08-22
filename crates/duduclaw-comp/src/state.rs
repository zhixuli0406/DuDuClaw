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
            Display, DisplayHandle, Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::{codrive, cursor, seat_order::SeatAdvertiseOrder, shell_control, CalloopData};

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
    /// CUR-1: the `wp_cursor_shape_manager_v1` global. Held only to keep the
    /// global alive for the process lifetime — every request it receives is
    /// dispatched by smithay straight into `SeatHandler::cursor_image`
    /// (`wayland/cursor_shape.rs`), so nothing in this crate reads the field.
    /// See `handlers/mod.rs` for why this protocol is worth advertising.
    pub cursor_shape_manager_state: CursorShapeManagerState,
    pub popups: PopupManager,

    /// The real human seat — every hardware/winit-forwarded input event
    /// goes here (see `input.rs`).
    pub seat: Seat<Self>,

    /// CUR-1: the human pointer's image — what clients asked for, the loaded
    /// XCursor theme, and the asset-free fallback. See `crate::cursor`'s
    /// module doc. The AGENT pointer is deliberately not represented here;
    /// it stays a compositor-owned amber cross (`codrive/cursor.rs`).
    pub cursor: cursor::CursorState,

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
    /// CD-3 `take_over` (`codrive/takeover.rs`): true while an agent-
    /// initiated takeover is active — a stronger freeze than an ordinary
    /// human-touched one (kills the shadow-bypass exception too). Main-
    /// thread-only `bool` mirrored onto `codrive.takeover_active`
    /// (`CodriveShared`) for `listener.rs`'s optimistic pre-check, same
    /// two-field pattern as `codrive_shadow_active`/`codrive.shadow_active`.
    pub codrive_takeover_active: bool,
    /// CD-3 `watch` (`codrive/watch.rs`): true while idle-based auto-pause
    /// supervision is active for the rest of this session's trajectory.
    pub codrive_watch_active: bool,
    /// CD-3: true while the seat is frozen SPECIFICALLY because of a
    /// watch-mode idle timeout (not human touch, not a takeover) — the only
    /// frozen cause `on_human_input` auto-clears without an explicit
    /// Super+Enter, since the input itself re-establishes "human present".
    pub codrive_watch_paused: bool,
    /// CD-3: instant of the most recent human-seat input event (ANY kind,
    /// updated on every `on_human_input` call, unlike `codrive_freeze_
    /// set_at` which only updates on the not-frozen→frozen transition).
    /// `codrive_check_watch_idle` compares `Instant::now()` against this.
    pub codrive_last_human_activity: std::time::Instant,
    /// WP-comp-shell-ipc (2026-08-22): cross-thread state shared with the
    /// shell-control socket thread (`shell_control::init`) — a SEPARATE
    /// channel/trust-boundary from `codrive` above, see that module's own
    /// doc comment for why. Just the audit log today (no freeze/token
    /// state — this channel has neither).
    pub shell_control: std::sync::Arc<shell_control::ShellControlShared>,
    /// A4-1 (udev/DRM backend): "something that can change a pixel happened
    /// since the last composite". Set by [`DuduclawComp::queue_redraw`],
    /// consumed (and cleared) by `udev_backend::dispatch_render`.
    ///
    /// The winit backend ignores this field entirely — it drives itself with
    /// `Window::request_redraw()` and always has, so wiring damage sources
    /// up cannot regress the already-verified nested path. Starts `true` so
    /// the very first frame is drawn without waiting for an event.
    pub pending_redraw: bool,
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
        // CUR-1: advertise `wp_cursor_shape_manager_v1`. Must exist before
        // any client binds, i.e. before `init_wayland_listener` below opens
        // the socket — hence its construction here with the other protocol
        // globals rather than lazily.
        let cursor_shape_manager_state = CursorShapeManagerState::new::<Self>(&dh);
        let popups = PopupManager::default();

        // A4-5: the ORDER these two `wl_seat` globals are created in is the
        // order clients see them advertised in, and that order decides
        // whether `duduclaw-shell` can receive keyboard input at all — gpui's
        // Wayland backend keeps only the LAST `wl_seat` it sees and releases
        // every other seat's keyboard/pointer. Read `seat_order`'s module doc
        // for the full root cause, the upstream line numbers, and the
        // tradeoff. Default is `AgentFirst` (agent seat advertised first, so
        // gpui's last-one-wins lands on the human seat);
        // `DUDUCLAW_COMP_SEAT_ORDER=human-first` restores the pre-A4-5 order.
        //
        // Neither branch changes the codrive safety model: two structurally
        // separate seats either way, agent input never merged into the human
        // seat, freeze / emergency stop / audit untouched.
        let seat_order = SeatAdvertiseOrder::from_env();
        tracing::info!(
            order = ?seat_order,
            "comp: wl_seat advertisement order (A4-5; override with {})",
            crate::seat_order::SEAT_ORDER_ENV
        );

        // A seat is a group of keyboard/pointer/touch devices. This spike
        // assumes a single always-present keyboard+pointer (real hotplug
        // tracking is out of scope for a winit-nested spike — there's no
        // real hardware to plug into).
        //
        // `codrive::init` is the agent seat's constructor (CD-0 codrive
        // spike: agent seat + injection socket + audit log). It must happen
        // while we still hold `&mut seat_state` locally (before it moves into
        // `Self` below) and while `event_loop` is available to register the
        // injection channel's calloop source — both hold in either branch.
        let make_human_seat = |seat_state: &mut SeatState<Self>| -> Seat<Self> {
            let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");
            seat.add_keyboard(Default::default(), 200, 25).unwrap();
            seat.add_pointer();
            seat
        };

        let (seat, agent_seat, codrive) = if seat_order.agent_first() {
            let (agent_seat, codrive) = codrive::init(&mut seat_state, &dh, event_loop);
            let seat = make_human_seat(&mut seat_state);
            (seat, agent_seat, codrive)
        } else {
            let seat = make_human_seat(&mut seat_state);
            let (agent_seat, codrive) = codrive::init(&mut seat_state, &dh, event_loop);
            (seat, agent_seat, codrive)
        };

        // WP-comp-shell-ipc (2026-08-22): needs no `SeatState`/
        // `DisplayHandle` (no new `wl_seat` — every op acts on the human
        // `seat` created above), so it can init any time after `event_loop`
        // is available; grouped here, right after `codrive::init`, since
        // both are "the compositor's IPC surfaces" set up in one place.
        let shell_control = shell_control::init(event_loop);

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

        // CUR-1: load the cursor theme once, here, rather than on first
        // pointer motion — a synchronous filesystem walk on the render path
        // would be a frame hitch, and doing it at startup is what makes the
        // "which theme did we get, and why not" line land in the boot log
        // where an operator can find it.
        let cursor = cursor::CursorState::from_env();
        tracing::info!(
            source = cursor.theme.source().as_str(),
            theme = %cursor.theme.theme_name(),
            "comp: human cursor configured (override with {}=system|brand, {}=<theme>)",
            cursor::source::CURSOR_SOURCE_ENV,
            cursor::source::CURSOR_THEME_ENV
        );

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
            cursor_shape_manager_state,
            popups,
            seat,
            cursor,

            agent_seat,
            codrive,
            codrive_freeze_set_at: None,
            codrive_highlight: None,
            shadow_output,
            codrive_shadow_active: false,
            codrive_logo_held_prev: false,
            codrive_takeover_active: false,
            codrive_watch_active: false,
            codrive_watch_paused: false,
            codrive_last_human_activity: start_time,
            shell_control,
            pending_redraw: true,
        }
    }

    /// A4-1: mark the compositor as visually dirty.
    ///
    /// Every call site is a place where the next composited frame could
    /// legitimately differ from the last one:
    /// - `handlers/compositor.rs::commit` — any client surface content,
    ///   window map/unmap, popup, or resize.
    /// - `handlers/xdg_shell.rs` — toplevel/popup creation and destruction.
    /// - `input.rs` — every human input event (the human cursor overlay
    ///   moves, focus may change, a grab may drag a window).
    /// - `codrive/mod.rs::handle_agent_inject` — agent cursor, click,
    ///   highlight box, shadow-workspace toggle.
    /// - `focus_window` (below) — activation state changes, which every
    ///   focus path in the crate funnels through, so click-to-focus,
    ///   Super+Tab, close-time reassignment, `shell_control` focus, and
    ///   codrive `activate_window` are all covered by that one call.
    /// - `udev_backend.rs`'s housekeeping tick — codrive highlight expiry.
    ///
    /// Being conservative here is cheap: a spurious `queue_redraw` costs one
    /// composite whose damage comes back empty, which then does NOT page
    /// flip (see `udev_backend`'s "Repaint scheduling"). Missing one costs a
    /// visibly stale screen, so when in doubt this is called.
    pub fn queue_redraw(&mut self) {
        self.pending_redraw = true;
    }

    /// The first REAL output, i.e. skipping the CD-2 shadow workspace's
    /// headless output.
    ///
    /// `Space::outputs()` yields insertion order and the shadow output is
    /// mapped first (in `new` above, before any backend has produced a real
    /// one), so a bare `space.outputs().next()` returns the shadow output —
    /// which sits at `codrive::SHADOW_ORIGIN` `(0, 100_000)`. Any caller
    /// that used it to map an absolute pointer position was placing the
    /// cursor 100 000 px below every real window. Found while wiring the
    /// udev backend's input path (A4-1); `input.rs`'s
    /// `PointerMotionAbsolute` arm was the one affected caller.
    pub fn primary_output(&self) -> Option<&Output> {
        self.space
            .outputs()
            .find(|o| *o != &self.shadow_output)
            .or_else(|| self.space.outputs().next())
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

    /// WP-A1 multi-window round: raises `window` (if given) to the top of
    /// the stack and gives it exclusive keyboard focus/activation on
    /// `seat`, deactivating every other mapped window and telling every
    /// client via a fresh configure. `window = None` clears keyboard focus
    /// and deactivates everything (a click on empty space).
    ///
    /// Shared by every call site that needs the same "one active window,
    /// matching keyboard focus, clients told" invariant: the human
    /// click-to-focus arm (`input.rs`'s `PointerButton` handling), the
    /// agent click-to-focus arm (`codrive/mod.rs`'s `InjectCmd::Button`
    /// handling), close-time focus handoff
    /// (`reassign_focus_on_window_removed` below), and Super+Tab cycling
    /// (`cycle_focus` below). Before this round the first two each
    /// open-coded their own copy of this loop — and neither one actually
    /// called `Window::set_activated(true)` on the window it was
    /// focusing, only `set_activated(false)` on the deselect-to-empty-
    /// space path, so a newly selected window's own xdg-shell `activated`
    /// state (and any client-side active/inactive titlebar styling keyed
    /// off it) never lit up. See BUILD.md's "A1 multi-window round"
    /// section for the live-run evidence this was fixed and stayed fixed.
    ///
    /// `seat` is a caller-owned clone (`Seat<D>` is a cheap `Arc`-backed
    /// handle, see smithay's own `impl Clone for Seat`), never a borrow of
    /// `self.seat`/`self.agent_seat` — that's what lets this take `&mut
    /// self` without a borrow-checker conflict at every call site.
    pub fn focus_window(&mut self, seat: &Seat<Self>, window: Option<&Window>, serial: Serial) {
        // A4-1: raising and (de)activating windows changes what is on screen
        // and, on clients that style their titlebar off xdg-shell
        // `activated`, what those windows draw. Every focus path in the
        // crate goes through here, so this one call covers click-to-focus
        // (human + agent), Super+Tab, close-time reassignment,
        // `shell_control` focus_window, and codrive `activate_window`.
        self.queue_redraw();
        if let Some(w) = window {
            self.space.raise_element(w, true);
        }
        let mut activated_count = 0u32;
        for element in self.space.elements() {
            let activate = window == Some(element);
            if activate {
                activated_count += 1;
            }
            element.set_activated(activate);
        }
        // Debug-level, not info: this runs on every click, not just
        // notable transitions (unlike the info!-level logs in
        // `cycle_focus`/`reassign_focus_on_window_removed` above, which
        // fire far less often). Exists so a live run can directly confirm
        // "exactly one window activated, matching the focus target" —
        // the specific invariant the WP-A1 fix restored — without needing
        // pixel/screenshot access to a headless container.
        tracing::debug!(
            target_surface_id = ?window.map(|w| w.toplevel().unwrap().wl_surface().id()),
            activated_count,
            total_windows = self.space.elements().len(),
            "focus: activation set"
        );
        if let Some(keyboard) = seat.get_keyboard() {
            let target = window.map(|w| w.toplevel().unwrap().wl_surface().clone());
            keyboard.set_focus(self, target, serial);
        }
        self.space.elements().for_each(|w| {
            w.toplevel().unwrap().send_pending_configure();
        });
    }

    /// WP-A1 multi-window round (task brief req 3, "視窗關閉焦點轉移規則
    /// （轉給 Z 序次高者）"): called from `XdgShellHandler::
    /// toplevel_destroyed` (`handlers/xdg_shell.rs`) right after the
    /// destroyed window has been unmapped from `self.space`. For EACH
    /// seat independently, only if that seat's keyboard focus WAS the
    /// just-destroyed surface, hands focus to the new topmost remaining
    /// window (or clears it if none remain). Deliberately does nothing to
    /// a seat whose focus was already somewhere else — closing a
    /// background window must never steal focus from whatever the human
    /// (or the agent) was actually interacting with on the other seat.
    pub fn reassign_focus_on_window_removed(&mut self, destroyed: &WlSurface) {
        for seat in [self.seat.clone(), self.agent_seat.clone()] {
            let Some(keyboard) = seat.get_keyboard() else {
                continue;
            };
            if keyboard.current_focus().as_ref() != Some(destroyed) {
                continue;
            }
            // `elements()` is bottom-to-top (see `focus_window`'s own
            // raise-to-end reasoning) — `next_back()` is therefore the new
            // topmost survivor, exactly "Z 序次高者".
            let next = self.space.elements().next_back().cloned();
            tracing::info!(
                next_surface_id = ?next.as_ref().map(|w| w.toplevel().unwrap().wl_surface().id()),
                "focus: closed window held focus — reassigning to the new topmost window"
            );
            let serial = SERIAL_COUNTER.next_serial();
            self.focus_window(&seat, next.as_ref(), serial);
        }
    }

    /// WP-A1 multi-window round (task brief req 3, "Super+Tab 視窗循環切
    /// 換"): called from `input.rs`'s human keyboard filter closure,
    /// alongside the existing Super+Esc/Super+Enter global bindings. No
    /// MRU list is tracked — instead every press promotes the CURRENT
    /// BOTTOM of the z-order stack to the top via `focus_window` (which
    /// raises it, per `Space::raise_element`'s remove-then-push-to-end
    /// behavior). That is a genuine full rotation through every mapped
    /// window, not a two-window oscillation: with a 3-window stack
    /// (bottom→top) `[A, B, C]`, press 1 raises the bottom (`A`) to
    /// `[B, C, A]`; press 2 raises the new bottom (`B`) to `[C, A, B]`;
    /// press 3 raises `C` to `[A, B, C]` — back to the start, having
    /// visited A, B, and C exactly once each. (An earlier, rejected design
    /// — "raise whichever window is one position below the current top" —
    /// only ever swaps the top two elements and can never reach a window
    /// three or more presses down; verified wrong by hand before writing
    /// this version, not assumed.) No new persistent state is needed
    /// beyond the space's own z-order, which every click-to-focus call
    /// already maintains. No-op with zero or one mapped windows (nothing
    /// meaningful to cycle to).
    pub fn cycle_focus(&mut self) {
        if self.space.elements().len() < 2 {
            // Nothing to rotate with 0 or 1 mapped windows.
            return;
        }
        let Some(next) = self.space.elements().next().cloned() else {
            return;
        };
        tracing::info!(
            surface_id = ?next.toplevel().unwrap().wl_surface().id(),
            window_count = self.space.elements().len(),
            "focus: Super+Tab cycling"
        );
        let seat = self.seat.clone();
        let serial = SERIAL_COUNTER.next_serial();
        self.focus_window(&seat, Some(&next), serial);
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
