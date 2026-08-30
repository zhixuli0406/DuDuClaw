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
            backend::{ClientData, ClientId, DisconnectReason, ObjectId},
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
        seat::WaylandFocus,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
        xwayland_shell::XWaylandShellState,
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
    /// WM-1: the `zxdg_decoration_manager_v1` global. Held only to keep the
    /// global alive for the process lifetime — every request it receives is
    /// dispatched by smithay straight into the `XdgDecorationHandler` impl in
    /// `handlers/xdg_shell.rs`, so nothing in this crate reads the field.
    /// Same shape (and same reason) as `cursor_shape_manager_state` above.
    pub xdg_decoration_state: XdgDecorationState,
    /// WM-3: the `zwlr_layer_shell_v1` global. Unlike the two above this field
    /// IS read — `WlrLayerShellHandler::shell_state` (`crate::layer_shell`)
    /// hands it back to smithay on every request. Layer surfaces themselves do
    /// not live here: smithay keeps one `LayerMap` per `Output`, in that
    /// output's own `UserDataMap`. See `crate::layer_shell`'s module doc.
    pub layer_shell_state: WlrLayerShellState,
    /// A4 (CP-1, XWayland): the `xwayland_shell_v1` global. Same "must exist
    /// before any client binds" constraint as the other protocol globals in
    /// this struct — `Xwayland` itself is a Wayland CLIENT (see
    /// `crate::xwayland::spawn`) and needs this bound before it can pair an
    /// `X11Surface` with a `wl_surface`. See `crate::xwayland`'s module doc.
    pub xwayland_shell_state: XWaylandShellState,
    /// A4: the running XWayland instance's negotiated X11 window manager
    /// (once ready) plus its display number. See `crate::xwayland`.
    pub xwayland: crate::xwayland::XWaylandState,
    /// D3-a (2026-08-23): the three input-method globals plus the live
    /// candidate window. See `crate::ime`'s module doc for why all three
    /// globals are mandatory and why the popup is not optional.
    pub ime: crate::ime::ImeState,
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
    /// E1a-1a (`codrive/human_seat.rs`): true only for the duration of one
    /// human-seat synthesised emission. `codrive::on_human_input` refuses to
    /// treat anything as human input while it is set.
    ///
    /// Defence in depth, not a live requirement: the synthesis helpers call
    /// `Seat` APIs directly and never reach `input.rs::process_input_event`,
    /// which is the only caller of `on_human_input`. The flag exists so that
    /// a future change which *does* route synthesis through the backend path
    /// fails loudly (an audited, warned no-op) instead of silently live-
    /// locking the agent — inject → freeze itself → drop — or forging "a
    /// human is present" and disarming watch mode's idle auto-pause. See
    /// DESIGN-codrive-desktop-2026-08.md §6.1.1 item ②. Main-thread-only
    /// `bool`, same as `codrive_shadow_active`/`codrive_takeover_active`.
    pub codrive_synthesizing: bool,
    /// A2 (`codrive/mode.rs`): driving-mode transition bookkeeping — the last
    /// observed mode plus the pending handover-trigger hint. NOT the source
    /// of truth: the mode is always derived (`codrive::derive_mode`) from
    /// `session_active`/`terminated`/`frozen`; this only lets
    /// `codrive_sync_mode` tell a real transition from a per-frame no-op.
    pub codrive_mode: codrive::CodriveModeCache,
    /// WP-comp-shell-ipc (2026-08-22): cross-thread state shared with the
    /// shell-control socket thread (`shell_control::init`) — a SEPARATE
    /// channel/trust-boundary from `codrive` above, see that module's own
    /// doc comment for why. Just the audit log today (no freeze/token
    /// state — this channel has neither).
    pub shell_control: std::sync::Arc<shell_control::ShellControlShared>,
    /// A1 (2026-08-2x): global compositor-level gestures (today just
    /// Super+K) that the shell has not yet been told about, oldest first.
    /// Pushed by the human keyboard's filter closure (`input.rs`), drained by
    /// the `shell_control` `take_shell_intents` op (`shell_control::mod`'s
    /// short-poll contract with the shell — see that op's own doc). Bounded
    /// at [`shell_control::MAX_PENDING_SHELL_INTENTS`]: a shell that stops
    /// polling (crashed, restarting) must not turn this into an unbounded
    /// leak — see `DuduclawComp::push_shell_intent`'s own doc for the
    /// drop-oldest-and-warn policy that enforces the bound.
    pub pending_shell_intents: std::collections::VecDeque<shell_control::ShellIntent>,
    /// D9-bug3/D9-bug4 (2026-08-24): whether the session shell has declared
    /// the screen LOCKED (`shell_control` op `set_session_locked`). Owns three
    /// behaviours while true — windows are not painted, only layer surfaces
    /// can take pointer input, and keys bypass the input method's keyboard
    /// grab straight to the shell. See `crate::session_lock`'s module doc for
    /// the whole rule and for the smithay source readings behind it. Never
    /// inferred, only ever set by that op: comp has no way to know what the
    /// shell is drawing.
    pub session_locked: bool,
    /// WM-1 (2026-08-23): how much of the output `duduclaw-shell`'s own menu
    /// bar and dock occupy, and therefore how much an ordinary application
    /// window must stay out of. Read once at startup; see
    /// `crate::window_policy`'s module doc for where the numbers come from and
    /// why this policy exists at all.
    pub reserved_bands: crate::window_policy::ReservedBands,
    /// WM-1: the `xdg_toplevel.app_id` that identifies the session shell
    /// (`window_policy::SHELL_APP_ID`, overridable via env for a stand-in
    /// shell).
    pub shell_app_id: String,
    /// WM-1: the toplevel comp currently treats as the session shell — the one
    /// window exempt from the reserved bands and from Super+Q. `None` until a
    /// toplevel claims the role (and again after that window is destroyed).
    /// See `window_policy::DuduclawComp::classify_shell_window` for the two
    /// rules that assign it.
    pub shell_surface: Option<WlSurface>,
    /// WM-1: true once [`Self::shell_surface`] was chosen by a matching
    /// `app_id` rather than by the first-mapped fallback. A confirmed role is
    /// never handed to a later window.
    pub shell_confirmed: bool,
    /// WM-2 (2026-08-23): everything the server-side decorations and the
    /// floating-window layout need that `Space` does not already hold — the
    /// embedded title fonts, the per-toplevel decoration-mode negotiation, the
    /// cached (id-stable) decoration buffers, the pre-maximize restore
    /// geometry, and the cascade counter. See `crate::decor`'s module doc for
    /// the geometry model, and `decor::paint`'s for why the buffers are
    /// cached rather than rebuilt per frame.
    pub decor: crate::decor::paint::DecorState,
    /// D2 (2026-08-2x): which appearance the session's server-side
    /// decorations (and the Alt-Tab switcher panel) currently draw with.
    /// `duduclaw-shell` is the single source of truth for this — it pushes
    /// the live value via `shell_control`'s `set_theme` op, both at its own
    /// boot and on every user toggle — so this field starts at
    /// [`crate::decor::Theme::default`] (`Light`, matching the shell's own
    /// `ThemeChoice::default()`) purely so a comp process that starts before
    /// the shell's first `set_theme` call already looks right, not as a
    /// comp-owned preference. See `crate::decor::Theme`'s own doc for the
    /// full reasoning, including why (unlike the cursor `source`/`size`
    /// settings) there is no persistence or env var for this on comp's side.
    pub theme: crate::decor::Theme,
    /// WM-3: windows that are minimized — unmapped from [`Self::space`] but
    /// still alive, still switchable, still listed by `shell_control`.
    ///
    /// A `Window` is an `Arc` handle, so holding one here is what keeps the
    /// toplevel from being reaped while it is off screen. Entries leave on
    /// restore ([`DuduclawComp::unminimize_window`]) and on destruction
    /// (`XdgShellHandler::toplevel_destroyed`) — those are the only two exits,
    /// which is what makes "minimized ⇒ recoverable" a real invariant rather
    /// than a hope.
    pub minimized: Vec<Window>,
    /// WM-3: most-recently-focused order, front = most recent. Maintained by
    /// [`DuduclawComp::focus_window`] (every focus path in the crate funnels
    /// through it) and drained by `crate::switcher`. Ids, not `Window`s, so a
    /// stale entry cannot keep a dead toplevel alive.
    pub focus_mru: Vec<ObjectId>,
    /// WM-3: the Alt-Tab switcher — an open session plus its cached panel
    /// buffers. See `crate::switcher`.
    pub switcher: crate::switcher::SwitcherState,
    /// WM-3: the previous title-bar press — `(window, when, where)` — used to
    /// recognise a double click. `when` is measured from
    /// [`Self::start_time`], so it is monotonic and needs no wall clock.
    pub last_titlebar_click: Option<(ObjectId, std::time::Duration, Point<f64, Logical>)>,
    /// A4-1 (udev/DRM backend): "something that can change a pixel happened
    /// since the last composite". Set by [`DuduclawComp::queue_redraw`],
    /// consumed (and cleared) by `udev_backend::dispatch_render`.
    ///
    /// The winit backend ignores this field entirely — it drives itself with
    /// `Window::request_redraw()` and always has, so wiring damage sources
    /// up cannot regress the already-verified nested path. Starts `true` so
    /// the very first frame is drawn without waiting for an event.
    pub pending_redraw: bool,

    /// D3-f2 (2026-08-23): live evdev fds for the devices libinput opened, so
    /// an ABSOLUTE pointing device's real position can be read on demand.
    /// Populated by the udev backend (`udev_backend::init_udev`); left empty
    /// by the winit backend, where every lookup misses and the caller falls
    /// back to the compositor's own pointer location. See
    /// [`crate::abs_pointer`] for why this exists and why it is a `dup()` of
    /// libinput's fd rather than an `open()` of our own.
    pub abs_pointer: crate::abs_pointer::AbsPointerTable,

    /// D3-f2: has a real `InputEvent::PointerMotion*` ever arrived on the
    /// human seat in this process's lifetime?
    ///
    /// Guards the one-shot startup seeding in
    /// [`DuduclawComp::seed_absolute_pointer_position`]: once the pointer has
    /// genuinely moved, its location is authoritative and a later device
    /// hot-plug must never teleport the cursor to that new device's idea of
    /// where it is.
    pub pointer_motion_seen: bool,
}

impl DuduclawComp {
    // A4 (CP-1, XWayland): `EventLoop<'static, CalloopData>` rather than the
    // elided form every OTHER function taking an `EventLoop` in this crate
    // still uses — this one calls `crate::xwayland::spawn`, which needs that
    // concrete bound. See `main.rs`'s matching root-declaration comment and
    // `crate::xwayland::spawn`'s own doc for why.
    pub fn new(event_loop: &mut EventLoop<'static, CalloopData>, display: Display<Self>) -> Self {
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
        // WM-1: advertise `zxdg_decoration_manager_v1`. Same "must exist
        // before any client binds" constraint as the cursor-shape global
        // above, hence its construction here rather than lazily. See
        // `handlers/xdg_shell.rs`'s `XdgDecorationHandler` impl for why comp
        // always answers `ClientSide`.
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        // WM-3: advertise `zwlr_layer_shell_v1`. Same "must exist before any
        // client binds" constraint as the two globals above. Advertising it is
        // safe with no client using it — nothing changes until something binds
        // (see `crate::layer_shell`'s scope note).
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        // A4 (CP-1, XWayland): advertise `xwayland_shell_v1`. Same "must
        // exist before any client binds" constraint as the globals above —
        // `Xwayland` itself is spawned as a client a few lines down, and its
        // binding of this global must find it already there.
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);
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

        // A4 (CP-1, XWayland): started here, alongside the other two
        // "extra IPC/protocol surface" constructors above — needs `&dh` (to
        // insert the XWayland client) and `event_loop` (to register its
        // ready-event source), both already in scope. See
        // `crate::xwayland::spawn`'s own doc for the startup-not-lazy
        // rationale and the fail-open-to-Wayland-only behaviour on spawn
        // failure.
        crate::xwayland::spawn(&dh, event_loop);

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
            requested = cursor.theme.requested().as_str(),
            // CUR-2: `origin` is what makes a support question answerable in
            // one line — "brand isn't sticking" is a completely different bug
            // depending on whether the live value came from `env` (an
            // operator pinned it), `persisted` (the stored preference loaded)
            // or `default` (nothing was found at all).
            origin = cursor.origin.as_str(),
            theme = %cursor.theme.theme_name(),
            "comp: human cursor configured (override with {}=system|brand, {}=<theme>; \
             switch live via the shell_control set_cursor_source op)",
            cursor::source::CURSOR_SOURCE_ENV,
            cursor::source::CURSOR_THEME_ENV
        );

        // WM-1: read the layout tunables once, here, alongside every other
        // startup-time env read — and log them, because "the app window is
        // the wrong height" is otherwise an unanswerable support question.
        let reserved_bands = crate::window_policy::ReservedBands::from_env();
        let shell_app_id = crate::window_policy::shell_app_id_from_env();
        tracing::info!(
            reserved_top = reserved_bands.top,
            reserved_bottom = reserved_bands.bottom,
            shell_app_id = %shell_app_id,
            "comp: window layout policy (override with {}/{}/{})",
            crate::window_policy::RESERVED_TOP_ENV,
            crate::window_policy::RESERVED_BOTTOM_ENV,
            crate::window_policy::SHELL_APP_ID_ENV,
        );

        // D3-a: the three IME globals. Same "must exist before any client
        // binds" constraint as the protocol globals above — and therefore
        // strictly before `init_wayland_listener` opens the socket.
        let ime = crate::ime::ImeState::new(&dh);

        // D3-c / E1a-1: arm the per-client agent-seat filter, now that both
        // seats exist. Runs its own self-check and stays OFF if that fails —
        // see `ime::seat_filter`'s module doc for what "off" then costs (the
        // measured Chromium input blackout comes back) and what still catches
        // the IME half (codrive's `paused_by_ime` guard).
        let seat_filter = crate::ime::seat_filter::arm(&seat, &agent_seat);
        tracing::info!(
            status = seat_filter.as_str(),
            "comp: per-client agent-seat visibility (E1a-1 + D3-c)"
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
            xdg_decoration_state,
            layer_shell_state,
            xwayland_shell_state,
            xwayland: crate::xwayland::XWaylandState::default(),
            ime,
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
            codrive_synthesizing: false,
            codrive_mode: codrive::CodriveModeCache::default(),
            shell_control,
            pending_shell_intents: std::collections::VecDeque::new(),
            session_locked: false,
            theme: crate::decor::Theme::default(),
            reserved_bands,
            shell_app_id,
            shell_surface: None,
            shell_confirmed: false,
            decor: crate::decor::paint::DecorState::new(),
            minimized: Vec::new(),
            focus_mru: Vec::new(),
            switcher: crate::switcher::SwitcherState::default(),
            last_titlebar_click: None,
            pending_redraw: true,
            abs_pointer: crate::abs_pointer::AbsPointerTable::default(),
            pointer_motion_seen: false,
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
                let data = Arc::new(ClientState::default());
                let client = state
                    .display_handle
                    .insert_client(client_stream, data.clone())
                    .unwrap();
                // D3-c / E1a-1: classify the peer ONCE, here. `can_view` runs
                // on the registry path for every global × every client and
                // gets no `DisplayHandle` to ask for credentials, so it reads
                // these cached flags instead of touching `/proc` per
                // advertisement.
                //
                // Setting them after `insert_client` is safe: client requests
                // are dispatched from a different calloop source (the
                // `Display` generic source below), never re-entrantly from
                // inside this accept callback, so no `can_view` can observe
                // the pre-classification value.
                let (class, comm) =
                    crate::ime::seat_filter::classify_client(&client, &state.display_handle);
                data.set_seat_class(class, comm);
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

    /// What client surface is under `pos`, and where that surface's origin is.
    ///
    /// WM-3 put layer surfaces on both sides of the window stack, in the exact
    /// order [`crate::layer_shell::geometry`] ranks them: overlay and top
    /// layers get first refusal, then ordinary windows, then bottom and
    /// background. Routing has to agree with rendering or a panel drawn over a
    /// window would not receive the clicks that visibly land on it.
    ///
    /// D9-bug4 (2026-08-24): while [`Self::session_locked`], the window band
    /// in the middle is skipped entirely — the same windows that are left out
    /// of the frame must also be unreachable by the pointer, or a click on a
    /// locked screen would land in an application nobody can see. Layer
    /// surfaces on BOTH sides are still routed normally, which is what keeps
    /// the lock screen's own power button (drawn on the shell's `Background`
    /// surface) clickable. See `crate::session_lock`'s module doc.
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some(hit) = self.layer_surface_under(pos, true) {
            return Some(hit);
        }
        if !self.session_locked {
            let window_hit = self.space.element_under(pos).and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            });
            if window_hit.is_some() {
                return window_hit;
            }
        }
        self.layer_surface_under(pos, false)
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
            // WM-3: every focus path funnels through here, so this one line is
            // what keeps the Alt-Tab order honest for click-to-focus, agent
            // activation, dock activation, close-time handoff and the switcher
            // itself. Deliberately NOT updated on `window == None` (a click on
            // empty space): "the last window you used" is still the last window
            // you used after you click the desktop.
            if let Some(toplevel) = w.toplevel() {
                crate::alt_tab::mru_promote(&mut self.focus_mru, toplevel.wl_surface().id());
            }
        }
        let mut activated_count = 0u32;
        for element in self.space.elements() {
            let activate = window == Some(element);
            if activate {
                activated_count += 1;
            }
            // `Window::set_activated` (smithay `desktop/wayland/window.rs`)
            // already dispatches on the underlying surface kind internally —
            // xdg_toplevel's `Activated` state for a Wayland window,
            // `X11Surface::set_activated` for an X11 one (A4, CP-1
            // XWayland) — so this call needed no change to stay safe once
            // X11 windows started sharing this space.
            element.set_activated(activate);
        }
        // Debug-level, not info: this runs on every click, not just
        // notable transitions (unlike the info!-level logs in
        // `cycle_focus`/`reassign_focus_on_window_removed` above, which
        // fire far less often). Exists so a live run can directly confirm
        // "exactly one window activated, matching the focus target" —
        // the specific invariant the WP-A1 fix restored — without needing
        // pixel/screenshot access to a headless container.
        //
        // A4 (CP-1, XWayland): `w.toplevel().unwrap()` used to be safe here
        // because every `Window` this crate ever mapped came from
        // `Window::new_wayland_window`. An X11 window's `.toplevel()` is
        // always `None` (see `Window::toplevel`'s own doc), so this — and
        // every other lookup below — now goes through the generic
        // `WaylandFocus::wl_surface()` accessor, which resolves for BOTH
        // window kinds (an X11 window's once XWayland has paired one).
        tracing::debug!(
            target_surface_id = ?window.and_then(|w| w.wl_surface()).map(|s| s.id()),
            activated_count,
            total_windows = self.space.elements().len(),
            "focus: activation set"
        );
        if let Some(keyboard) = seat.get_keyboard() {
            let target = window.and_then(|w| w.wl_surface()).map(|s| s.into_owned());
            keyboard.set_focus(self, target, serial);
        }
        self.space.elements().for_each(|w| {
            // Only an xdg toplevel has a "pending configure" to send — an
            // X11 window's activation notification already went out
            // synchronously inside `Window::set_activated` above
            // (`X11Surface::set_activated` performs the X11 property/message
            // round-trip itself, it does not defer to a later configure).
            if let Some(toplevel) = w.toplevel() {
                toplevel.send_pending_configure();
            }
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
                next_surface_id = ?next.as_ref().and_then(|w| w.wl_surface()).map(|s| s.id()),
                "focus: closed window held focus — reassigning to the new topmost window"
            );
            let serial = SERIAL_COUNTER.next_serial();
            self.focus_window(&seat, next.as_ref(), serial);
        }
    }

    // NOTE (WM-3): the WP-A1 round's `cycle_focus` used to live here. It
    // promoted the bottom of the z-order to the top on every Super+Tab press —
    // a genuine full rotation, but with the one property a task switcher must
    // not have: pressing it twice does not return you to where you started,
    // because each press permanently reorders the stack. WM-3 replaced it with
    // a real MRU switcher (`crate::switcher`, pure logic in `crate::alt_tab`),
    // bound to both Alt+Tab and Super+Tab.
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// D3-c: was this connection's peer process recognised as an input
    /// method at accept time?
    ///
    /// Atomic because `ClientData` is handed to wayland-server as a shared
    /// `Arc` — the flags are written once, immediately after `insert_client`
    /// returns the `Client` the credentials come from, and read-only after
    /// that.
    is_input_method: std::sync::atomic::AtomicBool,
    /// E1a-1: was this connection's peer process on the agent-seat allow
    /// list at accept time?
    ///
    /// **Defaults to `false`**, which is the fail-closed direction: a
    /// connection we could not classify (or, defensively, one that somehow
    /// carries no classification at all) is treated as an ordinary app and
    /// sees the human seat only. See `ime::seat_filter`'s module doc for why
    /// both checks live on the connection rather than in the filter, and why
    /// this direction is the safe one.
    agent_seat_allow_listed: std::sync::atomic::AtomicBool,
    /// The peer's `/proc/<pid>/comm`, when it could be read. Purely for
    /// naming the client in the codrive audit trail when an injection is
    /// dropped because that client cannot see the agent seat — a dropped
    /// injection that does not say *which* app it could not reach is not
    /// actionable. `OnceLock` rather than a `Mutex<Option<String>>`: written
    /// once at accept time, read-only afterwards.
    comm: std::sync::OnceLock<String>,
}

impl ClientState {
    /// Records the accept-time classification (`ime::seat_filter`).
    pub fn set_seat_class(&self, class: crate::ime::seat_filter::ClientClass, comm: Option<String>) {
        use std::sync::atomic::Ordering;
        self.is_input_method.store(class.is_input_method, Ordering::SeqCst);
        self.agent_seat_allow_listed
            .store(class.allow_listed, Ordering::SeqCst);
        if let Some(comm) = comm {
            let _ = self.comm.set(comm);
        }
    }

    /// Reads the accept-time classification back.
    pub fn seat_class(&self) -> crate::ime::seat_filter::ClientClass {
        use std::sync::atomic::Ordering;
        crate::ime::seat_filter::ClientClass {
            is_input_method: self.is_input_method.load(Ordering::SeqCst),
            allow_listed: self.agent_seat_allow_listed.load(Ordering::SeqCst),
        }
    }

    /// The peer's process name, if it was readable at accept time.
    pub fn comm(&self) -> Option<&str> {
        self.comm.get().map(String::as_str)
    }
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
