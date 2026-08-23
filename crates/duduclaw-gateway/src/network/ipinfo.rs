//! IP-layer status — `/proc/net/route` (default gateway + its owning
//! interface), resolv.conf (DNS), and DHCP-lease presence, feeding
//! [`crate::network::IpInfo`]. Same split as the rest of this module: a
//! thin collector (`read_*`) over a pure parser (`parse_*`), so the parsing
//! is unit-tested on every host this crate compiles for even though the
//! collectors themselves only ever see real data on Linux.
//!
//! Every read here degrades to an empty/`None` value on failure — this
//! feeds a best-effort status glance, not something that should ever fail
//! `network.status` outright (see [`crate::network::status`]'s own doc).

use crate::network::IpInfo;

/// Pure: parse `/proc/net/route`'s default-route row — the one whose
/// `Destination` field is `00000000` — into `(interface, gateway_dotted)`.
/// Both [`parse_default_gateway`] (the spec-required public entry point) and
/// [`collect`]'s own interface auto-detection read through this single
/// parser, so they can never disagree about which row is "the" default
/// route.
fn parse_default_route_row(proc_net_route: &str) -> Option<(String, String)> {
    for line in proc_net_route.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let iface = fields.next()?;
        let destination = fields.next()?;
        let gateway_hex = fields.next()?;
        if destination == "00000000" {
            let gateway = hex_le_to_dotted(gateway_hex)?;
            return Some((iface.to_string(), gateway));
        }
    }
    None
}

/// Parse `/proc/net/route` and return the default gateway's dotted-decimal
/// address, or `None` if there is no default route (or the file is
/// malformed/absent).
pub fn parse_default_gateway(proc_net_route: &str) -> Option<String> {
    parse_default_route_row(proc_net_route).map(|(_, gateway)| gateway)
}

/// `/proc/net/route`'s `Gateway` column is 8 hex digits, little-endian
/// (kernel-native byte order on every architecture this appliance ships
/// for). `"0102A8C0"` -> bytes `[01, 02, A8, C0]` -> reversed -> `192.168.2.1`.
fn hex_le_to_dotted(hex: &str) -> Option<String> {
    if hex.len() != 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok();
    let (b0, b1, b2, b3) = (byte(0)?, byte(1)?, byte(2)?, byte(3)?);
    Some(format!("{b3}.{b2}.{b1}.{b0}"))
}

/// Pure: extract `nameserver` lines from a resolv.conf body. Whole-token
/// match (`split_whitespace().next() == Some("nameserver")`), never a
/// substring/prefix check, so a comment like `# nameserver-ish note` can't
/// be mistaken for a directive.
pub fn parse_resolv_conf(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if parts.next() == Some("nameserver") {
            if let Some(addr) = parts.next() {
                out.push(addr.to_string());
            }
        }
    }
    out
}

fn read_default_route() -> Option<(String, String)> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    parse_default_route_row(&text)
}

fn read_dns_servers() -> Vec<String> {
    // systemd-resolved's own stub file first (matches this appliance's
    // network stack — see the D4a design doc's iwd + systemd-networkd
    // choice); `/etc/resolv.conf` as the universal fallback. First
    // non-empty result wins; nothing here ever merges the two.
    for path in ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            let dns = parse_resolv_conf(&text);
            if !dns.is_empty() {
                return dns;
            }
        }
    }
    Vec::new()
}

/// `/sys/class/net/<interface>/ifindex` — the kernel's own numeric
/// interface index, needed to find this interface's lease file under
/// `/run/systemd/netif/leases/`.
fn read_ifindex(interface: &str) -> Option<String> {
    std::fs::read_to_string(format!("/sys/class/net/{interface}/ifindex"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Does systemd-networkd have a live DHCP lease recorded for `interface`?
/// `Some(true)`/`Some(false)` only when the ifindex lookup itself succeeded
/// — the caller ([`collect`]) treats anything else as `"unknown"`, never a
/// guessed `"static"` (design §5.2: "不要猜 static").
fn has_dhcp_lease(interface: &str) -> Option<bool> {
    let ifindex = read_ifindex(interface)?;
    Some(std::path::Path::new(&format!("/run/systemd/netif/leases/{ifindex}")).is_file())
}

/// Addresses for one interface, read from the existing `device::
/// collect_network()` collector (never re-implemented here — see this
/// module's doc and the D4a task brief's explicit "不要重刻 ifaddrs").
fn addresses_for(interface: &str) -> Vec<String> {
    crate::device::collect_network()
        .into_iter()
        .find(|i| i.name == interface)
        .map(|i| i.addresses)
        .unwrap_or_default()
}

/// Assemble [`IpInfo`]. `interface`, when given, is used as-is (both for
/// picking addresses and for the DHCP-lease check); when `None`, the
/// interface is auto-detected from the default route itself (the row
/// [`parse_default_route_row`] finds also names its own `Iface`), so a
/// caller that doesn't know which interface is "the" active one (see
/// [`crate::network::status`]) still gets a coherent answer instead of an
/// empty one.
pub fn collect(interface: Option<&str>) -> IpInfo {
    let route = read_default_route();
    let gateway = route.as_ref().map(|(_, gw)| gw.clone());
    let resolved_interface = interface
        .map(str::to_string)
        .or_else(|| route.map(|(iface, _)| iface));

    let addresses = resolved_interface
        .as_deref()
        .map(addresses_for)
        .unwrap_or_default();
    let dns = read_dns_servers();
    let source = match resolved_interface.as_deref().and_then(has_dhcp_lease) {
        Some(true) => "dhcp".to_string(),
        // Covers both "confirmed no lease file" and "couldn't even check"
        // (no interface known, no ifindex, unreadable leases dir) — none of
        // those license inferring "static".
        _ => "unknown".to_string(),
    };

    IpInfo {
        interface: resolved_interface,
        addresses,
        gateway,
        dns,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_default_gateway / parse_default_route_row ───────────────────

    /// A realistic `/proc/net/route` body — header line plus one default
    /// route on `eth0` with the textbook `0102A8C0` gateway hex value
    /// (little-endian for `192.168.2.1`, the same example widely used to
    /// document this file's format).
    const REALISTIC_ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\t\
        Mask\t\tMTU\tWindow\tIRTT\n\
        eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        eth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn parses_realistic_default_gateway() {
        assert_eq!(
            parse_default_gateway(REALISTIC_ROUTE).as_deref(),
            Some("192.168.2.1")
        );
    }

    #[test]
    fn parses_default_route_interface_alongside_gateway() {
        assert_eq!(
            parse_default_route_row(REALISTIC_ROUTE),
            Some(("eth0".to_string(), "192.168.2.1".to_string()))
        );
    }

    #[test]
    fn no_default_route_is_none() {
        // Only a non-default (on-link) route present, no 00000000 row.
        let text = "Iface\tDestination\tGateway\tFlags\n\
                     eth0\t0000A8C0\t00000000\t0001\n";
        assert_eq!(parse_default_gateway(text), None);
    }

    #[test]
    fn empty_or_header_only_is_none() {
        assert_eq!(parse_default_gateway(""), None);
        assert_eq!(parse_default_gateway("Iface\tDestination\tGateway\n"), None);
    }

    #[test]
    fn garbage_route_text_is_none_never_panics() {
        assert_eq!(parse_default_gateway("not\na\nroute\ntable\n"), None);
        assert_eq!(parse_default_gateway("\n\n\n"), None);
    }

    #[test]
    fn malformed_gateway_hex_is_none() {
        let text = "Iface\tDestination\tGateway\n\
                     eth0\t00000000\tZZZZZZZZ\n";
        assert_eq!(parse_default_gateway(text), None);
        let short = "Iface\tDestination\tGateway\n\
                      eth0\t00000000\tABC\n";
        assert_eq!(parse_default_gateway(short), None);
    }

    #[test]
    fn hex_le_to_dotted_examples() {
        assert_eq!(hex_le_to_dotted("0102A8C0").as_deref(), Some("192.168.2.1"));
        assert_eq!(hex_le_to_dotted("00000000").as_deref(), Some("0.0.0.0"));
        assert_eq!(hex_le_to_dotted("0100007F").as_deref(), Some("127.0.0.1"));
    }

    // ── parse_resolv_conf ──────────────────────────────────────────────────

    #[test]
    fn parses_nameserver_lines() {
        let text = "nameserver 1.1.1.1\nnameserver 8.8.8.8\n";
        assert_eq!(parse_resolv_conf(text), vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn ignores_comments_and_other_directives() {
        let text =
            "# nameserver 9.9.9.9\nsearch example.com\nnameserver 1.1.1.1\n; nameserver 2.2.2.2\n";
        assert_eq!(parse_resolv_conf(text), vec!["1.1.1.1"]);
    }

    #[test]
    fn empty_resolv_conf_is_empty_vec() {
        assert!(parse_resolv_conf("").is_empty());
        assert!(parse_resolv_conf("search example.com\n").is_empty());
    }

    #[test]
    fn whitespace_variations_are_tolerated() {
        let text = "  nameserver   1.1.1.1  \n\tnameserver\t8.8.8.8\t\n";
        assert_eq!(parse_resolv_conf(text), vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn nameserver_word_must_be_whole_token() {
        // "nameserver-ish" as a directive keyword must NOT match — this is
        // the whole-token discipline coding convention 2 asks for.
        let text = "nameserver-ish 1.1.1.1\n";
        assert!(parse_resolv_conf(text).is_empty());
    }

    // ── collect() — smoke only (live /proc, /sys reads) ────────────────────

    #[test]
    fn collect_never_panics_with_or_without_an_interface_hint() {
        let _ = collect(None);
        let _ = collect(Some("nonexistent-iface-xyz"));
    }

    #[test]
    fn collect_source_is_never_a_guessed_static() {
        // Whatever this host's real state is, `source` must be one of the
        // three closed values, and specifically never fabricate "static"
        // just because we couldn't determine "dhcp".
        for info in [collect(None), collect(Some("nonexistent-iface-xyz"))] {
            assert!(
                matches!(info.source.as_str(), "dhcp" | "unknown"),
                "source must never be a guessed 'static': {}",
                info.source
            );
        }
    }

    #[test]
    fn collect_unknown_interface_has_no_addresses() {
        let info = collect(Some("nonexistent-iface-xyz"));
        assert!(info.addresses.is_empty());
        assert_eq!(info.interface.as_deref(), Some("nonexistent-iface-xyz"));
    }

    #[test]
    fn has_dhcp_lease_unknown_interface_is_none() {
        assert_eq!(has_dhcp_lease("nonexistent-iface-xyz"), None);
    }
}
