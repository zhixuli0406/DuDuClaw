// Launcher overlay content — Shell-S0 round 2, dark theme in Shell-S1.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Launcher.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/Launcher.dc.html`
// (dark) — both 1440×900 (same fixed window this crate always renders at,
// see `home.rs`'s own `cat_hero()` comment for why no responsive centering
// math is needed here either). The board's panel is `top:170px; left:50%;
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
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment for the light/dark token mapping.
// One structural note: NEITHER theme's panel root originally set an
// explicit `.text_color(...)` (every text node inside either set its own
// color, or fell through to gpui's own default text color, `black()` —
// verified in `gpui::style::TextStyle`'s own `Default` impl). That default
// happens to read acceptably close to `FOREGROUND` in LIGHT (`#09090b` is
// near-black), which is why the light board never needed a fallback — but
// it is NOT acceptable in dark (pure black text on a dark panel is
// unreadable). `render()` below therefore adds `.text_color(palette.
// foreground)` at the panel root ONLY when `palette.is_dark()` — light's
// chain is completely untouched (no new call at all in that branch), so
// this crate's "light stays byte-identical" constraint holds exactly, while
// dark gets a real, correct default instead of inheriting pure black.

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data;
use crate::palette::ShellPalette;

const PANEL_WIDTH: f32 = 660.;
const PANEL_LEFT: f32 = (1440. - PANEL_WIDTH) / 2.; // 390 — see header comment
const PANEL_TOP: f32 = 170.;

pub(super) fn render(palette: ShellPalette) -> Stateful<Div> {
    // Launcher.dc.html: bg `rgba(255,255,255,0.97)` light / `rgba(30,30,33,
    // 0.97)` dark — `surface_raised` in both (see `home.rs`'s
    // `menu_bar_ticker` comment for why not `surface`). Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark. Shadow: bespoke in
    // both themes (deeper than `floating_shadow()`'s own recipe — same
    // "kept literal to match the board's actual numbers" reasoning the
    // original light-only comment already gave).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let shadow = if palette.is_dark() {
        vec![
            BoxShadow::new(px(0.), px(24.), rgb(0x000000).opacity(0.55).into()).blur_radius(px(64.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x000000).opacity(0.35).into()).blur_radius(px(14.)),
        ]
    } else {
        vec![
            BoxShadow::new(px(0.), px(24.), rgb(0x0f172a).opacity(0.28).into()).blur_radius(px(64.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x0f172a).opacity(0.10).into()).blur_radius(px(14.)),
        ]
    };

    let mut panel = div()
        .id("overlay-launcher-panel")
        .absolute()
        .top(px(PANEL_TOP))
        .left(px(PANEL_LEFT))
        .w(px(PANEL_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.97))
        .border_1()
        .border_color(border_color)
        .shadow(shadow);
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel
        .child(query_row(palette))
        .child(delegate_section(palette))
        .child(apps_section(palette))
        .child(files_section(palette))
        .child(footer(palette))
}

fn query_row(palette: ShellPalette) -> Div {
    // Launcher.dc.html: border-bottom `#f0f0f2` light / `rgba(255,255,255,
    // 0.08)` dark.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(22.))
        .py(px(18.))
        .border_b_1()
        .border_color(border_color)
        .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).child(fake_data::LAUNCHER_QUERY))
        // The blinking text cursor — static bar, no actual blink animation
        // this round (task brief: static predisplay).
        .child(div().w(px(2.)).h(px(22.)).bg(theme::alpha(palette.brand, 1.0)))
}

fn section_label(label: &'static str, palette: ShellPalette) -> Div {
    // Launcher.dc.html: `#9f9fa9` light / `#71717b` dark — text ladder
    // rank 4, matches `text_faint` exactly (see `crate::palette`'s own
    // header comment).
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .px(px(10.))
        .py(px(4.))
        .child(label)
}

fn delegate_section(palette: ShellPalette) -> Div {
    // Launcher.dc.html: highlight card bg `#e8f1fb` / border `#cfe0f5`
    // light — brand-tinted `rgba(67,144,238,0.12)` bg / `rgba(67,144,238,
    // 0.35)` border dark. Hint pill: bg `#ffffff` / border `#cfe0f5` / text
    // `brand` light — bg `rgba(67,144,238,0.15)` / border `rgba(67,144,
    // 238,0.40)` / text `brand_bright` (`#59a6ff`) dark.
    let dark = palette.is_dark();
    let card_bg = if dark { theme::alpha(palette.brand, 0.12) } else { theme::alpha(0xe8f1fb, 1.0) };
    let card_border = if dark { theme::alpha(palette.brand, 0.35) } else { theme::alpha(0xcfe0f5, 1.0) };
    let plan_text = theme::alpha(palette.text_secondary, 1.0);
    let hint_bg = if dark { theme::alpha(palette.brand, 0.15) } else { theme::alpha(palette.surface_raised, 1.0) };
    let hint_border = if dark { theme::alpha(palette.brand, 0.40) } else { theme::alpha(0xcfe0f5, 1.0) };
    let hint_text = if dark { theme::alpha(palette.brand_bright, 1.0) } else { theme::alpha(palette.brand, 1.0) };

    div().pt(px(10.)).px(px(12.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_DELEGATE, palette)).child(
        div()
            .flex()
            .gap(px(12.))
            .bg(card_bg)
            .border_1()
            .border_color(card_border)
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
                            .text_color(theme::alpha(palette.brand_foreground, 1.0))
                            .child(fake_data::LAUNCHER_DELEGATE_AGENT_INITIAL),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(fake_data::LAUNCHER_DELEGATE_TITLE))
                    .child(div().mt(px(3.)).text_size(px(12.)).text_color(plan_text).child(fake_data::LAUNCHER_DELEGATE_PLAN)),
            )
            .child(
                div()
                    .bg(hint_bg)
                    .border_1()
                    .border_color(hint_border)
                    .rounded(px(6.))
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hint_text)
                    .child(fake_data::LAUNCHER_DELEGATE_HINT),
            ),
    )
}

fn apps_section(palette: ShellPalette) -> Div {
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in fake_data::LAUNCHER_APP_RESULTS {
        rows = rows.child(app_result_row(item, palette));
    }
    div().pt(px(6.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_APPS, palette)).child(rows)
}

fn app_result_row(item: &fake_data::LauncherAppResult, palette: ShellPalette) -> Stateful<Div> {
    // Same "white/near-white bordered tile -> dark gradient" exception
    // `home/home_dock.rs::dock_app` applies — see `crate::palette`'s own
    // header comment. Non-bordered (colored) icons keep their identity
    // gradient in both themes.
    let neutral_dark = item.bordered && palette.is_dark();
    let (gradient_top, gradient_bottom) =
        if neutral_dark { (palette.neutral_tile_top, palette.neutral_tile_bottom) } else { (item.gradient_top, item.gradient_bottom) };

    let mut icon = div()
        .w(px(30.))
        .h(px(30.))
        .rounded(px(8.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(gradient_top), 0.0), linear_color_stop(rgb(gradient_bottom), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        // The pre-existing light-only `0.14` already approximates the
        // board's own two distinct per-icon values (`.12` neutral / `.16`
        // colored, averaging to `.14`) as one flat number — kept exactly
        // (light must stay byte-identical). Dark mirrors the same
        // averaging strategy over ITS OWN two board values (`.30`/`.35`
        // -> `.32`), rather than discriminating by `item.bordered` (which
        // light's own code never did either).
        .shadow(palette.icon_shadow(0.14, 0.32))
        .child(item.glyph);
    icon = if neutral_dark {
        icon.text_color(theme::alpha(0xb0b0b8, 1.0))
    } else {
        icon.text_color(theme::alpha(palette.foreground, if item.bordered { 0.55 } else { 1.0 }))
    };
    if item.bordered {
        let border_color = if palette.is_dark() { palette.neutral_tile_border } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
        icon = icon.border_1().border_color(border_color);
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
        .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(item.tag))
}

fn files_section(palette: ShellPalette) -> Div {
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in fake_data::LAUNCHER_FILE_RESULTS {
        rows = rows.child(file_result_row(item, palette));
    }
    div().pt(px(6.)).pb(px(14.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_FILES, palette)).child(rows)
}

fn file_result_row(item: &fake_data::LauncherFileResult, palette: ShellPalette) -> Stateful<Div> {
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
                // File-type icon color is IDENTITY, not themed — same
                // convention `crate::palette`'s own header comment
                // documents for dock/app-tile colors (a folder is blue, an
                // xlsx is green, regardless of theme).
                .text_color(theme::alpha(item.glyph_hex, 1.0))
                .child(item.glyph),
        )
        .child(div().flex_1().text_size(px(13.5)).child(item.label))
        .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(item.meta))
}

fn footer(palette: ShellPalette) -> Div {
    // Launcher.dc.html: border-top `#f0f0f2` light / `rgba(255,255,255,
    // 0.08)` dark. Bg: `#fbfbfb` light (bespoke, no matching token) /
    // `#18181b` dark — the exact `surface` token (an asymmetry the board
    // itself draws: light picked an off-white one shade past `SURFACE`,
    // dark reused the token verbatim).
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    let bg = if palette.is_dark() { theme::alpha(palette.surface, 1.0) } else { theme::alpha(0xfbfbfb, 1.0) };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(20.))
        .py(px(10.))
        .border_t_1()
        .border_color(border_color)
        .bg(bg)
        .text_size(px(11.))
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .child(div().child(fake_data::LAUNCHER_FOOTER_LEFT))
        .child(div().child(fake_data::LAUNCHER_FOOTER_RIGHT))
}
