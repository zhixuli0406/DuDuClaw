// Screen 1 — language picker. First thing a person sees, full stop: no
// browser `navigator.language` sniff to fall back on (this is a desktop
// app), and the task this screen exists to fix is "the app defaulted to
// English" — so the fix is to never default silently. Picking a locale
// advances straight to the login screen.

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::i18n::Locale;
use crate::theme;
use crate::{RootView, Screen};

/// One selectable language card. Deliberately shows the language's OWN
/// native name (never translated — see `Locale::native_name`'s doc
/// comment): a reader of any of the three needs to recognize their own
/// option without already having picked a language.
fn locale_card(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id: &'static str = match locale {
        Locale::ZhTw => "lang-zh-tw",
        Locale::En => "lang-en",
        Locale::JaJp => "lang-ja-jp",
    };

    // MDS Card (spec §4): `rounded-xl border border-surface-border bg-surface
    // ... shadow-[var(--surface-shadow)]` — opaque, not translucent (a real
    // shift from Calm Glass, which faked glass via alpha everywhere). Hover
    // lightens to `surface-hover` (also opaque) with the border picking up a
    // brand tint as a selection affordance; press dims toward `surface`
    // (the `active:` cue MDS's Button variants use, applied here since
    // these tiles are effectively big single-select buttons).
    div()
        .id(id)
        .w_full()
        .px(px(24.))
        .py(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::alpha(theme::SURFACE_HOVER, 1.0))
                .border_color(theme::alpha(theme::BRAND, 0.45))
        })
        .active(|style| style.bg(theme::alpha(theme::SURFACE_SELECTED, 1.0)))
        .text_size(px(theme::TEXT_BASE))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .child(locale.native_name())
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.locale = locale;
            this.screen = Screen::Login;
            // Task item 4: persist so the next launch skips straight to
            // Login (see `main()`'s `config::load_locale` check). Fire-and-
            // forget — `save_locale` is fail-open by construction (logs to
            // stderr, never blocks/panics), and there's nothing meaningful
            // to show the user here even on failure.
            crate::config::save_locale(locale);
            cx.notify();
        }))
}

pub fn render(_state: &RootView, cx: &mut Context<RootView>) -> Div {
    // Built with a plain loop rather than `.children(Locale::ALL.map(...))`:
    // the map closure would need to call `locale_card(locale, cx)` on every
    // iteration, and `cx: &mut Context<RootView>` is a unique (non-`Copy`)
    // reference — a closure invoked repeatedly by `.map()` can't hold a move
    // of it across calls. A `for` loop re-borrows `cx` fresh each iteration
    // without that conflict.
    let mut cards = Vec::with_capacity(Locale::ALL.len());
    for locale in Locale::ALL {
        cards.push(locale_card(locale, cx));
    }

    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(theme::APP_SHELL, 1.0))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_8()
                .w(px(420.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(44.)).child("🐾"))
                        .child(
                            // "詳情 hero 標題" scale (spec §2 type table): text-2xl
                            // font-semibold — the largest defined size, used here
                            // for the one-time wordmark moment.
                            div()
                                .text_size(px(theme::TEXT_2XL))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child("DuDuClaw"),
                        )
                        .child(
                            // Trilingual by construction — this is the one line in
                            // the whole app that has to be readable BEFORE a
                            // language is chosen, so it can't come from the i18n
                            // catalogs (which key off the choice this screen makes).
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child("選擇語言 · Choose your language · 言語を選択"),
                        ),
                )
                .child(div().flex().flex_col().gap_3().w_full().children(cards)),
        )
}
