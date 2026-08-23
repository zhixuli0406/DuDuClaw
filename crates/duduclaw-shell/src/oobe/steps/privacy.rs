// Step 6 — 隱私與遙測（獨立屏，預設全關）. §B-1 row 6 + §A consensus #4, the
// STRONGEST consensus in the whole survey ("5/8，無反例"). See
// `OobeStep::Privacy`'s own doc comment in `oobe/mod.rs` for why this step
// is always visited (not in the task brief's skippable list) but every
// toggle on it defaults OFF — continuing past it untouched is always the
// safe/expected path, matching the loudest cross-OS agreement this survey
// found.
//
// Four independent opt-in toggles (task brief: "3-4 個 opt-in 開關"),
// `PrivacyToggle::ALL`-driven so adding a fifth later is a one-place change
// (the enum + its `label()`/`description()` match arms in `oobe/mod.rs`),
// not a new hand-written row here.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::icons;
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{OobeFlow, PrivacyToggle};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();
    let mut rows = div().flex().flex_col().gap(px(2.));
    for toggle in PrivacyToggle::ALL {
        rows = rows.child(toggle_row(flow, toggle, locale, palette, cx));
    }

    // ICON-3 (2026-08-23): the 32px `preferences-system-privacy` shield
    // `OOBE-KeySteps.dc.html` puts above this step's title. See
    // `steps::network::render`'s own comment for why the column keeps its
    // uniform 20px gap rather than the board's per-child margins.
    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = icons::icon_or_none(&[(icons::SHIELD, palette.muted_foreground)], 32.) {
        column = column.child(icon);
    }
    column.child(widgets::title(t(locale, Key::PrivacyTitle), palette)).child(widgets::subtitle(t(locale, Key::PrivacySubtitle), palette)).child(widgets::card(rows, palette))
}

fn toggle_row(flow: &OobeFlow, toggle: PrivacyToggle, locale: Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on = flow.privacy_toggle_on(toggle);
    let click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.toggle_privacy(toggle);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    div()
        // Stable across a language change — see `PrivacyToggle::slug()`'s
        // own doc comment in `oobe/mod.rs` for why this is no longer
        // `toggle.label()` (which now varies by locale).
        .id(toggle.slug())
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(11.))
        .px(px(4.))
        .py(px(9.))
        // ICON-3 (2026-08-23) — the operator's ruling ⑤, which overturns
        // this board's own GNOME-派 choice ("頁內四列全裸") in favour of the
        // elementary one: every row carries a 20px icon. The board's own
        // note is explicit that this is all-or-nothing ("要翻案就是整頁四列
        // 都加，不能只加一半"), which is why `icons::privacy_toggle_layers`
        // is keyed by `PrivacyToggle::slug()` and a fifth toggle added
        // without an icon fails a test rather than shipping a half-iconed
        // list.
        .child(icons::icon_or_none(&icons::privacy_toggle_layers(toggle.slug(), palette).unwrap_or_default(), 20.).unwrap_or_else(|| div().into_any_element()))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(toggle.label(locale)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child(toggle.description(locale)),
                ),
        )
        .child(widgets::toggle_pill(on, palette))
        .on_click(click)
}
