// Step (new, between Templates and Finish) — 選擇外觀（亮/暗）. See
// `OobeStep::Theme`'s own doc comment in `oobe/mod.rs` for why this step
// exists and where it sits. Pixel spec copied verbatim from
// `commercial/design/duduclaw-os-oobe/Theme.dc.html` (two side-by-side
// mini-desktop preview cards, brand-bordered selection, "已選擇" tag) — this
// crate's task brief calls that board the settled visual spec ("設計已由
// 使用者拍板"), so layout/colors below are transcribed, not re-designed.
//
// This is the ONE screen whose pick reskins EVERY OTHER OOBE screen live
// (the hard product requirement this whole step exists to satisfy): clicking
// a card calls `OobeFlow::set_theme` + `save_state` + `cx.notify()`, exactly
// like every other click-to-record setter in this crate (`set_language`,
// `set_template_choice`, …). The "live repaint" itself is not special-cased
// HERE at all — it falls straight out of `OobeFlow::palette()` being
// resolved fresh on every render call (see that method's own doc comment)
// the same way `flow.locale()` already reflows every screen on a language
// change. Nothing in this file (or anywhere else in `oobe/`) caches a
// palette across renders.
//
// ── The one deliberate exception to "no raw hex in oobe/" ────────────────
// The two mini-desktop PREVIEWS inside each option card (`theme_preview`
// below) are a fixed side-by-side illustration of what light/dark look
// like — not live chrome. They stay pixel-identical to the design board
// regardless of which theme is currently active: the dark preview must
// still look dark even while the operator is ON the light theme (and vice
// versa), since showing both options side by side at once is the entire
// point of this screen. So `theme_preview` uses literal hex/rgba copied
// straight from the board, never `palette`/`theme::light::*`/
// `theme::dark::*` — including two spots where the board itself does NOT
// reuse the exact semantic token: the dark preview's window-pane border is
// authored as `rgba(255,255,255,0.12)`, distinct from the top-bar
// hairline's `rgba(255,255,255,0.10)` (`theme::dark::SURFACE_BORDER_ALPHA`)
// — two different translucent-white weights in the same illustration, both
// kept exactly as drawn — and BOTH previews' brand pill is the LIGHT
// theme's brand hex `#2171cc`, never `theme::dark::BRAND`'s brighter
// `#4390ee` (the board's own dark-preview swatch was authored with the
// light brand color, so that is what ships here too).

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{OobeFlow, ThemeChoice};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let selected = flow.selections().theme;
    let locale = flow.locale();
    let palette = flow.palette();

    let body = div()
        .flex()
        .gap(px(14.))
        .child(theme_option_card(ThemeChoice::Light, selected == ThemeChoice::Light, locale, palette, cx))
        .child(theme_option_card(ThemeChoice::Dark, selected == ThemeChoice::Dark, locale, palette, cx));

    // ICON-3 (2026-08-23): `preferences-desktop-theme`, the 32px title icon
    // `OOBE-ProgressAndIcons.dc.html`'s assignment table gives this step.
    // The table's own 頁內 column for this step still reads 「不畫圖示」 —
    // the two mini-desktop previews below stay exactly as they are.
    // See `steps::network::render`'s own comment on the column's spacing.
    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = crate::icons::icon_or_none(&[(crate::icons::THEME_CONTRAST, palette.muted_foreground)], 32.) {
        column = column.child(icon);
    }
    column
        .child(widgets::title(t(locale, Key::ThemeTitle), palette))
        .child(widgets::subtitle(t(locale, Key::ThemeSubtitle), palette))
        .child(widgets::card(body, palette))
}

fn theme_option_card(choice: ThemeChoice, selected: bool, locale: Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let id = match choice {
        ThemeChoice::Light => "oobe-theme-light",
        ThemeChoice::Dark => "oobe-theme-dark",
    };
    let label = match choice {
        ThemeChoice::Light => t(locale, Key::ThemeLight),
        ThemeChoice::Dark => t(locale, Key::ThemeDark),
    };
    let click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_theme(choice);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    div()
        .id(id)
        .cursor_pointer()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if selected { palette.secondary } else { palette.surface }, 1.0))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0) } else { palette.surface_border })
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(theme_preview(choice))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(label))
                .when(selected, |el| {
                    el.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::alpha(palette.brand, 1.0))
                            .child(t(locale, Key::CommonSelected)),
                    )
                }),
        )
        .on_click(click)
}

/// The fixed light/dark mini-desktop illustration — see this file's header
/// comment for why every color here is a literal hex/rgba copied from
/// `Theme.dc.html`, never `palette`/`theme::light::`/`theme::dark::`.
fn theme_preview(choice: ThemeChoice) -> Div {
    let (bg, bar_bg, bar_border, pane_bg, pane_border, line1, line2) = match choice {
        ThemeChoice::Light => (
            theme::alpha(0xf3f3f4, 1.0),
            theme::alpha(0xffffff, 1.0),
            theme::alpha(0xececef, 1.0),
            theme::alpha(0xffffff, 1.0),
            theme::alpha(0xe4e4e7, 1.0),
            theme::alpha(0xe4e4e7, 1.0),
            theme::alpha(0xececef, 1.0),
        ),
        ThemeChoice::Dark => (
            theme::alpha(0x0c0c0e, 1.0),
            theme::alpha(0x18181b, 1.0),
            theme::alpha(0xffffff, 0.10),
            theme::alpha(0x18181b, 1.0),
            theme::alpha(0xffffff, 0.12),
            theme::alpha(0xffffff, 0.22),
            theme::alpha(0xffffff, 0.12),
        ),
    };
    // Same literal in both branches on purpose — see this file's header
    // comment ("both previews' brand pill is the LIGHT theme's brand hex").
    let brand_pill = theme::alpha(0x2171cc, 1.0);

    div()
        .relative()
        .h(px(140.))
        .rounded(px(8.))
        .overflow_hidden()
        .bg(bg)
        .child(div().absolute().top(px(0.)).left(px(0.)).right(px(0.)).h(px(14.)).bg(bar_bg).border_b_1().border_color(bar_border))
        .child(div().absolute().top(px(26.)).left(px(22.)).w(px(160.)).h(px(86.)).rounded(px(6.)).bg(pane_bg).border_1().border_color(pane_border))
        .child(div().absolute().top(px(40.)).left(px(34.)).w(px(90.)).h(px(6.)).rounded(px(3.)).bg(line1))
        .child(div().absolute().top(px(54.)).left(px(34.)).w(px(60.)).h(px(6.)).rounded(px(3.)).bg(line2))
        .child(div().absolute().top(px(78.)).left(px(34.)).w(px(44.)).h(px(12.)).rounded(px(6.)).bg(brand_pill))
}
