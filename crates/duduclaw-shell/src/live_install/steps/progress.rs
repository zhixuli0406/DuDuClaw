// Y20-P2 (2026-08-29) — Progress placeholder, the flow's terminal step.
//
// Real `dd`-style write progress is P3's job (task brief: "...dd 進度留
// P3"). This renders a static, honestly-labeled placeholder bar — fixed at
// 0%, not a fake animation pretending to advance — so the 4-step navigation
// has a fourth stop to land on.

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
        .child(widgets::title("安裝進度 · Installing", palette))
        .child(widgets::subtitle("真實寫入進度留待 P3 實作 · Real write progress lands in P3", palette))
        .child(widgets::card(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w_full()
                        .h(px(8.))
                        .rounded(px(8.))
                        .bg(theme::alpha(palette.muted, 1.0))
                        // Fixed at 0 width — an honest "not yet wired", not a
                        // fake progress animation. P3 will drive this from a
                        // real byte-count.
                        .child(div().w(px(0.)).h(px(8.)).rounded(px(8.)).bg(theme::alpha(palette.brand, 1.0))),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child("（開發中：0% — P3 will report real write progress here）"),
                ),
            palette,
        ))
}
