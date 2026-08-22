// WP-S6b3-R (S6b 第三波, 2026-08-22) — "GatewayPicker", originally shipped
// as a UI-only preview: the "本機" card was READ-ONLY (`api::GATEWAY_BASE_
// URL` was a hardcoded `&str` constant, `ws_state` was purely displayed),
// "已探索" was an honest empty stub (no mDNS dependency in this crate), and
// "手動輸入" was a disabled, unclickable field showing the same fixed
// constant. See git history for that version's full reasoning — most of it
// (the mDNS honesty, the small-window-as-centered-card layout, the "no
// fabricated LAN rows" convention) still applies verbatim below.
//
// WP-C-M2 (2026-08-22, this pass) makes the page real:
//
// - "本機" now shows LIVE `state.sidecar.status()` (`sidecar::
//   SidecarManager` — a real same-machine `duduclaw run` child process this
//   crate spawns/attaches/health-polls, see `sidecar.rs`) instead of just
//   the WS auth state. A "切換到本機" action (`RootView::switch_to_local`)
//   appears whenever the local target isn't the one currently in use.
// - "手動輸入" is a REAL editable field + "連線" button
//   (`RootView::begin_manual_connect`): health-checks the candidate over
//   `ws_status::health_check` (the background tokio runtime — gpui's own
//   executor can't drive `reqwest` directly, see `main.rs`'s gotchas list),
//   and only on success persists the selection (`config::
//   save_gateway_selection`) and retargets every future call this crate
//   makes (`api::set_gateway_base_url`).
// - "已探索" is STILL the honest empty stub — this task's own scope call
//   ("mDNS 若無依賴不硬加") keeps it that way; no `mdns`-family crate was
//   added to this crate's `Cargo.toml`.
//
// This is the SECOND page in the original S6b3 batch to have a real crate-
// side mechanism behind it (unlike `pet_studio.rs`, which has none) — see
// the original version's header comment, preserved in git history, for the
// grep-verified comparison against the Tauri shell's `gateway_picker.rs`
// (four Tauri commands + `desktop.json` this crate has no IPC channel to;
// `config::GatewaySelection`/`native-gui.toml` is this crate's OWN, smaller
// persistence mechanism, not a port of that file).

use gpui::{div, prelude::*, px, Context, Div, IntoElement, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::sidecar::SidecarStatus;
use crate::theme;
use crate::RootView;

const CARD_MAX_WIDTH: f32 = 520.0;

fn section_label(locale: Locale, key: &str) -> Div {
    div()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, key))
}

fn list_box(child: impl IntoElement) -> Div {
    div().rounded(px(theme::RADIUS_LG)).bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).overflow_hidden().child(child)
}

fn row_icon() -> Div {
    div().size(px(34.)).rounded(px(9.)).bg(theme::alpha(theme::MUTED, 1.0)).flex_shrink_0()
}

fn sidecar_status_label(locale: Locale, status: SidecarStatus) -> gpui::SharedString {
    match status {
        SidecarStatus::Running => i18n::t(locale, "gatewayPicker.local.status.running"),
        SidecarStatus::Starting => i18n::t(locale, "gatewayPicker.local.status.starting"),
        SidecarStatus::Stopped => i18n::t(locale, "gatewayPicker.local.status.stopped"),
        SidecarStatus::Error => i18n::t(locale, "gatewayPicker.local.status.error"),
    }
}

fn sidecar_status_color(status: SidecarStatus) -> u32 {
    match status {
        SidecarStatus::Running => theme::SUCCESS,
        SidecarStatus::Starting => theme::WARNING,
        SidecarStatus::Stopped => theme::MUTED_FOREGROUND,
        SidecarStatus::Error => theme::DESTRUCTIVE,
    }
}

/// The "本機" card. Left side: `state.sidecar.status()`/`.port()` — this
/// launch's REAL sidecar process state (see `sidecar.rs`'s `SidecarManager`
/// doc comments for the spawn/health-poll/orphan-reclaim/backoff-restart
/// mechanics behind it), not the connection-to-whatever-is-currently-active
/// `ws_state` the original version showed (that conflated "is the local
/// process alive" with "is THIS app's session authenticated to whichever
/// gateway currently active", which broke the moment "currently active"
/// could be remote). Right side: "使用中" when the local target IS the one
/// in effect, else a real "切換到本機" action.
fn local_card(state: &RootView, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let status = state.sidecar.status();
    let port = state.sidecar.port();
    let host = format!("127.0.0.1:{port}");
    let is_current = crate::api::is_local_gateway_url(&crate::api::gateway_base_url());

    // `.id(...)` on both arms (harmless on the non-interactive "使用中"
    // badge too) so this `if`/`else` unifies to `Stateful<Div>` — see
    // `main.rs`'s gpui-gotchas doc comment: `.id(...)` changes the concrete
    // element type from `Div`, so both branches must agree.
    let action = if is_current {
        div()
            .id("gateway-picker-local-in-use")
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::MUTED, 0.5))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .text_size(px(theme::TEXT_XS))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .px_3p5()
            .py_1p5()
            .child(i18n::t(locale, "gatewayPicker.inUse"))
    } else {
        div()
            .id("gateway-picker-switch-local")
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::BRAND, 1.0))
            .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
            .text_size(px(theme::TEXT_XS))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .px_3p5()
            .py_1p5()
            .cursor_pointer()
            .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
            .active(|style| style.bg(theme::alpha(theme::BRAND, 0.85)))
            .child(i18n::t(locale, "gatewayPicker.local.switchToLocal"))
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.switch_to_local(cx);
            }))
    };

    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .py_3p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(row_icon())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "gatewayPicker.local.name")))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(div().size(px(6.)).rounded_full().bg(theme::alpha(sidecar_status_color(status), 1.0)))
                                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(sidecar_status_label(locale, status)))
                                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(format!("· {host}"))),
                        ),
                ),
        )
        .child(action);

    div().flex().flex_col().gap_1p5().child(section_label(locale, "gatewayPicker.section.local")).child(list_box(row))
}

fn discovered_section(locale: Locale) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(locale, "gatewayPicker.section.discovered"))
        .child(list_box(empty_state(
            "📡",
            i18n::t(locale, "gatewayPicker.discovered.empty.title"),
            Some(i18n::t(locale, "gatewayPicker.discovered.empty.desc")),
            None::<Div>,
        )))
}

/// The "手動輸入" card — a real editable field (`state.gateway_manual_
/// field`) + "連線" button wired to `RootView::begin_manual_connect`
/// (validate → health-check off the gpui executor → persist + retarget on
/// success). While `state.gateway_connecting` is true the button shows a
/// loading/disabled state, same pattern `screens::login`'s submit button
/// already established for its own in-flight request.
fn manual_section(state: &RootView, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let connecting = state.gateway_connecting;

    let field = div()
        .flex_1()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::dark::input_bg())
        .border_1()
        .border_color(theme::dark::input_border())
        .px_3()
        .py_2()
        .text_size(px(12.5))
        .child(state.gateway_manual_field.clone());

    let button = div()
        .id("gateway-picker-manual-connect")
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::BRAND, if connecting { 0.5 } else { 1.0 }))
        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        .text_size(px(12.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .px(px(18.))
        .py_2()
        .child(i18n::t(locale, if connecting { "gatewayPicker.connecting" } else { "gatewayPicker.connect" }))
        .when(!connecting, |el| {
            el.cursor_pointer()
                .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
                .active(|style| style.bg(theme::alpha(theme::BRAND, 0.85)))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.begin_manual_connect(cx);
                }))
        });

    let mut col = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(locale, "gatewayPicker.section.manual"))
        .child(div().flex().items_center().gap_2().child(field).child(button));

    if let Some(err) = state.gateway_connect_error.clone() {
        col = col.child(div().text_size(px(11.)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err));
    } else {
        col = col.child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "gatewayPicker.manual.hint")));
    }

    col
}

// ── Top-level render ───────────────────────────────────────────────────
// No `WsConnState` auth GATE (unlike RPC-backed pages) — `state.ws_state`/
// `state.sidecar` are read as plain DATA here; this page has nothing that
// requires `Authenticated` to render correctly (switching gateways is, if
// anything, more useful while NOT authenticated to anything yet).

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;

    let header = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).child(i18n::t(locale, "app.name"))),
        )
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "gatewayPicker.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "gatewayPicker.subtitle")));

    let card = div()
        .w(px(CARD_MAX_WIDTH))
        .flex()
        .flex_col()
        .gap_5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::floating_shadow())
        .p_6()
        .child(header)
        .child(local_card(state, locale, cx))
        .child(discovered_section(locale))
        .child(manual_section(state, locale, cx));

    div().id("gateway-picker-page").size_full().overflow_y_scroll().flex().items_start().justify_center().p_6().child(card)
}
