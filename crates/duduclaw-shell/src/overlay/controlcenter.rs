// ControlCenter overlay content — Shell-S0 round 2, dark theme in Shell-S1.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/ControlCenter.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/ControlCenter.dc.html`
// (dark) — a right-docked floating panel (`top:40px; right:12px;
// width:372px`, no `bottom`/explicit height: the board lets it size to its
// own content, reproduced the same way by simply not calling `.h(...)`).
//
// Interaction scope this round (task brief: "開關做視覺 toggle 狀態
// （點擊可切換本地 bool 即可，不接後端）"): the phrase "開關" is read
// literally as the three AI-team TOGGLE-SWITCH widgets in the board's own
// "AI 團隊" card (自動化/主動行為/全部暫停 — pill-shaped switches with a
// sliding circular handle), which is exactly what got wired to
// `overlay::OverlayUiState` below. The top 3-tile quick-settings row
// (Wi-Fi/藍牙/勿擾) and the two sliders (volume/brightness) are CARDS and
// SLIDERS, not switches, and render as a static snapshot of the board's own
// state — consistent with the task brief's narrower "快速設定...等照畫板"
// wording for that part. Note the task brief's own switch-naming
// paraphrase ("自動化暫停/勿擾/接管模式") doesn't literally match this
// board's actual three switch labels (自動化/主動行為/全部暫停) — the board
// itself is the authoritative spec per this round's own instructions, so
// its literal labels/copy are what's implemented here.
//
// Layout: gpui does have a real `Display::Grid` (`Styled::grid()`, backed
// by Taffy) that could reproduce the board's `grid-template-columns:
// repeat(3, 1fr)` more literally, but nothing in this codebase has ever
// exercised that API — for a fixed 3-equal-column row, `flex()` + a
// `flex_1()` sizing constraint on each tile produces the identical visual
// result using the same primitive every other screen in this crate already
// relies on, so that's what's used here instead of introducing gpui's
// least-battle-tested layout mode for one call site.
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment. Same dark-only panel-root
// `.text_color(...)` fallback the sibling overlay files document applies
// here too. One deliberate NON-follow of the general text ladder: the
// quick-tile subtitle ("DuDu-Office"/"關閉") and the AI-team switch-row
// description text both use the LITERAL hex `#9f9fa9` in BOTH themes on the
// approved canvas — unlike every other `#9f9fa9`/`#71717b` pair in this
// crate's other overlay boards, this ONE specific role does not invert
// (checked against both `ControlCenter.dc.html` files directly, not
// assumed). Routing it through `text_faint` would silently produce the
// WRONG dark value (`0x71717b` instead of the board's actual `0x9f9fa9`),
// so it's kept as an unthemed literal at both call sites instead, each
// commented — see `quick_tile`/`switch_row` below.
//
// `toggle_pill`'s "off" track color (`#e4e4e7` light / `#3f3f46` dark) is
// ALSO reused by the two horizontal sliders' own track background — both
// are the same bespoke gray this file's `track_off_hex` helper centralizes,
// rather than being duplicated as a literal pair at three call sites.

use gpui::{div, prelude::*, px, relative, rgb, App, BoxShadow, ClickEvent, Context, Div, FontWeight, Stateful, Window};

use duduclaw_native_gui::theme;

use super::OverlayUiState;
use crate::palette::ShellPalette;
use crate::{fake_data, ShellView};

pub(super) fn render(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // ControlCenter.dc.html: bg `rgba(255,255,255,0.96)` light / `rgba(30,
    // 30,33,0.96)` dark — `surface_raised` in both. Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };

    let mut panel = div()
        .id("overlay-controlcenter-panel")
        .absolute()
        .top(px(40.))
        .right(px(12.))
        .w(px(372.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.96))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow())
        .p(px(16.));
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel.child(quick_tiles_row(palette)).child(sliders_card(palette)).child(ai_team_card(ui, palette, cx)).child(footer_row(palette))
}

/// The bespoke "inactive control" gray this file's header comment
/// describes — reused by `toggle_pill`'s off-track and both slider tracks.
fn track_off_hex(palette: ShellPalette) -> u32 {
    if palette.is_dark() { 0x3f3f46 } else { 0xe4e4e7 }
}

// ── Quick settings (static) ──────────────────────────────────────────────

fn quick_tiles_row(palette: ShellPalette) -> Div {
    let mut row = div().flex().gap(px(10.));
    for tile in fake_data::QUICK_TILES {
        row = row.child(quick_tile(tile, palette));
    }
    row
}

fn quick_tile(tile: &fake_data::QuickTile, palette: ShellPalette) -> Stateful<Div> {
    // Active (Wi-Fi) tile: bg `brand` in both themes; title/subtitle stay
    // the SAME literal (`#fafafa` opaque / `rgba(250,250,250,.75)`) in both
    // themes too — a colored tile's own white-on-brand text doesn't need to
    // change per theme. Inactive tiles: bg is `SECONDARY`/`MUTED`/
    // `SURFACE_HOVER` (all three tokens share one hex per theme, see
    // `theme.rs`'s own table), so `surface_hover` covers it in both; title
    // falls through to `palette.foreground` (no explicit color, matching
    // the original code's own `theme::light::FOREGROUND` — now resolved
    // per theme); icon glyph stroke is a bespoke pair (`#52525c`/`#b0b0b8`,
    // no clean token); subtitle is the ONE literal that does NOT invert —
    // see this file's header comment.
    let (bg_hex, title_hex) = if tile.active { (palette.brand, 0xfafafa) } else { (palette.surface_hover, palette.foreground) };
    let glyph_hex = if tile.active {
        0xfafafa
    } else if palette.is_dark() {
        0xb0b0b8
    } else {
        0x52525c
    };
    // Main.dc.html/ControlCenter.dc.html: `#9f9fa9`, unchanged across
    // themes — see this file's header comment.
    let (sub_hex, sub_alpha) = if tile.active { (0xfafafa, 0.75) } else { (0x9f9fa9, 1.0) };

    div()
        .id(tile.id)
        .flex_1()
        .bg(theme::alpha(bg_hex, 1.0))
        .rounded(px(13.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(glyph_hex, 1.0)).child(tile.glyph))
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(title_hex, 1.0)).child(tile.title))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(sub_hex, sub_alpha)).child(tile.subtitle)),
        )
}

fn sliders_card(palette: ShellPalette) -> Div {
    // ControlCenter.dc.html: bg `#ffffff` light / `#1e1e21` dark —
    // `surface_raised`. Border: opaque `border()` light / `rgba(255,255,
    // 255,0.10)` dark (bespoke, not `border()`'s own dark 0.06).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.10).into() } else { palette.border() };
    let mut card = div()
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(13.))
        .px(px(14.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(12.));
    for row in fake_data::SLIDER_ROWS {
        card = card.child(slider_row(row, palette));
    }
    card
}

fn slider_row(row: &fake_data::SliderRow, palette: ShellPalette) -> Div {
    let pct = row.pct.clamp(0.0, 1.0);
    // Icon glyph stroke: same bespoke `#52525c`/`#b0b0b8` pair
    // `quick_tile`'s inactive icon uses. Track bg: `track_off_hex` (this
    // file's shared helper) — same bespoke gray `toggle_pill`'s off-state
    // uses. NOTE: the approved canvas also draws a circular drag handle on
    // this slider; the pre-existing (light-only) implementation never drew
    // one (track + fill only) — that gap is unchanged by this round (dark
    // theming only, not new elements), so no handle is added here either.
    let glyph_hex = if palette.is_dark() { 0xb0b0b8 } else { 0x52525c };

    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().text_size(px(13.)).text_color(theme::alpha(glyph_hex, 1.0)).child(row.glyph))
        .child(
            div().flex_1().relative().h(px(5.)).rounded(px(5.)).bg(theme::alpha(track_off_hex(palette), 1.0)).child(
                div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(pct)).rounded(px(5.)).bg(theme::alpha(palette.brand, 1.0)),
            ),
        )
}

// ── AI 團隊 (interactive) ─────────────────────────────────────────────────

fn ai_team_card(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let automation_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_automation();
        cx.notify();
    });
    let proactive_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_proactive();
        cx.notify();
    });
    let pause_all_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_pause_all();
        cx.notify();
    });

    // Card bg/border: same `surface_raised`/`border()` pair `sliders_card`
    // uses. Section label ("AI 團隊"): text ladder rank 4 (`text_faint`) —
    // this one DOES invert (`#9f9fa9` light / `#71717b` dark), unlike the
    // switch-row description below it; verified against the board directly
    // rather than assumed, per this file's header comment. Divider under
    // the label: `#f0f0f2` light / `rgba(255,255,255,0.08)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let label_divider = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };

    div()
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(13.))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .px(px(14.))
                .pt(px(11.))
                .pb(px(9.))
                .border_b_1()
                .border_color(label_divider)
                .child(fake_data::CC_SECTION_AI_TEAM),
        )
        .child(switch_row(fake_data::CC_SWITCH_AUTOMATION_LABEL, fake_data::CC_SWITCH_AUTOMATION_DESC, ui.automation_on(), true, palette, automation_click))
        .child(switch_row(fake_data::CC_SWITCH_PROACTIVE_LABEL, fake_data::CC_SWITCH_PROACTIVE_DESC, ui.proactive_on(), true, palette, proactive_click))
        .child(switch_row(fake_data::CC_SWITCH_PAUSE_ALL_LABEL, fake_data::CC_SWITCH_PAUSE_ALL_DESC, ui.pause_all_on(), false, palette, pause_all_click))
}

fn switch_row(
    label: &'static str,
    desc: &'static str,
    on: bool,
    border_b: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    // ControlCenter.dc.html: this description text is `#9f9fa9` in BOTH
    // themes — the same non-inverting literal `quick_tile`'s subtitle uses,
    // see this file's header comment.
    let mut row = div()
        .id(label)
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(14.))
        .py(px(11.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(palette.foreground, 1.0)).child(label))
                .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(desc)),
        )
        .child(toggle_pill(on, palette))
        .on_click(on_click);
    if border_b {
        let color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
        row = row.border_b_1().border_color(color);
    }
    row
}

fn toggle_pill(on: bool, palette: ShellPalette) -> Div {
    let track_hex = if on { palette.brand } else { track_off_hex(palette) };
    // Handle: `#ffffff` light / `#fafafa` dark in both on/off states — the
    // off state additionally carries a shadow (navy light / black dark),
    // matching the board's own on/off asymmetry (the "on" handle has no
    // shadow in either theme, only "off" does).
    let handle_bg = if palette.is_dark() { 0xfafafa } else { 0xffffff };
    let mut handle = div().absolute().top(px(2.)).w(px(19.)).h(px(19.)).rounded(px(19.)).bg(theme::alpha(handle_bg, 1.0));
    if on {
        handle = handle.right(px(2.));
    } else {
        let shadow_base = if palette.is_dark() { 0x000000 } else { 0x0f172a };
        let shadow_opacity = if palette.is_dark() { 0.40 } else { 0.15 };
        handle = handle.left(px(2.)).shadow(vec![BoxShadow::new(px(0.), px(1.), rgb(shadow_base).opacity(shadow_opacity).into()).blur_radius(px(2.))]);
    }
    div().relative().w(px(40.)).h(px(23.)).rounded(px(23.)).bg(theme::alpha(track_hex, 1.0)).child(handle)
}

// ── Footer ────────────────────────────────────────────────────────────────

fn footer_row(palette: ShellPalette) -> Div {
    // ControlCenter.dc.html: status text is `muted_foreground` in both
    // themes (`#71717b` light / `#9f9fa9` dark); the link text is `brand`
    // light / `brand_bright` dark.
    let link_text = if palette.is_dark() { palette.brand_bright } else { palette.brand };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(4.))
        .py(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(small_avatar("杜", palette.brand, palette))
                .child(small_avatar("財", 0x0f766e, palette))
                .child(div().text_size(px(12.)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(fake_data::CC_FOOTER_STATUS)),
        )
        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(link_text, 1.0)).child(fake_data::CC_FOOTER_LINK))
}

fn small_avatar(initial: &'static str, bg_hex: u32, palette: ShellPalette) -> Div {
    div()
        .w(px(24.))
        .h(px(24.))
        .rounded(px(12.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.brand_foreground, 1.0)).child(initial))
}
