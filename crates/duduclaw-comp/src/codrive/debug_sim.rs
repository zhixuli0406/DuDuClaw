// CD-0 codrive spike — debug-only human-input / emergency-stop simulator.
//
// Headless nested weston (this crate's container-level live-run host, see
// BUILD.md "Nested headless live-run") advertises literally zero input
// devices — `duduclaw-shell`'s BUILD-LINUX.md documents the identical
// upstream constraint independently ("the wl_seat finding"). That means the
// REAL human-input path (`input.rs::process_input_event` →
// `DuduclawComp::on_human_input`, wired to actual winit-forwarded
// keyboard/pointer events) can never fire inside this container, and
// neither can the real Super+Esc/Super+Enter detectors in `input.rs`'s
// keyboard filter closure. All three are implemented for real hardware and
// exercised on a real seat in the VM/`cage` round (matching the same
// "implemented but container-unverified, VM-verified later" pattern
// BUILD.md already documents for `grabs/move_grab.rs`/`resize_grab.rs`).
//
// This module exists ONLY so this round's container-level verification can
// still exercise the freeze/resume/emergency-stop *state machine*
// end-to-end (does the flag flip, does it log with a latency figure, does
// the connection get force-closed — all logic strictly downstream of "a
// human input event happened," not the hardware event delivery itself). It
// is opt-in via `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` — unset (the default,
// including any real deployment), this reads nothing from stdin and
// registers nothing with the event loop.

use std::io::BufRead;

use smithay::{
    input::pointer::MotionEvent,
    reexports::calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{CalloopData, DuduclawComp};

/// Registers a stdin-reading calloop source that turns four magic lines
/// into synthetic human-seat events, iff `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` is
/// set. Lines recognized: `simulate_human` (calls `on_human_input`, the
/// same entry point `input.rs` calls for a real event), `simulate_super_esc`
/// (calls `emergency_stop` directly — the real Super+Esc detector lives in
/// `input.rs` and is not reachable from here), `simulate_super_enter`
/// (CD-1: calls `human_resume` directly — the real Super+Enter detector
/// likewise lives in `input.rs` and is not reachable from here), and
/// `simulate_super_tab` (WP-A1 multi-window round: calls `cycle_focus`
/// directly — same reasoning, the real Super+Tab keysym detector lives in
/// `input.rs`'s keyboard filter closure and needs a real keyboard device to
/// reach). All four match "this only drives the state machine, not the
/// hardware detection path" above.
///
/// CUR-1 adds a fifth: `simulate_pointer <x> <y>` moves the HUMAN pointer to
/// a logical position — see the match arm's own comment for why the cursor
/// work package needed it.
pub fn maybe_init_stdin_simulator(event_loop: &mut EventLoop<CalloopData>) {
    if std::env::var_os("DUDUCLAW_CODRIVE_DEBUG_STDIN").is_none() {
        return;
    }

    tracing::warn!(
        "codrive: DEBUG_STDIN human-input simulator ENABLED — reading lines from stdin \
         (\"simulate_human\" / \"simulate_super_esc\" / \"simulate_super_enter\" / \
         \"simulate_super_tab\" / \"simulate_pointer <x> <y>\"). This exists only for \
         headless-container verification \
         where no real input device can originate the events (see codrive/debug_sim.rs \
         module doc) — never set this env var in a real deployment."
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
                "simulate_super_enter" => {
                    tracing::info!("codrive: debug stdin — simulating Super+Enter human resume");
                    data.state.human_resume();
                }
                // CUR-1 (2026-08-22): the only addition that is not about the
                // codrive state machine. It exists because nested headless
                // weston advertises zero input devices (see this module's
                // header), so without it there is no way to move the HUMAN
                // pointer inside a container — and "does the cursor become an
                // I-beam over a text area" is precisely a
                // pointer-enters-a-surface question. Same opt-in gate, same
                // "drives the state machine, not the hardware detection path"
                // reasoning as the four commands above.
                other if other.starts_with("simulate_pointer") => {
                    match parse_pointer_args(other) {
                        Some((x, y)) => {
                            tracing::info!(x, y, "codrive: debug stdin — simulating human pointer motion");
                            simulate_human_pointer(&mut data.state, x, y);
                        }
                        None => {
                            tracing::warn!(
                                line = other,
                                "codrive: debug stdin — simulate_pointer needs two finite \
                                 numbers, e.g. \"simulate_pointer 320 240\""
                            );
                        }
                    }
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

/// Parses `simulate_pointer <x> <y>` into a logical position.
///
/// Pure and unit-tested (unlike everything else in this module, which needs a
/// live event loop) — argument parsing is the only part with a wrong answer
/// available, and `NaN`/`inf` reaching `PointerHandle::motion` would poison
/// every later `surface_under` comparison.
fn parse_pointer_args(line: &str) -> Option<(f64, f64)> {
    let mut it = line.split_whitespace();
    if it.next()? != "simulate_pointer" {
        return None;
    }
    let x: f64 = it.next()?.parse().ok()?;
    let y: f64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some((x, y))
}

/// Moves the HUMAN seat's pointer to a logical position, exactly the way
/// `input.rs`'s `PointerMotionAbsolute` arm does once it has resolved a
/// position: notify the codrive freeze machinery, then `motion` + `frame`
/// through the seat's own pointer handle so focus enter/leave — and therefore
/// the client's `wl_pointer.set_cursor` / `wp_cursor_shape` request — happen
/// for real.
fn simulate_human_pointer(state: &mut DuduclawComp, x: f64, y: f64) {
    // A real event would freeze the agent seat; a simulated one must too, or
    // this backdoor would be a way to bypass the codrive safety model in a
    // build where the debug flag is (wrongly) enabled.
    state.on_human_input("debug_stdin_simulated_pointer");

    let pos = Point::<f64, Logical>::from((x, y));
    let serial = SERIAL_COUNTER.next_serial();
    let pointer = state.seat.get_pointer().expect("human seat always has a pointer");
    let under = state.surface_under(pos);
    pointer.motion(
        state,
        under,
        &MotionEvent {
            location: pos,
            serial,
            time: 0,
        },
    );
    pointer.frame(state);
    state.queue_redraw();
}

#[cfg(test)]
mod tests {
    use super::parse_pointer_args;

    #[test]
    fn parses_two_numbers() {
        assert_eq!(parse_pointer_args("simulate_pointer 320 240"), Some((320.0, 240.0)));
        assert_eq!(parse_pointer_args("simulate_pointer  1.5   -2.25 "), Some((1.5, -2.25)));
    }

    #[test]
    fn rejects_wrong_arity_and_junk() {
        for line in [
            "simulate_pointer",
            "simulate_pointer 10",
            "simulate_pointer 10 20 30",
            "simulate_pointer x y",
            "simulate_pointerish 1 2",
            "",
        ] {
            assert!(parse_pointer_args(line).is_none(), "{line:?} should not parse");
        }
    }

    #[test]
    fn rejects_non_finite_positions() {
        // NaN compares false against everything, so a NaN pointer position
        // would make every later hit test silently miss.
        for line in ["simulate_pointer NaN 0", "simulate_pointer 0 inf", "simulate_pointer -inf 0"] {
            assert!(parse_pointer_args(line).is_none(), "{line:?} should not parse");
        }
    }
}
