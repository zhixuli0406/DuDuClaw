// Step 3 — 系統更新檢查. §B-1 row 3 + §A consensus #6 ("更新內建在 OOBE
// 中段、關鍵更新不可拒", all three device-type OSes surveyed). This round's
// stub always resolves the "already up to date" path (task brief: "假檢查
// （『已是最新』路徑），一鍵繼續") — no real update check ever runs, so
// Continue is always enabled (this step has no precondition in
// `OobeFlow::can_advance`, and it isn't in `is_skippable`'s allow-list
// either).

use gpui::{div, prelude::*, px, Div, FontWeight};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::oobe::widgets;
use crate::oobe::OobeFlow;

pub(super) fn render(flow: &OobeFlow) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    // ICON-3 (2026-08-23): `software-update-available`, the 32px title icon
    // `OOBE-ProgressAndIcons.dc.html`'s assignment table gives this step.
    // See `steps::network::render`'s own comment on the column's spacing.
    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = crate::icons::icon_or_none(&[(crate::icons::SOFTWARE_UPDATE, palette.muted_foreground)], 32.) {
        column = column.child(icon);
    }
    column
        .child(widgets::title(t(locale, Key::UpdateTitle), palette))
        .child(widgets::subtitle(t(locale, Key::UpdateChecking), palette))
        .child(widgets::card(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().w(px(8.)).h(px(8.)).rounded(px(8.)).bg(theme::alpha(palette.success, 1.0)))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(t(locale, Key::UpdateUpToDate))),
            palette,
        ))
}
