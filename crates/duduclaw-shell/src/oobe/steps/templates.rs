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

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::palette::ShellPalette;
use crate::oobe::widgets::{self, StepButtonVariant};
use crate::oobe::{fake_data, OobeFlow, TemplateChoice};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let choice = &flow.selections().template_choice;
    let is_express = matches!(choice, Some(TemplateChoice::Express));
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

    let mut custom_rows = div().flex().flex_col().gap(px(8.));
    for (index, tpl) in fake_data::FAKE_TEMPLATES.iter().enumerate() {
        custom_rows = custom_rows.child(template_card(tpl, index, choice, locale, palette, cx));
    }

    let skip_click = cx.listener(|view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.skip();
            crate::oobe::save_state(flow.state());
            if flow.completed() {
                view.oobe = None;
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
        .child(custom_rows)
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

fn template_card(
    tpl: &fake_data::FakeTemplate,
    index: usize,
    choice: &Option<TemplateChoice>,
    locale: Locale,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let id = tpl.id;
    let selected = matches!(choice, Some(TemplateChoice::Custom(c)) if c.as_str() == id);
    let click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_template_choice(TemplateChoice::Custom(id.to_string()));
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    div()
        .id(("oobe-template", index))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if selected { palette.secondary } else { palette.surface }, 1.0))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0) } else { palette.surface_border })
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(
            // `tpl.title`/`tpl.desc` stay hardcoded zh-TW — see
            // `oobe/fake_data.rs`'s header comment for why this one's a
            // knowingly-left stub, not the same "literal identifier" case
            // `net.ssid` (`steps::network`) is.
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(tpl.title))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(tpl.desc)),
        )
        .when(selected, |el| {
            el.child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::alpha(palette.brand, 1.0))
                    .child(t(locale, Key::CommonSelected)),
            )
        })
        .on_click(click)
}
