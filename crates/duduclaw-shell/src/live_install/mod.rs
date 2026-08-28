// Y20-P2 (2026-08-29) — the live-image graphical installer skeleton. See
// `commercial/docs/DESIGN-graphical-installer-2026-08.md` §3 for the full
// Y20 route-A design: a live boot runs the COMPLETE desktop stack (kiosk
// `User=root`, per Y20-P1) plus a graphical installer instead of the
// appliance's normal image-flash-then-reboot path. P1 (already PASS) baked
// `/etc/duduclaw-live` into the live image's rootfs and verified it reads
// back `MARKER_OK`; this round wires that marker to an actual render mode.
//
// ── Why a SEPARATE state machine, not another `OobeStep` ──────────────────
// `main.rs`'s render root already picks between several mutually exclusive
// full-screen modes via one if-else chain: OOBE, the lockscreen, the
// single-window Home, the layer-surface desktop. A live-install boot is a
// FIFTH such mode, checked first — it is not a fork inside OOBE's own
// ten-step flow. `OobeStep::ALL` is a hard-locked `[OobeStep; 10]` array
// with 30+ existing tests keyed to its exact order and count (`oobe::state`'s
// own header comment) — extending it, or branching inside `OobeFlow`, was
// explicitly ruled out for this task; this module is additive-only and
// leaves every `oobe::*` file byte-identical apart from the one visibility
// widening `widgets.rs`'s own header comment documents.
//
// ── P2 scope ────────────────────────────────────────────────────────────
// The 4-step state machine (`LiveInstallStep`/`LiveInstallFlow` — shape
// copied from `oobe::state`'s `OobeStep`/`OobeFlow`, see `state.rs`'s own
// header comment for the pattern this mirrors and where it deliberately
// simplifies), a render dispatcher that can walk forward/back through all
// four steps, and `main.rs` wiring: an unconditional (never `shipping::
// debug_env`-gated — see that call site's own comment for why) `/etc/
// duduclaw-live` existence check that decides whether this flow, instead of
// normal OOBE, is what a fresh boot lands on.
//
// `DiskSelect`/`Progress` were HONEST PLACEHOLDERS in P2 — no real disk
// enumeration, no real `dd`/write progress. See "Y20-P3" below for what
// replaces them.
//
// ── Y20-P3 (2026-08-29): the real end-to-end flow ──────────────────────────
// `disk_select`/`confirm`/`progress` are now wired to real I/O: a
// background-thread `lsblk` scan feeds `DiskSelect`'s pick list, `Confirm`'s
// destructive-write checkbox gates the shared bottom-nav action, and that
// same action (relabeled "開始安裝"/"重新開機" per step — see `render.rs`'s
// own header comment) drives the actual `duduclaw-os-install` child process
// and, once it reports success, a real reboot. The new `install_runner`
// module owns that child-process lifecycle (spawn, streamed
// `DUDUCLAW_PROGRESS:` parsing, the terminal reboot) — kept OUT of
// `steps::progress`, which stays pure rendering of whatever `LiveInstallFlow
// ::install()` currently says, same "state machine vs. the I/O that drives
// it" split `steps::disk_select`'s own scan already follows.

mod install_runner;
mod render;
mod state;
mod steps;

pub(crate) use render::render;
pub(crate) use state::{DiskInfo, DiskScanState, InstallState, LiveInstallFlow, LiveInstallStep};

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing `main.rs`'s render root actually depends on structurally
    /// — `live_install::render` must exist and be callable the same shape
    /// `oobe::render`/`lockscreen::render::render` are (a plain fn taking the
    /// flow + a `Context<ShellView>`). Exercised for real by every test in
    /// `state.rs`/`render.rs`/`steps/*.rs`; this is just a compile-time
    /// smoke check that the module's public re-export surface is wired.
    #[test]
    fn live_install_flow_starts_on_language_step() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.current(), LiveInstallStep::Language);
    }
}
