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
//! - No DRM/udev/libinput backend — `winit`-only, so it always runs nested
//!   inside a host compositor/X server rather than owning real hardware.
//! - No layer-shell server side yet (DESIGN §13.5 calls for it on L1
//!   eventually; smallvil doesn't implement it either, so this spike stays
//!   at parity with upstream rather than adding scope).
//! - No XWayland support.
//! - No screen-copy / damage-tracking protocols beyond what `desktop::space`
//!   gives for free.

mod handlers;

mod codrive;
mod grabs;
mod input;
mod state;
mod winit_backend;

use smithay::reexports::{
    calloop::EventLoop,
    wayland_server::{Display, DisplayHandle},
};

pub use state::DuduclawComp;

/// Bundles the compositor state together with the `wayland-server` display
/// handle so both can be threaded through calloop callbacks as one value.
pub struct CalloopData {
    state: DuduclawComp,
    display_handle: DisplayHandle,
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
    };

    crate::winit_backend::init_winit(&mut event_loop, &mut data)?;

    // CD-0 codrive spike verification aid — see codrive/debug_sim.rs module
    // doc. No-op (reads nothing, registers nothing) unless
    // DUDUCLAW_CODRIVE_DEBUG_STDIN=1 is set; never set that in a real
    // deployment.
    codrive::maybe_init_stdin_simulator(&mut event_loop);

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

    event_loop.run(None, &mut data, move |_| {
        // duduclaw-comp is running
    })?;

    Ok(())
}
