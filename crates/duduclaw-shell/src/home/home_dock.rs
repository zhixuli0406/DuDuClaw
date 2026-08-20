// Home surface — goal cards row, activity shelf, and dock. Split out of
// `home.rs` per that file's header comment (one screen, two files — same
// convention `duduclaw-native-gui/src/screens/shell.rs`'s own siblings
// establish). `pub(super)`, not `pub`: nothing outside `home.rs` needs
// these directly.
//
// Shell-S1 (2026-08-20): every fn here now takes a
// `palette: ShellPalette` parameter — see `home.rs`'s own header comment
// (unchanged reasoning, just extended to this sibling file) and `crate::
// palette`'s header comment for the Home/overlay light/dark token mapping.

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data::{self, AgentDockStatus, GoalDot};
use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::ShellView;

// ── Goal cards row ─────────────────────────────────────────────────────

pub(super) fn goal_cards_row(palette: ShellPalette) -> Div {
    div()
        .absolute()
        .top(px(360.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .justify_center()
        .gap(px(14.))
        .children(fake_data::GOAL_CARDS.iter().map(move |card| goal_card(card, palette)))
}

fn goal_card(card: &fake_data::GoalCard, palette: ShellPalette) -> Stateful<Div> {
    let dot = match card.dot {
        GoalDot::Outline(kind) => {
            div().w(px(10.)).h(px(10.)).rounded(px(10.)).border(px(2.5)).border_color(theme::alpha(palette.badge_accent(kind), 1.0))
        }
        GoalDot::Filled(kind) => div().w(px(10.)).h(px(10.)).rounded(px(10.)).bg(theme::alpha(palette.badge_accent(kind), 1.0)),
    };

    // Main.dc.html: bg `#ffffff` light / `#1e1e21` dark — `surface_raised`
    // (see `home.rs`'s `menu_bar_ticker` comment for why, not `surface`).
    // Border: opaque `border()` light (matches the board's own `#ececef`
    // exactly) / `rgba(255,255,255,0.10)` dark (bespoke, not `border()`'s
    // own dark 0.06 alpha). Both arms are typed `Hsla` — the dark arm goes
    // through `.into()` (Rgba -> Hsla, the exact same conversion `border()`
    // itself already performs internally) so the LIGHT arm can stay
    // `palette.border()`'s own value completely untouched, with no
    // Rgba<->Hsla round trip anywhere near it (this crate's "light stays
    // byte-identical" constraint applies to that value, not to dark's).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.10).into() } else { palette.border() };
    // Shadow: light matches `palette.surface_shadow()` exactly (`0 1px 2px
    // rgba(15,23,42,.04), 0 1px 1px rgba(15,23,42,.03)`, byte-for-byte the
    // same recipe); dark's own literal (`rgba(0,0,0,.30)`/`rgba(0,0,0,.20)`)
    // is deeper than that token's calibrated `.2`/`.16` — accepted as the
    // same small approximation gap `ShellPalette::floating_shadow`'s own
    // doc comment already documents for the overlay panels, rather than
    // hand-tuning a bespoke shadow for this one card too.

    div()
        .id(card.id)
        .w(px(300.))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(theme::RADIUS_XL))
        .px(px(16.))
        .py(px(14.))
        .shadow(palette.surface_shadow())
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
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(card.title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.badge_text(card.badge_kind))
                        .bg(palette.badge_bg(card.badge_kind))
                        .rounded(px(999.))
                        .px(px(8.))
                        .py(px(2.))
                        .child(card.badge_label),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(card.meta),
        )
}

// ── Activity shelf ──────────────────────────────────────────────────────

pub(super) fn activity_shelf(palette: ShellPalette) -> Div {
    // Main.dc.html: bg `rgba(255,255,255,0.7)` light / `rgba(24,24,27,0.70)`
    // dark — plain `surface` (NOT `surface_raised` — this glass pill, unlike
    // the dock/composer/ticker, deliberately sits on the DARKER menu-bar-
    // level layer, see `crate::palette`'s own header comment on why the two
    // are not interchangeable). Border: light `SURFACE_BORDER` at `0.8` /
    // dark `rgba(255,255,255,0.10)`.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.10) } else { theme::alpha(theme::light::SURFACE_BORDER, 0.8) };
    // Row text: light `#52525c` / dark `#d4d4d8` — text ladder rank 2,
    // matches `text_secondary` exactly (see `crate::palette`'s own header
    // comment for the ladder).
    let mut row = div()
        .id("shell-activity-shelf")
        .flex()
        .items_center()
        .gap(px(18.))
        .bg(theme::alpha(palette.surface, 0.7))
        .border_1()
        .border_color(border_color)
        .rounded(px(999.))
        .px(px(18.))
        .py(px(6.))
        .text_size(px(12.))
        .text_color(theme::alpha(palette.text_secondary, 1.0))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child("今日"),
        );

    // Main.dc.html: the "·" separator is `#d4d4d8` in LIGHT and `#52525c`
    // in DARK — the opposite of `text_secondary`'s own light/dark values
    // (light board deliberately reuses dark's mid-gray here, and vice
    // versa, to keep this specific dot equally faint against either
    // background) — NOT the ladder inversion `crate::palette`'s header
    // comment documents for ranks 3/4 (that's a different, real semantic
    // token; this is a one-off "borrow the other theme's shade" choice for
    // exactly one decorative glyph), so it's kept as its own inline swap
    // rather than aliased to `text_secondary`.
    let separator_hex = if palette.is_dark() { 0x52525c } else { 0xd4d4d8 };

    // Plain imperative loop, not `.children(iter().map(...))`: needs to
    // interleave a "·" separator BETWEEN entries (not after the last one),
    // which a single `.map()` can't express as cleanly. No `Context`/`cx`
    // capture involved either way (this file is presentation-only, see
    // `home.rs`'s header comment) — `duduclaw-native-gui/src/main.rs`'s own
    // gotcha about `&mut Context<V>` not being `Copy` in a repeated closure
    // doesn't apply here.
    for (i, line) in fake_data::ACTIVITY_SHELF.iter().enumerate() {
        if i > 0 {
            row = row.child(div().text_color(theme::alpha(separator_hex, 1.0)).child("·"));
        }
        row = row.child(div().child(*line));
    }

    div().absolute().bottom(px(120.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

// ── Dock ─────────────────────────────────────────────────────────────────

pub(super) fn dock(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // Main.dc.html: bg `rgba(255,255,255,0.78)` light / `rgba(30,30,33,
    // 0.78)` dark — `surface_raised` (matches `composer`'s own reasoning in
    // `home.rs`). Border: light `SURFACE_BORDER` at `0.9` / dark `rgba(255,
    // 255,255,0.12)`. Shadow: bespoke in both themes (`0 8px 24px rgba(15,
    // 23,42,.08)` light / `0 8px 24px rgba(0,0,0,.40)` dark).
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::SURFACE_BORDER, 0.9) };
    let shadow = if palette.is_dark() {
        vec![BoxShadow::new(px(0.), px(8.), rgb(0x000000).opacity(0.40).into()).blur_radius(px(24.))]
    } else {
        vec![BoxShadow::new(px(0.), px(8.), rgb(0x0f172a).opacity(0.08).into()).blur_radius(px(24.))]
    };

    let mut row = div()
        .id("shell-dock")
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(theme::alpha(palette.surface_raised, 0.78))
        .border_1()
        .border_color(border_color)
        .rounded(px(22.))
        .px(px(14.))
        .py(px(10.))
        .shadow(shadow);

    // Only "設" (settings) is interactive this round — every other icon
    // (apps, agents) stays an honest static stub, per the task brief ("其他
    // dock icon 維持靜態").
    for app in fake_data::DOCK_APPS {
        row = row.child(dock_app(app, palette));
    }
    row = row.child(dock_divider(palette));
    for agent in fake_data::DOCK_AGENTS {
        row = row.child(dock_agent(agent, palette));
    }
    row = row.child(dock_divider(palette));
    row = row.child(dock_settings(palette, cx));

    div().absolute().bottom(px(24.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

fn dock_divider(palette: ShellPalette) -> Div {
    // Main.dc.html: opaque `SURFACE_BORDER` light / `rgba(255,255,255,0.12)`
    // dark.
    let color = if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
    div().w(px(1.)).h(px(34.)).bg(color)
}

fn dock_app(app: &fake_data::DockApp, palette: ShellPalette) -> Stateful<Div> {
    // The "white/near-white bordered" app tiles (Files/Browser) are the one
    // documented exception to "identity colors stay hardcoded" — see
    // `crate::palette`'s own header comment. Every OTHER (colored,
    // non-bordered) app tile's gradient is identity and stays exactly as
    // `app.gradient_top`/`gradient_bottom` already store it, in both themes.
    let neutral_dark = app.bordered && palette.is_dark();
    let (gradient_top, gradient_bottom) =
        if neutral_dark { (palette.neutral_tile_top, palette.neutral_tile_bottom) } else { (app.gradient_top, app.gradient_bottom) };

    // Icon glyph color: light is `SURFACE_FOREGROUND` at 0.55 (bordered) /
    // 1.0 (colored) alpha — `SURFACE_FOREGROUND` and `FOREGROUND` share the
    // identical value in both themes (`theme.rs`'s own token table), so
    // `palette.foreground` reproduces it exactly. Dark's bordered case
    // swaps to a solid literal (`#b0b0b8`, the approved canvas's own
    // primary icon-stroke color) instead of a translucent foreground tint —
    // the translucency was only ever standing in for "a lighter gray than
    // the glyph's colored-tile counterpart", which dark expresses with a
    // genuinely different (not just alpha-reduced) hex.
    // Shadow: the approved canvas actually varies opacity per icon FAMILY
    // (colored `.20`/`.35`, neutral-bordered `.14`/`.30`) — but the
    // PRE-EXISTING light-only code already collapsed that distinction to
    // one flat `0.20` for every dock icon regardless of `bordered` (see the
    // original code, unchanged here: light must stay byte-identical, so
    // this does NOT start discriminating by `bordered` now). Dark mirrors
    // the SAME flat-approximation strategy in the SAME direction light
    // already picked (extend the colored-icon family's own value to every
    // icon) — `0.35`, not the neutral family's `0.30`.
    let icon_shadow = palette.icon_shadow(0.20, 0.35);

    let mut el = div()
        .id(app.id)
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(gradient_top), 0.0), linear_color_stop(rgb(gradient_bottom), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(15.))
        .font_weight(FontWeight::MEDIUM)
        .shadow(icon_shadow)
        .child(app.glyph);

    el = if neutral_dark {
        el.text_color(theme::alpha(0xb0b0b8, 1.0))
    } else {
        el.text_color(theme::alpha(palette.foreground, if app.bordered { 0.55 } else { 1.0 }))
    };

    if app.bordered {
        let border_color = if palette.is_dark() { palette.neutral_tile_border } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
        el = el.border_1().border_color(border_color);
    }
    el
}

fn dock_agent(agent: &fake_data::DockAgent, palette: ShellPalette) -> Stateful<Div> {
    // Status dot: `Running` uses the brand hue (matches its own avatar),
    // `NeedsHuman` uses `warning_dot` — NOT `badge_accent(Warning)`, see
    // `AgentDockStatus::badge_kind`'s own doc comment for why this small
    // circular dot is a different field from the badge/ring token in dark.
    let dot_hex = match agent.status {
        AgentDockStatus::Running => palette.brand,
        AgentDockStatus::NeedsHuman => palette.warning_dot,
    };

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
                .text_color(theme::alpha(palette.brand_foreground, 1.0))
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
                .bg(theme::alpha(dot_hex, 1.0))
                .border_2()
                // Main.dc.html: `#ffffff` light / `#1e1e21` dark —
                // `surface_raised` (the dot's border matches the DOCK
                // surface it visually sits against, not the bare `surface`
                // token).
                .border_color(theme::alpha(palette.surface_raised, 1.0)),
        )
}

/// Round 3: clicking the settings icon opens ControlCenter (task brief:
/// "dock 的「設」（設定）icon 點擊 → 開 ControlCenter").
fn dock_settings(palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] dock settings -> open ControlCenter"); }
        view.surface.open(Overlay::ControlCenter);
        cx.notify();
    });

    // Glyph text: light stays `brand_foreground` (byte-identical to before —
    // this crate's CJK-glyph "設" placeholder was already an approximation
    // of the board's own white gear-icon stroke, `#ffffff`, and that
    // approximation is preserved as-is). Dark: `text_secondary` (`#d4d4d8`)
    // matches the board's own dark gear-icon stroke exactly.
    let glyph_color = if palette.is_dark() { palette.text_secondary } else { palette.brand_foreground };

    div()
        .id("shell-dock-settings")
        .cursor_pointer()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(palette.settings_gradient_top), 0.0),
            linear_color_stop(rgb(palette.settings_gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(glyph_color, 1.0))
        .shadow(palette.icon_shadow(0.18, 0.30))
        .hover(|style| style.opacity(0.85))
        .child("設")
        .on_click(on_click)
}
