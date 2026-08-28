// Y20-P2 (2026-08-29) — the live-installer's own language step.
//
// Re-derives `oobe::steps::language`'s row-list UI (the same three
// `LanguageChoice` options, the same enum reused directly — `oobe::
// LanguageChoice` has zero OOBE-specific coupling of its own, see that
// type's doc comment in `oobe/selections.rs`) against THIS flow's own
// `view.live_install` field instead of `view.oobe`: the two wizards are
// separate state machines (see `live_install/mod.rs`'s header comment for
// why), so the click handler can't reuse OOBE's `set_language` closure
// verbatim even though the visual row is the same shape.
//
// No accessibility disclosure panel here (unlike `oobe::steps::language`) —
// P2 scope is the 4-step skeleton's navigation, not re-deriving every
// affordance OOBE's own language step carries, and nothing in the task
// brief asks for one on this wizard.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::oobe::widgets;
use crate::oobe::LanguageChoice;
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::LiveInstallFlow;

const LANGUAGES: &[LanguageChoice] = &[LanguageChoice::ZhTw, LanguageChoice::En, LanguageChoice::JaJp];

pub(super) fn render(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let selected = flow.language();
    let locale = flow.locale();
    let palette = flow.palette();

    let mut rows = div().flex().flex_col().gap(px(8.));
    for (index, lang) in LANGUAGES.iter().enumerate() {
        rows = rows.child(language_row(*lang, index, *lang == selected, locale, palette, cx));
    }

    // Trilingual title/subtitle, same exemption `oobe::steps::language`'s own
    // header comment gives its top caption: it has to be readable BEFORE a
    // language pick exists, so it can't itself come from `crate::i18n`
    // (which keys off the very choice this step makes).
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title("選擇語言 · Choose your language · 言語を選択", palette))
        .child(widgets::subtitle("安裝精靈的顯示語言 · Installer display language · インストーラーの表示言語", palette))
        .child(widgets::card(rows, palette))
}

fn language_row(
    lang: LanguageChoice,
    index: usize,
    selected: bool,
    locale: Locale,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            flow.set_language(lang);
        }
        cx.notify();
    });

    div()
        .id(("live-install-language", index))
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
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(lang.label()))
        .when(selected, |el| {
            el.child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::alpha(palette.brand, 1.0))
                    .child(t(locale, Key::CommonSelected)),
            )
        })
        .on_click(on_click)
}
