// Launcher overlay content — Shell-S0 round 2.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Launcher.dc.html`
// (1440×900 — same fixed window this crate always renders at, see
// `home.rs`'s own `cat_hero()` comment for why no responsive centering math
// is needed here either). The board's panel is `top:170px; left:50%;
// margin-left:-330px; width:660px` — for a FIXED 1440px window that's
// `left = (1440-660)/2 = 390px` exactly, reproduced as `PANEL_LEFT` below
// rather than carrying gpui a CSS-style percentage+negative-margin
// centering trick that only this one screen would ever use.
//
// Interaction scope this round (task brief: "本輪做「靜態預打字狀態」即
// 可"): the query text, its trailing cursor bar, and the "Enter 交辦"
// delegate suggestion all render as a STATIC snapshot of what a mid-type
// launcher query looks like — no live text input, no keyboard nav between
// result rows, no click handlers anywhere in this file (hence every row
// below returns `Stateful<Div>` purely so it carries a stable `.id()` for
// whenever real interactivity lands, not because anything here is
// clickable yet). Real typing/IME wiring is later-round scope — this
// crate's `duduclaw-native-gui` sibling's `ime_input` module is the
// reference for when that happens.
//
// Icon glyphs: same "single CJK character, no `gpui::svg()`" convention
// `home.rs`'s header comment establishes (this codebase has no
// `gpui::svg()` usage anywhere, and the bundled font stack has no
// guaranteed pictographic glyph coverage) — the board's own search-icon SVG
// is dropped rather than risk a tofu box, same as `home.rs`'s menu-bar bell.
// The Files/Mail app-result rows reuse `fake_data::DOCK_APPS`' own
// glyph+gradient choices verbatim (see `fake_data.rs`'s doc comment on
// `LAUNCHER_APP_RESULTS`) since the design board reuses the identical icons.

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data;

const PANEL_WIDTH: f32 = 660.;
const PANEL_LEFT: f32 = (1440. - PANEL_WIDTH) / 2.; // 390 — see header comment
const PANEL_TOP: f32 = 170.;

pub(super) fn render() -> Stateful<Div> {
    div()
        .id("overlay-launcher-panel")
        .absolute()
        .top(px(PANEL_TOP))
        .left(px(PANEL_LEFT))
        .w(px(PANEL_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::light::SURFACE, 0.97))
        .border_1()
        .border_color(theme::light::border())
        // Launcher.dc.html: `0 24px 64px rgba(15,23,42,.28), 0 4px 14px
        // rgba(15,23,42,.10)` — deeper than `theme::light::floating_shadow()`
        // (which Notifications/ControlCenter's own boards match exactly),
        // kept as literal `BoxShadow`s so this one panel's numbers stay
        // faithful rather than reusing a token that doesn't quite fit.
        .shadow(vec![
            BoxShadow::new(px(0.), px(24.), rgb(0x0f172a).opacity(0.28).into()).blur_radius(px(64.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x0f172a).opacity(0.10).into()).blur_radius(px(14.)),
        ])
        .child(query_row())
        .child(delegate_section())
        .child(apps_section())
        .child(files_section())
        .child(footer())
}

fn query_row() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(22.))
        .py(px(18.))
        .border_b_1()
        .border_color(theme::alpha(0xf0f0f2, 1.0)) // Launcher.dc.html: #f0f0f2
        .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).child(fake_data::LAUNCHER_QUERY))
        // The blinking text cursor — static bar, no actual blink animation
        // this round (task brief: static predisplay).
        .child(div().w(px(2.)).h(px(22.)).bg(theme::alpha(theme::light::BRAND, 1.0)))
}

fn section_label(label: &'static str) -> Div {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::alpha(0x9f9fa9, 1.0)) // Launcher.dc.html: #9f9fa9
        .px(px(10.))
        .py(px(4.))
        .child(label)
}

fn delegate_section() -> Div {
    div().pt(px(10.)).px(px(12.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_DELEGATE)).child(
        div()
            .flex()
            .gap(px(12.))
            .bg(theme::alpha(0xe8f1fb, 1.0)) // Launcher.dc.html: #e8f1fb
            .border_1()
            .border_color(theme::alpha(0xcfe0f5, 1.0)) // Launcher.dc.html: #cfe0f5
            .rounded(px(12.))
            .px(px(14.))
            .py(px(13.))
            .child(
                div()
                    .w(px(38.))
                    .h(px(38.))
                    .rounded(px(19.))
                    .bg(theme::alpha(fake_data::LAUNCHER_DELEGATE_AGENT_BG_HEX, 1.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
                            .child(fake_data::LAUNCHER_DELEGATE_AGENT_INITIAL),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(fake_data::LAUNCHER_DELEGATE_TITLE))
                    .child(
                        div()
                            .mt(px(3.))
                            .text_size(px(12.))
                            .text_color(theme::alpha(0x52525c, 1.0)) // Launcher.dc.html: #52525c
                            .child(fake_data::LAUNCHER_DELEGATE_PLAN),
                    ),
            )
            .child(
                div()
                    .bg(theme::alpha(theme::light::SURFACE, 1.0))
                    .border_1()
                    .border_color(theme::alpha(0xcfe0f5, 1.0))
                    .rounded(px(6.))
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::alpha(theme::light::BRAND, 1.0))
                    .child(fake_data::LAUNCHER_DELEGATE_HINT),
            ),
    )
}

fn apps_section() -> Div {
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in fake_data::LAUNCHER_APP_RESULTS {
        rows = rows.child(app_result_row(item));
    }
    div().pt(px(6.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_APPS)).child(rows)
}

fn app_result_row(item: &fake_data::LauncherAppResult) -> Stateful<Div> {
    let mut icon = div()
        .w(px(30.))
        .h(px(30.))
        .rounded(px(8.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(item.gradient_top), 0.0), linear_color_stop(rgb(item.gradient_bottom), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::light::SURFACE_FOREGROUND, if item.bordered { 0.55 } else { 1.0 }))
        .shadow(vec![BoxShadow::new(px(0.), px(2.), rgb(0x0f172a).opacity(0.14).into()).blur_radius(px(4.))])
        .child(item.glyph);
    if item.bordered {
        icon = icon.border_1().border_color(theme::alpha(theme::light::SURFACE_BORDER, 1.0));
    }

    div()
        .id(item.id)
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(icon)
        .child(div().flex_1().text_size(px(13.5)).child(item.label))
        .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(item.tag))
}

fn files_section() -> Div {
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in fake_data::LAUNCHER_FILE_RESULTS {
        rows = rows.child(file_result_row(item));
    }
    div().pt(px(6.)).pb(px(14.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_FILES)).child(rows)
}

fn file_result_row(item: &fake_data::LauncherFileResult) -> Stateful<Div> {
    div()
        .id(item.id)
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(
            div()
                .w(px(30.))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(item.glyph_hex, 1.0))
                .child(item.glyph),
        )
        .child(div().flex_1().text_size(px(13.5)).child(item.label))
        .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(item.meta))
}

fn footer() -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(20.))
        .py(px(10.))
        .border_t_1()
        .border_color(theme::alpha(0xf0f0f2, 1.0))
        .bg(theme::alpha(0xfbfbfb, 1.0)) // Launcher.dc.html: #fbfbfb
        .text_size(px(11.))
        .text_color(theme::alpha(0x9f9fa9, 1.0))
        .child(div().child(fake_data::LAUNCHER_FOOTER_LEFT))
        .child(div().child(fake_data::LAUNCHER_FOOTER_RIGHT))
}
