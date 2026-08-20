// Notifications overlay content — Shell-S0 round 2.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Notifications.dc.html`
// — a right-docked floating panel (`top:40px; right:12px; bottom:16px;
// width:390px`, no explicit height: CSS-style top+bottom anchoring stretches
// it to fill, reproduced the same way with gpui's own `.top()`/`.right()`/
// `.bottom()` absolute setters and no `.h(...)` call).
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

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgb, App, BoxShadow, ClickEvent, Context, Div, FontWeight, Rgba, Stateful, Window,
};

use duduclaw_native_gui::theme;

use super::{ApprovalDecision, OverlayUiState};
use crate::{fake_data, ShellView};

pub(super) fn render(ui: &OverlayUiState, cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
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
        .bg(theme::alpha(theme::light::SURFACE, 0.96))
        .border_1()
        .border_color(theme::light::border())
        .shadow(theme::light::floating_shadow())
        .child(header())
        .child(tabs())
        .child(content(ui, cx))
        .child(footer())
}

fn header() -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(18.))
        .pt(px(16.))
        .pb(px(10.))
        .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child(fake_data::NOTIF_HEADER_TITLE))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::light::BRAND, 1.0))
                .child(fake_data::NOTIF_MARK_ALL_READ),
        )
}

fn tabs() -> Div {
    let mut row = div().flex().gap(px(6.)).px(px(18.)).pb(px(12.));
    for (i, tab) in fake_data::NOTIF_TABS.iter().enumerate() {
        let active = i == 0; // Notifications.dc.html: "全部" is the selected tab
        row = row.child(
            div()
                .rounded(px(999.))
                .px(px(12.))
                .py(px(4.))
                .text_size(px(12.))
                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .bg(theme::alpha(if active { 0x09090b } else { 0xf4f4f5 }, 1.0))
                .text_color(theme::alpha(if active { 0xfafafa } else { 0x52525c }, 1.0))
                .child(*tab),
        );
    }
    row
}

fn content(ui: &OverlayUiState, cx: &mut Context<ShellView>) -> Div {
    let mut cards = Vec::with_capacity(fake_data::APPROVAL_CARDS.len());
    for (index, card) in fake_data::APPROVAL_CARDS.iter().enumerate() {
        cards.push(approval_card(card, index, ui.approval_decision(index), cx));
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
                .text_color(theme::alpha(0x9f9fa9, 1.0))
                .px(px(4.))
                .child(fake_data::NOTIF_TODAY_LABEL),
        )
        .children(fake_data::TODAY_ACTIVITY.iter().map(activity_row))
}

fn approval_card(card: &fake_data::ApprovalCard, index: usize, decision: ApprovalDecision, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Notifications.dc.html: pending cards get an amber `#f3d9a4` border;
    // resolved (approved/rejected) cards have no board precedent — settled
    // to the neutral panel-border hex (`#ececef`, same as `theme::light::
    // border()`'s literal value) so a resolved card visually calms down
    // rather than staying flagged.
    let border_hex = if decision == ApprovalDecision::Pending { 0xf3d9a4 } else { 0xececef };

    let mut root = div()
        .id(card.id)
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .border_1()
        .border_color(theme::alpha(border_hex, 1.0))
        .rounded(px(12.))
        .px(px(14.))
        .py(px(13.))
        .shadow(theme::light::surface_shadow())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(avatar(card.agent_initial, card.agent_bg_hex))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(card.question))
                .when(decision == ApprovalDecision::Pending, |el| {
                    el.child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(theme::light::BRAND, 1.0)))
                }),
        )
        .child(div().text_size(px(12.)).text_color(theme::alpha(0x71717b, 1.0)).child(card.detail));

    root = root.child(match decision {
        ApprovalDecision::Pending => approval_actions(card, index, cx),
        ApprovalDecision::Approved => {
            decision_badge(fake_data::NOTIF_APPROVED_LABEL, theme::alpha(theme::light::SUCCESS, 1.0), theme::alpha(0xe9f6eb, 1.0))
        }
        ApprovalDecision::Rejected => decision_badge(
            fake_data::NOTIF_REJECTED_LABEL,
            theme::alpha(theme::light::DESTRUCTIVE, 1.0),
            // No board precedent for a rejected state (the board only ever
            // shows pending cards) — approximated as a light tint of the
            // same destructive token rather than inventing an unrelated hex.
            theme::alpha(theme::light::DESTRUCTIVE, 0.12),
        ),
    });
    root
}

fn approval_actions(card: &fake_data::ApprovalCard, index: usize, cx: &mut Context<ShellView>) -> Div {
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
        .child(approve_button(card.approve_label, index, approve_click))
        .child(reject_button(card.reject_label, index, reject_click))
        .child(
            div()
                .flex_1()
                .text_size(px(11.5))
                .text_color(theme::alpha(0x9f9fa9, 1.0))
                .child(fake_data::NOTIF_NOTE_PLACEHOLDER),
        )
}

fn approve_button(label: &'static str, index: usize, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(("notif-approve", index))
        .cursor_pointer()
        .bg(theme::alpha(theme::light::BRAND, 1.0))
        .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
        .rounded(px(8.))
        .px(px(14.))
        .py(px(5.))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
        .on_click(on_click)
}

fn reject_button(label: &'static str, index: usize, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(("notif-reject", index))
        .cursor_pointer()
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .text_color(theme::alpha(0x52525c, 1.0))
        .border_1()
        .border_color(theme::light::border())
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

fn avatar(initial: &'static str, bg_hex: u32) -> Div {
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(13.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
                .child(initial),
        )
}

fn system_icon() -> Div {
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(7.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(0xd7dae0), 0.0), linear_color_stop(rgb(0x969ca6), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .shadow(vec![BoxShadow::new(px(0.), px(2.), rgb(0x0f172a).opacity(0.14).into()).blur_radius(px(4.))])
        // Update-available system row — the board's own icon is an SVG
        // download arrow; dropped per this file's parent module's glyph
        // convention, substituted with a plain CJK "更新" initial instead.
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(0xffffff, 1.0)).child("更"))
}

fn activity_row(row: &fake_data::ActivityRow) -> Stateful<Div> {
    let avatar_el = match row.avatar {
        fake_data::RowAvatar::Agent { initial, bg_hex } => avatar(initial, bg_hex),
        fake_data::RowAvatar::System => system_icon(),
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
                .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(row.line2)),
        );
    if let Some((label, text_hex, bg_hex)) = row.badge {
        el = el.child(decision_badge(label, theme::alpha(text_hex, 1.0), theme::alpha(bg_hex, 1.0)));
    }
    el
}

fn footer() -> Div {
    div()
        .px(px(18.))
        .py(px(10.))
        .border_t_1()
        .border_color(theme::alpha(0xf0f0f2, 1.0))
        .text_size(px(11.))
        .text_color(theme::alpha(0x9f9fa9, 1.0))
        .child(fake_data::NOTIF_FOOTER)
}
