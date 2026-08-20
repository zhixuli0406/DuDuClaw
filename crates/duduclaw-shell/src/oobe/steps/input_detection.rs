// Step (index 1, was index 0) — 輸入裝置偵測. §B-1 row 0: "輸入裝置偵測
// （無鍵盤→觸控/遙控模式）" — ChromeOS's `hid_detection`. No longer OOBE's
// FIRST step (see `oobe/mod.rs`'s header comment on the language-first
// correction) — still immediately after it. Static this round (task brief:
// "假資料『已偵測』即過場，觸控/遙控雙輸入是值班機需求——本輪視覺呈現即
// 可"): no live device enumeration, just a fixed "detected" snapshot. No
// precondition (`OobeFlow::can_advance` always allows leaving this step)
// and no skip affordance either — see `OobeStep::InputDetection`'s own doc
// comment for why.
//
// Real i18n as of round 2 (task brief item 2): `locale` comes from
// `flow.locale()`. As of the `Theme` step (2026-08-20) this fn takes the
// whole `&OobeFlow` (not just `Locale`) so it can also resolve
// `flow.palette()` for its own `widgets::title`/`subtitle`/`card` calls —
// see `steps/mod.rs`'s dispatcher comment.

use gpui::{div, prelude::*, px, Div, FontWeight};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::OobeFlow;

pub(super) fn render(flow: &OobeFlow) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::InputDetectionTitle), palette))
        .child(widgets::subtitle(t(locale, Key::InputDetectionSubtitle), palette))
        .child(widgets::card(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(detected_row(t(locale, Key::InputDetectionKeyboard), locale, palette))
                .child(detected_row(t(locale, Key::InputDetectionMouse), locale, palette)),
            palette,
        ))
}

fn detected_row(label: &'static str, locale: Locale, palette: ShellPalette) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.foreground, 1.0)).child(label))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(palette.success, 1.0)))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.success, 1.0))
                        .child(t(locale, Key::InputDetectionDetected)),
                ),
        )
}
