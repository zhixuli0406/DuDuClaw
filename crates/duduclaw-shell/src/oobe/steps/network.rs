// Step 2 — 網路（阻塞）. §B-1 row 2 + §A consensus #2 (network precedes
// account and blocks progress, cited against Windows 11 Home requiring
// network, §2 line 30, and ChromeOS blocking dead on no network, §6 line
// 84). The task brief itself settles the "can this be deferred like
// RuntimeAuth?" question explicitly: NO — there is no "稍後再說" escape
// hatch for this step; `OobeFlow::can_advance` refuses `next()` until
// `network_connected` is true (see `OobeStep::Network`'s own doc comment),
// and this step is also absent from `OobeStep::is_skippable`'s allow-list.
//
// ── Shell-S3 (2026-08-21): real Wi-Fi backend ─────────────────────────────
// Round 1/2's fake connect flow ("選取→假連線成功", no failure path at all)
// is replaced here with the real `oobe::network::NetworkBackend` trait
// (`oobe/network/mod.rs`) — scan/connect/status/forget over NetworkManager's
// D-Bus API on Linux, a deterministic in-memory fallback everywhere else
// (Mac dev loop, forced via `DUDUCLAW_SHELL_FAKE_NET=1`, or Linux with
// NetworkManager's D-Bus unreachable — see that module's own header
// comment). Every scan/connect call is real I/O, so — same pattern
// `steps::account`'s gateway RPC already established for this crate — it
// only ever runs from a `std::thread::spawn` closure, bridged back to
// `ShellView` via `std::sync::mpsc` + a `cx.spawn` poll loop
// (`kick_off_scan`/`kick_off_connect` below).
//
// Flow: `NeverScanned` (an explicit "掃描 Wi-Fi" button — this crate's
// established convention is that I/O is ALWAYS click-triggered, never a
// render-time side effect; see `steps::account`'s own header comment for
// the same discipline applied to the gateway claim) -> `Scanning` ->
// `Loaded`/`Failed` (`Loaded` with zero entries renders its own honest
// empty state — task brief's "三態：載入中/掃描失敗/空列表" maps onto
// `Scanning`/`Failed`/`Loaded([])`). Clicking a row: an OPEN network
// dispatches `connect(ssid, None)` immediately; a SECURED one first shows
// the PSK panel (`NetConnectState::AwaitingPsk`) and only dispatches once
// the operator submits a length-valid passphrase. A successful connect
// writes through to `OobeFlow::set_network` (the same PERSISTED field this
// step's `can_advance()` precondition reads) exactly as round 1 did; a
// failed one renders one of three operator-facing messages (see
// `oobe::NetConnectFailureKind`'s own doc comment for why there are three,
// not one).
//
// `net.ssid` itself is still NOT routed through `crate::i18n` — a Wi-Fi
// network's own name is data (now real, scanned data on Linux, previously
// `fake_data`'s literal table), the same category the task brief's own
// item 4 exempts for Home/overlay.
//
// ── D4a-5 (2026-08-23): gateway backend + wired/portal awareness ─────────
// `oobe::network::select_backend()`'s Linux/NetworkManager path is replaced
// by a gateway-HTTP path (see that module's own header comment for the
// D4a §1.2/§5.4 ship-blocker this fixes) — this file's own render logic
// changes in three ways to go with it:
//   1. Connect failures now carry the gateway's own nine-code classification
//      (`network::WifiFailureCode` -> `NetConnectFailureKind::from_code`)
//      instead of the coarser secured-network-implies-wrong-password
//      heuristic; that heuristic is KEPT as the fallback for the two legacy
//      backends (`fake.rs`/`nm.rs`), which never classify anything.
//   2. `kick_off_scan` also fetches the overall connectivity snapshot
//      (`NetworkBackend::network_status`) on the SAME background round
//      trip, storing it on `OobeUiState::net_status` — this is what lets
//      D4a §5.4-2 ("有線已連通時網路步應可通過") and §6 (captive-portal
//      notice) work without a second click-triggered I/O path.
//   3. A captive-portal notice (§6, M1: detect + tell only) renders
//      whenever the last-fetched status says `internet == Portal` — no
//      "open the login page" button is wired (see `portal_notice`'s own
//      doc comment for why: this crate has no reusable "open an arbitrary
//      URL" path today, and the task brief for this round explicitly says
//      not to invent one).
//
// ── D4a-7 (2026-08-31): the wired-only-machine Continue deadlock ──────────
// D4a-5 above deliberately left ONE gap, called out in `kick_off_scan`'s own
// doc comment at the time as "a known limitation, not silently worked
// around": `net_status` (and therefore `OobeUiState::wired_online()`) only
// ever gets fetched from a CLICK on "掃描 Wi-Fi"/"重新整理" — a machine with
// only a wired connection and no Wi-Fi adapter worth scanning has no reason
// to ever click that button, so `wired_online()` stayed `None`-derived
// `false` forever and the Continue button — built `disabled` whenever
// `!can_advance_with_wired`, and `widgets::step_button`'s documented
// contract is to drop `on_click` ENTIRELY while `disabled` — never
// responded to a click at all. A real QEMU wired-only activity run
// reproduced exactly this: two clicks on Continue, zero visible reaction,
// indistinguishable from a hang.
//
// This round closes the gap WITHOUT breaking the "I/O is always
// click-triggered, never a render-time side effect" rule stated above, by
// finding two clicks that were already happening and were not yet doing
// anything with this step:
//   1. `oobe/render.rs`'s Continue-button handler already runs a click
//      every time it advances the PREVIOUS step onto `Network` (`flow.
//      next_with_wired` succeeding with the new `flow.current() ==
//      OobeStep::Network`). That success IS a click event — the operator's
//      own click on the prior step's Continue — so triggering a scan/
//      status fetch from inside that same handler (`on_step_entered`
//      below) is exactly as click-triggered as `kick_off_scan` already is
//      from the "掃描 Wi-Fi" button, just a different click. This also
//      covers 返回→再繼續 re-entering `Network`: `back()` never touches
//      `net_scan`/`net_status`, and the SAME `continue_click` closure fires
//      again on re-entry, since it does not distinguish a first visit from
//      a return visit.
//   2. `widgets::step_button_ex` (see that fn's own doc comment in
//      `widgets.rs`) lets THIS ONE Continue button keep its `on_click`
//      attached even while `disabled` — so a blocked click is a REAL click
//      too, not a swallowed one. `handle_blocked_continue` below is what
//      that click now runs: one more scan/status fetch (guarded against a
//      still-in-flight one), plus flipping `OobeUiState::net_continue_
//      blocked` so the next render shows an honest reason instead of
//      nothing (`continue_blocked_notice`).
//
// Enter-key parity (`main.rs`'s `on_oobe_next` / `OobeFlow::enter_outcome`)
// is deliberately NOT touched this round — `enter_outcome` already treats a
// blocked `Network` step as a legitimate silent `Blocked` no-op (task brief
// scope was the mouse Continue button specifically, where the QEMU repro
// lived), tracked as follow-up, not silently worked around either.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, t1, Key};
use crate::icons;
use crate::oobe::widgets::{self, NetworkFields, StepButtonVariant};
use crate::oobe::{network, NetConnectFailureKind, NetConnectState, NetScanState, OobeFlow, OobeUiState};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let selected_ssid = flow.selections().network_ssid.clone();
    let connected = flow.selections().network_connected;
    let locale = flow.locale();
    let palette = flow.palette();

    // D4a §5.4-2 (2026-08-23): a wired (or portal-gated) connection gets its
    // own status line — checked AFTER the persisted Wi-Fi join (a real
    // completed join is always the more specific fact) and BEFORE the
    // generic "choose a network" prompt.
    let status = if let (Some(ssid), true) = (&selected_ssid, connected) {
        widgets::subtitle_dynamic(t1(locale, Key::NetworkConnectedTo, ssid), palette)
    } else if ui.wired_online() {
        widgets::subtitle(t(locale, Key::NetworkWiredConnected), palette)
    } else {
        widgets::subtitle(t(locale, Key::NetworkSubtitlePrompt), palette)
    };

    // ICON-3 (2026-08-23): `OOBE-KeySteps.dc.html` puts a 32px
    // `network-wireless` above this step's title. All four layers are
    // painted the SAME colour here — the level-dependent tinting is the ROW
    // indicator's job (`signal_arcs` below), not the title's, which is why
    // this goes through `wifi_plain_layers` and not `wifi_signal_layers`.
    //
    // Spacing note (applies to every step-title icon added this round): the
    // board's own rhythm is icon → 16px → title → 10px → subtitle → 20px →
    // card, but this column has used ONE uniform 20px gap since Shell-S1,
    // shared by all ten steps. Reproducing the board's per-child margins for
    // only the steps that gained an icon would make those steps inconsistent
    // with the seven that did not, so the uniform gap is kept deliberately.
    let mut column = div().flex().flex_col().items_center().gap(px(20.));
    if let Some(icon) = icons::icon_or_none(&icons::wifi_plain_layers(palette.muted_foreground), 32.) {
        column = column.child(icon);
    }
    let mut column = column.child(widgets::title(t(locale, Key::NetworkTitle), palette)).child(status);

    if ui.net_backend_kind == Some(network::NetBackendKind::Fake) {
        column = column.child(demo_mode_notice(locale, palette));
    }

    // D4a-7 (2026-08-31): the blocked-Continue-click notice — see
    // `OobeUiState::should_show_net_continue_blocked_notice`'s own doc
    // comment for why this is a pure re-derivation every render call (not a
    // one-shot latch) and `continue_blocked_notice`'s own doc comment for
    // the two messages it picks between.
    if ui.should_show_net_continue_blocked_notice(connected) {
        column = column.child(continue_blocked_notice(locale, palette, ui));
    }

    // D4a §6 (2026-08-23): captive-portal notice — independent of which row
    // (if any) is selected, since a WIRED connection can be portal-gated
    // too.
    if let Some(status) = &ui.net_status {
        if status.internet == network::InternetState::Portal {
            column = column.child(portal_notice(locale, palette, status, &selected_ssid, cx));
        }
    }

    column.child(widgets::card(scan_body(flow, ui, fields, cx), palette))
}

/// D4a §6, M1 scope: detect and tell, plus the one affordance the decision
/// list names ("captive portal M1 偵測＋開啟登入頁鈕").
///
/// The button appears ONLY when the gateway actually reported a
/// `portal_url`. Many portals redirect without a usable `Location`, and a
/// button that opens nothing is worse than no button — so the notice stands
/// alone in that case rather than pretending there is somewhere to go.
///
/// The URL is attacker-supplied (it comes from an access point the operator
/// has not authenticated to), which is why it goes through
/// `oobe::portal_browser::open_portal` rather than straight into a
/// `Command` — see that module's header comment for the full rule set.
fn portal_notice(
    locale: crate::i18n::Locale,
    palette: ShellPalette,
    status: &network::NetworkStatus,
    selected_ssid: &Option<String>,
    cx: &mut Context<ShellView>,
) -> Div {
    let label = status.wifi_ssid.clone().or_else(|| selected_ssid.clone()).unwrap_or_default();
    let mut notice = div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.warning, 0.14))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.warning, 1.0))
                .child(t1(locale, Key::NetworkPortalNotice, &label)),
        );

    if let Some(url) = status.portal_url.clone() {
        // Spawning a browser is real I/O, but it is fire-and-forget (no
        // reply to wait for), so unlike scan/connect it needs no background
        // thread + mpsc bridge — `open_portal` returns as soon as the child
        // is spawned or refused.
        let click = cx.listener(move |_view, _ev, _window, _cx| {
            crate::oobe::portal_browser::open_portal(&url);
        });
        notice = notice.child(div().flex().justify_end().child(widgets::step_button(
            "oobe-net-portal-open",
            t(locale, Key::NetworkPortalOpenButton),
            StepButtonVariant::Secondary,
            false,
            palette,
            click,
        )));
    }

    notice
}

/// The demo-mode disclosure — task brief: "Linux 偵測不到 NM...在 UI 誠實
/// 標示（不可假裝連線成功）", extended here (see `network::NetBackendKind`'s
/// own doc comment) to cover every situation the CURRENT backend is the
/// fake one, not only the Linux-detection-failure case.
fn demo_mode_notice(locale: crate::i18n::Locale, palette: ShellPalette) -> Div {
    div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.warning, 0.14))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(palette.warning, 1.0))
        .child(t(locale, Key::NetworkDemoModeNotice))
}

/// D4a-7 (2026-08-31): honest feedback for a Continue click that hit
/// `can_advance_with_wired == false` — the render side of `OobeUiState::
/// net_continue_blocked` (see that field's own doc comment for the setter,
/// `handle_blocked_continue` below, and the render-time gate,
/// `should_show_net_continue_blocked_notice`, that decides WHETHER this fn
/// gets called at all). Same visual shape as `demo_mode_notice` just above
/// (warning-tinted single-line banner) — this crate has exactly one "soft
/// warning" notice style, reused rather than inventing a second.
fn continue_blocked_notice(locale: crate::i18n::Locale, palette: ShellPalette, ui: &OobeUiState) -> Div {
    div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.warning, 0.14))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(palette.warning, 1.0))
        .child(t(locale, continue_blocked_notice_key(ui.net_scan == NetScanState::Scanning)))
}

/// Pure — split out of `continue_blocked_notice` above purely so the
/// message-choosing DECISION is unit-testable without a `ShellPalette`/
/// `Locale`/render call. `scanning` is true while the background recheck
/// `handle_blocked_continue` itself kicked off (or one already in flight)
/// hasn't settled yet: in that window the honest thing to say is "still
/// checking", not "you're not connected" — the second half of that claim
/// might already be stale by the time it's on screen.
fn continue_blocked_notice_key(scanning: bool) -> crate::i18n::Key {
    if scanning {
        Key::NetworkStatusCheckingNotice
    } else {
        Key::NetworkContinueBlockedNotice
    }
}

fn scan_body(flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    match &ui.net_scan {
        NetScanState::NeverScanned => never_scanned_panel(locale, palette, cx),
        NetScanState::Scanning => scanning_panel(locale, palette),
        NetScanState::Failed => failed_panel(locale, palette, ui, cx),
        NetScanState::Loaded(aps) if aps.is_empty() => empty_panel(locale, palette, cx),
        NetScanState::Loaded(aps) => loaded_panel(aps, flow, ui, fields, cx),
    }
}

fn never_scanned_panel(locale: crate::i18n::Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::NetworkNeverScanned)))
        .child(widgets::step_button("oobe-net-scan", t(locale, Key::NetworkScanButton), StepButtonVariant::Primary, false, palette, click))
}

fn scanning_panel(locale: crate::i18n::Locale, palette: ShellPalette) -> Div {
    div().flex().items_center().justify_center().py(px(16.)).child(
        div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::NetworkScanningStatus)),
    )
}

/// D4a §5.4 (ship-blocker fix, 2026-08-23): when the CURRENT backend is
/// `NetBackendKind::Unavailable` (an appliance whose gateway network
/// backend didn't answer — see `network::select_backend`'s own doc
/// comment), this panel adds a second, more specific line + a "use a wired
/// connection" hint on top of the generic "scan failed, retry" message —
/// never a fabricated SSID list, never a silent claim of success.
fn failed_panel(locale: crate::i18n::Locale, palette: ShellPalette, ui: &OobeUiState, cx: &mut Context<ShellView>) -> Div {
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    let mut column = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.destructive, 1.0)).child(t(locale, Key::NetworkScanFailedStatus)));
    if ui.net_backend_kind == Some(network::NetBackendKind::Unavailable) {
        column = column.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.destructive, 1.0)).child(t(locale, Key::NetworkUnavailableHint)));
    }
    column.child(widgets::step_button("oobe-net-rescan", t(locale, Key::NetworkRescanButton), StepButtonVariant::Secondary, false, palette, click))
}

fn empty_panel(locale: crate::i18n::Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::NetworkScanEmptyStatus)))
        .child(widgets::step_button("oobe-net-rescan", t(locale, Key::NetworkRescanButton), StepButtonVariant::Secondary, false, palette, click))
}

fn loaded_panel(aps: &[network::AccessPoint], flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    let rescan_click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));

    let mut rows = div().flex().flex_col().gap(px(6.));
    for (index, ap) in aps.iter().enumerate() {
        rows = rows.child(wifi_row(ap, index, flow, ui, fields, cx));
    }

    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div().flex().justify_end().child(widgets::step_button(
                "oobe-net-rescan",
                t(locale, Key::NetworkRescanButton),
                StepButtonVariant::Ghost,
                ui.net_connect == NetConnectState::Connecting,
                palette,
                rescan_click,
            )),
        )
        .child(rows);

    if ui.net_connect != NetConnectState::Idle {
        if let Some(ssid) = &ui.net_selected_ssid {
            column = column.child(connect_panel(ssid, flow, ui, fields, cx));
        }
    }

    column
}

fn wifi_row(ap: &network::AccessPoint, index: usize, flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let locale = flow.locale();
    let palette = flow.palette();
    let is_connected_row = flow.selections().network_connected && flow.selections().network_ssid.as_deref() == Some(ap.ssid.as_str());
    let busy = ui.net_connect == NetConnectState::Connecting;

    let ssid = ap.ssid.clone();
    let secured = ap.secured();
    let known = ap.known;
    let psk_entity = fields.psk.clone();
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if view.oobe_ui.net_connect == NetConnectState::Connecting {
            // A connect attempt is already in flight — see
            // `kick_off_connect`'s own doc comment for why this guard is
            // the authoritative one (the row is also visually inert while
            // busy, see `busy` above, but that's cosmetic).
            return;
        }
        let already_connected =
            view.oobe.as_ref().is_some_and(|f| f.selections().network_connected && f.selections().network_ssid.as_deref() == Some(ssid.as_str()));
        if already_connected {
            return;
        }
        // D4a §5.2 (2026-08-23): a network with a saved credential
        // (`known`) connects straight away, same as an open network — the
        // gateway already has what it needs, and re-prompting for a
        // password the operator would have to guess (they may never have
        // typed it into THIS session at all) would be dishonest UX. Only a
        // secured, NOT-yet-known network shows the PSK prompt.
        if secured && !known {
            view.oobe_ui.start_net_awaiting_psk(&ssid);
            psk_entity.update(cx, |field, cx| field.clear(cx));
            cx.notify();
        } else {
            kick_off_connect(view, cx, ssid.clone(), None, secured);
        }
    });

    let mut row = div()
        .id(("oobe-wifi", index))
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if is_connected_row { palette.secondary } else { palette.surface }, 1.0))
        .child(
            div()
                .flex()
                .items_center()
                // 11px, the board's own row gap (was 10 when the indicator
                // was a four-div bar stack).
                .gap(px(11.))
                .child(signal_arcs(ap.signal_bars, palette))
                .child(div().text_size(px(theme::TEXT_SM)).child(ap.ssid.clone()))
                .when(ap.secured() && !ap.known, |el| {
                    // ICON-3 (2026-08-23): `network-wireless-encrypted`, a
                    // 14px padlock, replaces the "需密碼" text
                    // (`OOBE-KeySteps.dc.html`, and the assignment table's
                    // own "取代「需密碼」文字"). The text stays in the
                    // catalog as this icon's `icon_or_glyph` fallback, so a
                    // missing asset degrades to the pre-ICON-3 row rather
                    // than dropping the "you'll need a password" signal
                    // entirely. D4a §5.2 (2026-08-23): gated on `!ap.known`
                    // too — a network the gateway already has a saved
                    // credential for is not going to ask for a password, so
                    // showing "需密碼" would be actively misleading.
                    el.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(theme::alpha(palette.muted_foreground, 1.0))
                            .child(icons::icon_or_glyph(
                                &[(icons::LOCK_CLOSED_SM, palette.muted_foreground)],
                                14.,
                                t(locale, Key::NetworkSecuredBadge),
                            )),
                    )
                }),
        )
        .child(if is_connected_row {
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.success, 1.0))
                .child(t(locale, Key::NetworkConnectedBadge))
        } else {
            div()
        });

    if !busy && !is_connected_row {
        row = row.cursor_pointer().hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(on_click);
    }
    row
}

/// Signal-strength indicator — ICON-3 (2026-08-23).
///
/// This used to be four plain `div()` bars of increasing height, and the
/// comment here used to say the OOBE boards "contain no SVG at all". That
/// was true of the Shell-S1 boards; it is not true of
/// `OOBE-KeySteps.dc.html`, which draws the real five-level concentric-arc
/// family and explicitly retires the bar stack ("現行的四格 div 條…退休").
/// `crate::icons::wifi_signal_layers` owns the level → tint decision; this
/// fn is now only the 20px frame around it.
///
/// Degrades to NOTHING (not to a substitute glyph) if an asset is missing —
/// the row still reads correctly without it (SSID + lock + connected badge
/// are all still there), and there is no honest single character that means
/// "three of five bars". Same call `icon_or_none`'s own doc comment
/// describes for the menu-bar Wi-Fi indicator.
fn signal_arcs(strength: u8, palette: ShellPalette) -> Div {
    let layers = icons::wifi_signal_layers(strength, palette);
    match icons::icon_or_none(&layers, 20.) {
        Some(icon) => div().flex().flex_none().child(icon),
        None => div().flex_none(),
    }
}

/// The PSK-entry / connecting-progress / failure panel — shown once a row
/// has been picked (`ui.net_selected_ssid.is_some()`) and the connect flow
/// hasn't settled back to `Idle`. `ui.net_selected_secured` (captured at
/// SELECTION time, see that field's own doc comment) decides whether the
/// PSK field renders at all: an open network's Connecting/Failed states
/// still land here, just without a password to type.
fn connect_panel(ssid: &str, flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();
    let secured = ui.net_selected_secured;
    let in_flight = ui.net_connect == NetConnectState::Connecting;

    let mut body = div().flex().flex_col().gap(px(10.));

    if secured {
        body = body.child(labeled_field(t(locale, Key::NetworkPskLabel), fields.psk.clone(), palette));
    }

    match ui.net_connect {
        NetConnectState::Connecting => {
            body = body.child(message_line(t(locale, Key::NetworkConnectingStatus), palette.muted_foreground));
        }
        NetConnectState::Failed(NetConnectFailureKind::PasswordTooShort) => {
            body = body.child(message_line(t(locale, Key::NetworkPskLengthError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::WrongPassword) => {
            body = body.child(message_line(t(locale, Key::NetworkWrongPasswordError), palette.destructive));
        }
        // D4a §5.3 (2026-08-23): the remaining eight arms mirror the
        // gateway's own nine-code classification (`WifiFailureCode` ->
        // `NetConnectFailureKind::from_code`) one message per code, per the
        // design doc's own error table. `NoIp`/`Portal` interpolate the
        // SSID (`t1`) — the other six are static.
        NetConnectState::Failed(NetConnectFailureKind::NotFound) => {
            body = body.child(message_line(t(locale, Key::NetworkNotFoundError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::OutOfRange) => {
            body = body.child(message_line(t(locale, Key::NetworkOutOfRangeError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::NoAdapter) => {
            body = body.child(message_line(t(locale, Key::NetworkNoAdapterError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::DriverMissing) => {
            body = body.child(message_line(t(locale, Key::NetworkDriverMissingError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::NoIp) => {
            body = body.child(message_line_dynamic(t1(locale, Key::NetworkNoIpError, ssid), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::Portal) => {
            // Rendered as a WARNING, not a destructive/error color — D4a
            // §5.4-2/§6: a portal classification is a soft success at the
            // link layer (see `apply_connect_result`'s own comment on why
            // it still records the join), not a failure the operator needs
            // to retry.
            body = body.child(message_line_dynamic(t1(locale, Key::NetworkPortalNotice, ssid), palette.warning));
        }
        NetConnectState::Failed(NetConnectFailureKind::BackendUnavailable) => {
            body = body.child(message_line(t(locale, Key::NetworkBackendUnavailableError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::UnsupportedSecurity) => {
            body = body.child(message_line(t(locale, Key::NetworkUnsupportedSecurityError), palette.destructive));
        }
        NetConnectState::Failed(NetConnectFailureKind::Unreachable) => {
            body = body.child(message_line(t(locale, Key::NetworkConnectUnreachableError), palette.destructive));
        }
        NetConnectState::AwaitingPsk | NetConnectState::Idle => {}
    }

    let connect_click = cx.listener(move |view, _ev, _window, cx| try_submit(view, cx));

    let psk_entity_for_cancel = fields.psk.clone();
    let cancel_click = cx.listener(move |view, _ev, _window, cx| {
        if view.oobe_ui.net_connect == NetConnectState::Connecting {
            // Backing out mid-flight would leave the background thread's
            // eventual result with nothing meaningful to apply to (the
            // selection would already be cleared) — same "ignore clicks
            // while in flight" guard every other button in this panel
            // uses.
            return;
        }
        view.oobe_ui.reset_net_connect();
        view.oobe_ui.clear_net_selected_ssid();
        psk_entity_for_cancel.update(cx, |field, cx| field.clear(cx));
        cx.notify();
    });

    let connect_label = if in_flight { t(locale, Key::NetworkConnectingButton) } else { t(locale, Key::NetworkConnectButton) };
    body = body.child(
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(widgets::step_button("oobe-net-connect", connect_label, StepButtonVariant::Primary, in_flight, palette, connect_click))
            .child(widgets::step_button("oobe-net-cancel", t(locale, Key::NetworkCancelButton), StepButtonVariant::Ghost, in_flight, palette, cancel_click)),
    );

    widgets::card(body, palette)
}

/// The "連線" submit for whichever row is currently selected
/// (`ui.net_selected_ssid`) — validates the PSK (if the row is secured),
/// then dispatches the connect attempt on a background thread. Extracted
/// out of `connect_panel`'s `connect_click` closure (WP-oobe-enter,
/// 2026-08-23) so it has exactly ONE body reachable from TWO triggers: the
/// button's own click, and `main.rs`'s `on_oobe_next` (bound to Enter) via
/// `super::handle_enter_submit` — see `OobeFlow::enter_outcome`'s own doc
/// comment in `state.rs` for why Enter needs this at all: without it,
/// Enter while typing a Wi-Fi passphrase was a silent no-op, since `next_
/// with_wired` alone can never satisfy `Network`'s own precondition before
/// a connect attempt has actually settled. Reads `view.oobe_ui`/`view.
/// oobe_network_fields` fresh at call time rather than taking pre-cloned
/// entity handles as parameters — same reasoning `steps::account::try_
/// submit`'s own doc comment gives.
pub(super) fn try_submit(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.oobe_ui.net_connect == NetConnectState::Connecting {
        return;
    }
    let Some(ssid) = view.oobe_ui.net_selected_ssid.clone() else {
        // Nothing selected — there is no row for this submit to act on.
        // Not normally reachable (the connect panel that owns this action
        // only ever renders once a row is picked, and `OobeFlow::enter_
        // outcome` only routes here when `net_connect != Idle`, which
        // implies a selection), a defensive no-op either way.
        return;
    };
    let secured = view.oobe_ui.net_selected_secured;
    let psk = if secured { Some(view.oobe_network_fields.psk.read(cx).content(cx)) } else { None };
    if secured {
        let len = psk.as_ref().map(|p| p.chars().count()).unwrap_or(0);
        // Mirrors the real WPA-PSK passphrase length rule (8..=63 ASCII
        // characters) — same "catch it before ever dispatching a request"
        // shape `oobe::claim`'s own password-length gate uses. Also
        // re-checked, defense-in-depth, inside `network::
        // FakeNetworkBackend::connect` and effectively enforced by
        // NetworkManager itself for the real backend.
        if !(8..=63).contains(&len) {
            view.oobe_ui.set_net_connect_failed(NetConnectFailureKind::PasswordTooShort);
            cx.notify();
            return;
        }
    }
    kick_off_connect(view, cx, ssid, psk, secured);
}

fn labeled_field(label: &'static str, field: gpui::Entity<widgets::OobeTextField>, palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}

fn message_line(text: &'static str, color: u32) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text)
}

/// Same as `message_line`, for a runtime-computed string (e.g. `t1`'s SSID
/// interpolation) — mirrors `widgets::subtitle_dynamic`'s relationship to
/// `widgets::subtitle`.
fn message_line_dynamic(text: String, color: u32) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text)
}

/// Kicks off a scan on a background thread and bridges its result back to
/// `ShellView` — same background-thread -> `std::sync::mpsc` ->
/// `cx.spawn` poll-loop pattern `steps::account`'s click handler already
/// established for the gateway claim RPC (see that file's own header
/// comment for why: no `reqwest`/`tokio` in this crate, and gpui's main
/// thread must never block on real I/O). Also fetches `NetworkBackend::
/// status()` in the same background call — see the `Ok(aps)` arm below for
/// why: this is the ONE production call site that gives `status()` (a
/// trait method the task brief requires but this round's UI flow never
/// otherwise needed) a real purpose, rather than leaving it reachable only
/// from `fake.rs`'s own unit tests.
///
/// D4a-5 (2026-08-23): ALSO fetches `NetworkBackend::network_status()` on
/// this SAME round trip — this is the "順手取一次 status" D4a §5.4-2/§6
/// call for, piggybacked on the operator's existing "掃描 Wi-Fi"/"重新整
/// 理" click rather than a new click-triggered path, per this crate's own
/// "I/O is always click-triggered, never a render-time side effect"
/// discipline (see this file's own header comment).
///
/// D4a-7 (2026-08-31): this fn used to auto-fire ONLY from the "掃描
/// Wi-Fi"/"重新整理" buttons, which left a wired-only machine (no reason to
/// ever click either) with `wired_online()` stuck `false` forever — this
/// file's own header comment records the full QEMU repro and the fix.
/// `kick_off_scan` ITSELF is unchanged (still exactly one background round
/// trip, still only reachable from a real click's handler — nothing here
/// runs at render time); what changed is which clicks reach it: `on_step_
/// entered` and `handle_blocked_continue` below are two MORE click-driven
/// callers, on top of the pre-existing scan/rescan buttons.
fn kick_off_scan(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.oobe_ui.set_net_scanning();
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (backend, kind) = network::select_backend();
        let scan_result = backend.scan();
        let status = backend.status();
        let net_status = backend.network_status();
        let _ = tx.send((kind, scan_result, status, net_status));
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok((kind, scan_result, status, net_status)) => {
                let _ = weak.update(cx, |view, cx| {
                    // `Err` (every backend but `GatewayNetworkBackend`, or a
                    // gateway that itself failed this round) clears any
                    // previous snapshot rather than keeping a stale one —
                    // "空結果優於假結果": a wired cable that was unplugged
                    // since the last successful scan must not keep showing
                    // as connected just because the LATEST refresh happened
                    // to fail.
                    view.oobe_ui.net_status = net_status.ok();
                    match scan_result {
                        Ok(aps) => {
                            view.oobe_ui.set_net_scan_loaded(aps, kind);
                            // The backend may already be joined to a
                            // network OOBE's own persisted selection
                            // doesn't know about yet (e.g. this is a
                            // resumed OOBE run and the device stayed
                            // associated across the restart) — adopt it
                            // rather than making the operator re-pick and
                            // re-type a password the system already has.
                            // Never DROPS an existing persisted selection
                            // on `Disconnected`/`Connecting`: those are
                            // "no new information" states, not evidence
                            // the prior selection went stale.
                            if let network::ConnStatus::Connected { ssid } = status {
                                if let Some(flow) = view.oobe.as_mut() {
                                    let already_recorded = flow.selections().network_connected && flow.selections().network_ssid.as_deref() == Some(ssid.as_str());
                                    if !already_recorded {
                                        flow.set_network(&ssid, true);
                                        crate::oobe::save_state(flow.state());
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[oobe/network] scan failed: {e:?}");
                            view.oobe_ui.set_net_scan_failed(kind);
                        }
                    }
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
    })
    .detach();
}

/// `kick_off_scan`, guarded against firing a SECOND background round trip
/// while one is already in flight — the two new call sites below
/// (`on_step_entered`, `handle_blocked_continue`) both need this: a fast
/// double-click on the previous step's Continue button, or a Continue click
/// on `Network` landing right after step-entry already kicked one off, must
/// never race two threads writing `oobe_ui.net_scan`/`net_status` out of
/// order. The pre-existing scan/rescan buttons (`never_scanned_panel`,
/// `failed_panel`, `empty_panel`, `loaded_panel`'s rescan) don't need this
/// guard themselves — each only ever renders (and is clickable) while
/// `net_scan` is settled (`Failed`/`Loaded(_)`), never while `Scanning`
/// (that state renders `scanning_panel`, with no button at all) — so they
/// keep calling `kick_off_scan` directly, unchanged.
fn kick_off_scan_if_idle(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.oobe_ui.net_scan == NetScanState::Scanning {
        return;
    }
    kick_off_scan(view, cx);
}

/// D4a-7 (2026-08-31): fires when a click on the PREVIOUS step's own
/// Continue button lands ON `Network` — see this file's own header comment
/// for why that's still a real click event, not render-time I/O, and
/// `render.rs`'s `continue_click` for the one call site (via `steps::
/// on_network_step_entered`, the thin visibility-bridging wrapper `steps/
/// mod.rs` needs since `render.rs` is a SIBLING of this module, not a
/// descendant — same shape `handle_enter_submit` already establishes for
/// crossing that same boundary).
///
/// Clears `net_continue_blocked` FIRST — a stale notice from an earlier
/// visit to this step must never flash on screen for even one frame before
/// this visit's own fresh scan settles — then kicks the guarded scan/status
/// recheck.
pub(super) fn on_step_entered(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.oobe_ui.set_net_continue_blocked(false);
    kick_off_scan_if_idle(view, cx);
}

/// D4a-7 (2026-08-31): the `Network` step's OWN Continue-button handler when
/// `OobeFlow::can_advance_with_wired` is still false — reachable at all only
/// because `render.rs`'s `button_row` builds this ONE Continue button
/// through `widgets::step_button_ex` with `clickable_while_disabled: true`
/// instead of plain `step_button` (see that fn's own doc comment in
/// `widgets.rs`); every other step's disabled Continue stays fully inert,
/// unchanged.
///
/// (a) kicks the guarded background rescan+status recheck — this click IS
/// the trigger, same "I/O is click-triggered" discipline this file's header
/// comment states; (b) flips `net_continue_blocked` so the very next render
/// shows an honest reason (`continue_blocked_notice`) instead of nothing.
/// Does NOT itself re-check `can_advance_with_wired` or attempt to advance
/// — this fn only runs from the branch of `render.rs`'s `continue_click`
/// where `next_with_wired` already just returned `false`; if the recheck
/// this kicks off later flips `wired_online()` true, the OPERATOR'S NEXT
/// click is what advances (`button_row` recomputes `can_advance` fresh every
/// render — see that fn's own header comment), never this one automatically.
pub(super) fn handle_blocked_continue(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.oobe_ui.set_net_continue_blocked(true);
    kick_off_scan_if_idle(view, cx);
    cx.notify();
}

/// Kicks off a connect attempt on a background thread — same bridge shape
/// as `kick_off_scan` above. `psk` is moved into the thread closure and
/// dropped once `backend.connect` returns; it is never logged (`apply_
/// connect_result` below only ever sees `Result<(), network::NetError>`,
/// which carries no PSK) and never stored on `ShellView`/`OobeState` —
/// only the SSID and the connected flag are persisted (`OobeFlow::
/// set_network`), matching how the `AccountCreate` step's password is
/// handled (see `OobeSelections::operator_name`'s own doc comment for that
/// precedent).
fn kick_off_connect(view: &mut ShellView, cx: &mut Context<ShellView>, ssid: String, psk: Option<String>, secured: bool) {
    view.oobe_ui.set_net_connecting(&ssid, secured);
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    let ssid_for_thread = ssid.clone();
    std::thread::spawn(move || {
        let (backend, _kind) = network::select_backend();
        let result = backend.connect(&ssid_for_thread, psk.as_deref());
        let _ = tx.send(result);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_connect_result(view, &ssid, secured, result);
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
    })
    .detach();
}

/// Applies a settled `NetworkBackend::connect` result — the weak-entity
/// update body run from `kick_off_connect`'s poll loop.
///
/// D4a-5 (2026-08-23): three cases, in order of specificity —
///   1. `Classified(code)` — the gateway backend classified the failure
///      itself (D4a §5.3); `NetConnectFailureKind::from_code` carries that
///      through verbatim so `connect_panel` renders the SPECIFIC message.
///      `Portal` is special-cased: D4a §5.4-2 treats a captive portal as a
///      soft SUCCESS at the link layer ("portal 表示實體連通只是要登入"),
///      so the join is still recorded (`flow.set_network`) even though the
///      UI still surfaces the login-required notice via `NetConnectState::
///      Failed` — the operator is not blocked on the OOBE Wi-Fi step by a
///      network they DID successfully join.
///   2. `NotFound` — the legacy backends' own unclassified "no such SSID"
///      (`fake.rs`/`nm.rs`); maps straight onto the SAME UI variant a
///      gateway `Classified(NotFound)` would, rather than falling into the
///      coarser secured-network heuristic below.
///   3. Everything else — the pre-D4a-5 heuristic (secured ⇒ probably wrong
///      password, else probably unreachable), kept as-is for the two
///      legacy backends' remaining unclassified failure variants (`network::
///      NetError`'s own doc comment for why that classification lives here,
///      not in the error type itself).
fn apply_connect_result(view: &mut ShellView, ssid: &str, secured: bool, result: Result<(), network::NetError>) {
    match result {
        Ok(()) => {
            if let Some(flow) = view.oobe.as_mut() {
                flow.set_network(ssid, true);
                crate::oobe::save_state(flow.state());
            }
            view.oobe_ui.reset_net_connect();
        }
        Err(network::NetError::Classified(code)) => {
            eprintln!("[oobe/network] connect to {ssid:?} failed, gateway classified: {code:?}");
            if code == network::WifiFailureCode::Portal {
                if let Some(flow) = view.oobe.as_mut() {
                    flow.set_network(ssid, true);
                    crate::oobe::save_state(flow.state());
                }
            }
            view.oobe_ui.set_net_connect_failed(NetConnectFailureKind::from_code(code));
        }
        Err(network::NetError::NotFound) => {
            eprintln!("[oobe/network] connect to {ssid:?} failed: not found");
            view.oobe_ui.set_net_connect_failed(NetConnectFailureKind::NotFound);
        }
        Err(e) => {
            eprintln!("[oobe/network] connect to {ssid:?} failed: {e:?}");
            let kind = if secured { NetConnectFailureKind::WrongPassword } else { NetConnectFailureKind::Unreachable };
            view.oobe_ui.set_net_connect_failed(kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── D4a-7: continue_blocked_notice_key ─────────────────────────────
    // Pure logic only, same style every other `network_ui`/`ui_state` test
    // in this crate already uses — no `gpui::Context`, no background
    // thread, no `ShellView` needed for this one decision.

    #[test]
    fn continue_blocked_notice_key_picks_the_checking_message_while_scanning() {
        assert_eq!(continue_blocked_notice_key(true), Key::NetworkStatusCheckingNotice);
    }

    #[test]
    fn continue_blocked_notice_key_picks_the_not_connected_message_when_idle() {
        assert_eq!(continue_blocked_notice_key(false), Key::NetworkContinueBlockedNotice);
    }
}
