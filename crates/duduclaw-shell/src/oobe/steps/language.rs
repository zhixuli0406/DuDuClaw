// Step (index 0, was index 1) — 語言 + 無障礙. §B-1 row 1 + §A consensus #1
// (language first, before any account/consent — 6/8, the STRONGEST
// agreement in the whole survey) + consensus #7 (accessibility entry point
// in the first batch of screens, every device-type OS). PROMOTED to `ALL[0]`
// this round — see `oobe/mod.rs`'s header comment for why the original
// literal-§B-1-row order (input detection first) was a correction-worthy
// slip against this very step's own citation. Not skippable — see
// `OobeStep::LanguageAccessibility`'s own doc comment.
//
// i18n is REAL as of this round (task brief item 2, superseding round 1's
// stub note here): picking a language calls `OobeFlow::set_language` (persists
// to disk, survives a restart) AND every OTHER OOBE screen re-renders through
// `crate::i18n` using that choice on the very next frame (`cx.notify()` on
// the same click handler that sets it) — the whole point of promoting this
// step to `ALL[0]`.
//
// This screen's own TOP CAPTION (the two lines below the "選擇語言" title)
// is the one deliberate exception: it stays trilingual by construction,
// same reasoning `duduclaw-native-gui/src/screens/language_picker.rs`'s own
// header comment gives for its own one pre-selection line — it has to be
// readable BEFORE a language is chosen, so it can't itself come from
// `crate::i18n` (which keys off the very choice this step makes). Everything
// else on this screen (the accessibility entry, its expand/collapse state,
// the placeholder panel, the "已選擇"/"Selected"/"選択済み" tag) DOES route
// through `crate::i18n` — that's meaningful even before any click, since
// `LanguageChoice::default()` is `ZhTw` (§B-1 row 1: "zh-TW 預設高亮"), so
// the very first frame already reads correctly through the zh-TW catalog.
//
// The "無障礙入口" is a real click target (task brief: "視覺入口，點開佔
// 位") — clicking it expands an inline placeholder panel via `OobeUiState::
// accessibility_open` (ephemeral, not persisted — see that struct's own doc
// comment), rather than navigating anywhere or doing nothing at all.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key};
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{LanguageChoice, OobeFlow, OobeUiState};
use crate::ShellView;

const LANGUAGES: &[LanguageChoice] = &[LanguageChoice::ZhTw, LanguageChoice::En, LanguageChoice::JaJp];

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, cx: &mut Context<ShellView>) -> Div {
    let selected = flow.selections().language;
    let locale = flow.locale();
    let palette = flow.palette();

    let mut lang_rows = div().flex().flex_col().gap(px(8.));
    for (index, lang) in LANGUAGES.iter().enumerate() {
        lang_rows = lang_rows.child(language_row(*lang, index, *lang == selected, locale, palette, cx));
    }

    let accessibility_click = cx.listener(|view, _ev, _window, cx| {
        view.oobe_ui.toggle_accessibility();
        cx.notify();
    });

    let accessibility_entry = div()
        .id("oobe-accessibility-entry")
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.secondary, 1.0))
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(t(locale, Key::LanguageAccessibilityEntry)))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(if ui.accessibility_open { t(locale, Key::LanguageAccessibilityCollapse) } else { t(locale, Key::LanguageAccessibilityExpand) }),
        )
        .on_click(accessibility_click);

    let mut body = div().flex().flex_col().gap(px(14.)).child(lang_rows).child(accessibility_entry);

    if ui.accessibility_open {
        body = body.child(
            div()
                .px(px(14.))
                .py(px(12.))
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(palette.muted, 1.0))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(t(locale, Key::LanguageAccessibilityPlaceholder)),
        );
    }

    // Trilingual caption — see this file's own header comment for why this
    // pair is the one exempt piece of copy on the whole screen.
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title("選擇語言 · Choose your language · 言語を選択", palette))
        .child(widgets::subtitle("之後可以在設定中變更 · Change this anytime in Settings · あとで設定から変更できます", palette))
        .child(widgets::card(body, palette))
}

fn language_row(
    lang: LanguageChoice,
    index: usize,
    selected: bool,
    locale: crate::i18n::Locale,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_language(lang);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    div()
        .id(("oobe-language", index))
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
