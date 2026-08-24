//! duduclaw-comp — Shell-S0 smithay spike.
//!
//! Adapted from smithay's `smallvil` example (MIT License):
//! <https://github.com/Smithay/smithay/tree/v0.7.0/smallvil>
//! Copyright (c) Victor Berger, Drakulix (Victoria Brekenfeld), and the
//! Smithay contributors. See <https://github.com/Smithay/smithay/blob/v0.7.0/LICENSE.txt>.
//!
//! This is intentionally the anvil/cage-weight end of the spectrum, not a
//! feature-complete compositor: single output, xdg-shell server side, a
//! `winit` nested backend (runs as a window inside whatever Wayland/X11
//! session invoked it), and minimal move/resize window management. It
//! exists to answer one question for DuDuClaw OS L1 — "can we build our own
//! Wayland compositor" — not to replace anvil. See
//! `commercial/docs/DESIGN-native-gui-gpui-2026-08.md` §13.5 for the design
//! context (D11: smithay self-built compositor, MIT, closed-source-capable).
//!
//! What this spike deliberately does NOT carry over from smallvil/anvil:
//! - ~~No layer-shell server side yet~~ — **WM-3 (2026-08-23) added it**
//!   (`crate::layer_shell`): `zwlr_layer_shell_v1`, the four-band z-order,
//!   exclusive zones feeding the window work area, and pointer routing on both
//!   sides of the window stack. `duduclaw-shell` does not use it yet; see that
//!   module's scope note.
//! - No XWayland support.
//! - No screen-copy protocols.
//!
//! **A4-1 (2026-08-22) ended the "nested backend only" limitation** stated in
//! the paragraph above. There are now
//! two backends in this one binary, picked at runtime by
//! `backend_choice::choose_backend`:
//! - `winit_backend.rs` — nested inside a host Wayland/X11 session
//!   (development, CI, BUILD.md's container rounds).
//! - `udev_backend.rs` — libseat + DRM/KMS + libinput, owning real hardware.
//!   See that module's doc comment for exactly what it does and does not
//!   support.

mod handlers;

mod abs_pointer;
mod alt_tab;
mod backend_choice;
mod codrive;
mod cursor;
mod decor;
mod grabs;
mod ime;
mod input;
mod layer_shell;
mod minimize;
mod output_prefs;
mod render;
mod seat_order;
mod session_lock;
mod shell_control;
mod state;
mod switcher;
mod udev_backend;
mod window_policy;
mod winit_backend;

use smithay::reexports::{
    calloop::EventLoop,
    wayland_server::{Display, DisplayHandle},
};

use backend_choice::BackendKind;

pub use state::DuduclawComp;

/// Bundles the compositor state together with the `wayland-server` display
/// handle so both can be threaded through calloop callbacks as one value.
pub struct CalloopData {
    state: DuduclawComp,
    display_handle: DisplayHandle,
    /// A4-1: `Some` only on the udev/DRM backend. Every calloop source the
    /// udev backend registers reaches its state through this field and
    /// degrades to a no-op while it is `None` — that is what keeps the winit
    /// path from paying for (or being perturbed by) hardware plumbing it
    /// never uses.
    udev: Option<udev_backend::UdevBackendState>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;

    let display: Display<DuduclawComp> = Display::new()?;
    let display_handle = display.handle();
    let state = DuduclawComp::new(&mut event_loop, display);

    let mut data = CalloopData {
        state,
        display_handle,
        udev: None,
    };

    // A4-1: runtime backend selection. See `backend_choice.rs` for the rule
    // and `udev_backend.rs` for what the hardware path does.
    let backend = crate::backend_choice::choose_backend_from_env()?;
    tracing::info!(
        backend = backend.as_str(),
        wayland_display = ?std::env::var("WAYLAND_DISPLAY").ok(),
        display = ?std::env::var("DISPLAY").ok(),
        "duduclaw-comp: selected display backend"
    );
    match backend {
        BackendKind::Winit => crate::winit_backend::init_winit(&mut event_loop, &mut data)?,
        BackendKind::Udev => {
            // Deliberately NOT falling back to winit on failure: on a bare
            // TTY there is nothing to nest inside, so a "fallback" would
            // just be a second, more confusing error. Fail loudly with the
            // real reason (missing seatd, no DRM device, no CRTC, …).
            let udev = crate::udev_backend::init_udev(&mut event_loop, &mut data)?;
            tracing::info!(seat = %udev.seat_name(), "duduclaw-comp: udev backend ready");
            data.udev = Some(udev);
        }
    }

    // CD-0 codrive spike verification aid — see codrive/debug_sim.rs module
    // doc. No-op (reads nothing, registers nothing) unless
    // DUDUCLAW_CODRIVE_DEBUG_STDIN=1 is set; never set that in a real
    // deployment.
    //
    // Q1 (2026-08-24): "never set that in a real deployment" is now enforced
    // rather than advised. This simulator injects synthetic HUMAN-seat input
    // — including `simulate_super_esc` (the agent emergency stop) and
    // `simulate_super_enter` (hand control back) — which is exactly the
    // provenance guarantee `input.rs`'s filter closure is built around, so a
    // shipping binary must not carry the path at all. The gate is a Cargo
    // feature (`debug-affordances`, off by default) rather than an env check,
    // because the appliance's kiosk launcher sources
    // `/etc/duduclaw/kiosk.env` with `set -a` into the session tree on a
    // read-write root: one file write would otherwise re-enable it,
    // persistently and invisibly. `codrive/` itself is untouched — the gate
    // lives here, at the one call site.
    //
    // `if cfg!(...)` rather than `#[cfg(...)]` deliberately: the item stays
    // REFERENCED, so gating it does not turn `maybe_init_stdin_simulator`
    // (and everything reachable only from it) into a dead-code warning inside
    // a module this round is not allowed to edit. The condition is a
    // compile-time constant, so the call is folded away exactly as an
    // attribute would fold it, and the simulator is never reached in a
    // shipping build either way.
    if cfg!(feature = "debug-affordances") {
        codrive::maybe_init_stdin_simulator(&mut event_loop);
    }

    // Spawn an optional test client so a run of this binary is
    // self-verifying: `-c/--command <client>` picks the wl client to launch
    // against the freshly created socket (e.g. `foot`, `weston-terminal`,
    // `weston-simple-egl`). With no argument we don't guess — nothing is
    // spawned and the compositor just sits there listening, since the spike
    // environment has no client installed by default (see BUILD.md's next-
    // run plan for what to install alongside this).
    let mut args = std::env::args().skip(1);
    let flag = args.next();
    let arg = args.next();

    if let (Some("-c") | Some("--command"), Some(command)) = (flag.as_deref(), arg) {
        std::process::Command::new(command).spawn().ok();
    }

    tracing::info!(
        socket_name = ?data.state.socket_name,
        "duduclaw-comp listening; export WAYLAND_DISPLAY to connect a client"
    );

    // Timeout `None` = block in `epoll` until an event source fires. On the
    // udev backend that is what makes "nothing changed" cost literally zero
    // CPU: nothing schedules a redraw, so nothing wakes us (bar the udev
    // backend's own 1 Hz housekeeping tick). The winit backend keeps its
    // own `request_redraw()` self-perpetuating loop and is unaffected.
    event_loop.run(None, &mut data, move |data| {
        // No-op unless the udev backend is active; see `dispatch_render`.
        crate::udev_backend::dispatch_render(data);
        // Flush pending events to clients EVERY loop iteration, not just on
        // the render path. Found live on the appliance VM (2026-08-22, A4-1
        // acceptance): `render_surface` ends with its own `flush_clients()`,
        // but `dispatch_render` returns early whenever nothing is dirty — so
        // a client that had just connected and was still doing its opening
        // `wl_registry` / `wl_display.sync` roundtrip never had those replies
        // written back to its socket. Nothing it does during that roundtrip
        // damages an output, so nothing ever scheduled the render that would
        // have flushed them. `duduclaw-shell` hung forever in `do_sys_poll`
        // on a single thread at 0% CPU with comp showing only "xdg client
        // connected" — i.e. the appliance would boot to an empty desktop.
        // The winit backend never showed this because its self-perpetuating
        // `request_redraw()` loop flushes every frame regardless of damage,
        // which is exactly the property the udev backend deliberately drops.
        // Idle cost is unchanged: with a `None` timeout this callback only
        // runs after a source actually fired, so an idle compositor still
        // blocks in `epoll` and flushes nothing.
        let _ = data.display_handle.flush_clients();
    })?;

    Ok(())
}
