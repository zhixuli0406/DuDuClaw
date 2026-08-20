// Step 2 — 網路（阻塞）. §B-1 row 2 + §A consensus #2 (network precedes
// account and blocks progress — cited against Windows 11 Home requiring
// network, §2 line 30, and ChromeOS blocking dead on no network, §6 line
// 84). The task brief itself settles the "can this be deferred like
// RuntimeAuth?" question explicitly: NO — there is no "稍後再說" escape
// hatch for this step; `OobeFlow::can_advance` refuses `next()` until
// `network_connected` is true (see `OobeStep::Network`'s own doc comment),
// and this step is also absent from `OobeStep::is_skippable`'s allow-list.
//
// Fake connect flow (task brief: "選取→假連線成功"): clicking any of the
// static SSIDs immediately marks it connected — there is no failure path to
// retry in this round's stub (an honest simplification: real Wi-Fi
// scanning/joining is out of scope for S1, not a silently hidden gap).
//
// Real i18n as of this round (`flow.locale()`) covers every piece of CHROME
// on this screen — title/subtitle/badges. `net.ssid` itself (from
// `fake_data::FAKE_WIFI_NETWORKS`) is deliberately NOT routed through
// `crate::i18n`: a Wi-Fi network's own name is data, the same category the
// task brief's own item 4 exempts for Home/overlay (see `oobe/fake_data.rs`'s
// header comment) — no real OS translates a neighbor's SSID.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, t1, Key};
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{fake_data, OobeFlow};
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, cx: &mut Context<ShellView>) -> Div {
    let selected_ssid = flow.selections().network_ssid.clone();
    let connected = flow.selections().network_connected;
    let locale = flow.locale();
    let palette = flow.palette();

    let mut rows = div().flex().flex_col().gap(px(6.));
    for (index, net) in fake_data::FAKE_WIFI_NETWORKS.iter().enumerate() {
        let is_selected = connected && selected_ssid.as_deref() == Some(net.ssid);
        rows = rows.child(wifi_row(net, index, is_selected, locale, palette, cx));
    }

    let status = match &selected_ssid {
        Some(ssid) if connected => widgets::subtitle_dynamic(t1(locale, Key::NetworkConnectedTo, ssid), palette),
        _ => widgets::subtitle(t(locale, Key::NetworkSubtitlePrompt), palette),
    };

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::NetworkTitle), palette))
        .child(status)
        .child(widgets::card(rows, palette))
}

fn wifi_row(
    net: &fake_data::FakeWifiNetwork,
    index: usize,
    selected: bool,
    locale: crate::i18n::Locale,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let ssid = net.ssid;
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.oobe.as_mut() {
            flow.set_network(ssid, true);
            crate::oobe::save_state(flow.state());
        }
        cx.notify();
    });

    div()
        .id(("oobe-wifi", index))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if selected { palette.secondary } else { palette.surface }, 1.0))
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(signal_bars(net.signal_bars, palette))
                .child(div().text_size(px(theme::TEXT_SM)).child(net.ssid))
                .when(net.secured, |el| {
                    el.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(theme::alpha(palette.muted_foreground, 1.0))
                            .child(t(locale, Key::NetworkSecuredBadge)),
                    )
                }),
        )
        .child(if selected {
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.success, 1.0))
                .child(t(locale, Key::NetworkConnectedBadge))
        } else {
            div()
        })
        .on_click(on_click)
}

/// Signal-strength indicator — four bars of increasing height, filled up to
/// `strength`. Plain colored `div()`s, not a glyph — same "no `gpui::
/// svg()` usage yet, no guaranteed pictographic glyph coverage" constraint
/// `home.rs`'s header comment documents for this whole crate.
fn signal_bars(strength: u8, palette: ShellPalette) -> Div {
    let mut row = div().flex().items_end().gap(px(2.));
    for bar in 1..=4u8 {
        let filled = bar <= strength;
        row = row.child(
            div()
                .w(px(3.))
                .h(px(4. + f32::from(bar) * 2.))
                .rounded(px(1.))
                .bg(if filled { theme::alpha(palette.brand, 1.0) } else { palette.surface_border }),
        );
    }
    row
}
