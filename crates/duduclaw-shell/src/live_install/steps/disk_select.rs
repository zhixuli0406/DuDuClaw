// Y20-P2 (2026-08-29) — DiskSelect placeholder.
//
// Real block-device enumeration + a pick that gates `LiveInstallFlow::
// can_advance` is P3's job (task brief: "真實列碟...留 P3"). This step
// exists purely so the 4-step navigation has a second stop to land on — it
// renders an honest "開發中" placeholder, never a fake device list dressed
// up as real data (this crate's own `fake_data.rs` module is for OOBE's
// STAND-IN example values like a Wi-Fi SSID; a disk picker with no real
// backing data is a gap to disclose, not fill with fiction).

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
        .child(widgets::title("選擇安裝目標磁碟 · Select install disk", palette))
        .child(widgets::subtitle("真實磁碟列表留待 P3 實作 · Real disk enumeration lands in P3", palette))
        .child(widgets::card(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child("（開發中：磁碟列表 placeholder — P3 will list real block devices here）"),
            palette,
        ))
}
