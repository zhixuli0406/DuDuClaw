// Step 7 — 挑選產業板模（可跳＋express）. §B-1 row 7: "產業板模/AI 員工
// （可選可跳，預設『一鍵 CEO』express）" — SKIPPABLE, plus an explicit
// default express option (macOS's own "Make This Your New Mac" one-click
// precedent, §1 step 8; elementary's "non-essential setup must not block"
// HIG principle, §5). See `OobeStep::Templates`'s own doc comment in
// `oobe/mod.rs`.
//
// Three real outcomes (task brief): an "Express" recommended card (one
// click applies it), a custom list of `fake_data::FAKE_TEMPLATES` cards
// (single-select, click again on a different card to change), and a "略
// 過" ghost button that calls the SAME `OobeFlow::skip()` the bottom-nav
// "略過" button already uses — both routes converge on one state
// transition, so there is no way for the in-card and bottom-nav skip
// controls to disagree.

use gpui::{div, prelude::*, px, Context, Div, FontWeight};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::oobe::widgets::{self, StepButtonVariant};
use crate::oobe::{OobeFlow, TemplateChoice};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let is_express = matches!(flow.selections().template_choice, Some(TemplateChoice::Express));
    let locale = flow.locale();
    let palette = flow.palette();

    let express_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_template_choice(TemplateChoice::Express);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    let express_card = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(14.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if is_express { palette.secondary } else { palette.muted }, 1.0))
        .border_1()
        .border_color(if is_express { theme::alpha(palette.brand, 1.0) } else { palette.surface_border })
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::BOLD).child(t(locale, Key::TemplatesExpressTitle)))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::TemplatesExpressDesc)))
        .child(widgets::step_button(
            "oobe-template-express",
            if is_express { t(locale, Key::TemplatesExpressApplied) } else { t(locale, Key::TemplatesExpressApply) },
            StepButtonVariant::Primary,
            false,
            palette,
            express_click,
        ));

    // 2026-09-05: the three industry cards that used to render here were
    // `fake_data::FAKE_TEMPLATES` — invented entries no click could apply.
    // The gateway's real catalogue (`templates.industries`) is a Pro
    // feature and comes back locked on a fresh Personal install, so say
    // exactly that instead of showing pretend choices.
    let locked_hint = div()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.surface, 1.0))
        .border_1()
        .border_color(palette.surface_border)
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(palette.muted_foreground, 1.0))
        .child(t(locale, Key::TemplatesPremiumLocked));

    let skip_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            crate::oobe::save_state(flow.state());
            if flow.completed() {
                // Same single completion path as every other site — see
                // `ShellView::complete_oobe`'s own doc comment (this site
                // used to drop the flow without adopting theme or name).
                view.complete_oobe(cx);
            }
        }
        cx.notify();
    });

    let body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(express_card)
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::TemplatesCustomHint)))
        .child(locked_hint)
        .child(widgets::step_button("oobe-template-skip", t(locale, Key::TemplatesSkip), StepButtonVariant::Ghost, false, palette, skip_click));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::TemplatesTitle), palette))
        .child(widgets::subtitle(t(locale, Key::TemplatesSubtitle), palette))
        .child(widgets::card(body, palette))
}

