//! Wired network settings — the `network.wired_status` / `network.wired_config`
//! dashboard RPCs (dispatch + admin/appliance gating lives in `handlers.rs`,
//! same split as the rest of this module — see `network/mod.rs`'s doc).
//!
//! Storage + boot re-apply live here specifically so `handlers.rs` stays
//! thin glue (see the system-settings task brief's "Discipline" section):
//! the desired static-IP config the operator sets is persisted under
//! `<home_dir>/network/wired.json`, because the `duduclaw-sysd`
//! `NetworkWiredConfig` verb writes to `/run/systemd/network/` — tmpfs, by
//! design, so a future read-only root stays writable — which means the
//! setting does NOT survive a reboot on its own. [`reapply_wired_config_on_boot`]
//! is the boot-time step that re-issues the sysd call once, best-effort,
//! from this persisted file.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use duduclaw_core::with_file_lock;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Wired-interface selection (pure) ────────────────────────────────────

/// `<linux/if_arp.h>` `ARPHRD_ETHER` — the one `/sys/class/net/<name>/type`
/// value this module treats as "a wired Ethernet device".
const ARPHRD_ETHER: u32 = 1;

/// Minimal `/sys` probe result for one candidate interface name, fed to
/// [`is_wired`]/[`select_wired_interface`] by a probe closure ([`probe_interface`])
/// so the selection RULE itself is pure and unit-tested with synthetic data,
/// independent of a real `/sys/class/net` tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiredProbe {
    /// `/sys/class/net/<name>/wireless` exists.
    pub has_wireless_dir: bool,
    /// `/sys/class/net/<name>/type`, parsed as `u32`.
    pub arphrd_type: Option<u32>,
}

/// Pure: is this candidate a wired (Ethernet) interface? No wireless sysfs
/// directory AND the kernel reports `ARPHRD_ETHER`. Both conditions must
/// hold — a bridge/tunnel interface can lack a `wireless` dir while also not
/// being `ARPHRD_ETHER`, and must not be picked either.
pub fn is_wired(probe: &WiredProbe) -> bool {
    !probe.has_wireless_dir && probe.arphrd_type == Some(ARPHRD_ETHER)
}

/// Pure selection: given candidate interface names (already filtered to
/// exclude loopback — see [`detect_wired_interface`]) each paired with a
/// [`WiredProbe`], pick the first name [`is_wired`] accepts. Deterministic
/// by input order (the order `device::collect_network()` enumerates in).
pub fn select_wired_interface(candidates: &[(String, WiredProbe)]) -> Option<String> {
    candidates
        .iter()
        .find(|(_, probe)| is_wired(probe))
        .map(|(name, _)| name.clone())
}

fn probe_interface(name: &str) -> WiredProbe {
    let has_wireless_dir = Path::new(&format!("/sys/class/net/{name}/wireless")).exists();
    let arphrd_type = std::fs::read_to_string(format!("/sys/class/net/{name}/type"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    WiredProbe { has_wireless_dir, arphrd_type }
}

/// Detect "the" wired interface for a status glance — the first candidate
/// (in `device::collect_network()`'s enumeration order, loopback excluded)
/// [`is_wired`] accepts. Reuses `device::collect_network()` for the
/// candidate name list rather than a second interface enumeration (per the
/// task brief); `None` when no such interface exists (no Ethernet hardware
/// present, or off-Linux where `/sys/class/net` never matches).
pub fn detect_wired_interface() -> Option<String> {
    let candidates: Vec<(String, WiredProbe)> = crate::device::collect_network()
        .into_iter()
        .map(|i| i.name)
        .filter(|name| name != "lo")
        .map(|name| {
            let probe = probe_interface(&name);
            (name, probe)
        })
        .collect();
    select_wired_interface(&candidates)
}

// ── CIDR addresses (a second, purpose-built getifaddrs reader) ─────────────
//
// `device::collect_network()` deliberately omits netmask (its consumers —
// `device.status`/`device.network` — never needed CIDR notation), but the
// `network.wired_status` contract's `addresses` field does
// (`"192.168.1.50/24"`). Rather than change `device::NetworkInterface`'s
// shape (risking behavior other consumers already depend on), this reads
// netmask directly via a second `getifaddrs()` call scoped to one interface.

/// Pure: CIDR prefix length from a netmask IP, by counting set bits. Doesn't
/// validate contiguity — every netmask an OS hands out is contiguous, and a
/// malformed one degrading to "a number that doesn't look right" (rather
/// than panicking) is an acceptable failure mode for a dashboard glance.
pub fn netmask_to_prefix_len(mask: IpAddr) -> u8 {
    match mask {
        IpAddr::V4(v4) => v4.octets().iter().map(|b| b.count_ones() as u8).sum(),
        IpAddr::V6(v6) => v6.octets().iter().map(|b| b.count_ones() as u8).sum(),
    }
}

#[cfg(unix)]
fn sockaddr_to_ip(addr: &nix::sys::socket::SockaddrStorage) -> Option<IpAddr> {
    addr.as_sockaddr_in()
        .map(|v4| IpAddr::V4(v4.ip()))
        .or_else(|| addr.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip())))
}

#[cfg(unix)]
fn read_cidr_addresses(interface: &str) -> Vec<String> {
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for a in addrs {
        if a.interface_name != interface {
            continue;
        }
        let Some(ip) = a.address.as_ref().and_then(sockaddr_to_ip) else {
            continue;
        };
        match a.netmask.as_ref().and_then(sockaddr_to_ip) {
            Some(mask) => out.push(format!("{ip}/{}", netmask_to_prefix_len(mask))),
            None => out.push(ip.to_string()),
        }
    }
    out
}

#[cfg(not(unix))]
fn read_cidr_addresses(_interface: &str) -> Vec<String> {
    Vec::new()
}

// ── Persisted desired config ────────────────────────────────────────────

/// Persisted desired wired-network configuration — see this module's doc
/// for why a persisted copy is needed at all (the sysd verb's effect lives
/// on tmpfs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WiredConfig {
    pub interface: String,
    /// `"dhcp"` | `"static"`.
    pub mode: String,
    pub address: Option<String>,
    pub gateway: Option<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    pub updated_at: String,
}

/// The `network.wired_status` response's `"configured"` field shape — a
/// projection of [`WiredConfig`] without `interface`/`updated_at`, per the
/// frozen RPC contract's example payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConfiguredDesired {
    pub mode: String,
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

impl From<&WiredConfig> for ConfiguredDesired {
    fn from(c: &WiredConfig) -> Self {
        ConfiguredDesired {
            mode: c.mode.clone(),
            address: c.address.clone(),
            gateway: c.gateway.clone(),
            dns: c.dns.clone(),
        }
    }
}

fn config_path(home_dir: &Path) -> PathBuf {
    home_dir.join("network").join("wired.json")
}

/// Load the persisted desired config. Missing or corrupt file degrades to
/// `None` — "degrade, don't fabricate" (same discipline as
/// `working_state.rs::load`).
pub fn load_wired_config(home_dir: &Path) -> Option<WiredConfig> {
    let raw = std::fs::read_to_string(config_path(home_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Atomic persist: temp file + rename inside a cross-process advisory lock
/// (same pattern as `working_state.rs::persist`/`set_entry`).
pub fn save_wired_config(home_dir: &Path, cfg: &WiredConfig) -> std::io::Result<()> {
    let path = config_path(home_dir);
    with_file_lock(&path, || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(cfg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    })
}

/// Delete the persisted desired config (returning to the shipped image's
/// DHCP default, `mode: "dhcp"`'s write path). Missing file is not an error.
pub fn delete_wired_config(home_dir: &Path) -> std::io::Result<()> {
    let path = config_path(home_dir);
    with_file_lock(&path, || match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    })
}

// ── Validation (pure, closed error taxonomy) ────────────────────────────

/// Closed error taxonomy for `network.wired_config` — mirrors
/// `crate::network::WifiErrorCode`'s shape/discipline (one `code()` wire
/// value + one zh-TW `message()`). `address` and `gateway` share
/// [`Self::InvalidAddress`] (rather than two separate codes) because the
/// task brief's contract explicitly unions them: "IPv6 in address/gateway
/// ⇒ refuse invalid_address".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiredConfigErrorCode {
    NoInterface,
    InvalidMode,
    InvalidAddress,
    InvalidDns,
    BackendUnavailable,
    ApplyFailed,
}

impl WiredConfigErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoInterface => "no_interface",
            Self::InvalidMode => "invalid_mode",
            Self::InvalidAddress => "invalid_address",
            Self::InvalidDns => "invalid_dns",
            Self::BackendUnavailable => "backend_unavailable",
            Self::ApplyFailed => "apply_failed",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::NoInterface => "找不到有線網路介面，請確認網路線已插上。",
            Self::InvalidMode => "mode 必須是 \"dhcp\" 或 \"static\"。",
            Self::InvalidAddress => {
                "IP 位址或路由器位址格式不正確，需為合法的 IPv4 格式（例如 192.168.1.50/24）；本版尚未支援 IPv6。"
            }
            Self::InvalidDns => "DNS 伺服器最多 3 筆，且每筆需為合法的 IP 位址。",
            Self::BackendUnavailable => "網路設定服務未啟動，請重新開機或聯絡支援。",
            Self::ApplyFailed => "套用有線網路設定失敗。",
        }
    }
}

/// Parse `"192.168.1.50/24"` into `(Ipv4Addr, prefix)`, prefix `1..=32`.
/// IPv6 (and anything else unparseable) is honestly [`WiredConfigErrorCode::
/// InvalidAddress`] — this appliance's static-IP support is IPv4-only this
/// round.
pub fn parse_ipv4_cidr(s: &str) -> Result<(Ipv4Addr, u8), WiredConfigErrorCode> {
    let (addr_part, prefix_part) = s.split_once('/').ok_or(WiredConfigErrorCode::InvalidAddress)?;
    let addr: Ipv4Addr = addr_part.parse().map_err(|_| WiredConfigErrorCode::InvalidAddress)?;
    let prefix: u8 = prefix_part.parse().map_err(|_| WiredConfigErrorCode::InvalidAddress)?;
    if !(1..=32).contains(&prefix) {
        return Err(WiredConfigErrorCode::InvalidAddress);
    }
    Ok((addr, prefix))
}

/// Parse a gateway string as IPv4 — same [`WiredConfigErrorCode::InvalidAddress`]
/// refusal as [`parse_ipv4_cidr`] for anything else (including a
/// syntactically valid IPv6 address).
pub fn parse_ipv4_gateway(s: &str) -> Result<Ipv4Addr, WiredConfigErrorCode> {
    s.parse::<Ipv4Addr>().map_err(|_| WiredConfigErrorCode::InvalidAddress)
}

/// `<=3` entries, each a parseable [`IpAddr`] — v4 OR v6 (DNS resolvers are
/// commonly IPv6, unlike the address/gateway fields above, which are
/// IPv4-only this round).
pub fn validate_dns_list(dns: &[String]) -> Result<(), WiredConfigErrorCode> {
    if dns.len() > 3 {
        return Err(WiredConfigErrorCode::InvalidDns);
    }
    for entry in dns {
        entry.parse::<IpAddr>().map_err(|_| WiredConfigErrorCode::InvalidDns)?;
    }
    Ok(())
}

/// Closed set: `"dhcp"` | `"static"`.
pub fn validate_mode(mode: &str) -> Result<(), WiredConfigErrorCode> {
    match mode {
        "dhcp" | "static" => Ok(()),
        _ => Err(WiredConfigErrorCode::InvalidMode),
    }
}

/// Full request validation — the gateway-side half of "defense in depth"
/// (sysd validates again independently on its own side, same doctrine as
/// `crate::network::validate_psk`'s WPA-length double-check doc comment).
/// `address` is required when `mode == "static"`; `gateway`/`dns` stay
/// optional in both modes.
pub fn validate_wired_config_request(
    mode: &str,
    address: Option<&str>,
    gateway: Option<&str>,
    dns: &[String],
) -> Result<(), WiredConfigErrorCode> {
    validate_mode(mode)?;
    if mode == "static" {
        let addr = address.ok_or(WiredConfigErrorCode::InvalidAddress)?;
        parse_ipv4_cidr(addr)?;
    }
    if let Some(gw) = gateway {
        parse_ipv4_gateway(gw)?;
    }
    validate_dns_list(dns)?;
    Ok(())
}

// ── Read-path orchestration ─────────────────────────────────────────────

/// `network.wired_status` response shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WiredStatus {
    pub interface: Option<String>,
    pub link_up: bool,
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    /// `"dhcp"` | `"static"` | `"unknown"` — same vocabulary as
    /// [`crate::network::IpInfo::source`].
    pub source: String,
    pub configured: Option<ConfiguredDesired>,
}

/// Assemble [`WiredStatus`]: live half from [`detect_wired_interface`] +
/// `device::collect_network()` (link state) + [`read_cidr_addresses`] +
/// `crate::network::ipinfo::collect` (gateway/dns/source — reused rather
/// than re-derived, per the task brief); desired half from
/// [`load_wired_config`].
pub fn collect_wired_status(home_dir: &Path) -> WiredStatus {
    let interface = detect_wired_interface();
    let link_up = interface
        .as_deref()
        .and_then(|name| {
            crate::device::collect_network()
                .into_iter()
                .find(|i| i.name == name)
                .map(|i| i.is_up)
        })
        .unwrap_or(false);
    let addresses = interface.as_deref().map(read_cidr_addresses).unwrap_or_default();
    let ip = crate::network::ipinfo::collect(interface.as_deref());
    let configured = load_wired_config(home_dir).as_ref().map(ConfiguredDesired::from);
    WiredStatus {
        interface,
        link_up,
        addresses,
        gateway: ip.gateway,
        dns: ip.dns,
        source: ip.source,
        configured,
    }
}

// ── Boot re-apply ────────────────────────────────────────────────────────

/// Re-issue a persisted `mode: "static"` config to `duduclaw-sysd` once at
/// gateway boot, best-effort. See this module's doc for WHY: the sysd
/// `NetworkWiredConfig` verb writes to `/run/systemd/network/` (tmpfs by
/// design), so the setting does not survive a reboot on its own.
///
/// Never blocks or fails boot: called fire-and-forget from `server::
/// start_gateway`, every failure is logged and swallowed. A no-op on a
/// non-appliance install, an appliance with no persisted config, or one
/// whose persisted config is `mode: "dhcp"` (nothing to re-apply — DHCP is
/// the shipped image's own default, already active without help).
pub async fn reapply_wired_config_on_boot(home_dir: &Path) {
    if !duduclaw_core::is_appliance() {
        return;
    }
    let Some(cfg) = load_wired_config(home_dir) else {
        return;
    };
    if cfg.mode != "static" {
        return;
    }
    let Some(ops) = crate::device_ops::select_sysd_ops() else {
        warn!(
            interface = %cfg.interface,
            "boot-time wired network re-apply skipped — duduclaw-sysd not reachable"
        );
        return;
    };
    match ops
        .network_wired_config(&cfg.interface, &cfg.mode, cfg.address.as_deref(), cfg.gateway.as_deref(), &cfg.dns)
        .await
    {
        Ok(out) if out.success => {
            info!(interface = %cfg.interface, "boot-time wired network config re-applied");
        }
        Ok(out) => {
            warn!(
                interface = %cfg.interface,
                stderr = %duduclaw_core::truncate_chars(&out.stderr, 300),
                "boot-time wired network re-apply ran but reported failure"
            );
        }
        Err(e) => {
            warn!(interface = %cfg.interface, error = %e, "boot-time wired network re-apply failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_wired / select_wired_interface ────────────────────────────

    #[test]
    fn is_wired_requires_no_wireless_dir_and_arphrd_ether() {
        assert!(is_wired(&WiredProbe { has_wireless_dir: false, arphrd_type: Some(1) }));
        assert!(!is_wired(&WiredProbe { has_wireless_dir: true, arphrd_type: Some(1) }));
        assert!(!is_wired(&WiredProbe { has_wireless_dir: false, arphrd_type: Some(801) })); // wlan ARPHRD_IEEE80211
        assert!(!is_wired(&WiredProbe { has_wireless_dir: false, arphrd_type: None }));
    }

    #[test]
    fn select_wired_interface_picks_first_match_in_order() {
        let candidates = vec![
            ("wlan0".to_string(), WiredProbe { has_wireless_dir: true, arphrd_type: Some(1) }),
            ("docker0".to_string(), WiredProbe { has_wireless_dir: false, arphrd_type: Some(772) }),
            ("enp1s0".to_string(), WiredProbe { has_wireless_dir: false, arphrd_type: Some(1) }),
            ("eth1".to_string(), WiredProbe { has_wireless_dir: false, arphrd_type: Some(1) }),
        ];
        assert_eq!(select_wired_interface(&candidates).as_deref(), Some("enp1s0"));
    }

    #[test]
    fn select_wired_interface_none_when_nothing_matches() {
        let candidates = vec![("wlan0".to_string(), WiredProbe { has_wireless_dir: true, arphrd_type: Some(1) })];
        assert_eq!(select_wired_interface(&candidates), None);
        assert_eq!(select_wired_interface(&[]), None);
    }

    #[test]
    fn detect_wired_interface_never_panics() {
        // Real /sys reads — host-dependent result, contract under test is
        // "never panics" (macOS dev machine has no /sys/class/net at all).
        let _ = detect_wired_interface();
    }

    // ── netmask_to_prefix_len ─────────────────────────────────────────

    #[test]
    fn netmask_to_prefix_len_examples() {
        assert_eq!(netmask_to_prefix_len(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 0))), 24);
        assert_eq!(netmask_to_prefix_len(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255))), 32);
        assert_eq!(netmask_to_prefix_len(IpAddr::V4(Ipv4Addr::new(255, 255, 0, 0))), 16);
        assert_eq!(netmask_to_prefix_len(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))), 0);
    }

    #[test]
    fn read_cidr_addresses_never_panics() {
        let _ = read_cidr_addresses("nonexistent-iface-xyz");
    }

    // ── WiredConfig persistence ────────────────────────────────────────

    fn sample_config() -> WiredConfig {
        WiredConfig {
            interface: "enp1s0".to_string(),
            mode: "static".to_string(),
            address: Some("192.168.1.50/24".to_string()),
            gateway: Some("192.168.1.1".to_string()),
            dns: vec!["1.1.1.1".to_string()],
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = sample_config();
        save_wired_config(dir.path(), &cfg).unwrap();
        let loaded = load_wired_config(dir.path()).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_wired_config(dir.path()).is_none());
    }

    #[test]
    fn load_corrupt_file_degrades_to_none_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("network")).unwrap();
        std::fs::write(dir.path().join("network").join("wired.json"), b"not json").unwrap();
        assert!(load_wired_config(dir.path()).is_none());
    }

    #[test]
    fn delete_removes_config_and_missing_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = sample_config();
        save_wired_config(dir.path(), &cfg).unwrap();
        assert!(load_wired_config(dir.path()).is_some());
        delete_wired_config(dir.path()).unwrap();
        assert!(load_wired_config(dir.path()).is_none());
        // Second delete (nothing left to delete) must still be Ok.
        delete_wired_config(dir.path()).unwrap();
    }

    #[test]
    fn configured_desired_projection_drops_interface_and_updated_at() {
        let cfg = sample_config();
        let projected = ConfiguredDesired::from(&cfg);
        let json = serde_json::to_value(&projected).unwrap();
        assert!(json.get("interface").is_none());
        assert!(json.get("updated_at").is_none());
        assert_eq!(json["mode"], "static");
        assert_eq!(json["address"], "192.168.1.50/24");
    }

    // ── validation ──────────────────────────────────────────────────────

    #[test]
    fn validate_mode_closed_set() {
        assert!(validate_mode("dhcp").is_ok());
        assert!(validate_mode("static").is_ok());
        assert_eq!(validate_mode("bogus"), Err(WiredConfigErrorCode::InvalidMode));
        assert_eq!(validate_mode(""), Err(WiredConfigErrorCode::InvalidMode));
    }

    #[test]
    fn parse_ipv4_cidr_accepts_valid_and_rejects_bad_shapes() {
        assert_eq!(parse_ipv4_cidr("192.168.1.50/24"), Ok((Ipv4Addr::new(192, 168, 1, 50), 24)));
        assert_eq!(parse_ipv4_cidr("10.0.0.1/1"), Ok((Ipv4Addr::new(10, 0, 0, 1), 1)));
        assert_eq!(parse_ipv4_cidr("10.0.0.1/32"), Ok((Ipv4Addr::new(10, 0, 0, 1), 32)));
        assert_eq!(parse_ipv4_cidr("10.0.0.1/0"), Err(WiredConfigErrorCode::InvalidAddress));
        assert_eq!(parse_ipv4_cidr("10.0.0.1/33"), Err(WiredConfigErrorCode::InvalidAddress));
        assert_eq!(parse_ipv4_cidr("10.0.0.1"), Err(WiredConfigErrorCode::InvalidAddress));
        assert_eq!(parse_ipv4_cidr("not-an-ip/24"), Err(WiredConfigErrorCode::InvalidAddress));
    }

    #[test]
    fn parse_ipv4_cidr_rejects_ipv6() {
        assert_eq!(parse_ipv4_cidr("2001:db8::1/64"), Err(WiredConfigErrorCode::InvalidAddress));
    }

    #[test]
    fn parse_ipv4_gateway_accepts_v4_rejects_v6_and_garbage() {
        assert_eq!(parse_ipv4_gateway("192.168.1.1"), Ok(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_ipv4_gateway("2001:db8::1"), Err(WiredConfigErrorCode::InvalidAddress));
        assert_eq!(parse_ipv4_gateway("not-an-ip"), Err(WiredConfigErrorCode::InvalidAddress));
    }

    #[test]
    fn validate_dns_list_accepts_v4_and_v6_up_to_three() {
        assert!(validate_dns_list(&["1.1.1.1".to_string()]).is_ok());
        assert!(
            validate_dns_list(&["1.1.1.1".to_string(), "2001:4860:4860::8888".to_string()]).is_ok()
        );
        assert!(validate_dns_list(&[]).is_ok());
    }

    #[test]
    fn validate_dns_list_rejects_over_three_or_unparseable() {
        assert_eq!(
            validate_dns_list(&[
                "1.1.1.1".to_string(),
                "8.8.8.8".to_string(),
                "9.9.9.9".to_string(),
                "1.0.0.1".to_string()
            ]),
            Err(WiredConfigErrorCode::InvalidDns)
        );
        assert_eq!(validate_dns_list(&["not-an-ip".to_string()]), Err(WiredConfigErrorCode::InvalidDns));
    }

    #[test]
    fn validate_wired_config_request_dhcp_needs_no_address() {
        assert!(validate_wired_config_request("dhcp", None, None, &[]).is_ok());
    }

    #[test]
    fn validate_wired_config_request_static_requires_address() {
        assert_eq!(
            validate_wired_config_request("static", None, None, &[]),
            Err(WiredConfigErrorCode::InvalidAddress)
        );
        assert!(validate_wired_config_request("static", Some("192.168.1.50/24"), None, &[]).is_ok());
    }

    #[test]
    fn validate_wired_config_request_checks_gateway_and_dns_too() {
        assert_eq!(
            validate_wired_config_request(
                "static",
                Some("192.168.1.50/24"),
                Some("2001:db8::1"),
                &[]
            ),
            Err(WiredConfigErrorCode::InvalidAddress)
        );
        assert_eq!(
            validate_wired_config_request(
                "static",
                Some("192.168.1.50/24"),
                Some("192.168.1.1"),
                &["garbage".to_string()]
            ),
            Err(WiredConfigErrorCode::InvalidDns)
        );
    }

    #[test]
    fn error_codes_are_stable_strings() {
        assert_eq!(WiredConfigErrorCode::NoInterface.code(), "no_interface");
        assert_eq!(WiredConfigErrorCode::InvalidMode.code(), "invalid_mode");
        assert_eq!(WiredConfigErrorCode::InvalidAddress.code(), "invalid_address");
        assert_eq!(WiredConfigErrorCode::InvalidDns.code(), "invalid_dns");
        assert_eq!(WiredConfigErrorCode::BackendUnavailable.code(), "backend_unavailable");
        assert_eq!(WiredConfigErrorCode::ApplyFailed.code(), "apply_failed");
    }

    // ── collect_wired_status orchestration smoke test ────────────────────

    #[test]
    fn collect_wired_status_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let status = collect_wired_status(dir.path());
        // No persisted config in a fresh tempdir.
        assert!(status.configured.is_none());
        assert!(matches!(status.source.as_str(), "dhcp" | "unknown"));
    }

    #[test]
    fn collect_wired_status_reports_persisted_config() {
        let dir = tempfile::tempdir().unwrap();
        save_wired_config(dir.path(), &sample_config()).unwrap();
        let status = collect_wired_status(dir.path());
        assert_eq!(status.configured.as_ref().map(|c| c.mode.as_str()), Some("static"));
    }

    // ── reapply_wired_config_on_boot ──────────────────────────────────────

    #[tokio::test]
    async fn reapply_on_boot_is_a_noop_off_appliance() {
        // Precondition matches the rest of this crate's tests: this test
        // process never sets DUDUCLAW_APPLIANCE.
        assert!(!duduclaw_core::is_appliance());
        let dir = tempfile::tempdir().unwrap();
        save_wired_config(dir.path(), &sample_config()).unwrap();
        // Must return promptly without touching the (nonexistent) sysd
        // socket — the is_appliance() check short-circuits first.
        reapply_wired_config_on_boot(dir.path()).await;
    }

    #[tokio::test]
    async fn reapply_on_boot_is_a_noop_with_no_persisted_config() {
        let dir = tempfile::tempdir().unwrap();
        reapply_wired_config_on_boot(dir.path()).await;
    }
}
