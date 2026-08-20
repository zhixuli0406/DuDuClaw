// Notifications overlay content — Shell-S0 round 2, dark theme in Shell-S1.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Notifications.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/Notifications.dc.html`
// (dark) — a right-docked floating panel (`top:40px; right:12px;
// bottom:16px; width:390px`, no explicit height: CSS-style top+bottom
// anchoring stretches it to fill, reproduced the same way with gpui's own
// `.top()`/`.right()`/`.bottom()` absolute setters and no `.h(...)` call).
//
// Interactive core (task brief: "審批卡第一公民（內嵌核准/駁回鈕＋脈絡摘
// 要）") — the two approval cards' 核准/駁回 (or 批准/先不要, per-card
// literal labels, see `fake_data::ApprovalCard`'s doc comment) buttons are
// real `cx.listener`s writing into `overlay::OverlayUiState`, re-rendering
// the card as a resolved pill badge once decided (`decision_badge`, shared
// with the "完成"/"等你決定" activity-row badges below it — same visual
// language, one function). Everything else in this file (header, filter
// tabs, activity rows, footer) is static per the task brief's narrower ask
// ("分組/時間戳照畫板" only, no filtering/mark-all-read wiring this round).
//
// `cx: &mut Context<ShellView>` is threaded through via a plain `for` loop
// when building the two approval cards, NOT `.iter().map(...)` — this
// crate's own `duduclaw-native-gui` sibling already hit and documented this
// exact wall (`main.rs`'s gotcha list: "A `&mut Context<V>` is NOT `Copy`,
// so it cannot be captured into a closure that runs more than once ... use
// a plain `for` loop instead"), so the workaround is applied here from the
// start rather than rediscovered.
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment. Same dark-only panel-root
// `.text_color(...)` fallback `overlay/launcher.rs`'s own header comment
// documents (this panel's root never set one either — verified against the
// original code) applies here too, for the identical reason.

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, App, ClickEvent, Context, Div, FontWeight, Rgba, Stateful, Window};

use duduclaw_native_gui::theme;

use super::{ApprovalDecision, OverlayUiState};
use crate::palette::ShellPalette;
use crate::{fake_data, ShellView};

pub(super) fn render(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Notifications.dc.html: bg `rgba(255,255,255,0.96)` light / `rgba(30,
    // 30,33,0.96)` dark — `surface_raised` in both. Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };

    let mut panel = div()
        .id("overlay-notifications-panel")
        .absolute()
        .top(px(40.))
        .right(px(12.))
        .bottom(px(16.))
        .w(px(390.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.96))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow());
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel.child(header(palette)).child(tabs(palette)).child(content(ui, palette, cx)).child(footer(palette))
}

fn header(palette: ShellPalette) -> Div {
    // Notifications.dc.html: "全部標為已讀" is `brand` light / `brand_bright`
    // dark (`#2171cc`/`#59a6ff`).
    let link_text = if palette.is_dark() { palette.brand_bright } else { palette.brand };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(18.))
        .pt(px(16.))
        .pb(px(10.))
        .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child(fake_data::NOTIF_HEADER_TITLE))
        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(link_text, 1.0)).child(fake_data::NOTIF_MARK_ALL_READ))
}

fn tabs(palette: ShellPalette) -> Div {
    // Notifications.dc.html: the ACTIVE tab is an INVERTED chip in both
    // themes (`bg=#09090b/text=#fafafa` light, `bg=#fafafa/text=#18181b`
    // dark) — `active_bg` is `palette.foreground` in both cases; `active_
    // text` has no single field that holds both values (light's `0xfafafa`
    // isn't `surface`/`surface_raised` in light, which is `0xffffff`), so
    // it's a bespoke per-theme pair. The INACTIVE tab is a clean token pair
    // in both themes (`SECONDARY`/`MUTED`/`SURFACE_HOVER` all share one hex
    // per theme already, per `theme.rs`'s own token table, so `surface_
    // hover` covers it) and its text is ladder rank 2 (`text_secondary`).
    let active_bg = palette.foreground;
    let active_text = if palette.is_dark() { palette.surface } else { 0xfafafa };

    let mut row = div().flex().gap(px(6.)).px(px(18.)).pb(px(12.));
    for (i, tab) in fake_data::NOTIF_TABS.iter().enumerate() {
        let active = i == 0; // Notifications.dc.html: "全部" is the selected tab
        let (bg_hex, text_hex) = if active { (active_bg, active_text) } else { (palette.surface_hover, palette.text_secondary) };
        row = row.child(
            div()
                .rounded(px(999.))
                .px(px(12.))
                .py(px(4.))
                .text_size(px(12.))
                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .bg(theme::alpha(bg_hex, 1.0))
                .text_color(theme::alpha(text_hex, 1.0))
                .child(*tab),
        );
    }
    row
}

fn content(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let mut cards = Vec::with_capacity(fake_data::APPROVAL_CARDS.len());
    for (index, card) in fake_data::APPROVAL_CARDS.iter().enumerate() {
        cards.push(approval_card(card, index, ui.approval_decision(index), palette, cx));
    }

    div()
        .flex_1()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(14.))
        .children(cards)
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .px(px(4.))
                .child(fake_data::NOTIF_TODAY_LABEL),
        )
        .children(fake_data::TODAY_ACTIVITY.iter().map(move |row| activity_row(row, palette)))
}

fn approval_card(
    card: &fake_data::ApprovalCard,
    index: usize,
    decision: ApprovalDecision,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    // Notifications.dc.html: pending cards get an amber border — `#f3d9a4`
    // light (opaque) / `rgba(203,148,0,0.45)` dark (`warning` at 45%
    // alpha). Resolved (approved/rejected) cards have no board precedent in
    // EITHER theme (the board only ever renders pending cards) — light
    // settles to the neutral panel-border hex, same as the original
    // light-only code; dark's analog uses this file's own neutral
    // panel-border convention (`rgba(255,255,255,0.12)`, matching the panel
    // root's own border elsewhere in this file) rather than inventing a
    // fourth color.
    let border_color: Rgba = match (decision, palette.is_dark()) {
        (ApprovalDecision::Pending, true) => theme::alpha(palette.warning, 0.45),
        (ApprovalDecision::Pending, false) => theme::alpha(0xf3d9a4, 1.0),
        (_, true) => theme::alpha(0xffffff, 0.12),
        (_, false) => theme::alpha(0xececef, 1.0),
    };

    let mut root = div()
        .id(card.id)
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(12.))
        .px(px(14.))
        .py(px(13.))
        .shadow(palette.surface_shadow())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(avatar(card.agent_initial, card.agent_bg_hex, palette))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(card.question))
                .when(decision == ApprovalDecision::Pending, |el| {
                    el.child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(palette.brand, 1.0)))
                }),
        )
        .child(div().text_size(px(12.)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(card.detail));

    root = root.child(match decision {
        ApprovalDecision::Pending => approval_actions(card, index, palette, cx),
        ApprovalDecision::Approved => decision_badge(fake_data::NOTIF_APPROVED_LABEL, palette.badge_text(fake_data::BadgeKind::Success), palette.badge_bg(fake_data::BadgeKind::Success)),
        ApprovalDecision::Rejected => decision_badge(
            fake_data::NOTIF_REJECTED_LABEL,
            theme::alpha(palette.destructive, 1.0),
            // No board precedent for a rejected state (the board only ever
            // shows pending cards) — approximated as a light tint of the
            // same destructive token, same as the original light-only code;
            // dark's tint bumps to 0.16 to match the family convention the
            // OTHER three badge kinds use in dark (see `crate::palette::
            // ShellPalette::badge_bg`'s own doc comment), rather than
            // reusing light's 0.12 verbatim.
            theme::alpha(palette.destructive, if palette.is_dark() { 0.16 } else { 0.12 }),
        ),
    });
    root
}

fn approval_actions(card: &fake_data::ApprovalCard, index: usize, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let approve_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.approve(index);
        cx.notify();
    });
    let reject_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.reject(index);
        cx.notify();
    });

    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(approve_button(card.approve_label, index, palette, approve_click))
        .child(reject_button(card.reject_label, index, palette, reject_click))
        .child(div().flex_1().text_size(px(11.5)).text_color(theme::alpha(palette.text_faint, 1.0)).child(fake_data::NOTIF_NOTE_PLACEHOLDER))
}

fn approve_button(
    label: &'static str,
    index: usize,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(("notif-approve", index))
        .cursor_pointer()
        .bg(theme::alpha(palette.brand, 1.0))
        .text_color(theme::alpha(palette.brand_foreground, 1.0))
        .rounded(px(8.))
        .px(px(14.))
        .py(px(5.))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
        .on_click(on_click)
}

fn reject_button(
    label: &'static str,
    index: usize,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    // Notifications.dc.html: bg `#ffffff` light / `#27272a` dark — dark
    // deliberately picks `surface_hover`, NOT `surface_raised`, so the
    // button visibly stands out from the card it sits on (light leaves the
    // card/button bg identical, distinguishing the button by border alone —
    // see this file's header comment for why the two themes diverge here).
    // Border: `border()` light (opaque) / `rgba(255,255,255,0.14)` dark
    // (bespoke, not `border()`'s own dark 0.06 alpha).
    let bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };
    div()
        .id(("notif-reject", index))
        .cursor_pointer()
        .bg(theme::alpha(bg, 1.0))
        .text_color(theme::alpha(palette.text_secondary, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(8.))
        .px(px(14.))
        .py(px(5.))
        .text_size(px(12.))
        .child(label)
        .on_click(on_click)
}

/// Shared pill badge — resolved approval outcomes (已核准/已駁回) AND the
/// activity rows' own status pills (完成/等你決定) are visually the same
/// component in the design board, just different label/color pairs.
fn decision_badge(label: &'static str, text: Rgba, bg: Rgba) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(text).bg(bg).rounded(px(999.)).px(px(8.)).py(px(2.)).child(label)
}

fn avatar(initial: &'static str, bg_hex: u32, palette: ShellPalette) -> Div {
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(13.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.brand_foreground, 1.0)).child(initial))
}

fn system_icon(palette: ShellPalette) -> Div {
    // Same neutral gray "settings"/"system update" gradient
    // `home/home_dock.rs::dock_settings` uses — see `crate::palette`'s own
    // header comment (`settings_gradient_top`/`bottom`). Glyph stroke:
    // `#ffffff` light (kept literal, byte-identical to the original) /
    // `text_secondary` (`#d4d4d8`) dark, matching the board's own dark
    // stroke exactly.
    let glyph_color = if palette.is_dark() { palette.text_secondary } else { 0xffffff };
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(7.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(palette.settings_gradient_top), 0.0),
            linear_color_stop(rgb(palette.settings_gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .shadow(palette.icon_shadow(0.14, 0.30))
        // Update-available system row — the board's own icon is an SVG
        // download arrow; dropped per this file's parent module's glyph
        // convention, substituted with a plain CJK "更新" initial instead.
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(glyph_color, 1.0)).child("更"))
}

fn activity_row(row: &fake_data::ActivityRow, palette: ShellPalette) -> Stateful<Div> {
    let avatar_el = match row.avatar {
        fake_data::RowAvatar::Agent { initial, bg_hex } => avatar(initial, bg_hex, palette),
        fake_data::RowAvatar::System => system_icon(palette),
    };

    let mut el = div()
        .id(row.id)
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(6.))
        .py(px(8.))
        .rounded(px(10.))
        .child(avatar_el)
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(12.5)).child(row.line1))
                .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(row.line2)),
        );
    if let Some((label, kind)) = row.badge {
        el = el.child(decision_badge(label, palette.badge_text(kind), palette.badge_bg(kind)));
    }
    el
}

fn footer(palette: ShellPalette) -> Div {
    // Notifications.dc.html: border-top `#f0f0f2` light / `rgba(255,255,
    // 255,0.08)` dark.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    div()
        .px(px(18.))
        .py(px(10.))
        .border_t_1()
        .border_color(border_color)
        .text_size(px(11.))
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .child(fake_data::NOTIF_FOOTER)
}
