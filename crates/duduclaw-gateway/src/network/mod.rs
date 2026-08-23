//! Network settings (Wi-Fi) — D4a. See
//! `commercial/docs/DESIGN-network-settings-2026-08.md` for the full
//! selection rationale (iwd + systemd-networkd), permission topology
//! (gateway joins the `netdev` group and speaks iwd D-Bus directly — no
//! sysd verb, see design §3), and the API contract (§5) this module
//! implements.
//!
//! ## Module shape
//!
//! - This file: shared types, the closed error taxonomy (§5.3), and every
//!   pure function (signal→bars, iwd error classification, PSK length
//!   validation, network dedup/sort) — unit-tested here, on every host this
//!   crate compiles for.
//! - [`iwd`] (Linux-only): the zbus D-Bus client against `net.connman.iwd`.
//!   Kept as thin as the zvariant coupling allows — any logic that can be
//!   expressed as `&str -> X` without touching a zbus/zvariant type lives
//!   HERE instead, specifically so it is exercised by `cargo test` on this
//!   project's macOS dev machines too, not only on a Linux CI runner. `zbus`
//!   itself is a `target_os = "linux"`-gated dependency in this crate's
//!   `Cargo.toml`, so `iwd.rs` literally cannot compile off-Linux — the
//!   facade functions below have a separate non-Linux branch for that reason,
//!   not merely a `#[cfg]` decoration.
//! - [`sysfs`]: adapter-presence probing (`/sys/class/ieee80211`,
//!   `/sys/bus/pci`) — cross-platform-compiled (degrades to empty results
//!   off-Linux), so its parsing is also unit-tested everywhere.
//! - [`ipinfo`]: IP-layer status (`/proc/net/route`, resolv.conf, DHCP lease
//!   presence) — same split.
//! - [`portal`]: captive-portal detection (M1 scope: detect + report only,
//!   see design §6).
//!
//! ## PSK masking discipline (hard requirement)
//!
//! No `tracing::*` call, no `Debug` impl, and no audit payload anywhere in
//! this module (or its submodules) may render a Wi-Fi passphrase.
//! [`WifiError::detail`] is assembled only from D-Bus error text and static
//! strings — never from a caller-supplied `psk`. [`WifiConnectRequest`]'s
//! `Debug` impl is hand-written for exactly this reason (see its doc).

pub mod ipinfo;
pub mod portal;
pub mod sysfs;
// ── System-settings app: wired network (`network.wired_status` /
// `network.wired_config`) — storage, boot re-apply, and pure validation. ──
pub mod wired;

#[cfg(target_os = "linux")]
mod iwd;

use serde::Serialize;

// ── Types (design §5.2) ─────────────────────────────────────────────────

/// One scan-result row. `security` and `WifiLink::state`/[`NetworkStatus::
/// internet`]/[`IpInfo::source`] each use ONE closed vocabulary shared by
/// every producer — see each field's own doc for its exact value set, so a
/// dashboard/shell client never has to reconcile two spellings of the same
/// concept (design §5.2 calls this out explicitly: no second `"WPA2-
/// Personal"`-style vocabulary alongside `"psk"`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 1..=4, never 0 — see [`dbm_centi_to_bars`].
    pub signal_bars: u8,
    /// `"open"` | `"wep"` | `"psk"` | `"8021x"` | `"unknown"`.
    pub security: String,
    pub connected: bool,
    /// Has a stored credential (an iwd `KnownNetwork`).
    pub known: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanResult {
    pub networks: Vec<WifiNetwork>,
    pub scanning: bool,
}

/// Link-layer-only view (no IP information — see [`IpInfo`] and this
/// module's own status-assembly doc for why the two are deliberately
/// separate).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WifiLink {
    /// `"connected"` | `"connecting"` | `"disconnected"` | `"unavailable"`.
    pub state: String,
    pub ssid: Option<String>,
    pub signal_bars: Option<u8>,
    /// `"open"` | `"wep"` | `"psk"` | `"8021x"` | `"unknown"`, same
    /// vocabulary as [`WifiNetwork::security`].
    pub security: Option<String>,
    pub frequency: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IpInfo {
    pub interface: Option<String>,
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    /// `"dhcp"` | `"static"` | `"unknown"` — see [`ipinfo::collect`]'s doc
    /// for why an unreadable lease directory maps to `"unknown"`, never a
    /// guessed `"static"`.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NetworkStatus {
    pub wifi: WifiLink,
    pub ip: IpInfo,
    /// `"online"` | `"portal"` | `"offline"` | `"unknown"`.
    pub internet: String,
    pub portal_url: Option<String>,
    pub interfaces: Vec<crate::device::NetworkInterface>,
}

/// One `network.wifi_connect` / `POST /api/first-run/network/connect`
/// request body — the single shape both entry points parse into, so the
/// psk-redaction `Debug` impl (and its test) exists in exactly one place
/// instead of being duplicated per call site. Deliberately does NOT derive
/// `Serialize`: re-serializing this struct anywhere would put the real
/// passphrase back into a `serde_json::Value`, defeating the whole point of
/// the hand-written `Debug` below — callers that need the raw psk read
/// `.psk` directly, they never need this type serialized.
#[derive(Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WifiConnectRequest {
    pub ssid: String,
    /// `None` = open network, or use the already-stored credential.
    #[serde(default)]
    pub psk: Option<String>,
}

impl std::fmt::Debug for WifiConnectRequest {
    /// Hand-written (never derived) so any future `tracing::debug!(?req)`
    /// anywhere in the gateway renders the passphrase as `<redacted>`,
    /// never the real value — see this module's PSK masking discipline.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WifiConnectRequest")
            .field("ssid", &self.ssid)
            .field("psk", &self.psk.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

// ── Errors (design §5.3 — nine-way closed taxonomy) ─────────────────────

/// Closed error taxonomy. Every variant maps to exactly one `code` (wire
/// value) and one zh-TW `message` (design §5.3's table, copied verbatim) —
/// deliberately NOT open-ended, so a shell/dashboard client can render a
/// fixed switch instead of falling back to raw D-Bus text for an unknown
/// case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiErrorCode {
    WrongPassword,
    NotFound,
    OutOfRange,
    NoAdapter,
    DriverMissing,
    NoIp,
    Portal,
    BackendUnavailable,
    UnsupportedSecurity,
}

impl WifiErrorCode {
    /// Stable, snake_case wire value — never renamed once shipped (a client
    /// switches on this string).
    pub fn code(&self) -> &'static str {
        match self {
            Self::WrongPassword => "wrong_password",
            Self::NotFound => "not_found",
            Self::OutOfRange => "out_of_range",
            Self::NoAdapter => "no_adapter",
            Self::DriverMissing => "driver_missing",
            Self::NoIp => "no_ip",
            Self::Portal => "portal",
            Self::BackendUnavailable => "backend_unavailable",
            Self::UnsupportedSecurity => "unsupported_security",
        }
    }

    /// zh-TW copy the end user sees, design §5.3's table verbatim for the
    /// six codes whose message never mentions an SSID.
    pub fn message(&self) -> &'static str {
        match self {
            Self::WrongPassword => "密碼不正確，請重新輸入",
            Self::NotFound => "找不到這個網路，可能已離開範圍",
            Self::OutOfRange => "訊號太弱連不上，請靠近路由器再試",
            Self::NoAdapter => "這台機器沒有偵測到 Wi-Fi 硬體，請改用網路線",
            Self::DriverMissing => "Wi-Fi 硬體無法啟動（缺少驅動韌體），請改用網路線並回報型號",
            Self::NoIp => "已連上，但沒有取得網路位址（可能是路由器 DHCP 問題）",
            Self::Portal => "已連上，需要在瀏覽器完成登入",
            Self::BackendUnavailable => "網路服務未啟動，請重新開機或聯絡支援",
            Self::UnsupportedSecurity => "這個網路使用過舊的加密方式（WEP），系統不支援",
        }
    }

    /// `no_ip` / `portal` carry the SSID in design §5.3's copy (`已連上
    /// %s，...`); every other code's message doesn't mention a network at
    /// all, so this falls back to [`Self::message`] unchanged for them.
    pub fn message_with_ssid(&self, ssid: &str) -> String {
        match self {
            Self::NoIp => format!("已連上 {ssid}，但沒有取得網路位址（可能是路由器 DHCP 問題）"),
            Self::Portal => format!("已連上 {ssid}，需要在瀏覽器完成登入"),
            other => other.message().to_string(),
        }
    }
}

/// `detail` is a technical string for `tracing` ONLY — it is assembled from
/// D-Bus error text / static strings, deliberately never from a `psk`
/// argument, and it must never reach a client-facing JSON payload (see
/// [`error_to_json`], which reads `code`/`message` and drops `detail`).
#[derive(Debug, Clone)]
pub struct WifiError {
    pub code: WifiErrorCode,
    pub detail: String,
}

// ── Pure functions ───────────────────────────────────────────────────────

/// iwd's `Station.GetOrderedNetworks()` reports signal as `i16` centi-dBm
/// (e.g. `-6000` = -60 dBm). Maps to a 1..=4 bar count, never 0 — a network
/// that shows up in a scan at all is shown as "at least one bar", matching
/// how every consumer OS renders Wi-Fi signal.
pub fn dbm_centi_to_bars(signal: i16) -> u8 {
    if signal >= -5000 {
        4
    } else if signal >= -6000 {
        3
    } else if signal >= -7000 {
        2
    } else {
        1
    }
}

/// iwd's `Network.Type` property, passed through for the four values it
/// actually emits; anything else (a future iwd type we haven't seen, or a
/// malformed read) becomes `"unknown"` rather than an error — this feeds a
/// display field, not a security decision.
pub fn iwd_network_type_to_security(t: &str) -> &'static str {
    match t {
        "open" => "open",
        "wep" => "wep",
        "psk" => "psk",
        "8021x" => "8021x",
        _ => "unknown",
    }
}

/// Classify an iwd D-Bus method-error name into our closed taxonomy.
///
/// **Honesty note**: this mapping was derived from iwd's upstream D-Bus
/// documentation and source comments, NOT from spot-checking it against a
/// running `iwd` — the design brief this module implements says so
/// explicitly and asks that this doc comment say so too. Only the suffix
/// after the LAST `.` is compared (bus error names look like
/// `net.connman.iwd.Failed`), case-sensitively, against the known set below;
/// anything unrecognized — including a suffix from a future iwd version we
/// haven't seen — is the fail-safe [`WifiErrorCode::BackendUnavailable`]
/// rather than a guess.
pub fn classify_iwd_error(dbus_error_name: &str, psk_supplied: bool) -> WifiErrorCode {
    let suffix = dbus_error_name
        .rsplit('.')
        .next()
        .unwrap_or(dbus_error_name);
    match suffix {
        // iwd reports both "wrong password" and "AP not reachable" as the
        // same generic `Failed` — the only signal we have to tell them apart
        // is whether THIS attempt even supplied a passphrase to be wrong.
        "Failed" => {
            if psk_supplied {
                WifiErrorCode::WrongPassword
            } else {
                WifiErrorCode::OutOfRange
            }
        }
        "Timeout" | "Aborted" => WifiErrorCode::OutOfRange,
        "NotFound" | "NoNetwork" => WifiErrorCode::NotFound,
        "NotSupported" => WifiErrorCode::UnsupportedSecurity,
        "NoAgent" | "ServiceUnknown" | "NameHasNoOwner" => WifiErrorCode::BackendUnavailable,
        "Busy" | "InProgress" => WifiErrorCode::BackendUnavailable,
        // iwd rejects an out-of-range passphrase (not 8..=63 chars) this way.
        "InvalidArguments" | "InvalidFormat" => WifiErrorCode::WrongPassword,
        _ => WifiErrorCode::BackendUnavailable,
    }
}

/// Decide [`WifiErrorCode::NoAdapter`] vs [`WifiErrorCode::DriverMissing`]
/// when no `net.connman.iwd.Station` object exists at all. Only meaningful
/// when there is in fact no phy — callers that already know `phy_count > 0`
/// have no business asking this question, hence the `debug_assert`.
pub fn classify_adapter_absence(phy_count: usize, wireless_pci_present: bool) -> WifiErrorCode {
    debug_assert_eq!(
        phy_count, 0,
        "classify_adapter_absence should only be called when no wifi phy is present"
    );
    if wireless_pci_present {
        WifiErrorCode::DriverMissing
    } else {
        WifiErrorCode::NoAdapter
    }
}

/// WPA-PSK passphrase length is 8..=63 **characters** (`chars().count()`,
/// not bytes) — a CJK SSID/passphrase is a real thing this platform's
/// userbase actually uses, and byte length would reject a valid 20-character
/// Chinese passphrase for being "too long". An empty/absent psk is NOT this
/// function's concern (open network or reuse of a stored credential) —
/// callers only invoke this when a psk was actually supplied.
pub fn validate_psk(psk: &str) -> Result<(), WifiErrorCode> {
    let len = psk.chars().count();
    if (8..=63).contains(&len) {
        Ok(())
    } else {
        Err(WifiErrorCode::WrongPassword)
    }
}

/// Same-SSID dedup (keep the strongest signal) + sort (signal_bars desc,
/// then ssid asc). Sorting FIRST and then removing later duplicates by
/// first-occurrence is what makes this correct in one pass: after a desc-by-
/// signal sort, the first entry seen for any given SSID is necessarily its
/// strongest, and `Vec::retain` preserves the relative order of survivors —
/// so the final vector is simultaneously deduped AND still correctly sorted,
/// with no second sort pass needed.
pub fn sort_and_dedup_networks(mut v: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    v.sort_by(|a, b| {
        b.signal_bars
            .cmp(&a.signal_bars)
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    let mut seen = std::collections::HashSet::new();
    v.retain(|n| seen.insert(n.ssid.clone()));
    v
}

/// Map iwd's `Station.State` values onto the three-state-plus-unavailable
/// vocabulary [`WifiLink::state`] uses. `roaming` counts as `connected`
/// (link layer is up, iwd is just moving between APs on the same ESS) and
/// `disconnecting` counts as `disconnected` (the UI has no "tearing down"
/// state to render). An unrecognized value — a future iwd state this module
/// hasn't seen — degrades to `disconnected` rather than inventing a new UI
/// state or panicking. Pure `&str -> &str` on purpose (see module doc: kept
/// here, not in `iwd.rs`, specifically so it's exercised by `cargo test` on
/// every host, not only Linux).
pub fn normalize_station_state(raw: &str) -> &'static str {
    match raw {
        "connected" | "roaming" => "connected",
        "connecting" => "connecting",
        _ => "disconnected",
    }
}

/// `{"networks": [...], "scanning": bool}` — the exact `network.wifi_scan`
/// result shape (design §5.2).
pub fn scan_result_to_json(result: &ScanResult) -> serde_json::Value {
    serde_json::json!({
        "networks": result.networks,
        "scanning": result.scanning,
    })
}

/// `{"code": ..., "message": ...}` — `detail` deliberately never reaches
/// this (or any other client-facing) JSON. Use [`error_to_json_with_ssid`]
/// at a call site that knows which SSID the failing operation was about.
pub fn error_to_json(err: &WifiError) -> serde_json::Value {
    serde_json::json!({
        "code": err.code.code(),
        "message": err.code.message(),
    })
}

/// Same shape as [`error_to_json`], but rendering the message through
/// [`WifiErrorCode::message_with_ssid`] — used by `network.wifi_connect` and
/// its OOBE pre-auth twin, which both know the SSID the caller attempted.
pub fn error_to_json_with_ssid(err: &WifiError, ssid: &str) -> serde_json::Value {
    serde_json::json!({
        "code": err.code.code(),
        "message": err.code.message_with_ssid(ssid),
    })
}

// ── Facade (async) ────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub async fn wifi_scan(rescan: bool) -> Result<ScanResult, WifiError> {
    iwd::scan(rescan).await
}

#[cfg(not(target_os = "linux"))]
pub async fn wifi_scan(_rescan: bool) -> Result<ScanResult, WifiError> {
    Err(non_linux_error())
}

#[cfg(target_os = "linux")]
pub async fn wifi_connect(ssid: &str, psk: Option<&str>) -> Result<(), WifiError> {
    iwd::connect(ssid, psk).await
}

#[cfg(not(target_os = "linux"))]
pub async fn wifi_connect(_ssid: &str, _psk: Option<&str>) -> Result<(), WifiError> {
    Err(non_linux_error())
}

#[cfg(target_os = "linux")]
pub async fn wifi_forget(ssid: &str) -> Result<(), WifiError> {
    iwd::forget(ssid).await
}

#[cfg(not(target_os = "linux"))]
pub async fn wifi_forget(_ssid: &str) -> Result<(), WifiError> {
    Err(non_linux_error())
}

#[cfg(not(target_os = "linux"))]
fn non_linux_error() -> WifiError {
    WifiError {
        code: WifiErrorCode::BackendUnavailable,
        detail: "Wi-Fi control is only supported on the Linux appliance image".to_string(),
    }
}

/// `network.status` — assembles link + IP + connectivity from three
/// independent sources. Always `Ok`: every sub-source degrades to an honest
/// "unavailable"/"unknown" value on failure rather than failing the whole
/// call, per design §5.2's "link 層與 IP 層分開回報" rule — `wifi.state ==
/// "connected"` with an empty `ip.addresses` is a real, meaningful state
/// (associated but no DHCP yet), not an error to collapse away. The `Result`
/// return type is kept for symmetry with the other three facade functions
/// and as room for a genuine future failure mode; nothing in today's
/// implementation constructs an `Err`.
pub async fn status() -> Result<NetworkStatus, WifiError> {
    let wifi = wifi_link_status_or_unavailable().await;
    // `None`: we deliberately do not thread a Wi-Fi-specific interface name
    // through here. `ipinfo::collect` auto-detects the interface that
    // actually owns the default route (wired or wireless), which is the
    // right notion of "the" IP info for a status glance — a machine on
    // Ethernet with Wi-Fi merely associated (no default route) should show
    // the Ethernet address, not an empty Wi-Fi one.
    let ip = ipinfo::collect(None);
    let (internet, portal_url) = match portal::probe().await {
        portal::InternetVerdict::Online => ("online".to_string(), None),
        portal::InternetVerdict::Portal { url } => ("portal".to_string(), url),
        portal::InternetVerdict::Offline => ("offline".to_string(), None),
        portal::InternetVerdict::Unknown => ("unknown".to_string(), None),
    };
    Ok(NetworkStatus {
        wifi,
        ip,
        internet,
        portal_url,
        interfaces: crate::device::collect_network(),
    })
}

#[cfg(target_os = "linux")]
async fn wifi_link_status_or_unavailable() -> WifiLink {
    iwd::link_status()
        .await
        .unwrap_or_else(|_| unavailable_link())
}

#[cfg(not(target_os = "linux"))]
async fn wifi_link_status_or_unavailable() -> WifiLink {
    unavailable_link()
}

fn unavailable_link() -> WifiLink {
    WifiLink {
        state: "unavailable".to_string(),
        ssid: None,
        signal_bars: None,
        security: None,
        frequency: None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────
// Split into `tests_network.rs` to keep this file under the project's
// 800-line cap — see that file's own doc for why `#[path]` (not a normal
// sibling `mod`) preserves full access to this module's private items.

#[cfg(test)]
#[path = "tests_network.rs"]
mod tests;
