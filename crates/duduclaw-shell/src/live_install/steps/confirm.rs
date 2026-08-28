// Y20-P2 (2026-08-29) — Confirm placeholder.
//
// Once P3 wires up a real disk pick on `DiskSelect`, this step will
// summarize exactly what's about to happen (target device, size, an
// explicit destructive-write warning) before anything irreversible runs. P2
// renders static placeholder copy — there is no real target to summarize
// yet, so this deliberately does not fabricate one.

use gpui::{div, prelude::*, px, Div};

use duduclaw_native_gui::theme;

use crate::oobe::widgets;

use super::super::LiveInstallFlow;

pub(super) fn render(flow: &LiveInstallFlow) -> Div {
    let palette = flow.palette();

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title("確認安裝 · Confirm installation", palette))
        .child(widgets::subtitle("真實摘要（目標磁碟／警告）留待 P3 實作 · Real summary lands in P3", palette))
        .child(widgets::card(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child("（開發中：安裝摘要 placeholder — P3 will show the real picked disk + warning here）"),
            palette,
        ))
}
