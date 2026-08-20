// ControlCenter overlay content — Shell-S0 round 2.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/ControlCenter.dc.html`
// — a right-docked floating panel (`top:40px; right:12px; width:372px`, no
// `bottom`/explicit height: the board lets it size to its own content,
// reproduced the same way by simply not calling `.h(...)`).
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

use gpui::{div, prelude::*, px, relative, rgb, App, BoxShadow, ClickEvent, Context, Div, FontWeight, Stateful, Window};

use duduclaw_native_gui::theme;

use super::OverlayUiState;
use crate::{fake_data, ShellView};

pub(super) fn render(ui: &OverlayUiState, cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
        .id("overlay-controlcenter-panel")
        .absolute()
        .top(px(40.))
        .right(px(12.))
        .w(px(372.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::light::SURFACE, 0.96))
        .border_1()
        .border_color(theme::light::border())
        .shadow(theme::light::floating_shadow())
        .p(px(16.))
        .child(quick_tiles_row())
        .child(sliders_card())
        .child(ai_team_card(ui, cx))
        .child(footer_row())
}

// ── Quick settings (static) ──────────────────────────────────────────────

fn quick_tiles_row() -> Div {
    let mut row = div().flex().gap(px(10.));
    for tile in fake_data::QUICK_TILES {
        row = row.child(quick_tile(tile));
    }
    row
}

fn quick_tile(tile: &fake_data::QuickTile) -> Stateful<Div> {
    let (bg_hex, title_hex, sub_alpha_hex) =
        if tile.active { (theme::light::BRAND, 0xfafafa, 0xfafafa) } else { (0xf4f4f5, theme::light::FOREGROUND, 0x9f9fa9) };
    let glyph_hex = if tile.active { 0xfafafa } else { 0x52525c };
    let sub_alpha = if tile.active { 0.75 } else { 1.0 };

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
                .child(div().text_size(px(10.5)).text_color(theme::alpha(sub_alpha_hex, sub_alpha)).child(tile.subtitle)),
        )
}

fn sliders_card() -> Div {
    let mut card = div().bg(theme::alpha(theme::light::SURFACE, 1.0)).border_1().border_color(theme::light::border()).rounded(px(13.)).px(px(14.)).py(px(12.)).flex().flex_col().gap(px(12.));
    for row in fake_data::SLIDER_ROWS {
        card = card.child(slider_row(row));
    }
    card
}

fn slider_row(row: &fake_data::SliderRow) -> Div {
    let pct = row.pct.clamp(0.0, 1.0);
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().text_size(px(13.)).text_color(theme::alpha(0x52525c, 1.0)).child(row.glyph))
        .child(
            div()
                .flex_1()
                .relative()
                .h(px(5.))
                .rounded(px(5.))
                .bg(theme::alpha(0xe4e4e7, 1.0))
                .child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(pct)).rounded(px(5.)).bg(theme::alpha(theme::light::BRAND, 1.0))),
        )
}

// ── AI 團隊 (interactive) ─────────────────────────────────────────────────

fn ai_team_card(ui: &OverlayUiState, cx: &mut Context<ShellView>) -> Div {
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

    div()
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .border_1()
        .border_color(theme::light::border())
        .rounded(px(13.))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(0x9f9fa9, 1.0))
                .px(px(14.))
                .pt(px(11.))
                .pb(px(9.))
                .border_b_1()
                .border_color(theme::alpha(0xf0f0f2, 1.0))
                .child(fake_data::CC_SECTION_AI_TEAM),
        )
        .child(switch_row(fake_data::CC_SWITCH_AUTOMATION_LABEL, fake_data::CC_SWITCH_AUTOMATION_DESC, ui.automation_on(), true, automation_click))
        .child(switch_row(fake_data::CC_SWITCH_PROACTIVE_LABEL, fake_data::CC_SWITCH_PROACTIVE_DESC, ui.proactive_on(), true, proactive_click))
        .child(switch_row(fake_data::CC_SWITCH_PAUSE_ALL_LABEL, fake_data::CC_SWITCH_PAUSE_ALL_DESC, ui.pause_all_on(), false, pause_all_click))
}

fn switch_row(
    label: &'static str,
    desc: &'static str,
    on: bool,
    border_b: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
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
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(theme::light::FOREGROUND, 1.0)).child(label))
                .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(desc)),
        )
        .child(toggle_pill(on))
        .on_click(on_click);
    if border_b {
        row = row.border_b_1().border_color(theme::alpha(0xf0f0f2, 1.0));
    }
    row
}

fn toggle_pill(on: bool) -> Div {
    let track_hex = if on { theme::light::BRAND } else { 0xe4e4e7 };
    let mut handle = div().absolute().top(px(2.)).w(px(19.)).h(px(19.)).rounded(px(19.)).bg(theme::alpha(0xffffff, 1.0));
    if on {
        handle = handle.right(px(2.));
    } else {
        handle = handle.left(px(2.)).shadow(vec![BoxShadow::new(px(0.), px(1.), rgb(0x0f172a).opacity(0.15).into()).blur_radius(px(2.))]);
    }
    div().relative().w(px(40.)).h(px(23.)).rounded(px(23.)).bg(theme::alpha(track_hex, 1.0)).child(handle)
}

// ── Footer ────────────────────────────────────────────────────────────────

fn footer_row() -> Div {
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
                .child(small_avatar("杜", theme::light::BRAND))
                .child(small_avatar("財", 0x0f766e))
                .child(div().text_size(px(12.)).text_color(theme::alpha(0x71717b, 1.0)).child(fake_data::CC_FOOTER_STATUS)),
        )
        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(theme::light::BRAND, 1.0)).child(fake_data::CC_FOOTER_LINK))
}

fn small_avatar(initial: &'static str, bg_hex: u32) -> Div {
    div()
        .w(px(24.))
        .h(px(24.))
        .rounded(px(12.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(0xfafafa, 1.0)).child(initial))
}
