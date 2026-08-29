// Installer-settings-integration WP1 (2026-08-29,
// `commercial/docs/DESIGN-installer-settings-integration-2026-08.md` §3.1) —
// the live-installer's own Theme step. Re-derives `oobe::steps::theme`'s
// two-card layout (the same `widgets::theme_preview` illustration — promoted
// out of `oobe::steps::theme` and into `oobe::widgets` this same round so
// both wizards can call it, see that fn's own doc comment — the same
// brand-bordered selection + "已選擇" tag) against THIS flow's own
// `view.live_install` field instead of `view.oobe`: the two wizards are
// separate state machines (see `live_install/mod.rs`'s header comment for
// why), so the click handler can't reuse OOBE's `set_theme` closure verbatim
// even though the visual row is the same shape — the exact same "re-derive a
// thin UI glue layer over shared plain data/widgets" split `steps::
// language`'s own header comment establishes as this crate's validated
// template for reusing an `oobe::*` step.
//
// Unlike `oobe::steps::theme::theme_option_card`'s click handler, this one
// does NOT call `oobe::save_state` — a live-install session persists nothing
// to disk of its own (see `state.rs`'s own header comment: a reboot out of
// the live image re-runs this whole binary from step 0 regardless of what
// any on-disk file would say, so persisting here would have no reader).
// `flow.set_theme` + `cx.notify()` is the whole click; the live repaint
// itself falls straight out of `LiveInstallFlow::palette()` being resolved
// fresh on every render pass (see that method's own doc comment), same as
// every OTHER screen already reflows on a language change.
//
// No accessibility-icon lookup here (unlike `oobe::steps::theme`'s ICON-3
// title icon) — same P2 scope decision `steps::language`'s own header
// comment already makes for skipping OOBE affordances beyond this wizard's
// core navigation; every other live-install step (`disk_select`/`confirm`)
// is icon-free too.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::oobe::widgets;
use crate::oobe::ThemeChoice;
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::LiveInstallFlow;

pub(super) fn render(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let selected = flow.theme_choice();
    let locale = flow.locale();
    let palette = flow.palette();

    let body = div()
        .flex()
        .gap(px(14.))
        .child(theme_option_card(ThemeChoice::Light, selected == ThemeChoice::Light, locale, palette, cx))
        .child(theme_option_card(ThemeChoice::Dark, selected == ThemeChoice::Dark, locale, palette, cx));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::ThemeTitle), palette))
        .child(widgets::subtitle(t(locale, Key::ThemeSubtitle), palette))
        .child(widgets::card(body, palette))
}

fn theme_option_card(choice: ThemeChoice, selected: bool, locale: Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let id = match choice {
        ThemeChoice::Light => "live-install-theme-light",
        ThemeChoice::Dark => "live-install-theme-dark",
    };
    let label = match choice {
        ThemeChoice::Light => t(locale, Key::ThemeLight),
        ThemeChoice::Dark => t(locale, Key::ThemeDark),
    };
    let click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            flow.set_theme(choice);
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
        .child(widgets::theme_preview(choice))
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
