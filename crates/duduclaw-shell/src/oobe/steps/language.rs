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
use crate::icons;
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{LanguageChoice, OobeFlow, OobeUiState};
use crate::ShellView;

const LANGUAGES: &[LanguageChoice] = &[LanguageChoice::ZhTw, LanguageChoice::En, LanguageChoice::JaJp];

/// The five accessibility categories `OOBE-ProgressAndIcons.dc.html` lists
/// inside this step's expanded panel — ICON-3 (2026-08-23), replacing the
/// single placeholder line that used to be the panel's whole content.
///
/// Defined HERE rather than in `oobe/selections.rs` alongside
/// `PrivacyToggle`: nothing about these is a SELECTION. They are five
/// informational rows with no state, nothing persisted, and no click target
/// — the board draws them without a toggle or a chevron, and this shell has
/// no accessibility setting to attach to any of them yet, so making one
/// clickable would be an affordance that leads nowhere. The panel's closing
/// line (`Key::LanguageAccessibilityPlaceholder`) is what says so out loud.
///
/// `slug()` is the stable, locale-independent id `crate::icons::
/// a11y_category_layers` keys off — same split `PrivacyToggle::slug()` vs.
/// `PrivacyToggle::label()` already documents (a display label varies with
/// the operator's language; an icon mapping must not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum A11yCategory {
    Seeing,
    Hearing,
    Typing,
    Pointing,
    Zoom,
}

impl A11yCategory {
    pub(crate) const ALL: [A11yCategory; 5] =
        [A11yCategory::Seeing, A11yCategory::Hearing, A11yCategory::Typing, A11yCategory::Pointing, A11yCategory::Zoom];

    pub(crate) fn slug(self) -> &'static str {
        match self {
            A11yCategory::Seeing => "a11y-seeing",
            A11yCategory::Hearing => "a11y-hearing",
            A11yCategory::Typing => "a11y-typing",
            A11yCategory::Pointing => "a11y-pointing",
            A11yCategory::Zoom => "a11y-zoom",
        }
    }

    fn label(self, locale: crate::i18n::Locale) -> &'static str {
        t(
            locale,
            match self {
                A11yCategory::Seeing => Key::LanguageA11ySeeingLabel,
                A11yCategory::Hearing => Key::LanguageA11yHearingLabel,
                A11yCategory::Typing => Key::LanguageA11yTypingLabel,
                A11yCategory::Pointing => Key::LanguageA11yPointingLabel,
                A11yCategory::Zoom => Key::LanguageA11yZoomLabel,
            },
        )
    }

    fn description(self, locale: crate::i18n::Locale) -> &'static str {
        t(
            locale,
            match self {
                A11yCategory::Seeing => Key::LanguageA11ySeeingDesc,
                A11yCategory::Hearing => Key::LanguageA11yHearingDesc,
                A11yCategory::Typing => Key::LanguageA11yTypingDesc,
                A11yCategory::Pointing => Key::LanguageA11yPointingDesc,
                A11yCategory::Zoom => Key::LanguageA11yZoomDesc,
            },
        )
    }
}

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

    // ICON-3 (2026-08-23): the row gained the board's own 20px
    // `preferences-desktop-accessibility` icon on the left and swapped its
    // "展開 ▼"/"收合 ▲" text for a chevron on the right (`go-next` collapsed,
    // `go-down` expanded — `OOBE-ProgressAndIcons.dc.html`'s own row-1
    // note). The old text survives as each chevron's `icon_or_glyph`
    // fallback, so a missing asset degrades to the pre-ICON-3 wording
    // instead of leaving the row with no disclosure affordance at all.
    let (chevron_key, chevron_fallback) = if ui.accessibility_open {
        (icons::CHEVRON_DOWN, t(locale, Key::LanguageAccessibilityCollapse))
    } else {
        (icons::CHEVRON_RIGHT, t(locale, Key::LanguageAccessibilityExpand))
    };

    let accessibility_entry = div()
        .id("oobe-accessibility-entry")
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(9.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.secondary, 1.0))
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_color(theme::alpha(palette.text_secondary, 1.0))
                .child(icons::icon_or_none(&[(icons::ACCESSIBILITY, palette.text_secondary)], 20.).unwrap_or_else(|| div().into_any_element()))
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(t(locale, Key::LanguageAccessibilityEntry)),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(icons::icon_or_glyph(&[(chevron_key, palette.muted_foreground)], 16., chevron_fallback)),
        )
        .on_click(accessibility_click);

    let mut body = div().flex().flex_col().gap(px(14.)).child(lang_rows).child(accessibility_entry);

    if ui.accessibility_open {
        let mut categories = div().flex().flex_col();
        for category in A11yCategory::ALL {
            categories = categories.child(category_row(category, locale, palette));
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(categories)
                // The five rows above are a PREVIEW, not controls (see this
                // file's header comment) — this line is what keeps that
                // honest, and is the reason the rows carry no toggle, no
                // chevron and no click target.
                .child(
                    div()
                        .px(px(14.))
                        .py(px(10.))
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(palette.muted, 1.0))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child(t(locale, Key::LanguageAccessibilityPlaceholder)),
                ),
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

/// One accessibility-category row. Deliberately a plain `Div`, not a
/// `Stateful<Div>`: no `.id()`, no `.cursor_pointer()`, no `.on_click(...)`
/// — see `A11yCategory`'s own doc comment for why these are informational
/// and must not look clickable.
///
/// Geometry from `OOBE-ProgressAndIcons.dc.html`: 20px icon, gap 11,
/// `padding: 9px 14px 9px 20px` (the extra left inset is what visually nests
/// the list under the disclosure row above it).
fn category_row(category: A11yCategory, locale: crate::i18n::Locale, palette: ShellPalette) -> Div {
    let icon = icons::a11y_category_layers(category.slug(), palette).unwrap_or_default();
    div()
        .flex()
        .items_center()
        .gap(px(11.))
        .pl(px(20.))
        .pr(px(14.))
        .py(px(9.))
        .child(icons::icon_or_none(&icon, 20.).unwrap_or_else(|| div().into_any_element()))
        .child(
            div()
                .flex_1()
                .text_size(px(13.5))
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(category.label(locale)),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.text_faint, 1.0)).child(category.description(locale)))
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
