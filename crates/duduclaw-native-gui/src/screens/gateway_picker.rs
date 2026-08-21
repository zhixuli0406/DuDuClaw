// WP-S6b3-R (S6b 第三波, 2026-08-22) — "GatewayPicker" (`GatewayPicker.dc.
// html`, B23 獨立小視窗). No `nav.rs` entry — self-attached in `screens/
// shell.rs` only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=gatewayPicker`, same "D
// 先掛好分支就直接可達，未掛就自己掛" precedent every prior self-attached S5b/
// S6b page already establishes.
//
// ── Execution-time attribution: read directly, not assumed ──────────────
// `web/src/pages/GatewayPickerPage.tsx`'s own header comment: "the desktop
// shell's pre-login landing page... Desktop-only: outside Tauri it redirects
// to `/`". Its ENTIRE data layer is `web/src/lib/gateway-picker.ts` — a
// typed wrapper file whose own header comment says it outright: "typed
// wrappers over the Tauri `gateway_*` commands". Every one of `gatewayDis
// cover`/`gatewayHealth`/`gatewaySelect`/`gatewayLast`/`gatewayLocalStatus`/
// `gatewayStartLocal` bottoms out in that file's `invoke(cmd, args)` →
// `window.__TAURI__.core.invoke(cmd, args)` (grep-verified: zero `ws.send`/
// gateway-WS-RPC calls anywhere in either file — mDNS LAN discovery, the
// local-sidecar start/stop lifecycle, and persisted "last gateway" state are
// all `src-tauri`-side Rust commands, a separate binary/runtime this crate
// has no IPC channel to, the same "genuinely nothing to call, not a scope
// cut" structural gap `pet_studio.rs`'s own header comment documents for its
// page). This is the SECOND page in this S6b3 batch with that shape — see
// `mascot_overlay.rs` for the one page in the batch that ISN'T fully in
// this category.
//
// ── UNLIKE petStudio, though: `duduclaw-native-gui` has its OWN gateway-
// connection mechanism — much simpler than Tauri's, but real ─────────────
// This crate is not gateway-agnostic like the web dashboard: it is hard-
// wired to exactly ONE local gateway. `api::GATEWAY_BASE_URL: &str =
// "http://127.0.0.1:18789"` is that address (`api.rs`'s own doc comment,
// verbatim: "Not configurable in S2 — every other part of this crate... also
// hardcodes `127.0.0.1:18789`; a settings screen to override it is future
// scope") — `ws_status.rs`'s private `WS_URL` constant points at the same
// host:port for the `/ws` endpoint. Grep-verified across the whole crate:
// no env var, no config file, no dashboard RPC anywhere reads or writes
// this value — it is a compile-time constant, full stop. Per this task's
// own instruction ("若存在...就把「手動輸入」段真接到該機制的顯示（唯讀呈現
// 目前連線目標）"), the "手動輸入" section below shows this REAL constant,
// read-only — not a fake editable+submit flow with nowhere real to send a
// different value. The "本機" card's connection status is ALSO real: `state.
// ws_state` (`WsConnState`) is this crate's actual live WS auth state,
// reusing `shell_sidebar.rs`'s own already-localized `short_label`/
// `dot_color` helpers (the same connection indicator the persistent sidebar
// shows) rather than fabricating a Tauri-style "本機側車 執行中" line this
// runtime cannot verify (there is no sidecar process to poll — the gateway
// this crate talks to might be a full standalone `duduclaw gateway` install,
// not a Tauri-spawned sidecar at all).
//
// ── "已探索" (LAN mDNS discovery) — honest empty state ───────────────────
// No such capability exists in this crate (mDNS browsing is entirely
// `gateway_discover`, Tauri-side) — rendered as an honest empty state, same
// "no fabricated records" convention `pet_studio.rs`'s gallery region and
// `screens::world.rs`'s "no fabricated speech-bubble text" precedent both
// establish. The canvas's own two example rows ("辦公室 Gateway"/"小杜筆電")
// are NOT reproduced — they are the design tool's illustrative sample data,
// not real discoverable gateways this page could ever actually list.
//
// ── Small-window canvas, rendered as a centered card in the normal shell
// (NOT a root-swap) ────────────────────────────────────────────────────────
// The canvas draws this as its own tiny 900×760 chrome-framed window
// (traffic-light dots + centered content) — the SAME design-tool preview-
// frame convention `migrate.rs`'s own header comment documents stripping
// from every other already-ported canvas in this crate (billing.rs/
// license.rs/…). This page keeps the sidebar/three-column shell (unlike
// `migrate.rs`/`launcher.rs`'s full-bleed root swaps) and renders its
// content as a centered max-width card, per this task's own explicit
// instruction ("小視窗版面在標準視窗內以置中卡呈現（誠實：獨立視窗形態留
// desktop 整合）") — a real separate OS-level small window is desktop-shell
// integration scope, not attempted here.
//
// ── No RPC, no page-local mutable state at all ───────────────────────────
// Every value on this page is either a compile-time constant
// (`GATEWAY_BASE_URL`) or already lives on `RootView` (`ws_state`) — there
// is nothing to fetch and nothing to edit, so (unlike every other page in
// this crate) there is no `Global` state struct, no `ensure_state`, no
// `maybe_fetch` here at all. `render` is a pure function of `&RootView`.

use gpui::{div, prelude::*, px, Context, Div, IntoElement, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
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

/// The "本機" card — real data: `api::GATEWAY_BASE_URL` (this build's one
/// fixed target) + `state.ws_state` (this crate's actual live connection
/// state, same source `shell_sidebar.rs`'s persistent indicator reads).
fn local_card(state: &RootView, locale: Locale) -> Div {
    let host = crate::api::GATEWAY_BASE_URL.trim_start_matches("http://").trim_start_matches("https://");

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
                                .child(div().size(px(6.)).rounded_full().bg(theme::alpha(state.ws_state.dot_color(), 1.0)))
                                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(state.ws_state.short_label(locale)))
                                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(format!("· {host}"))),
                        ),
                ),
        )
        .child(
            div()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .px_3p5()
                .py_1p5()
                .child(i18n::t(locale, "gatewayPicker.inUse")),
        );

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

/// The "手動輸入" card — a READ-ONLY display of the one real target this
/// build can ever reach, per this file's header comment. Not a `TextField`:
/// there is no write path this could feed (this task's own "唯讀呈現" call).
fn manual_section(locale: Locale) -> Div {
    let value = div()
        .flex_1()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::dark::input_bg())
        .border_1()
        .border_color(theme::dark::input_border())
        .px_3()
        .py_2()
        .text_size(px(12.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(crate::api::GATEWAY_BASE_URL);

    let disabled_button = div()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::MUTED, 0.4))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .text_size(px(12.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .px(px(18.))
        .py_2()
        .child(i18n::t(locale, "gatewayPicker.connect"));

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(locale, "gatewayPicker.section.manual"))
        .child(div().flex().items_center().gap_2().child(value).child(disabled_button))
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "gatewayPicker.manual.hint")))
}

// ── Top-level render ───────────────────────────────────────────────────
// No `WsConnState` auth GATE (unlike RPC-backed pages) — `state.ws_state`
// is read as plain DATA here, same as `shell_sidebar.rs`'s own indicator;
// this page has nothing that requires `Authenticated` to render correctly.

pub fn render(state: &RootView, _cx: &mut Context<RootView>) -> Stateful<Div> {
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
        .child(local_card(state, locale))
        .child(discovered_section(locale))
        .child(manual_section(locale));

    div().id("gateway-picker-page").size_full().overflow_y_scroll().flex().items_start().justify_center().p_6().child(card)
}
