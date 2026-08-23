// Step 5 — AI runtime 授權（可延後）. §B-1 row 5: "AI runtime 授權（可『稍後
// 設定』→明示降級）" — SKIPPABLE, explicit defer semantics (not a silent
// skip), same two-tier vocabulary as macOS's own "Not Now"/"Set Up Later"
// (§1 "互動/排版/文案"). See `OobeStep::RuntimeAuth`'s own doc comment in
// `oobe/mod.rs`.
//
// Two paths, both real (task brief): "立即設定" (Primary, sets
// `selections.runtime_authorized = true` via `OobeFlow::
// set_runtime_authorized`) and "稍後再說" (Secondary/Ghost, calls the SAME
// `OobeFlow::skip()` the bottom-nav "略過" button already uses — sets
// `runtime_deferred = true` AND advances, matching macOS's "Not Now"
// behavior of just moving on rather than requiring a separate Continue
// click). Once authorized, the card swaps to a confirmation row (same
// green-dot recipe `steps::update`'s "已是最新版本" row uses) instead of
// re-showing both buttons — nothing left to choose once the choice is made.

use gpui::{div, prelude::*, px, Context, Div, FontWeight};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::oobe::widgets::{self, StepButtonVariant};
use crate::oobe::OobeFlow;
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let authorized = flow.selections().runtime_authorized;
    let locale = flow.locale();
    let palette = flow.palette();

    let authorize_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_runtime_authorized(true);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });
    let defer_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    let body = if authorized {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().w(px(8.)).h(px(8.)).rounded(px(8.)).bg(theme::alpha(palette.success, 1.0)))
            .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(t(locale, Key::RuntimeAuthAuthorized)))
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(widgets::step_button(
                "oobe-runtime-authorize",
                t(locale, Key::RuntimeAuthAuthorizeNow),
                StepButtonVariant::Primary,
                false,
                palette,
                authorize_click,
            ))
            .child(widgets::step_button(
                "oobe-runtime-defer",
                t(locale, Key::RuntimeAuthDeferLater),
                StepButtonVariant::Ghost,
                false,
                palette,
                defer_click,
            ))
    };

    // ICON-3 (2026-08-23): `dialog-password` — a key, i.e. "an
    // authorization credential", the 32px title icon
    // `OOBE-ProgressAndIcons.dc.html`'s assignment table gives this step.
    // See `steps::network::render`'s own comment on the column's spacing.
    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = crate::icons::icon_or_none(&[(crate::icons::KEY, palette.muted_foreground)], 32.) {
        column = column.child(icon);
    }
    column
        .child(widgets::title(t(locale, Key::RuntimeAuthTitle), palette))
        .child(widgets::subtitle(t(locale, Key::RuntimeAuthSubtitle), palette))
        .child(widgets::card(body, palette))
}
