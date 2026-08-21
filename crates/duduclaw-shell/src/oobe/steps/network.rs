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

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::i18n::{t, t1, Key};
use crate::oobe::widgets::{self, NetworkFields, StepButtonVariant};
use crate::oobe::{network, NetConnectFailureKind, NetConnectState, NetScanState, OobeFlow, OobeUiState};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(super) fn render(flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let selected_ssid = flow.selections().network_ssid.clone();
    let connected = flow.selections().network_connected;
    let locale = flow.locale();
    let palette = flow.palette();

    let status = match (&selected_ssid, connected) {
        (Some(ssid), true) => widgets::subtitle_dynamic(t1(locale, Key::NetworkConnectedTo, ssid), palette),
        _ => widgets::subtitle(t(locale, Key::NetworkSubtitlePrompt), palette),
    };

    let mut column = div().flex().flex_col().items_center().gap(px(20.)).child(widgets::title(t(locale, Key::NetworkTitle), palette)).child(status);

    if ui.net_backend_kind == Some(network::NetBackendKind::Fake) {
        column = column.child(demo_mode_notice(locale, palette));
    }

    column.child(widgets::card(scan_body(flow, ui, fields, cx), palette))
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

fn scan_body(flow: &OobeFlow, ui: &OobeUiState, fields: &NetworkFields, cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    match &ui.net_scan {
        NetScanState::NeverScanned => never_scanned_panel(locale, palette, cx),
        NetScanState::Scanning => scanning_panel(locale, palette),
        NetScanState::Failed => failed_panel(locale, palette, cx),
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

fn failed_panel(locale: crate::i18n::Locale, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.destructive, 1.0)).child(t(locale, Key::NetworkScanFailedStatus)))
        .child(widgets::step_button("oobe-net-rescan", t(locale, Key::NetworkRescanButton), StepButtonVariant::Secondary, false, palette, click))
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
    let secured = ap.secured;
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
        if secured {
            view.oobe_ui.start_net_awaiting_psk(&ssid);
            psk_entity.update(cx, |field, cx| field.clear(cx));
            cx.notify();
        } else {
            kick_off_connect(view, cx, ssid.clone(), None, false);
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
                .gap(px(10.))
                .child(signal_bars(ap.signal_bars, palette))
                .child(div().text_size(px(theme::TEXT_SM)).child(ap.ssid.clone()))
                .when(ap.secured, |el| {
                    el.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(theme::alpha(palette.muted_foreground, 1.0))
                            .child(t(locale, Key::NetworkSecuredBadge)),
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
        NetConnectState::Failed(NetConnectFailureKind::Unreachable) => {
            body = body.child(message_line(t(locale, Key::NetworkConnectUnreachableError), palette.destructive));
        }
        NetConnectState::AwaitingPsk | NetConnectState::Idle => {}
    }

    let ssid_owned = ssid.to_string();
    let psk_entity = fields.psk.clone();
    let connect_click = cx.listener(move |view, _ev, _window, cx| {
        if view.oobe_ui.net_connect == NetConnectState::Connecting {
            return;
        }
        let secured = view.oobe_ui.net_selected_secured;
        let psk = if secured { Some(psk_entity.read(cx).content.clone()) } else { None };
        if secured {
            let len = psk.as_ref().map(|p| p.chars().count()).unwrap_or(0);
            // Mirrors the real WPA-PSK passphrase length rule (8..=63
            // ASCII characters) — same "catch it before ever dispatching a
            // request" shape `oobe::claim`'s own password-length gate
            // uses. Also re-checked, defense-in-depth, inside
            // `network::FakeNetworkBackend::connect` and effectively
            // enforced by NetworkManager itself for the real backend.
            if !(8..=63).contains(&len) {
                view.oobe_ui.set_net_connect_failed(NetConnectFailureKind::PasswordTooShort);
                cx.notify();
                return;
            }
        }
        kick_off_connect(view, cx, ssid_owned.clone(), psk, secured);
    });

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
fn kick_off_scan(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.oobe_ui.set_net_scanning();
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (backend, kind) = network::select_backend();
        let scan_result = backend.scan();
        let status = backend.status();
        let _ = tx.send((kind, scan_result, status));
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok((kind, scan_result, status)) => {
                let _ = weak.update(cx, |view, cx| {
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
/// update body run from `kick_off_connect`'s poll loop. `secured` decides
/// which of the two backend-layer failure kinds to render (see
/// `oobe::NetConnectFailureKind`'s own doc comment for why that
/// classification lives here, not in `network::NetError` itself).
fn apply_connect_result(view: &mut ShellView, ssid: &str, secured: bool, result: Result<(), network::NetError>) {
    match result {
        Ok(()) => {
            if let Some(flow) = view.oobe.as_mut() {
                flow.set_network(ssid, true);
                crate::oobe::save_state(flow.state());
            }
            view.oobe_ui.reset_net_connect();
        }
        Err(e) => {
            eprintln!("[oobe/network] connect to {ssid:?} failed: {e:?}");
            let kind = if secured { NetConnectFailureKind::WrongPassword } else { NetConnectFailureKind::Unreachable };
            view.oobe_ui.set_net_connect_failed(kind);
        }
    }
}
