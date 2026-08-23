// D4b — 網路. The one page that covers both links this machine can have.
//
// ── Wi-Fi (D4a-6) ──────────────────────────────────────────────────────
// Reuses the D4a backend wholesale: `network.status`, `network.wifi_scan`,
// `network.wifi_connect`, `network.wifi_forget`, all admin+appliance WS RPCs
// against iwd over D-Bus. The shell never talks to iwd — that separation is
// the whole point of the D4a design (§3.3: keep the kiosk shell, the
// machine's largest attack surface, out of direct Wi-Fi control), and this
// page does not weaken it.
//
// The nine classified failure codes come back on the wire and are rendered
// as the gateway's own zh-TW copy (see `client::SettingsRpcError::
// user_message`), so a wrong password says "密碼不正確" and a missing
// adapter says "請改用網路線" — the operator's next action differs, which
// is the whole reason that taxonomy exists.
//
// ── Wired (new this round) ─────────────────────────────────────────────
// `network.wired_status` / `network.wired_config`, which land with this work
// package. The wired half is what an actual duty box is usually plugged
// into, and until now there was no UI for it at all.
//
// Two facts an operator has to be able to tell apart, and which this card
// keeps separate on screen:
//   * LIVE state — what address the interface currently holds, and where it
//     came from (`source`: dhcp / static / unknown).
//   * CONFIGURED intent — what THIS box was told to do (`configured`), which
//     is `null` when nothing was ever set and the shipped DHCP default is in
//     force.
// They diverge exactly when a static config was applied and did not take,
// and collapsing them into one row would hide that.
//
// ── Both links coexist; wired wins ─────────────────────────────────────
// Not an either/or switch. Both interfaces stay up and the routing metric
// decides (the wired drop-in this page writes carries a lower metric than
// the wireless default), so unplugging the cable falls back to Wi-Fi on its
// own — the macOS/Windows convention.

use std::net::{IpAddr, Ipv4Addr};

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;
use serde_json::Value;

use super::widgets::{self, Tone};
use super::{client, spawn_rpc, Load};
use crate::icons;
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) const WIRED_STATUS_METHOD: &str = "network.wired_status";
pub(crate) const WIRED_CONFIG_METHOD: &str = "network.wired_config";
pub(crate) const WIFI_STATUS_METHOD: &str = "network.status";
pub(crate) const WIFI_SCAN_METHOD: &str = "network.wifi_scan";
pub(crate) const WIFI_CONNECT_METHOD: &str = "network.wifi_connect";
pub(crate) const WIFI_FORGET_METHOD: &str = "network.wifi_forget";

/// How many networks the list shows. Nothing in this crate scrolls (see
/// `settings/mod.rs`), and a scan in an office can return 30+. The tail is
/// stated, never silently dropped.
const MAX_NETWORKS_SHOWN: usize = 5;
/// WPA-PSK passphrase bounds, the real 802.11 rule. Checked here so an
/// obviously-wrong length never costs a round trip; the gateway checks again.
const PSK_MIN_CHARS: usize = 8;
const PSK_MAX_CHARS: usize = 63;
/// systemd-resolved accepts more, but three is what a `.network` drop-in
/// realistically needs and what sysd's own validator caps at.
const MAX_DNS_ENTRIES: usize = 3;

// ── Wired types ──────────────────────────────────────────────────────────

/// How the wired link is meant to be configured. A closed set — the wire
/// carries exactly these two strings and anything else is a bug, not a third
/// mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WiredMode {
    Dhcp,
    Static,
}

impl WiredMode {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            WiredMode::Dhcp => "dhcp",
            WiredMode::Static => "static",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "dhcp" => Some(WiredMode::Dhcp),
            "static" => Some(WiredMode::Static),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            WiredMode::Dhcp => "自動取得（DHCP）",
            WiredMode::Static => "固定 IP",
        }
    }
}

/// What this box was TOLD to do, as opposed to what it is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WiredConfigured {
    pub(crate) mode: Option<WiredMode>,
    pub(crate) address: Option<String>,
    pub(crate) gateway: Option<String>,
    pub(crate) dns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WiredStatus {
    pub(crate) interface: Option<String>,
    pub(crate) link_up: bool,
    pub(crate) addresses: Vec<String>,
    pub(crate) gateway: Option<String>,
    pub(crate) dns: Vec<String>,
    /// `dhcp` / `static` / `unknown` — where the LIVE address came from.
    pub(crate) source: String,
    /// `None` ⇒ nothing was ever configured on this box, i.e. the image's
    /// shipped DHCP default is in force. NOT the same as `Some(Dhcp)`, which
    /// means someone explicitly chose it.
    pub(crate) configured: Option<WiredConfigured>,
}

impl WiredStatus {
    /// Which mode the segmented control shows as selected. Configured intent
    /// wins over observed source: if a static config was applied and the
    /// interface has not picked it up yet, the operator's own choice is what
    /// should still be highlighted (the divergence is reported separately).
    pub(crate) fn effective_mode(&self) -> WiredMode {
        match self.configured.as_ref().and_then(|c| c.mode) {
            Some(mode) => mode,
            None => WiredMode::parse(&self.source).unwrap_or(WiredMode::Dhcp),
        }
    }

    /// True when a static config was asked for but the live address does not
    /// come from it. The one condition this card exists to surface.
    pub(crate) fn config_not_in_effect(&self) -> bool {
        self.configured.as_ref().and_then(|c| c.mode) == Some(WiredMode::Static) && self.source != "static"
    }
}

// ── Wi-Fi types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WifiNetwork {
    pub(crate) ssid: String,
    pub(crate) signal_bars: u8,
    /// `open` / `wep` / `psk` / `8021x` / `unknown`, straight from iwd.
    pub(crate) security: String,
    pub(crate) connected: bool,
    pub(crate) known: bool,
}

impl WifiNetwork {
    /// Whether this shell can join it at all. WEP is refused by iwd itself,
    /// and 802.1X needs an identity/certificate flow this kiosk has no UI
    /// for — both are shown, greyed, with the reason, rather than hidden
    /// (a network the operator can SEE on their phone but not in this list
    /// reads as a broken scanner).
    pub(crate) fn joinable(&self) -> bool {
        matches!(self.security.as_str(), "open" | "psk" | "unknown")
    }

    pub(crate) fn needs_passphrase(&self) -> bool {
        self.security != "open" && !self.known
    }

    pub(crate) fn unsupported_reason(&self) -> Option<&'static str> {
        match self.security.as_str() {
            "wep" => Some("使用過舊的加密方式（WEP），系統不支援"),
            "8021x" => Some("需要企業帳號憑證，請改用網路線或聯絡支援"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WifiSnapshot {
    /// `connected` / `connecting` / `disconnected` / `unavailable`.
    pub(crate) link_state: String,
    pub(crate) link_ssid: Option<String>,
    /// `online` / `portal` / `offline` / `unknown`.
    pub(crate) internet: String,
    pub(crate) networks: Vec<WifiNetwork>,
}

// ── Page state ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NetworkPageState {
    pub(crate) wired: Load<WiredStatus>,
    /// The mode the operator has SELECTED but not yet applied. `None` means
    /// "showing whatever the box reports" — a separate field so switching to
    /// 固定 IP reveals the form without pretending the change already
    /// happened.
    pub(crate) wired_draft: Option<WiredMode>,
    wired_in_flight: bool,
    wired_result: Option<(String, bool)>,
    wired_typed_error: Option<&'static str>,

    pub(crate) wifi: Load<WifiSnapshot>,
    wifi_in_flight: bool,
    /// The SSID whose passphrase prompt is open, with whether it needs one.
    pub(crate) wifi_selected: Option<String>,
    wifi_result: Option<(String, bool)>,
    wifi_typed_error: Option<&'static str>,
}

impl NetworkPageState {
    fn begin_wired(&mut self) -> bool {
        if self.wired_in_flight {
            return false;
        }
        self.wired_in_flight = true;
        self.wired_typed_error = None;
        true
    }

    fn begin_wifi(&mut self) -> bool {
        if self.wifi_in_flight {
            return false;
        }
        self.wifi_in_flight = true;
        self.wifi_typed_error = None;
        true
    }

    pub(crate) fn wired_busy(&self) -> bool {
        self.wired_in_flight
    }

    pub(crate) fn wifi_busy(&self) -> bool {
        self.wifi_in_flight
    }

    /// Which wired mode the form should be built around: the draft if the
    /// operator picked one, else whatever the box reports.
    pub(crate) fn wired_form_mode(&self) -> WiredMode {
        self.wired_draft.unwrap_or_else(|| self.wired.value().map(WiredStatus::effective_mode).unwrap_or(WiredMode::Dhcp))
    }
}

// ── Parsing (pure) ───────────────────────────────────────────────────────

fn opt_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty())
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

pub(crate) fn parse_wired(payload: &Value) -> WiredStatus {
    WiredStatus {
        interface: opt_string(payload.get("interface")),
        link_up: payload.get("link_up").and_then(Value::as_bool).unwrap_or(false),
        addresses: string_list(payload.get("addresses")),
        gateway: opt_string(payload.get("gateway")),
        dns: string_list(payload.get("dns")),
        source: opt_string(payload.get("source")).unwrap_or_else(|| "unknown".to_string()),
        configured: payload.get("configured").filter(|v| !v.is_null()).map(|c| WiredConfigured {
            mode: c.get("mode").and_then(Value::as_str).and_then(WiredMode::parse),
            address: opt_string(c.get("address")),
            gateway: opt_string(c.get("gateway")),
            dns: string_list(c.get("dns")),
        }),
    }
}

pub(crate) fn parse_wifi(status: &Value, scan: &Value) -> WifiSnapshot {
    let wifi = status.get("wifi");
    WifiSnapshot {
        link_state: wifi.and_then(|w| w.get("state")).and_then(Value::as_str).unwrap_or("disconnected").to_string(),
        link_ssid: opt_string(wifi.and_then(|w| w.get("ssid"))),
        internet: opt_string(status.get("internet")).unwrap_or_else(|| "unknown".to_string()),
        networks: parse_networks(scan),
    }
}

/// Skips (never fabricates) any entry with no usable SSID, and hidden
/// networks — no name to show is no row to render, the same call
/// `oobe::network::gateway::parse_networks` already makes.
pub(crate) fn parse_networks(scan: &Value) -> Vec<WifiNetwork> {
    let Some(list) = scan.get("networks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in list {
        let Some(ssid) = entry.get("ssid").and_then(Value::as_str) else {
            continue;
        };
        if ssid.trim().is_empty() || entry.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        out.push(WifiNetwork {
            ssid: ssid.to_string(),
            signal_bars: entry.get("signal_bars").and_then(Value::as_u64).map(|n| n.clamp(1, 4) as u8).unwrap_or(1),
            security: entry.get("security").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            connected: entry.get("connected").and_then(Value::as_bool).unwrap_or(false),
            known: entry.get("known").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    out
}

// ── Validation (pure) ────────────────────────────────────────────────────

/// A validated static-IP form, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaticPlan {
    pub(crate) address: String,
    pub(crate) gateway: Option<String>,
    pub(crate) dns: Vec<String>,
}

/// Pure: the three typed values -> a plan or one specific complaint.
///
/// IPv4 only, and it says so rather than silently ignoring an IPv6 value —
/// the `.network` drop-in sysd writes is IPv4-only this round, so accepting
/// an IPv6 address here would produce a config that quietly does nothing.
pub(crate) fn validate_static(address: &str, gateway: &str, dns: &str) -> Result<StaticPlan, &'static str> {
    let address = address.trim();
    if address.is_empty() {
        return Err("請輸入 IP 位址，例如 192.168.1.50/24。");
    }
    let Some((host, prefix)) = address.split_once('/') else {
        return Err("IP 位址要包含子網路長度，例如 192.168.1.50/24。");
    };
    if host.parse::<Ipv4Addr>().is_err() {
        return Err(if host.parse::<IpAddr>().is_ok() {
            "本版只支援 IPv4 位址。"
        } else {
            "IP 位址格式不正確，例如 192.168.1.50/24。"
        });
    }
    match prefix.parse::<u8>() {
        Ok(n) if (1..=32).contains(&n) => {}
        _ => return Err("子網路長度要介於 1 到 32 之間，例如 /24。"),
    }

    let gateway = gateway.trim();
    let gateway = if gateway.is_empty() {
        None
    } else if gateway.parse::<Ipv4Addr>().is_ok() {
        Some(gateway.to_string())
    } else {
        return Err(if gateway.parse::<IpAddr>().is_ok() { "本版只支援 IPv4 閘道位址。" } else { "閘道位址格式不正確。" });
    };

    let mut servers = Vec::new();
    for token in dns.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token.parse::<IpAddr>().is_err() {
            return Err("DNS 伺服器位址格式不正確，多筆請用逗號分隔。");
        }
        servers.push(token.to_string());
    }
    if servers.len() > MAX_DNS_ENTRIES {
        return Err("最多只能設定 3 個 DNS 伺服器。");
    }

    Ok(StaticPlan { address: address.to_string(), gateway, dns: servers })
}

/// Pure: is this passphrase a plausible WPA-PSK one? Counting CHARACTERS,
/// which is what 802.11's 8..=63 rule is stated in.
pub(crate) fn validate_psk(psk: &str) -> Result<&str, &'static str> {
    let len = psk.chars().count();
    if len < PSK_MIN_CHARS {
        return Err("Wi-Fi 密碼至少要 8 個字元。");
    }
    if len > PSK_MAX_CHARS {
        return Err("Wi-Fi 密碼最多 63 個字元。");
    }
    Ok(psk)
}

// ── Kick-offs ────────────────────────────────────────────────────────────

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    ensure_wired_loaded(view, cx);
    ensure_wifi_loaded(view, cx);
}

fn ensure_wired_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.network.wired.needs_load() {
        return;
    }
    view.settings_ui.network.wired = Load::Loading;
    spawn_rpc(
        cx,
        || client::call(WIRED_STATUS_METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.network.wired = match result {
                Ok(payload) => Load::Loaded(parse_wired(&payload)),
                Err(e) => {
                    eprintln!("[settings/network] {WIRED_STATUS_METHOD} failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

fn ensure_wifi_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.network.wifi.needs_load() {
        return;
    }
    view.settings_ui.network.wifi = Load::Loading;
    // Two round trips in ONE background closure: the link state and the scan
    // are separate RPCs, and firing them as two independent spawns would let
    // a page render a scan list next to a stale link state.
    spawn_rpc(
        cx,
        || {
            let status = client::call(WIFI_STATUS_METHOD, serde_json::json!({}))?;
            let scan = client::call(WIFI_SCAN_METHOD, serde_json::json!({ "rescan": true }))?;
            Ok::<_, client::SettingsRpcError>(parse_wifi(&status, &scan))
        },
        |view, result, cx| {
            view.settings_ui.network.wifi = match result {
                Ok(snapshot) => Load::Loaded(snapshot),
                Err(e) => {
                    eprintln!("[settings/network] wifi read failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

fn apply_wired(view: &mut ShellView, params: Value, cx: &mut Context<ShellView>) {
    if !view.settings_ui.network.begin_wired() {
        return;
    }
    view.settings_ui.network.wired_result = None;
    cx.notify();
    spawn_rpc(
        cx,
        move || client::call(WIRED_CONFIG_METHOD, params),
        |view, result, cx| {
            view.settings_ui.network.wired_in_flight = false;
            view.settings_ui.network.wired_result = Some(match result {
                Ok(_) => {
                    // The draft has landed; stop overriding what the box says.
                    view.settings_ui.network.wired_draft = None;
                    ("有線網路設定已套用。".to_string(), true)
                }
                Err(e) => {
                    eprintln!("[settings/network] {WIRED_CONFIG_METHOD} failed: {e:?}");
                    (e.user_message(), false)
                }
            });
            view.settings_ui.network.wired = Load::NotLoaded;
            ensure_wired_loaded(view, cx);
            cx.notify();
        },
    );
}

fn submit_wired_static(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.settings_ui.network.wired_busy() {
        return;
    }
    let address = view.settings_fields.ip_address.read(cx).content(cx);
    let gateway = view.settings_fields.ip_gateway.read(cx).content(cx);
    let dns = view.settings_fields.ip_dns.read(cx).content(cx);
    match validate_static(&address, &gateway, &dns) {
        Ok(plan) => {
            let mut params = serde_json::Map::new();
            params.insert("mode".into(), Value::String(WiredMode::Static.wire().to_string()));
            params.insert("address".into(), Value::String(plan.address));
            if let Some(gw) = plan.gateway {
                params.insert("gateway".into(), Value::String(gw));
            }
            if !plan.dns.is_empty() {
                params.insert("dns".into(), Value::Array(plan.dns.into_iter().map(Value::String).collect()));
            }
            apply_wired(view, Value::Object(params), cx);
        }
        Err(complaint) => {
            view.settings_ui.network.wired_typed_error = Some(complaint);
            cx.notify();
        }
    }
}

fn connect_wifi(view: &mut ShellView, ssid: String, psk: Option<String>, cx: &mut Context<ShellView>) {
    if !view.settings_ui.network.begin_wifi() {
        return;
    }
    view.settings_ui.network.wifi_result = None;
    cx.notify();
    let mut params = serde_json::Map::new();
    params.insert("ssid".into(), Value::String(ssid));
    if let Some(psk) = psk {
        params.insert("psk".into(), Value::String(psk));
    }
    spawn_rpc(
        cx,
        move || client::call(WIFI_CONNECT_METHOD, Value::Object(params)),
        |view, result, cx| {
            view.settings_ui.network.wifi_in_flight = false;
            match result {
                Ok(_) => {
                    view.settings_ui.network.wifi_result = Some(("已連上。".to_string(), true));
                    view.settings_ui.network.wifi_selected = None;
                    // The passphrase must not keep sitting in the field.
                    view.settings_fields.clear_wifi_psk(cx);
                }
                Err(e) => {
                    // Never log this call's params — one of them is a
                    // passphrase. The classified kind is enough.
                    eprintln!("[settings/network] {WIFI_CONNECT_METHOD} failed: {}", e.code().unwrap_or("unclassified"));
                    view.settings_ui.network.wifi_result = Some((e.user_message(), false));
                }
            }
            view.settings_ui.network.wifi = Load::NotLoaded;
            ensure_wifi_loaded(view, cx);
            cx.notify();
        },
    );
}

fn forget_wifi(view: &mut ShellView, ssid: String, cx: &mut Context<ShellView>) {
    if !view.settings_ui.network.begin_wifi() {
        return;
    }
    view.settings_ui.network.wifi_result = None;
    cx.notify();
    spawn_rpc(
        cx,
        move || client::call(WIFI_FORGET_METHOD, serde_json::json!({ "ssid": ssid })),
        |view, result, cx| {
            view.settings_ui.network.wifi_in_flight = false;
            view.settings_ui.network.wifi_result = Some(match result {
                Ok(_) => ("已忘記這個網路。".to_string(), true),
                Err(e) => {
                    eprintln!("[settings/network] {WIFI_FORGET_METHOD} failed: {e:?}");
                    (e.user_message(), false)
                }
            });
            view.settings_ui.network.wifi = Load::NotLoaded;
            ensure_wifi_loaded(view, cx);
            cx.notify();
        },
    );
}

fn submit_selected_wifi(view: &mut ShellView, ssid: String, needs_psk: bool, cx: &mut Context<ShellView>) {
    if view.settings_ui.network.wifi_busy() {
        return;
    }
    if !needs_psk {
        connect_wifi(view, ssid, None, cx);
        return;
    }
    let psk = view.settings_fields.wifi_psk.read(cx).content(cx);
    match validate_psk(&psk) {
        Ok(_) => connect_wifi(view, ssid, Some(psk), cx),
        Err(complaint) => {
            view.settings_ui.network.wifi_typed_error = Some(complaint);
            cx.notify();
        }
    }
}

// ── Render ───────────────────────────────────────────────────────────────

pub(crate) fn render(
    body: Div,
    state: &NetworkPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    body.child(wired_card(state, fields, palette, cx)).child(wifi_card(state, fields, palette, cx))
}

fn wired_card(
    state: &NetworkPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let refresh = widgets::button(
        "settings-network-wired-refresh",
        "重新整理".to_string(),
        widgets::ButtonWeight::Secondary,
        !state.wired_busy() && !matches!(state.wired, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.network.wired = Load::NotLoaded;
            ensure_wired_loaded(view, cx);
            cx.notify();
        }),
    );
    let card = widgets::card(palette).child(widgets::card_header("有線網路", Some(refresh.into_any_element()), palette));

    let status = match &state.wired {
        Load::NotLoaded | Load::Loading => return card.child(widgets::notice_static("讀取中…", Tone::Muted, palette)),
        Load::Failed(e) if e.is_not_appliance() => {
            return card.child(widgets::notice_static("這台電腦不是 DuDuClaw 值班機，網路設定由作業系統自己管理。", Tone::Muted, palette))
        }
        Load::Failed(e) => return card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(status) => status,
    };

    let Some(interface) = status.interface.clone() else {
        return card.child(widgets::notice_static("這台機器上找不到有線網路介面。", Tone::Warning, palette));
    };

    let mut card = card
        .child(widgets::value_row("介面", interface, palette))
        .child(widgets::value_row(
            "狀態",
            if status.link_up { "已插線".to_string() } else { "未插線".to_string() },
            palette,
        ))
        .child(widgets::value_row("IP 位址", status.addresses.join("、"), palette))
        .child(widgets::value_row("閘道", status.gateway.clone().unwrap_or_default(), palette))
        .child(widgets::value_row("DNS", status.dns.join("、"), palette))
        .child(widgets::value_row("位址來源", source_label(&status.source), palette));

    if status.config_not_in_effect() {
        card = card.child(widgets::notice_static(
            "已設定固定 IP，但這張網卡目前不是用這組設定，請確認網路線是否插好或稍候再看。",
            Tone::Warning,
            palette,
        ));
    }

    let form_mode = state.wired_form_mode();
    let enabled = !state.wired_busy();
    let mut modes = div().flex().gap(px(8.));
    for (index, mode) in [WiredMode::Dhcp, WiredMode::Static].into_iter().enumerate() {
        modes = modes.child(widgets::segment(
            ("settings-network-wired-mode", index),
            mode.label().to_string(),
            mode == form_mode,
            enabled,
            palette,
            cx.listener(move |view, _ev, _window, cx| {
                // Picking DHCP applies immediately (there is nothing to fill
                // in); picking 固定 IP only reveals the form — the change is
                // not made until 套用, because a half-typed address would
                // take the machine off the network.
                match mode {
                    WiredMode::Dhcp => {
                        view.settings_ui.network.wired_draft = Some(WiredMode::Dhcp);
                        apply_wired(view, serde_json::json!({ "mode": "dhcp" }), cx);
                    }
                    WiredMode::Static => {
                        view.settings_ui.network.wired_draft = Some(WiredMode::Static);
                        cx.notify();
                    }
                }
            }),
        ));
    }
    card = card.child(
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pt(px(12.))
            .border_t_1()
            .border_color(palette.border())
            .child(widgets::field_label("位址設定方式", palette))
            .child(modes),
    );

    if form_mode == WiredMode::Static {
        card = card.child(static_form(state, fields, enabled, palette, cx));
    }
    if let Some(complaint) = state.wired_typed_error {
        card = card.child(widgets::notice_static(complaint, Tone::Danger, palette));
    }
    if let Some((message, ok)) = &state.wired_result {
        card = card.child(widgets::notice(message.clone(), if *ok { Tone::Success } else { Tone::Danger }, palette));
    }
    card
}

fn static_form(
    state: &NetworkPageState,
    fields: &crate::oobe::SettingsFields,
    enabled: bool,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(labeled_field("IP 位址 / 長度", fields.ip_address.clone(), palette))
                .child(labeled_field("閘道（可留空）", fields.ip_gateway.clone(), palette))
                .child(labeled_field("DNS（逗號分隔，最多 3 個）", fields.ip_dns.clone(), palette)),
        )
        .child(
            div().flex().items_center().gap(px(10.)).child(widgets::button(
                "settings-network-wired-apply",
                if state.wired_busy() { "套用中…".to_string() } else { "套用".to_string() },
                widgets::ButtonWeight::Primary,
                enabled,
                palette,
                cx.listener(|view, _ev, _window, cx| submit_wired_static(view, cx)),
            )),
        )
        .child(widgets::notice_static(
            "套用後網路會短暫中斷。若填錯導致連不上，可拔掉網路線改用 Wi-Fi，或請支援人員協助。",
            Tone::Muted,
            palette,
        ))
}

fn labeled_field(label: &'static str, field: gpui::Entity<crate::oobe::SettingsTextField>, palette: ShellPalette) -> Div {
    div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(4.)).child(widgets::field_label(label, palette)).child(field)
}

pub(crate) fn source_label(source: &str) -> String {
    match source {
        "dhcp" => "自動取得（DHCP）".to_string(),
        "static" => "固定 IP".to_string(),
        _ => "未知".to_string(),
    }
}

fn wifi_card(
    state: &NetworkPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let rescan = widgets::button(
        "settings-network-wifi-rescan",
        "重新掃描".to_string(),
        widgets::ButtonWeight::Secondary,
        !state.wifi_busy() && !matches!(state.wifi, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.network.wifi = Load::NotLoaded;
            ensure_wifi_loaded(view, cx);
            cx.notify();
        }),
    );
    let card = widgets::card(palette).child(widgets::card_header("Wi-Fi", Some(rescan.into_any_element()), palette));

    let snapshot = match &state.wifi {
        Load::NotLoaded | Load::Loading => return card.child(widgets::notice_static("掃描中…", Tone::Muted, palette)),
        Load::Failed(e) if e.is_not_appliance() => {
            return card.child(widgets::notice_static("這台電腦不是 DuDuClaw 值班機，Wi-Fi 設定由作業系統自己管理。", Tone::Muted, palette))
        }
        Load::Failed(e) => return card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(snapshot) => snapshot,
    };

    let mut card = card.child(widgets::value_row("目前連線", link_label(snapshot), palette));
    if snapshot.internet == "portal" {
        card = card.child(widgets::notice_static("這個網路需要先在瀏覽器登入才能上網。", Tone::Warning, palette));
    }
    if snapshot.link_state == "unavailable" {
        return card.child(widgets::notice_static("這台機器沒有可用的 Wi-Fi 硬體，請改用網路線。", Tone::Warning, palette));
    }

    if snapshot.networks.is_empty() {
        card = card.child(widgets::notice_static("掃描不到任何 Wi-Fi 網路。", Tone::Muted, palette));
    } else {
        let mut list = div().flex().flex_col().gap(px(6.));
        for (index, network) in snapshot.networks.iter().take(MAX_NETWORKS_SHOWN).enumerate() {
            list = list.child(network_row(index, network, state, palette, cx));
        }
        card = card.child(list);
        if snapshot.networks.len() > MAX_NETWORKS_SHOWN {
            card = card.child(widgets::notice(
                format!("另有 {} 個網路未顯示。", snapshot.networks.len() - MAX_NETWORKS_SHOWN),
                Tone::Muted,
                palette,
            ));
        }
    }

    if let Some(ssid) = &state.wifi_selected {
        if let Some(network) = snapshot.networks.iter().find(|n| &n.ssid == ssid) {
            card = card.child(passphrase_form(network, state, fields, palette, cx));
        }
    }
    if let Some(complaint) = state.wifi_typed_error {
        card = card.child(widgets::notice_static(complaint, Tone::Danger, palette));
    }
    if let Some((message, ok)) = &state.wifi_result {
        card = card.child(widgets::notice(message.clone(), if *ok { Tone::Success } else { Tone::Danger }, palette));
    }
    card
}

/// The one line that says whether Wi-Fi is actually carrying traffic —
/// "associated" and "online" are different facts (D4a §5.2 separates the
/// link layer from the IP layer precisely so a UI can say which one failed).
pub(crate) fn link_label(snapshot: &WifiSnapshot) -> String {
    let ssid = snapshot.link_ssid.clone().unwrap_or_default();
    match (snapshot.link_state.as_str(), snapshot.internet.as_str()) {
        ("connected", "online") => format!("{ssid}（可上網）"),
        ("connected", "portal") => format!("{ssid}（需要登入）"),
        ("connected", "offline") => format!("{ssid}（已連上，但沒有網路）"),
        ("connected", _) => ssid,
        ("connecting", _) => format!("連線中…（{ssid}）"),
        ("unavailable", _) => "沒有可用的 Wi-Fi 硬體".to_string(),
        _ => "未連線".to_string(),
    }
}

/// Returns `AnyElement`, not `Div`: a joinable row is wrapped in a
/// `Stateful<Div>` (it needs an id to take clicks) and an unjoinable one is
/// not, so the two branches have different concrete types.
fn network_row(
    index: usize,
    network: &WifiNetwork,
    state: &NetworkPageState,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> gpui::AnyElement {
    let selectable = network.joinable() && !network.connected && !state.wifi_busy();
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(10.))
        .py(px(8.))
        .rounded(px(9.))
        .bg(theme::alpha(if state.wifi_selected.as_deref() == Some(network.ssid.as_str()) { palette.surface_selected } else { palette.surface_raised }, 1.0))
        .border_1()
        .border_color(palette.border())
        .child(
            div()
                .w(px(18.))
                .h(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icons::icon_or_glyph(&icons::wifi_signal_layers(network.signal_bars, palette), 16., "·")),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.foreground, 1.0)).child(network.ssid.clone()))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::alpha(palette.text_faint, 1.0))
                        .child(network_subtitle(network)),
                ),
        );

    if network.connected {
        row = row.child(widgets::status_pill("已連線".to_string(), Tone::Success, palette));
    }
    if network.known {
        let ssid = network.ssid.clone();
        row = row.child(widgets::button(
            "settings-network-wifi-forget",
            "忘記".to_string(),
            widgets::ButtonWeight::Secondary,
            !state.wifi_busy(),
            palette,
            cx.listener(move |view, _ev, _window, cx| forget_wifi(view, ssid.clone(), cx)),
        ));
    }

    if selectable {
        let ssid = network.ssid.clone();
        let needs_psk = network.needs_passphrase();
        return div()
            .id(("settings-network-wifi", index))
            .cursor_pointer()
            .child(row)
            .on_click(cx.listener(move |view, _ev, _window, cx| {
                // A network whose passphrase we already hold (or that has
                // none) joins on the tap; anything else only OPENS the
                // prompt. Tapping must never silently attempt a join with an
                // empty password.
                if needs_psk {
                    view.settings_ui.network.wifi_selected = Some(ssid.clone());
                    view.settings_ui.network.wifi_typed_error = None;
                    cx.notify();
                } else {
                    submit_selected_wifi(view, ssid.clone(), false, cx);
                }
            }))
            .into_any_element();
    }
    if !network.joinable() {
        row = row.opacity(0.6);
    }
    row.into_any_element()
}

/// The row's second line: what an operator needs to decide whether to tap
/// it. Unsupported networks say WHY here rather than being hidden.
pub(crate) fn network_subtitle(network: &WifiNetwork) -> String {
    if let Some(reason) = network.unsupported_reason() {
        return reason.to_string();
    }
    match (network.security.as_str(), network.known) {
        ("open", _) => "開放網路，不需要密碼".to_string(),
        (_, true) => "已儲存密碼，點一下即可連線".to_string(),
        _ => "需要密碼".to_string(),
    }
}

fn passphrase_form(
    network: &WifiNetwork,
    state: &NetworkPageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let ssid_connect = network.ssid.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(12.))
        .border_t_1()
        .border_color(palette.border())
        .child(widgets::field_label("Wi-Fi 密碼", palette))
        .child(fields.wifi_psk.clone())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(widgets::button(
                    "settings-network-wifi-connect",
                    if state.wifi_busy() { "連線中…".to_string() } else { "連線".to_string() },
                    widgets::ButtonWeight::Primary,
                    !state.wifi_busy(),
                    palette,
                    cx.listener(move |view, _ev, _window, cx| submit_selected_wifi(view, ssid_connect.clone(), true, cx)),
                ))
                .child(widgets::button(
                    "settings-network-wifi-cancel",
                    "取消".to_string(),
                    widgets::ButtonWeight::Secondary,
                    !state.wifi_busy(),
                    palette,
                    cx.listener(|view, _ev, _window, cx| {
                        view.settings_ui.network.wifi_selected = None;
                        view.settings_ui.network.wifi_typed_error = None;
                        // Backing out must not leave a password from a
                        // previous attempt in the box.
                        view.settings_fields.clear_wifi_psk(cx);
                        cx.notify();
                    }),
                )),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── wired ────────────────────────────────────────────────────────────

    #[test]
    fn a_dhcp_box_that_was_never_configured_reports_no_intent() {
        let s = parse_wired(&json!({
            "interface": "enp1s0", "link_up": true, "addresses": ["192.168.1.23/24"],
            "gateway": "192.168.1.1", "dns": ["192.168.1.1"], "source": "dhcp", "configured": null
        }));
        assert_eq!(s.configured, None, "the shipped default is not an explicit choice");
        assert_eq!(s.effective_mode(), WiredMode::Dhcp);
        assert!(!s.config_not_in_effect());
    }

    /// The divergence this card exists to surface: intent says static, the
    /// live address says otherwise.
    #[test]
    fn a_static_config_that_did_not_take_is_reported_as_such() {
        let s = parse_wired(&json!({
            "interface": "enp1s0", "link_up": false, "addresses": [], "source": "unknown",
            "configured": { "mode": "static", "address": "192.168.1.50/24" }
        }));
        assert!(s.config_not_in_effect());
        assert_eq!(s.effective_mode(), WiredMode::Static, "the operator's own choice stays selected");
    }

    #[test]
    fn a_static_config_in_effect_is_not_flagged() {
        let s = parse_wired(&json!({
            "interface": "enp1s0", "link_up": true, "addresses": ["192.168.1.50/24"], "source": "static",
            "configured": { "mode": "static", "address": "192.168.1.50/24" }
        }));
        assert!(!s.config_not_in_effect());
    }

    #[test]
    fn a_machine_with_no_wired_interface_parses_without_panicking() {
        let s = parse_wired(&json!({ "interface": null, "link_up": false }));
        assert_eq!(s.interface, None);
        assert_eq!(s.source, "unknown");
        assert!(s.addresses.is_empty());
    }

    #[test]
    fn an_unknown_configured_mode_is_dropped_rather_than_guessed() {
        let s = parse_wired(&json!({ "source": "dhcp", "configured": { "mode": "pppoe" } }));
        assert_eq!(s.configured.and_then(|c| c.mode), None);
    }

    #[test]
    fn source_labels_never_leak_the_wire_token() {
        assert_eq!(source_label("dhcp"), "自動取得（DHCP）");
        assert_eq!(source_label("static"), "固定 IP");
        assert_eq!(source_label("nonsense"), "未知");
    }

    // ── static-IP validation ─────────────────────────────────────────────

    #[test]
    fn a_complete_static_form_validates() {
        let plan = validate_static("192.168.1.50/24", "192.168.1.1", "1.1.1.1, 8.8.8.8").unwrap();
        assert_eq!(plan.address, "192.168.1.50/24");
        assert_eq!(plan.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(plan.dns, vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn a_gateway_and_dns_are_both_optional() {
        let plan = validate_static("10.0.0.5/8", "  ", "   ").unwrap();
        assert_eq!(plan.gateway, None);
        assert!(plan.dns.is_empty());
    }

    /// IPv6 is refused with a message that says WHY, rather than being
    /// silently dropped into a drop-in that would do nothing.
    #[test]
    fn ipv6_is_refused_explicitly_not_silently_ignored() {
        let err = validate_static("2001:db8::1/64", "", "").unwrap_err();
        assert!(err.contains("IPv4"), "{err}");
        let err = validate_static("192.168.1.50/24", "2001:db8::1", "").unwrap_err();
        assert!(err.contains("IPv4"), "{err}");
    }

    #[test]
    fn each_malformed_field_gets_its_own_complaint() {
        let missing_prefix = validate_static("192.168.1.50", "", "").unwrap_err();
        let bad_prefix = validate_static("192.168.1.50/33", "", "").unwrap_err();
        let bad_host = validate_static("999.1.1.1/24", "", "").unwrap_err();
        let bad_dns = validate_static("192.168.1.50/24", "", "not-an-ip").unwrap_err();
        let mut all = vec![missing_prefix, bad_prefix, bad_host, bad_dns];
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 4, "four different problems collapsed onto fewer messages");
    }

    #[test]
    fn a_zero_prefix_and_an_empty_address_are_both_refused() {
        assert!(validate_static("192.168.1.50/0", "", "").is_err());
        assert!(validate_static("   ", "", "").is_err());
    }

    #[test]
    fn more_than_three_dns_servers_is_refused() {
        let err = validate_static("192.168.1.50/24", "", "1.1.1.1,8.8.8.8,9.9.9.9,4.4.4.4").unwrap_err();
        assert!(err.contains('3'), "{err}");
    }

    #[test]
    fn dns_entries_may_be_separated_by_commas_or_whitespace() {
        assert_eq!(validate_static("192.168.1.50/24", "", "1.1.1.1 8.8.8.8").unwrap().dns.len(), 2);
        assert_eq!(validate_static("192.168.1.50/24", "", "1.1.1.1,8.8.8.8").unwrap().dns.len(), 2);
    }

    // ── Wi-Fi ────────────────────────────────────────────────────────────

    #[test]
    fn hidden_and_nameless_networks_are_skipped_not_rendered_blank() {
        let networks = parse_networks(&json!({ "networks": [
            { "ssid": "DuDu-Office", "signal_bars": 4, "security": "psk", "known": true },
            { "ssid": "", "signal_bars": 1, "security": "open" },
            { "ssid": "secret", "signal_bars": 3, "security": "psk", "hidden": true },
            { "signal_bars": 2 }
        ]}));
        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].ssid, "DuDu-Office");
    }

    #[test]
    fn out_of_range_signal_bars_are_clamped_and_missing_security_defaults() {
        let networks = parse_networks(&json!({ "networks": [{ "ssid": "Weird", "signal_bars": 99 }] }));
        assert_eq!(networks[0].signal_bars, 4);
        assert_eq!(networks[0].security, "unknown");
    }

    /// WEP and 802.1X are visible-but-unjoinable, with the reason on the
    /// row — hiding them would read as a broken scanner.
    #[test]
    fn unsupported_security_types_are_shown_with_a_reason_rather_than_hidden() {
        let wep = WifiNetwork { ssid: "Old".into(), signal_bars: 3, security: "wep".into(), connected: false, known: false };
        assert!(!wep.joinable());
        assert!(wep.unsupported_reason().is_some());
        assert_eq!(network_subtitle(&wep), wep.unsupported_reason().unwrap());

        let ent = WifiNetwork { ssid: "Corp".into(), signal_bars: 3, security: "8021x".into(), connected: false, known: false };
        assert!(!ent.joinable());
        assert!(ent.unsupported_reason().is_some());
    }

    #[test]
    fn an_open_network_needs_no_passphrase_and_a_known_one_reuses_its_saved_credential() {
        let open = WifiNetwork { ssid: "Guest".into(), signal_bars: 2, security: "open".into(), connected: false, known: false };
        assert!(!open.needs_passphrase());
        let known = WifiNetwork { ssid: "Office".into(), signal_bars: 4, security: "psk".into(), connected: false, known: true };
        assert!(!known.needs_passphrase());
        let fresh = WifiNetwork { ssid: "Office".into(), signal_bars: 4, security: "psk".into(), connected: false, known: false };
        assert!(fresh.needs_passphrase());
    }

    /// The link layer and the IP layer are separate facts; "associated but
    /// no internet" must be sayable.
    #[test]
    fn the_link_line_distinguishes_associated_from_online() {
        let mut s = WifiSnapshot {
            link_state: "connected".into(),
            link_ssid: Some("DuDu-Office".into()),
            internet: "online".into(),
            networks: vec![],
        };
        assert!(link_label(&s).contains("可上網"));
        s.internet = "offline".into();
        assert!(link_label(&s).contains("沒有網路"));
        s.internet = "portal".into();
        assert!(link_label(&s).contains("需要登入"));
        s.link_state = "disconnected".into();
        assert_eq!(link_label(&s), "未連線");
        s.link_state = "unavailable".into();
        assert!(link_label(&s).contains("沒有可用"));
    }

    #[test]
    fn psk_length_is_checked_in_characters_against_the_real_802_11_rule() {
        assert!(validate_psk("1234567").is_err());
        assert!(validate_psk("12345678").is_ok());
        assert!(validate_psk(&"a".repeat(63)).is_ok());
        assert!(validate_psk(&"a".repeat(64)).is_err());
        // 8 CJK characters is 24 bytes but 8 characters — the rule is stated
        // in characters and must be enforced in them.
        assert!(validate_psk("密碼一二三四五六").is_ok());
    }

    // ── state machine ────────────────────────────────────────────────────

    #[test]
    fn the_two_halves_of_the_page_have_independent_in_flight_guards() {
        let mut state = NetworkPageState::default();
        assert!(state.begin_wired());
        assert!(!state.begin_wired());
        assert!(state.begin_wifi(), "a wired write must not block a Wi-Fi scan");
        assert!(!state.begin_wifi());
    }

    #[test]
    fn a_fresh_page_has_asked_nothing_and_drafts_nothing() {
        let state = NetworkPageState::default();
        assert!(state.wired.needs_load());
        assert!(state.wifi.needs_load());
        assert_eq!(state.wired_draft, None);
        assert_eq!(state.wired_form_mode(), WiredMode::Dhcp, "the safe default before anything is known");
    }

    /// Choosing 固定 IP reveals the form without claiming the change
    /// happened — the box is still on whatever it was on.
    #[test]
    fn drafting_static_changes_the_form_but_not_the_reported_state() {
        let mut state = NetworkPageState::default();
        state.wired = Load::Loaded(parse_wired(&json!({ "interface": "enp1s0", "source": "dhcp" })));
        assert_eq!(state.wired_form_mode(), WiredMode::Dhcp);
        state.wired_draft = Some(WiredMode::Static);
        assert_eq!(state.wired_form_mode(), WiredMode::Static);
        assert_eq!(state.wired.value().unwrap().source, "dhcp", "the reported source must not move on a draft");
    }

    #[test]
    fn wired_mode_tokens_round_trip_and_reject_anything_else() {
        for mode in [WiredMode::Dhcp, WiredMode::Static] {
            assert_eq!(WiredMode::parse(mode.wire()), Some(mode));
        }
        assert_eq!(WiredMode::parse("STATIC"), None, "the wire token is exact, never case-folded");
        assert_eq!(WiredMode::parse(""), None);
    }
}
