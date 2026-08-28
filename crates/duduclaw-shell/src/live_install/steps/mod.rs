// Y20-P2 (2026-08-29) — per-step content dispatcher, mirrors `oobe::steps`'s
// own dispatcher shape (see that module's own header comment) at this
// flow's much smaller 4-step scope. `render.rs`'s frame owns the chrome
// (background, progress dots, bottom nav); each module below owns only the
// middle content area for one step.
//
// Y20-P3 (2026-08-29): `disk_select`/`confirm` now need `cx` too (a
// background-thread `lsblk` scan click, a checkbox toggle click — both real
// `cx.listener` closures, same reason `language::render` already needed
// it). `progress` stays `cx`-free — it is pure rendering of `LiveInstallFlow
// ::install()`, driven by `install_runner` rather than any click of its
// own (see that step's own header comment).

mod confirm;
mod disk_select;
mod language;
mod progress;

use gpui::{Context, Div};

use super::{LiveInstallFlow, LiveInstallStep};
use crate::ShellView;

pub(super) fn render(step: LiveInstallStep, flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    match step {
        LiveInstallStep::Language => language::render(flow, cx),
        LiveInstallStep::DiskSelect => disk_select::render(flow, cx),
        LiveInstallStep::Confirm => confirm::render(flow, cx),
        LiveInstallStep::Progress => progress::render(flow),
    }
}
