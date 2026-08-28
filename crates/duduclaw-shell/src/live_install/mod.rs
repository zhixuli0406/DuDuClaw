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
// `DiskSelect`/`Progress` are HONEST PLACEHOLDERS this round — no real disk
// enumeration, no real `dd`/write progress. Both are explicitly deferred to
// P3 per the task brief ("真實列碟/dd 進度留 P3"); see each step's own
// header comment in `steps/`.

mod render;
mod state;
mod steps;

pub(crate) use render::render;
pub(crate) use state::{LiveInstallFlow, LiveInstallStep};

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
