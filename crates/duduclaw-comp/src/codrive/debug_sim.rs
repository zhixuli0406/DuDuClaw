// CD-0 codrive spike — debug-only human-input / emergency-stop simulator.
//
// Headless nested weston (this crate's container-level live-run host, see
// BUILD.md "Nested headless live-run") advertises literally zero input
// devices — `duduclaw-shell`'s BUILD-LINUX.md documents the identical
// upstream constraint independently ("the wl_seat finding"). That means the
// REAL human-input path (`input.rs::process_input_event` →
// `DuduclawComp::on_human_input`, wired to actual winit-forwarded
// keyboard/pointer events) can never fire inside this container, and
// neither can the real Super+Esc detector in `input.rs`'s keyboard filter
// closure. Both are implemented for real hardware and exercised on a real
// seat in the VM/`cage` round (matching the same "implemented but
// container-unverified, VM-verified later" pattern BUILD.md already
// documents for `grabs/move_grab.rs`/`resize_grab.rs`).
//
// This module exists ONLY so this round's container-level verification can
// still exercise the freeze/emergency-stop *state machine* end-to-end (does
// the flag flip, does it log with a latency figure, does the connection get
// force-closed — all logic strictly downstream of "a human input event
// happened," not the hardware event delivery itself). It is opt-in via
// `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` — unset (the default, including any real
// deployment), this reads nothing from stdin and registers nothing with the
// event loop.

use std::io::BufRead;

use smithay::reexports::calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction};

use crate::CalloopData;

/// Registers a stdin-reading calloop source that turns two magic lines into
/// synthetic human-seat events, iff `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` is set.
/// Lines recognized: `simulate_human` (calls `on_human_input`, the same
/// entry point `input.rs` calls for a real event) and `simulate_super_esc`
/// (calls `emergency_stop` directly — the real Super+Esc detector lives in
/// `input.rs` and is not reachable from here, matching "this only drives
/// the state machine, not the hardware detection path" above).
pub fn maybe_init_stdin_simulator(event_loop: &mut EventLoop<CalloopData>) {
    if std::env::var_os("DUDUCLAW_CODRIVE_DEBUG_STDIN").is_none() {
        return;
    }

    tracing::warn!(
        "codrive: DEBUG_STDIN human-input simulator ENABLED — reading lines from stdin \
         (\"simulate_human\" / \"simulate_super_esc\"). This exists only for headless- \
         container verification where no real input device can originate the events \
         (see codrive/debug_sim.rs module doc) — never set this env var in a real \
         deployment."
    );

    let stdin = std::io::stdin();
    let source = Generic::new(stdin, Interest::READ, Mode::Level);
    let mut line = String::new();

    event_loop
        .handle()
        .insert_source(source, move |_readiness, stdin, data: &mut CalloopData| {
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => {
                    // EOF on stdin (e.g. the launching shell closed it) —
                    // stop watching rather than busy-looping on repeated
                    // zero-byte reads.
                    return Ok(PostAction::Remove);
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(PostAction::Continue);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "codrive: debug stdin read error — continuing");
                    return Ok(PostAction::Continue);
                }
            }

            match line.trim() {
                "simulate_human" => {
                    tracing::info!("codrive: debug stdin — simulating human input");
                    data.state.on_human_input("debug_stdin_simulated");
                }
                "simulate_super_esc" => {
                    tracing::info!("codrive: debug stdin — simulating Super+Esc emergency stop");
                    data.state.emergency_stop("debug_stdin_simulated_super_esc");
                }
                "" => {}
                other => {
                    tracing::warn!(line = other, "codrive: debug stdin — unrecognized command, ignoring");
                }
            }

            Ok(PostAction::Continue)
        })
        .expect("codrive: failed to register the debug stdin simulator source");
}
