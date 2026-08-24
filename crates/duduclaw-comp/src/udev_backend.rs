//! A4-1: the real-hardware backend — libseat session + DRM/KMS + GBM/EGL +
//! libinput, driven by udev device discovery.
//!
//! This is what lets `duduclaw-comp` be the thing that owns the display,
//! instead of being a fullscreen client of `cage` (which was itself the
//! thing owning the display). `BUILD.md`'s three-layer appliance stack
//! (`cage` → `duduclaw-comp`(winit) → client) collapses to two
//! (`duduclaw-comp`(udev) → client) once this backend is selected.
//!
//! ## Deliberate scope (minimum viable, honestly stated)
//!
//! Modelled on smithay 0.7.0's `anvil` udev backend but cut down hard. What
//! is **implemented**:
//!
//! - libseat session (`seatd`, or `logind` if libseat was built with it) for
//!   DRM-master and input-device fd acquisition, with session
//!   pause/activate handling (so a VT switch performed by *something else*
//!   doesn't leave us wedged).
//! - udev discovery of the primary GPU (`primary_gpu()`, falling back to
//!   `all_gpus()`, overridable with `DUDUCLAW_COMP_DRM_DEVICE`).
//! - Connector enumeration on that one GPU: every **connected** connector
//!   gets its preferred mode, a free CRTC, a `DrmSurface`, and a wayland
//!   `Output` mapped left-to-right into the shared `Space`.
//! - GBM allocator + EGL/GLES renderer, one `GbmBufferedSurface` swapchain
//!   per output.
//! - libinput seat feeding the **existing** `DuduclawComp::process_input_event`
//!   path — no new input semantics, no second copy of the codrive
//!   freeze/emergency-stop rules.
//! - Damage-driven, on-demand repaint (see "Repaint scheduling" below).
//!
//! What is **NOT** implemented, and is not pretended to be:
//!
//! - **Multi-GPU.** One GPU node is opened. A second GPU's connectors are
//!   ignored (they are logged). No `GpuManager`/`MultiRenderer` copy-between-
//!   GPUs path exists here — that is anvil's `renderer_multi` feature and it
//!   roughly doubles this file.
//! - **Hotplug.** The `UdevBackend` event source is registered and every
//!   Added/Changed/Removed event is logged, but nothing rescans: connecting
//!   a monitor after startup will not create an output, and unplugging one
//!   leaves a dead surface until restart. Deliberate — a correct
//!   implementation needs the `smithay-drm-extras` `DrmScanner`
//!   add/remove/CRTC-reallocation state machine, which is out of scope for
//!   this round.
//! - **DMA-BUF / linux-dmabuf-v1 / `wl_drm`.** Clients still go through
//!   `wl_shm` exactly as they do on the winit backend; `bind_wl_display` is
//!   not called and no `DmabufState` global is advertised. GPU clients will
//!   fall back to software buffers.
//! - **Hardware cursor / overlay planes / direct scanout.** Everything is
//!   composited into the primary plane. The agent cursor is an ordinary
//!   `SolidColorRenderElement` (`codrive/cursor.rs`) and the human cursor an
//!   ordinary memory/surface element (`crate::cursor`, CUR-1) — neither is
//!   promoted to a DRM cursor plane. That costs a full composite per pointer
//!   motion instead of a plane update; acceptable at this stage, and it is
//!   the reason `Kind::Cursor` is set on those elements (smithay's damage
//!   tracker uses the hint).
//! - **VT switching.** No `Ctrl+Alt+F<n>` binding is installed. Two reasons:
//!   (a) the appliance kiosk unit is deliberately not a PAM/logind session
//!   (`appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.service`), so
//!   there is no other session to switch *to*; (b) installing a new global
//!   chord would mean touching the human keyboard filter closure in
//!   `input.rs`, which carries the Super+Esc emergency-stop / Super+Enter
//!   handback semantics this work package is explicitly forbidden to
//!   change. Session **pause/activate** events *are* handled, so an
//!   externally-driven VT switch (e.g. `chvt`) still works correctly.
//! - **VRR, 10-bit colour, explicit sync fences on the plane.** `ARGB8888`/
//!   `XRGB8888` only.
//!
//! ## Repaint scheduling (the CPU requirement)
//!
//! The winit backend calls `window.request_redraw()` at the end of every
//! frame, which is an unconditional full-rate composite whether or not
//! anything changed — that is the loop behind the appliance's measured
//! "cage at ~100% CPU with a nested comp on top". This backend never does
//! that. Instead:
//!
//! 1. Anything that can change a pixel calls `DuduclawComp::queue_redraw()`
//!    (client commits, human input, agent injections, focus changes,
//!    toplevel map/unmap — see that method's doc comment for the full list).
//! 2. `dispatch_render` runs after every calloop dispatch and renders only
//!    the outputs whose `needs_redraw` flag is set and that are not already
//!    waiting on a page flip.
//! 3. `render_output` returns `damage: None` when the frame is pixel-identical
//!    to the last one. In that case **no page flip is queued at all** — the
//!    scanout keeps showing the buffer it already has and the process goes
//!    back to blocking in `epoll`.
//! 4. `calloop`'s dispatch timeout is `None` (see `main.rs`), so with nothing
//!    to draw the process is genuinely blocked, not spinning. The only
//!    periodic wake-up is a 1 Hz housekeeping tick (see `TICK_IDLE`), which
//!    tightens to [`TICK_HIGHLIGHT`] only while a codrive highlight box is
//!    counting down.
//!
//! A frame that *does* produce damage is paced by the vblank itself. The one
//! case with no hardware pacing is "queue_redraw fired but the frame turned
//! out identical" — repeated quickly that would re-composite as fast as the
//! CPU allows, so [`min_render_gap`] holds those to one per refresh period.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, GbmBufferedSurface},
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, gles::GlesTexture, Bind},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
        },
        // `drm` itself (not just the items below) so `drm::control::Mode` /
        // `drm::control::ResourceHandles` can be named in signatures without
        // this crate taking a direct dependency on the `drm` crate — smithay
        // re-exports the exact version it was built against, which is the
        // only version whose handle types are compatible with `DrmDevice`.
        drm,
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{backend::GlobalId, DisplayHandle},
    },
    utils::{DeviceFd, Logical, Point, Transform},
};

use crate::{render::CodriveElement, state::DuduclawComp, CalloopData};

/// Background colour of an empty output. Same value the winit backend uses,
/// so a nested dev run and a real-hardware run look identical.
const CLEAR_COLOR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

/// Colour formats offered to `GbmBufferedSurface`, in preference order.
/// Deliberately 8-bit only — see the module doc's "not implemented" list.
const SUPPORTED_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Xrgb8888];

/// Housekeeping tick while nothing time-based is pending. One wake-up per
/// second is what keeps `codrive_check_watch_idle` (whose own threshold has
/// a 5 s floor, `codrive/watch.rs`) honest without a busy loop.
const TICK_IDLE: Duration = Duration::from_secs(1);
/// Housekeeping tick while a codrive highlight box is counting down. The
/// highlight duration clamps to a 100 ms minimum (`clamp_highlight_ms`), so
/// this bounds "highlight visibly overstays its deadline" to one tick.
const TICK_HIGHLIGHT: Duration = Duration::from_millis(50);

/// Environment override for the DRM device node, mirroring anvil's
/// `ANVIL_DRM_DEVICE`. Useful on a machine with more than one GPU where
/// udev's idea of "primary" is not the one wired to the panel.
pub const DRM_DEVICE_ENV: &str = "DUDUCLAW_COMP_DRM_DEVICE";

/// Everything the udev/DRM backend owns. Lives in
/// [`crate::CalloopData::udev`] so the calloop sources (DRM vblank, session
/// notifier, libinput, udev, housekeeping tick) can all reach it alongside
/// the compositor state.
pub struct UdevBackendState {
    /// Kept alive for the whole process: dropping it hands back the seat.
    /// Also the thing that re-opens device fds after an activate.
    session: LibSeatSession,
    /// Single GPU — see the module doc.
    drm: DrmDevice,
    #[allow(dead_code)]
    gbm: GbmDevice<DrmDeviceFd>,
    renderer: GlesRenderer,
    surfaces: Vec<SurfaceData>,
    /// CD-2 shadow workspace PiP render target, allocated lazily on first
    /// use exactly like the winit backend's own locals. Only ever composited
    /// onto the first output (index 0).
    pip_texture: Option<GlesTexture>,
    pip_damage_tracker: OutputDamageTracker,
    /// False between `SessionEvent::PauseSession` and
    /// `SessionEvent::ActivateSession`. While false we own neither DRM
    /// master nor the input fds, so every render is skipped.
    session_active: bool,
    /// Needed to arm the one-shot re-try timer that closes
    /// [`should_render`]'s throttle window. `'static` (rather than a
    /// borrowed lifetime) for the same reason anvil's `AnvilState.handle` is
    /// — a borrowed `LoopHandle<'l, CalloopData>` would put `'l` into
    /// `CalloopData`, which is the loop's own `Data` type.
    loop_handle: LoopHandle<'static, CalloopData>,
    /// Guards against stacking one re-try timer per `dispatch_render` call.
    throttle_armed: bool,
}

/// One scanout pipeline: connector → CRTC → swapchain → wayland `Output`.
struct SurfaceData {
    /// Purely informational (logs / vblank routing).
    crtc: crtc::Handle,
    connector_name: String,
    output: Output,
    /// Held so the `wl_output` global lives as long as the surface does.
    _global: GlobalId,
    gbm_surface: GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>,
    damage_tracker: OutputDamageTracker,
    /// Set by `dispatch_render` from the compositor-wide
    /// `DuduclawComp::pending_redraw`; cleared when this output is composited.
    needs_redraw: bool,
    /// True between `queue_buffer` and the matching vblank. Rendering again
    /// while true would starve the swapchain.
    awaiting_vblank: bool,
    /// When the last composite happened, for [`min_render_gap`] throttling of
    /// the no-damage path (which has no vblank to pace it).
    last_render: Instant,
    /// Cached from the output mode so the throttle doesn't re-read it.
    frame_interval: Duration,
}

/// Minimum gap between two composites of the same output when the previous
/// one produced no damage (and therefore no page flip). Pure function so the
/// scheduling rule is testable without a GPU.
///
/// `refresh_mhz` is smithay's `Mode::refresh` — millihertz, so 60 Hz is
/// `60_000`. Nonsense values (0, negative, or an implausible rate) fall back
/// to 60 Hz rather than producing a zero-length gap (which would be the busy
/// loop this whole mechanism exists to avoid) or a multi-second one.
pub fn min_render_gap(refresh_mhz: i32) -> Duration {
    const DEFAULT: u64 = 16_666_667; // 60 Hz in nanoseconds
    if !(1_000..=1_000_000).contains(&refresh_mhz) {
        return Duration::from_nanos(DEFAULT);
    }
    // period_ns = 1e9 / (refresh_mhz / 1000) = 1e12 / refresh_mhz
    Duration::from_nanos(1_000_000_000_000u64 / refresh_mhz as u64)
}

/// How long until the next housekeeping tick. Pure so the "idle costs one
/// wake-up per second" claim is a test, not a comment.
pub fn next_tick(highlight_active: bool) -> Duration {
    if highlight_active {
        TICK_HIGHLIGHT
    } else {
        TICK_IDLE
    }
}

/// The whole repaint decision, as a pure predicate, so the property this
/// work package actually promises — "no screen change ⇒ no work" — is
/// covered by a test that runs anywhere, not only on a machine with a GPU.
///
/// - `needs_redraw`: something called [`DuduclawComp::queue_redraw`] since
///   this output was last composited.
/// - `awaiting_vblank`: a page flip is already in flight; compositing again
///   now would exhaust the swapchain and gains nothing (the flip that is
///   pending will show a frame at least as new).
/// - `since_last_render` / `frame_interval`: the no-damage path has no
///   vblank to pace it, so consecutive composites are held one refresh
///   period apart.
pub fn should_render(
    needs_redraw: bool,
    awaiting_vblank: bool,
    since_last_render: Duration,
    frame_interval: Duration,
) -> bool {
    needs_redraw && !awaiting_vblank && since_last_render >= frame_interval
}

#[derive(Debug)]
pub enum UdevInitError {
    Session(String),
    NoGpu(String),
    OpenDevice(String),
    Drm(String),
    Egl(String),
    NoOutput(String),
    Loop(String),
}

impl std::fmt::Display for UdevInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, msg) = match self {
            UdevInitError::Session(m) => ("session", m),
            UdevInitError::NoGpu(m) => ("no-gpu", m),
            UdevInitError::OpenDevice(m) => ("open-device", m),
            UdevInitError::Drm(m) => ("drm", m),
            UdevInitError::Egl(m) => ("egl", m),
            UdevInitError::NoOutput(m) => ("no-output", m),
            UdevInitError::Loop(m) => ("event-loop", m),
        };
        write!(f, "udev backend init failed ({kind}): {msg}")
    }
}

impl std::error::Error for UdevInitError {}

/// Brings up the whole hardware backend. On success the caller must store
/// the returned state in [`crate::CalloopData::udev`] — every event source
/// registered here reaches it through that field and degrades to a no-op
/// while it is `None`.
pub fn init_udev(
    event_loop: &mut EventLoop<'static, CalloopData>,
    data: &mut CalloopData,
) -> Result<UdevBackendState, UdevInitError> {
    // ---- 1. session -------------------------------------------------
    // libseat. On the appliance this talks to `seatd` (Debian runs it as
    // `seatd -g video`, and the kiosk user is in `video`+`render`); on a
    // developer box with logind it transparently uses that instead.
    // LIBSEAT_BACKEND=builtin (root only) is libseat's own escape hatch and
    // needs no code here.
    let (session, session_notifier) =
        LibSeatSession::new().map_err(|e| UdevInitError::Session(format!("{e:?}")))?;
    let seat_name = session.seat();
    tracing::info!(seat = %seat_name, "udev: libseat session acquired");

    // ---- 2. pick the GPU --------------------------------------------
    let device_path = match std::env::var_os(DRM_DEVICE_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => primary_gpu(&seat_name)
            .map_err(|e| UdevInitError::NoGpu(format!("primary_gpu(): {e}")))?
            .or_else(|| {
                all_gpus(&seat_name)
                    .ok()
                    .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))
            })
            .ok_or_else(|| {
                UdevInitError::NoGpu(format!(
                    "udev reports no DRM device on seat {seat_name:?} (set {DRM_DEVICE_ENV} to override)"
                ))
            })?,
    };
    tracing::info!(device = %device_path.display(), "udev: using DRM device");

    // ---- 3. open it through the session -----------------------------
    // Going through libseat (not `open()`) is what makes this work without
    // being root and what lets the seat manager revoke the fd on a VT
    // switch.
    let mut session_for_open = session.clone();
    let fd = session_for_open
        .open(
            &device_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| UdevInitError::OpenDevice(format!("{}: {e:?}", device_path.display())))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (mut drm, drm_notifier) =
        DrmDevice::new(fd.clone(), true).map_err(|e| UdevInitError::Drm(format!("{e}")))?;
    let gbm = GbmDevice::new(fd).map_err(|e| UdevInitError::Drm(format!("gbm: {e}")))?;

    // ---- 4. renderer -------------------------------------------------
    // SAFETY: `gbm` is a live GBM device on the fd we just opened and is
    // kept alive for the whole process in `UdevBackendState`; that is
    // exactly the invariant `EGLDisplay::new` documents.
    let egl_display =
        unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|e| UdevInitError::Egl(format!("{e}")))?;
    let egl_context = EGLContext::new(&egl_display).map_err(|e| UdevInitError::Egl(format!("{e}")))?;
    // SAFETY: the context was created on this thread and is not current
    // anywhere else — same contract smithay's own winit backend satisfies.
    let renderer =
        unsafe { GlesRenderer::new(egl_context) }.map_err(|e| UdevInitError::Egl(format!("{e}")))?;
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );

    // ---- 5. connectors → outputs ------------------------------------
    let surfaces = build_surfaces(&mut drm, &allocator, &render_formats, data)?;
    if surfaces.is_empty() {
        return Err(UdevInitError::NoOutput(format!(
            "no connected connector on {} could be given a CRTC",
            device_path.display()
        )));
    }

    // ---- 6. libinput --------------------------------------------------
    // D3-f2: the session interface is wrapped so comp keeps a `dup()` of every
    // evdev fd seatd opens on libinput's behalf. That is the only way to read
    // an absolute device's real position without asking for `input`-group
    // access comp deliberately does not have (`crate::abs_pointer`'s module
    // doc has the measured permissions). `RecordingInterface` delegates every
    // decision to the wrapped `LibinputSessionInterface` — it opens nothing
    // itself, so it cannot widen what this process may touch.
    let (recording_interface, abs_pointer_table) = crate::abs_pointer::RecordingInterface::new(
        LibinputSessionInterface::<LibSeatSession>::from(session.clone()),
    );
    data.state.abs_pointer = abs_pointer_table;
    let mut libinput_context = Libinput::new_with_udev::<
        crate::abs_pointer::RecordingInterface<LibinputSessionInterface<LibSeatSession>>,
    >(recording_interface);
    libinput_context
        .udev_assign_seat(&seat_name)
        .map_err(|e| UdevInitError::Session(format!("udev_assign_seat({seat_name:?}): {e:?}")))?;
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    event_loop
        .handle()
        .insert_source(libinput_backend, move |event, _, data: &mut CalloopData| {
            // Straight into the EXISTING input path. `process_input_event`
            // is generic over `InputBackend`, so the codrive freeze /
            // Super+Esc / Super+Enter / Super+Tab semantics are literally
            // the same code the winit backend runs — there is no second
            // implementation to keep in sync.
            data.state.process_input_event(event);
            // Any input can move a cursor overlay or change focus.
            data.state.queue_redraw();
        })
        .map_err(|e| UdevInitError::Loop(format!("libinput source: {e}")))?;

    // ---- 7. DRM vblank -----------------------------------------------
    event_loop
        .handle()
        .insert_source(drm_notifier, move |event, _meta, data: &mut CalloopData| match event {
            DrmEvent::VBlank(crtc) => on_vblank(data, crtc),
            DrmEvent::Error(err) => {
                tracing::error!(?err, "udev: DRM device reported an error");
            }
        })
        .map_err(|e| UdevInitError::Loop(format!("drm notifier source: {e}")))?;

    // ---- 8. session pause/activate ------------------------------------
    let mut libinput_for_session = libinput_context;
    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, data: &mut CalloopData| match event {
            SessionEvent::PauseSession => {
                tracing::info!("udev: session paused (VT switched away) — dropping DRM master");
                libinput_for_session.suspend();
                if let Some(udev) = data.udev.as_mut() {
                    udev.session_active = false;
                    udev.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("udev: session activated — reacquiring DRM master");
                if let Err(e) = libinput_for_session.resume() {
                    tracing::error!(error = ?e, "udev: failed to resume libinput after session activate");
                }
                if let Some(udev) = data.udev.as_mut() {
                    if let Err(e) = udev.drm.activate(false) {
                        tracing::error!(error = %e, "udev: DrmDevice::activate failed");
                    }
                    for s in udev.surfaces.iter_mut() {
                        if let Err(e) = s.gbm_surface.surface().reset_state() {
                            tracing::warn!(error = %e, connector = %s.connector_name, "udev: DrmSurface::reset_state failed");
                        }
                        s.gbm_surface.reset_buffers();
                        s.awaiting_vblank = false;
                        s.needs_redraw = true;
                    }
                    udev.session_active = true;
                }
                data.state.queue_redraw();
            }
        })
        .map_err(|e| UdevInitError::Loop(format!("session notifier source: {e}")))?;

    // ---- 9. udev hotplug (observed, not acted on — see module doc) -----
    let udev_source = UdevBackend::new(&seat_name)
        .map_err(|e| UdevInitError::NoGpu(format!("UdevBackend::new({seat_name:?}): {e}")))?;
    event_loop
        .handle()
        .insert_source(udev_source, move |event, _, _data: &mut CalloopData| {
            let (kind, dev) = match event {
                UdevEvent::Added { device_id, path } => ("added", format!("{device_id} {}", path.display())),
                UdevEvent::Changed { device_id } => ("changed", device_id.to_string()),
                UdevEvent::Removed { device_id } => ("removed", device_id.to_string()),
            };
            tracing::info!(
                kind,
                device = %dev,
                "udev: DRM device hotplug event observed — NOT acted on (this backend does not rescan; restart to pick up new outputs)"
            );
        })
        .map_err(|e| UdevInitError::Loop(format!("udev source: {e}")))?;

    // ---- 10. housekeeping tick ----------------------------------------
    // The only periodic wake-up in the whole backend. See the module doc's
    // "Repaint scheduling" section for why it exists and why it is 1 Hz.
    event_loop
        .handle()
        .insert_source(Timer::immediate(), move |_, _, data: &mut CalloopData| {
            let now = Instant::now();
            // These two are per-redraw hooks on the winit backend. On an
            // on-demand backend "per redraw" can mean "never", so they get
            // their own clock.
            let frozen_before = data.state.codrive.is_frozen();
            data.state.codrive_check_watch_idle(now);
            // D3-c backstop: see the matching call in `winit_backend.rs`. On
            // this backend it is the ONLY refresh an idle desktop gets, which
            // is what stops the `ime_paused` mirror latching after the input
            // method exits.
            data.state.codrive_refresh_ime_pause();
            // A2: the per-frame reconciliation the winit backend does in its
            // redraw arm. On THIS backend it is the only clock an idle
            // desktop has, and it is the only place the main thread can
            // observe the socket thread flipping `session_active` — so a
            // co-drive session starting or ending on an otherwise-still
            // screen still produces its one `driving_mode` line, event, and
            // redraw (the mode changes both the ghost cursor and the
            // screen-edge indicator). Queues its own redraw when it fires.
            data.state.codrive_sync_mode();
            if data.state.codrive.is_frozen() != frozen_before {
                // A watch-mode idle auto-pause just flipped the agent
                // cursor between amber and dimmed red (`codrive/cursor.rs`),
                // which is a visible change nothing else would report.
                data.state.queue_redraw();
            }
            let highlight_active = data.state.codrive_highlight.is_some();
            if highlight_active {
                // Either it is still visible (it will be redrawn with the
                // right remaining time) or it just expired and the stale box
                // has to be composited away.
                data.state.queue_redraw();
            }
            TimeoutAction::ToDuration(next_tick(highlight_active))
        })
        .map_err(|e| UdevInitError::Loop(format!("housekeeping timer: {e}")))?;

    let pip_damage_tracker = OutputDamageTracker::from_output(&data.state.shadow_output);

    tracing::info!(
        outputs = surfaces.len(),
        connectors = ?surfaces.iter().map(|s| s.connector_name.as_str()).collect::<Vec<_>>(),
        "udev: backend up — duduclaw-comp now owns the display directly (no cage layer)"
    );

    Ok(UdevBackendState {
        session,
        drm,
        gbm,
        renderer,
        surfaces,
        pip_texture: None,
        pip_damage_tracker,
        session_active: true,
        loop_handle: event_loop.handle(),
        throttle_armed: false,
    })
}

/// Enumerates connectors on `drm`, and for every **connected** one picks a
/// mode + a free CRTC and builds the scanout pipeline. Outputs are laid out
/// left-to-right starting at `(0, 0)`; the codrive shadow output sits at
/// `codrive::SHADOW_ORIGIN` (`(0, 100_000)`) and can never collide with them.
fn build_surfaces(
    drm: &mut DrmDevice,
    allocator: &GbmAllocator<DrmDeviceFd>,
    render_formats: &smithay::backend::allocator::format::FormatSet,
    data: &mut CalloopData,
) -> Result<Vec<SurfaceData>, UdevInitError> {
    let res = drm
        .resource_handles()
        .map_err(|e| UdevInitError::Drm(format!("resource_handles: {e}")))?;

    let mut used_crtcs: Vec<crtc::Handle> = Vec::new();
    let mut surfaces: Vec<SurfaceData> = Vec::new();
    let mut next_x: i32 = 0;

    for conn_handle in res.connectors() {
        let info = match drm.get_connector(*conn_handle, false) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(error = %e, "udev: failed to read a connector, skipping it");
                continue;
            }
        };
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());

        if info.state() != connector::State::Connected {
            tracing::debug!(connector = %name, state = ?info.state(), "udev: connector not connected, skipping");
            continue;
        }
        let Some(drm_mode) = pick_mode(&info) else {
            tracing::warn!(connector = %name, "udev: connected connector advertises no usable mode, skipping");
            continue;
        };
        let Some(crtc) = pick_crtc(drm, &res, &info, &used_crtcs) else {
            tracing::warn!(connector = %name, "udev: no free CRTC for this connector, skipping (multi-head over-subscription)");
            continue;
        };

        let drm_surface = match drm.create_surface(crtc, drm_mode, &[info.handle()]) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(connector = %name, error = %e, "udev: create_surface failed, skipping this connector");
                continue;
            }
        };

        let gbm_surface = match GbmBufferedSurface::new(
            drm_surface,
            allocator.clone(),
            SUPPORTED_FORMATS,
            render_formats.clone(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(connector = %name, error = %e, "udev: GbmBufferedSurface::new failed, skipping this connector");
                continue;
            }
        };

        let wl_mode = WlMode::from(drm_mode);
        let (phys_w, phys_h) = info.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (phys_w as i32, phys_h as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "DuDuClaw".into(),
                model: "duduclaw-comp (drm)".into(),
            },
        );
        let global = output.create_global::<DuduclawComp>(&data.display_handle);
        // `Transform::Normal`, unlike the winit backend's `Flipped180` — the
        // flip there is a winit/EGL surface-orientation quirk, not something
        // real scanout needs.
        //
        // WP-comp-shell-display D4b-3: restore a scale chosen via a previous
        // run's `set_output_scale`, in this SAME `change_current_state` call
        // — see `winit_backend.rs::init_winit`'s matching comment for why
        // that (not a follow-up call) is what avoids every client's first
        // `wl_output.scale` announcement being wrong. Keyed by THIS
        // connector's own name (`output_prefs`'s module doc: stable across
        // boots as long as the same monitor stays on the same connector); a
        // renamed/reconnected-elsewhere output simply finds no entry and
        // boots at the implicit 100% default, same as before this round.
        let initial_scale = crate::output_prefs::load_scale_pct(&name)
            .map(|pct| smithay::output::Scale::Fractional(pct as f64 / 100.0));
        output.change_current_state(
            Some(wl_mode),
            Some(Transform::Normal),
            initial_scale,
            Some((next_x, 0).into()),
        );
        output.set_preferred(wl_mode);
        data.state.space.map_output(&output, (next_x, 0));

        tracing::info!(
            connector = %name,
            crtc = ?crtc,
            mode = ?(wl_mode.size.w, wl_mode.size.h),
            refresh_mhz = wl_mode.refresh,
            location = ?(next_x, 0),
            "udev: output created"
        );

        next_x += wl_mode.size.w;
        used_crtcs.push(crtc);
        surfaces.push(SurfaceData {
            crtc,
            connector_name: name,
            damage_tracker: OutputDamageTracker::from_output(&output),
            output,
            _global: global,
            gbm_surface,
            needs_redraw: true,
            awaiting_vblank: false,
            last_render: Instant::now() - Duration::from_secs(60),
            frame_interval: min_render_gap(wl_mode.refresh),
        });
    }

    Ok(surfaces)
}

/// Preferred mode if the connector declares one, otherwise the first mode it
/// lists (which the kernel orders best-first).
fn pick_mode(info: &connector::Info) -> Option<drm::control::Mode> {
    info.modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| info.modes().first())
        .copied()
}

/// First CRTC reachable from any of this connector's encoders that no
/// earlier connector has already taken.
fn pick_crtc(
    drm: &DrmDevice,
    res: &drm::control::ResourceHandles,
    info: &connector::Info,
    used: &[crtc::Handle],
) -> Option<crtc::Handle> {
    for enc_handle in info.encoders() {
        let Ok(enc) = drm.get_encoder(*enc_handle) else {
            continue;
        };
        for crtc in res.filter_crtcs(enc.possible_crtcs()) {
            if !used.contains(&crtc) {
                return Some(crtc);
            }
        }
    }
    None
}

/// A page flip completed. Release the buffer back to the swapchain and, if
/// something changed while it was in flight, draw the next frame right away.
fn on_vblank(data: &mut CalloopData, crtc: crtc::Handle) {
    let Some(udev) = data.udev.as_mut() else {
        return;
    };
    let Some(idx) = udev.surfaces.iter().position(|s| s.crtc == crtc) else {
        tracing::debug!(?crtc, "udev: vblank for a CRTC we don't own — ignoring");
        return;
    };
    {
        let s = &mut udev.surfaces[idx];
        s.awaiting_vblank = false;
        if let Err(e) = s.gbm_surface.frame_submitted() {
            tracing::error!(error = %e, connector = %s.connector_name, "udev: frame_submitted failed");
        }
    }
    dispatch_render(data);
}

/// Called after every calloop dispatch (from `main.rs`) and from
/// [`on_vblank`]. Renders exactly the outputs that need it.
pub fn dispatch_render(data: &mut CalloopData) {
    let CalloopData {
        state,
        display_handle,
        udev,
    } = data;
    let Some(udev) = udev.as_mut() else {
        return;
    };
    if !udev.session_active {
        return;
    }

    if state.pending_redraw {
        state.pending_redraw = false;
        for s in udev.surfaces.iter_mut() {
            s.needs_redraw = true;
        }
    }

    let now = Instant::now();
    // Shortest wait among outputs that are dirty but inside their throttle
    // window, so one timer covers all of them.
    let mut retry_after: Option<Duration> = None;
    for idx in 0..udev.surfaces.len() {
        let s = &udev.surfaces[idx];
        let since = now.duration_since(s.last_render);
        if should_render(s.needs_redraw, s.awaiting_vblank, since, s.frame_interval) {
            render_surface(state, display_handle, udev, idx);
            continue;
        }
        // Dirty but throttled: the previous composite produced no damage, so
        // there is no vblank to pace us and nothing else will come back to
        // this output. Without the timer below, a single late frame would
        // sit stale until the 1 Hz housekeeping tick.
        if s.needs_redraw && !s.awaiting_vblank && since < s.frame_interval {
            let wait = s.frame_interval - since;
            retry_after = Some(match retry_after {
                Some(w) if w <= wait => w,
                _ => wait,
            });
        }
    }

    if let (Some(wait), false) = (retry_after, udev.throttle_armed) {
        udev.throttle_armed = true;
        if let Err(e) = udev.loop_handle.insert_source(
            Timer::from_duration(wait),
            |_, _, data: &mut CalloopData| {
                if let Some(u) = data.udev.as_mut() {
                    u.throttle_armed = false;
                }
                dispatch_render(data);
                TimeoutAction::Drop
            },
        ) {
            tracing::warn!(error = %e, "udev: failed to arm the repaint retry timer");
            udev.throttle_armed = false;
        }
    }
}

fn render_surface(
    state: &mut DuduclawComp,
    display_handle: &mut DisplayHandle,
    udev: &mut UdevBackendState,
    idx: usize,
) {
    let UdevBackendState {
        renderer,
        surfaces,
        pip_texture,
        pip_damage_tracker,
        ..
    } = udev;
    let surface = &mut surfaces[idx];
    surface.needs_redraw = false;
    surface.last_render = Instant::now();

    let output = surface.output.clone();
    // Custom render elements are built in this output's own coordinate
    // space, but the two codrive cursors and the highlight box live in the
    // global logical space of `Space`. On a single output at (0, 0) — the
    // winit case and the overwhelmingly common hardware case — the offset is
    // zero and this is byte-identical to what the winit backend does.
    //
    // WP-comp-shell-display D4b-3: this same `output_geometry` call also
    // gives the output's TRUE logical SIZE (physical mode size / scale) —
    // reused below for the mode indicator's frame instead of the raw
    // physical `Mode::size` every caller here used to feed it mislabeled as
    // `Logical` (only harmless while scale was always 1.0).
    let output_geo = state.space.output_geometry(&output).unwrap_or_default();
    let output_loc = output_geo.loc;
    let offset = Point::<f64, Logical>::from((-(output_loc.x as f64), -(output_loc.y as f64)));
    // Single source of truth for every overlay element built below — see
    // `render::output_render_scale`'s own doc.
    let scale = crate::render::output_render_scale(&output);

    let now = Instant::now();
    let agent_pos = state.agent_seat.get_pointer().unwrap().current_location() + offset;
    // A2: see the matching comment in `winit_backend.rs` — the mode decides
    // whether an agent pointer is drawn at all, in what colour, and whether
    // the screen-edge indicator appears.
    let driving_mode = state.codrive_driving_mode();

    // CUR-1: the human cursor is built first so it sorts above every other
    // custom element, and it takes `offset` itself (it reads the pointer
    // position internally) rather than a pre-offset position, unlike the
    // agent cross below.
    let mut elements: Vec<CodriveElement> = state.build_human_cursor_elements(renderer, offset, scale);
    elements.extend(
        crate::codrive::build_agent_cursor_elements(agent_pos, driving_mode, scale)
            .into_iter()
            .map(CodriveElement::Solid),
    );
    elements.extend(
        state
            .codrive_highlight_elements_at(now, offset, scale)
            .into_iter()
            .map(CodriveElement::Solid),
    );
    // A2 (DESIGN §3.3.2(d)): the screen-edge frame, on EVERY output — a
    // co-drive session is a property of the session, not of one monitor.
    // Deliberately NOT offset by `-output.loc` the way the highlight box
    // above is: the frame is defined in the output's OWN space (its origin,
    // its mode size), which is already the space these elements are
    // interpreted in — applying the global→local offset here would push a
    // second monitor's frame off its own screen. See
    // `codrive/mode_indicator.rs`'s "Coordinate space" section.
    let indicator_size = output_geo.size;
    elements.extend(
        crate::codrive::build_mode_indicator_elements(indicator_size, driving_mode, scale)
            .into_iter()
            .map(CodriveElement::Solid),
    );
    state.codrive_check_watch_idle(now);
    // A2: same per-frame reconciliation as the housekeeping tick — a
    // compositing desktop should not have to wait up to a second for the
    // mode to catch up with a session that just started or ended.
    state.codrive_sync_mode();

    // PiP preview of the shadow workspace goes on the first output only —
    // it is a single fixed-corner overlay, not a per-monitor decoration.
    if idx == 0 {
        let mode_size = surface.gbm_surface.current_mode().size();
        let size = smithay::utils::Size::from((mode_size.0 as i32, mode_size.1 as i32));
        if let Some(pip) = state.codrive_render_pip(renderer, pip_texture, pip_damage_tracker, size) {
            elements.push(CodriveElement::Pip(pip));
        }
    }

    // WM-2: fold the per-window decorations and the windows themselves into
    // one interleaved list. Replaces `desktop::space::render_output`, which
    // could only stack every custom element above every window — see
    // `decor/paint.rs`'s module doc. Built BEFORE `next_buffer`/`bind`, like
    // the cursor and PiP elements above, so nothing holds a second mutable
    // borrow of the renderer across the bind.
    let elements = state.build_output_elements(renderer, &output, elements);

    let (mut dmabuf, age) = match surface.gbm_surface.next_buffer() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, connector = %surface.connector_name, "udev: next_buffer failed — skipping this frame");
            return;
        }
    };

    let damage: Option<Vec<smithay::utils::Rectangle<i32, smithay::utils::Physical>>>;
    let sync;
    {
        let mut framebuffer = match renderer.bind(&mut dmabuf) {
            Ok(fb) => fb,
            Err(e) => {
                tracing::error!(error = %e, connector = %surface.connector_name, "udev: renderer.bind failed — skipping this frame");
                return;
            }
        };
        match surface.damage_tracker.render_output(
            renderer,
            &mut framebuffer,
            age as usize,
            &elements,
            CLEAR_COLOR,
        ) {
            Ok(res) => {
                damage = res.damage.cloned();
                sync = res.sync;
            }
            Err(e) => {
                tracing::error!(error = ?e, connector = %surface.connector_name, "udev: render_output failed — skipping this frame");
                return;
            }
        }
    }

    if let Some(damage) = damage {
        // Something actually changed — pay for a page flip.
        match surface.gbm_surface.queue_buffer(Some(sync), Some(damage), ()) {
            Ok(()) => surface.awaiting_vblank = true,
            Err(e) => {
                tracing::error!(error = %e, connector = %surface.connector_name, "udev: queue_buffer failed");
            }
        }
    } else {
        // Pixel-identical frame. Do NOT flip: the scanout already shows this
        // exact content. This branch is the entire reason idle CPU is not
        // pinned the way the cage+winit stack was.
        tracing::trace!(connector = %surface.connector_name, "udev: no damage — skipping page flip");
    }

    let elapsed = state.start_time.elapsed();
    state.space.elements().for_each(|window| {
        window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()))
    });
    // CUR-1: see the identical call in `winit_backend.rs` — a client-provided
    // cursor surface lives outside `space` and needs its own frame callback.
    state.send_cursor_frames(&output, elapsed);
    // WM-3: and so do layer surfaces (`crate::layer_shell`).
    state.send_layer_frames_and_cleanup(&output, elapsed);
    state.space.refresh();
    state.popups.cleanup();
    let _ = display_handle.flush_clients();
}

impl UdevBackendState {
    /// Seat name this backend runs on — logged at shutdown and useful in
    /// tests/diagnostics.
    pub fn seat_name(&self) -> String {
        self.session.seat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixty_hertz_gap_is_one_frame() {
        let gap = min_render_gap(60_000);
        assert!(
            gap >= Duration::from_micros(16_600) && gap <= Duration::from_micros(16_700),
            "60 Hz must map to ~16.67 ms, got {gap:?}"
        );
    }

    #[test]
    fn high_refresh_rates_get_a_shorter_gap() {
        assert!(min_render_gap(144_000) < min_render_gap(60_000));
        assert!(min_render_gap(240_000) < min_render_gap(144_000));
    }

    #[test]
    fn nonsense_refresh_rates_never_produce_a_zero_gap() {
        // A zero gap would reinstate exactly the unbounded repaint loop this
        // throttle exists to prevent, so every degenerate input must fall
        // back to a real frame time.
        for bogus in [0, -1, i32::MIN, i32::MAX, 999, 1_000_001] {
            let gap = min_render_gap(bogus);
            assert!(
                gap >= Duration::from_micros(16_600) && gap <= Duration::from_micros(16_700),
                "refresh={bogus} should fall back to 60 Hz, got {gap:?}"
            );
        }
    }

    #[test]
    fn boundary_refresh_values_are_accepted_not_clamped() {
        assert_eq!(min_render_gap(1_000), Duration::from_nanos(1_000_000_000));
        assert_eq!(min_render_gap(1_000_000), Duration::from_nanos(1_000_000));
    }

    #[test]
    fn idle_housekeeping_is_one_wakeup_per_second() {
        assert_eq!(next_tick(false), Duration::from_secs(1));
    }

    const FRAME: Duration = Duration::from_micros(16_667);
    const LONG_AGO: Duration = Duration::from_secs(60);

    #[test]
    fn nothing_dirty_means_no_work_at_all() {
        // The whole point of this work package: an idle desktop must not
        // composite, no matter how long it has been since the last frame.
        assert!(!should_render(false, false, LONG_AGO, FRAME));
    }

    #[test]
    fn dirty_and_free_renders_immediately() {
        assert!(should_render(true, false, LONG_AGO, FRAME));
    }

    #[test]
    fn a_flip_in_flight_blocks_the_next_composite() {
        // Rendering again before the vblank would drain the swapchain and
        // show nothing newer than the flip already pending.
        assert!(!should_render(true, true, LONG_AGO, FRAME));
    }

    #[test]
    fn back_to_back_dirty_frames_are_held_to_one_refresh_period() {
        // This is the no-damage path (no page flip ⇒ no hardware pacing).
        assert!(!should_render(true, false, Duration::ZERO, FRAME));
        assert!(!should_render(true, false, FRAME - Duration::from_micros(1), FRAME));
        assert!(should_render(true, false, FRAME, FRAME));
    }

    #[test]
    fn a_counting_down_highlight_tightens_the_tick() {
        let tick = next_tick(true);
        assert!(tick < next_tick(false));
        // Must stay under the 100 ms floor `clamp_highlight_ms` enforces, or
        // the shortest legal highlight could expire between two ticks and
        // visibly overstay.
        assert!(tick <= Duration::from_millis(100));
    }
}
