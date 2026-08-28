// Y20-P3 (2026-08-29) — real Progress step, replacing the P2 static-0%
// placeholder. Renders whatever `LiveInstallFlow::install()` currently says
// — `install_runner::start_install` (kicked off from Confirm's own
// bottom-nav action, see that fn's own header comment) is the ONLY writer
// of this state; this file is pure rendering, no I/O of its own, same
// "state machine vs. the I/O that drives it" split `steps::disk_select`'s
// own scan already follows.
//
// `Running { percent: Some(_), .. }` paints the determinate fill-bar shape
// `overlay/controlcenter.rs`'s own volume track already establishes
// (`.relative()` on the track + `.absolute()` + `.w(relative(fraction))` on
// the fill — see that file's own `volume_slider_row`/`track` code for the
// pattern this borrows); `percent: None` (no `DUDUCLAW_PROGRESS:` sample has
// arrived yet, or the target build has no `pv` — see `duduclaw-os-
// install.sh`'s own fallback) renders the SAME bar shape at a fixed low
// fill with an "unknown" label instead of animating a fake sweep — an
// honest "we don't know the real fraction yet", not a fabricated animation.
//
// No separate "重新開機" button lives in THIS file's own card — that action
// is the shared bottom-nav slot, relabeled for this step (see `render.rs`'s
// own header comment for why: same "one shared action slot, not a second
// step-owned button" reasoning `confirm.rs`'s own header comment gives for
// "開始安裝"). This file's `Done` body only shows the completion TEXT.

use gpui::{div, prelude::*, px, relative, Div};

use duduclaw_native_gui::theme;

use crate::oobe::widgets;
use crate::palette::ShellPalette;

use super::super::{InstallState, LiveInstallFlow};

pub(super) fn render(flow: &LiveInstallFlow) -> Div {
    let palette = flow.palette();

    let (headline, body): (&'static str, Div) = match flow.install() {
        InstallState::Idle => ("準備安裝 · Preparing", idle_body(palette)),
        InstallState::Running { percent, status } => ("安裝進度 · Installing", running_body(*percent, status, palette)),
        InstallState::Done => ("安裝完成 · Installation complete", done_body(palette)),
        InstallState::Failed(message) => ("安裝失敗 · Installation failed", failed_body(message, palette)),
    };

    div().flex().flex_col().items_center().gap(px(20.)).child(widgets::title(headline, palette)).child(widgets::card(body, palette))
}

/// The determinate/indeterminate fill-bar track. `percent: None` renders a
/// small fixed fill (never animated — this crate has no per-frame render
/// hook cheap enough to sweep a fake bar, and a static-but-honest "we don't
/// know yet" reads better than a bar that looks stuck at a fabricated
/// value).
fn progress_bar(percent: Option<u8>, palette: ShellPalette) -> Div {
    let fraction = percent.map(|p| f32::from(p.min(100)) / 100.0).unwrap_or(0.08);
    div()
        .relative()
        .w_full()
        .h(px(10.))
        .rounded(px(10.))
        .bg(theme::alpha(palette.muted, 1.0))
        .child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(fraction)).rounded(px(10.)).bg(theme::alpha(palette.brand, 1.0)))
}

fn status_line(text: &str, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text.to_string())
}

fn idle_body(palette: ShellPalette) -> Div {
    div().flex().flex_col().gap(px(8.)).child(progress_bar(None, palette)).child(status_line("等待開始… · Waiting to start…", palette))
}

fn running_body(percent: Option<u8>, status: &str, palette: ShellPalette) -> Div {
    let label = match percent {
        Some(p) => format!("{p}% — {status}"),
        None => format!("進度未知（無 pv） · Progress unknown (no pv) — {status}"),
    };
    div().flex().flex_col().gap(px(8.)).child(progress_bar(percent, palette)).child(status_line(&label, palette))
}

fn done_body(palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(progress_bar(Some(100), palette))
        .child(status_line("安裝完成，請移除安裝媒介後重新開機 · Done — remove the install media, then reboot", palette))
}

fn failed_body(message: &str, palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.destructive, 1.0))
                .child(format!("安裝失敗 · Install failed：{message}")),
        )
        .child(status_line("請返回上一步重試 · Go back and try again", palette))
}
