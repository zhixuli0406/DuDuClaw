// Home surface — goal cards row, activity shelf, and dock. Split out of
// `home.rs` per that file's header comment (one screen, two files — same
// convention `duduclaw-native-gui/src/screens/shell.rs`'s own siblings
// establish). `pub(super)`, not `pub`: nothing outside `home.rs` needs
// these directly.

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data::{self, GoalDot};
use crate::surface::Overlay;
use crate::ShellView;

// ── Goal cards row ─────────────────────────────────────────────────────

pub(super) fn goal_cards_row() -> Div {
    div()
        .absolute()
        .top(px(360.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .justify_center()
        .gap(px(14.))
        .children(fake_data::GOAL_CARDS.iter().map(goal_card))
}

fn goal_card(card: &fake_data::GoalCard) -> Stateful<Div> {
    let dot = match card.dot {
        GoalDot::Outline(hex) => {
            div().w(px(10.)).h(px(10.)).rounded(px(10.)).border(px(2.5)).border_color(theme::alpha(hex, 1.0))
        }
        GoalDot::Filled(hex) => div().w(px(10.)).h(px(10.)).rounded(px(10.)).bg(theme::alpha(hex, 1.0)),
    };

    div()
        .id(card.id)
        .w(px(300.))
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .border_1()
        .border_color(theme::light::border())
        .rounded(px(theme::RADIUS_XL))
        .px(px(16.))
        .py(px(14.))
        .shadow(theme::light::surface_shadow())
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot)
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::light::FOREGROUND, 1.0))
                        .child(card.title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(card.badge_text_hex, 1.0))
                        .bg(theme::alpha(card.badge_bg_hex, 1.0))
                        .rounded(px(999.))
                        .px(px(8.))
                        .py(px(2.))
                        .child(card.badge_label),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::alpha(theme::light::MUTED_FOREGROUND, 1.0))
                .child(card.meta),
        )
}

// ── Activity shelf ──────────────────────────────────────────────────────

pub(super) fn activity_shelf() -> Div {
    let mut row = div()
        .id("shell-activity-shelf")
        .flex()
        .items_center()
        .gap(px(18.))
        .bg(theme::alpha(theme::light::SURFACE, 0.7))
        .border_1()
        .border_color(theme::alpha(theme::light::SURFACE_BORDER, 0.8))
        .rounded(px(999.))
        .px(px(18.))
        .py(px(6.))
        .text_size(px(12.))
        // Main.dc.html: `#52525c`, no matching `light::` token.
        .text_color(theme::alpha(0x52525c, 1.0))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::light::FOREGROUND, 1.0))
                .child("今日"),
        );

    // Plain imperative loop, not `.children(iter().map(...))`: needs to
    // interleave a "·" separator BETWEEN entries (not after the last one),
    // which a single `.map()` can't express as cleanly. No `Context`/`cx`
    // capture involved either way (this file is presentation-only, see
    // `home.rs`'s header comment) — `duduclaw-native-gui/src/main.rs`'s own
    // gotcha about `&mut Context<V>` not being `Copy` in a repeated closure
    // doesn't apply here.
    for (i, line) in fake_data::ACTIVITY_SHELF.iter().enumerate() {
        if i > 0 {
            // Main.dc.html: `#d4d4d8` separator dot color.
            row = row.child(div().text_color(theme::alpha(0xd4d4d8, 1.0)).child("·"));
        }
        row = row.child(div().child(*line));
    }

    div().absolute().bottom(px(120.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

// ── Dock ─────────────────────────────────────────────────────────────────

pub(super) fn dock(cx: &mut Context<ShellView>) -> Div {
    let mut row = div()
        .id("shell-dock")
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(theme::alpha(theme::light::SURFACE, 0.78))
        .border_1()
        .border_color(theme::alpha(theme::light::SURFACE_BORDER, 0.9))
        .rounded(px(22.))
        .px(px(14.))
        .py(px(10.))
        .shadow(vec![BoxShadow::new(px(0.), px(8.), rgb(0x0f172a).opacity(0.08).into()).blur_radius(px(24.))]);

    // Only "設" (settings) is interactive this round — every other icon
    // (apps, agents) stays an honest static stub, per the task brief ("其他
    // dock icon 維持靜態").
    for app in fake_data::DOCK_APPS {
        row = row.child(dock_app(app));
    }
    row = row.child(dock_divider());
    for agent in fake_data::DOCK_AGENTS {
        row = row.child(dock_agent(agent));
    }
    row = row.child(dock_divider());
    row = row.child(dock_settings(cx));

    div().absolute().bottom(px(24.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

fn dock_divider() -> Div {
    div().w(px(1.)).h(px(34.)).bg(theme::alpha(theme::light::SURFACE_BORDER, 1.0))
}

fn dock_app(app: &fake_data::DockApp) -> Stateful<Div> {
    let mut el = div()
        .id(app.id)
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(app.gradient_top), 0.0),
            linear_color_stop(rgb(app.gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(15.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(
            theme::light::SURFACE_FOREGROUND,
            if app.bordered { 0.55 } else { 1.0 },
        ))
        .shadow(vec![BoxShadow::new(px(0.), px(3.), rgb(0x0f172a).opacity(0.20).into()).blur_radius(px(6.))])
        .child(app.glyph);
    if app.bordered {
        el = el.border_1().border_color(theme::alpha(theme::light::SURFACE_BORDER, 1.0));
    }
    el
}

fn dock_agent(agent: &fake_data::DockAgent) -> Stateful<Div> {
    div()
        .id(agent.id)
        .relative()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(22.))
        .bg(theme::alpha(agent.bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
                .child(agent.initial),
        )
        .child(
            div()
                .absolute()
                .bottom(px(1.))
                .right(px(1.))
                .w(px(11.))
                .h(px(11.))
                .rounded(px(11.))
                .bg(theme::alpha(agent.status.dot_hex(), 1.0))
                .border_2()
                .border_color(theme::alpha(theme::light::SURFACE, 1.0)),
        )
}

/// Round 3: clicking the settings icon opens ControlCenter (task brief:
/// "dock 的「設」（設定）icon 點擊 → 開 ControlCenter").
fn dock_settings(cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] dock settings -> open ControlCenter"); }
        view.surface.open(Overlay::ControlCenter);
        cx.notify();
    });

    div()
        .id("shell-dock-settings")
        .cursor_pointer()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(0xd7dae0), 0.0), linear_color_stop(rgb(0x969ca6), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
        .hover(|style| style.opacity(0.85))
        .child("設")
        .on_click(on_click)
}
