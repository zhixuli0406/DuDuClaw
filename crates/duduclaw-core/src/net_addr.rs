//! The one answer to "may an outbound request connect to this address?".
//!
//! Every SSRF gate in the workspace (the gateway's `web_fetch` validator and
//! DNS re-pin, the computer-use navigation pinning, the Odoo URL check, the
//! wiki-federation peer check) classifies addresses with [`is_public_ip`].
//! A second copy of "which ranges are internal" is how an SSRF gate rots out
//! of sync with itself, so new callers must use this function rather than
//! `Ipv4Addr::is_private()` & co. (which miss most of the ranges below).
//!
//! An address is public unless it falls in one of these blocks:
//!
//! IPv4: `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10` (shared/CGNAT),
//! `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.0.0.0/24`,
//! `192.0.2.0/24`, `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`,
//! `203.0.113.0/24`, `224.0.0.0/4` (multicast), `240.0.0.0/4` (reserved,
//! including `255.255.255.255`).
//!
//! IPv6: only global unicast (`2000::/3`) is public, so everything else is
//! refused — notably `::` and `::1`, `::/96` (IPv4-compatible, deprecated),
//! `100::/64` (discard), `64:ff9b:1::/48` (local-use NAT64), `fc00::/7`,
//! `fe80::/10`, `fec0::/10`, `ff00::/8` — and inside `2000::/3` these are
//! refused too: `2001::/32` (Teredo), `2001:db8::/32` and `3fff::/20`
//! (documentation).
//!
//! IPv6 forms that carry an IPv4 address in a fixed position are classified
//! by that embedded address: `::ffff:0:0/96` (IPv4-mapped), `64:ff9b::/96`
//! (well-known NAT64 prefix) and `2002::/16` (6to4). So `::ffff:127.0.0.1`
//! and `64:ff9b::a9fe:a9fe` are refused, while `64:ff9b::101:101` (1.1.1.1
//! behind DNS64) stays reachable. Forms whose embedded address cannot be
//! extracted reliably — IPv4-compatible, Teredo (two embedded addresses, one
//! obfuscated) and local-use NAT64 (operator-chosen prefix length) — are
//! refused as a whole class.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `true` when an outbound connection to `ip` reaches the public internet.
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

/// IPv4 half of [`is_public_ip`].
pub fn is_public_ipv4(ip: &Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || (a == 100 && (64..=127).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // 224.0.0.0/4 multicast and 240.0.0.0/4 reserved + broadcast.
        || a >= 224)
}

/// IPv6 half of [`is_public_ip`].
pub fn is_public_ipv6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    let low_v4 = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);

    // ::ffff:0:0/96 — IPv4-mapped: the embedded address decides.
    if s[..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff {
        return is_public_ipv4(&low_v4());
    }
    // ::/96 — unspecified, loopback and the deprecated IPv4-compatible form.
    if s[..6] == [0, 0, 0, 0, 0, 0] {
        return false;
    }
    // 64:ff9b::/96 — well-known NAT64 prefix (RFC 6052): embedded address.
    if s[..6] == [0x0064, 0xff9b, 0, 0, 0, 0] {
        return is_public_ipv4(&low_v4());
    }
    // 64:ff9b:1::/48 — local-use NAT64 (RFC 8215), prefix length unknown.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001 {
        return false;
    }
    // 2002::/16 — 6to4: the IPv4 sits in bits 16..48.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return is_public_ipv4(&v4);
    }
    let first = s[0];
    // Only 2000::/3 is global unicast; everything outside it (::/8 apart
    // from the forms above, 100::/64, fc00::/7, fe80::/10, fec0::/10,
    // ff00::/8 …) is special-purpose or unallocated.
    if (first & 0xe000) != 0x2000 {
        return false;
    }
    !(
        // 2001::/32 Teredo.
        (first == 0x2001 && s[1] == 0x0000)
        // 2001:db8::/32 documentation.
        || (first == 0x2001 && s[1] == 0x0db8)
        // 3fff::/20 documentation (RFC 9637).
        || (first == 0x3fff && s[1] < 0x1000)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(s: &str) -> bool {
        is_public_ip(&s.parse::<IpAddr>().unwrap_or_else(|e| panic!("{s}: {e}")))
    }

    /// (first, last, neighbour below, neighbour above) for every refused
    /// IPv4 block; a neighbour of `None` sits inside another refused block
    /// or outside the address space.
    const V4_BLOCKS: &[(&str, &str, Option<&str>, Option<&str>)] = &[
        ("0.0.0.0", "0.255.255.255", None, Some("1.0.0.0")),
        ("10.0.0.0", "10.255.255.255", Some("9.255.255.255"), Some("11.0.0.0")),
        ("100.64.0.0", "100.127.255.255", Some("100.63.255.255"), Some("100.128.0.0")),
        ("127.0.0.0", "127.255.255.255", Some("126.255.255.255"), Some("128.0.0.0")),
        ("169.254.0.0", "169.254.255.255", Some("169.253.255.255"), Some("169.255.0.0")),
        ("172.16.0.0", "172.31.255.255", Some("172.15.255.255"), Some("172.32.0.0")),
        ("192.0.0.0", "192.0.0.255", Some("191.255.255.255"), Some("192.0.1.0")),
        ("192.0.2.0", "192.0.2.255", Some("192.0.1.255"), Some("192.0.3.0")),
        ("192.168.0.0", "192.168.255.255", Some("192.167.255.255"), Some("192.169.0.0")),
        ("198.18.0.0", "198.19.255.255", Some("198.17.255.255"), Some("198.20.0.0")),
        ("198.51.100.0", "198.51.100.255", Some("198.51.99.255"), Some("198.51.101.0")),
        ("203.0.113.0", "203.0.113.255", Some("203.0.112.255"), Some("203.0.114.0")),
        ("224.0.0.0", "239.255.255.255", Some("223.255.255.255"), None),
        ("240.0.0.0", "255.255.255.255", None, None),
    ];

    const V6_BLOCKS: &[(&str, &str, Option<&str>, Option<&str>)] = &[
        // Everything below global unicast: ::/96 (::, ::1, IPv4-compatible),
        // the rest of ::/8, 100::/64 and 64:ff9b:1::/48 included.
        ("::", "1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None, Some("2000::")),
        ("::1", "::ffff:ffff", None, None),
        ("100::", "100::ffff:ffff:ffff:ffff", None, None),
        ("64:ff9b:1::", "64:ff9b:1:ffff:ffff:ffff:ffff:ffff", None, None),
        ("2001::", "2001:0:ffff:ffff:ffff:ffff:ffff:ffff", Some("2000:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), Some("2001:1::")),
        ("2001:db8::", "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", Some("2001:db7:ffff:ffff:ffff:ffff:ffff:ffff"), Some("2001:db9::")),
        ("3fff::", "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff", Some("3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), Some("3fff:1000::")),
        // Everything above global unicast: fc00::/7, fe80::/10, fec0::/10,
        // ff00::/8 included.
        ("4000::", "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", Some("3fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), None),
        ("fc00::", "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None, None),
        ("fe80::", "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None, None),
        ("fec0::", "feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None, None),
        ("ff00::", "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None, None),
    ];

    fn check(blocks: &[(&str, &str, Option<&str>, Option<&str>)]) {
        for (first, last, below, above) in blocks {
            assert!(!public(first), "{first} must be refused");
            assert!(!public(last), "{last} must be refused");
            for n in [below, above].into_iter().flatten() {
                assert!(public(n), "{n} (just outside {first}..{last}) must stay public");
            }
        }
    }

    #[test]
    fn every_refused_ipv4_block_first_last_and_neighbours() {
        check(V4_BLOCKS);
    }

    #[test]
    fn every_refused_ipv6_block_first_last_and_neighbours() {
        check(V6_BLOCKS);
    }

    #[test]
    fn ordinary_public_addresses_pass() {
        for ok in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:4700::1111", "2001:4860:4860::8888"] {
            assert!(public(ok), "{ok}");
        }
    }

    /// Embedded-IPv4 forms with a fixed position follow the embedded address
    /// across its whole range (first/last of the class + a public probe).
    #[test]
    fn embedded_ipv4_forms_follow_the_embedded_address() {
        for (prefix, refused, allowed) in [
            ("::ffff:", ["0.0.0.0", "127.0.0.1", "169.254.169.254", "10.0.0.1", "100.64.0.1", "255.255.255.255"], "1.1.1.1"),
            ("64:ff9b::", ["0.0.0.0", "127.0.0.1", "169.254.169.254", "10.0.0.1", "100.64.0.1", "255.255.255.255"], "1.1.1.1"),
        ] {
            for r in refused {
                assert!(!public(&format!("{prefix}{r}")), "{prefix}{r}");
            }
            assert!(public(&format!("{prefix}{allowed}")), "{prefix}{allowed}");
        }
        // Hex spellings of the same addresses.
        assert!(!public("::ffff:7f00:1"));
        assert!(!public("::ffff:a9fe:a9fe"));
        assert!(!public("64:ff9b::a9fe:a9fe"));
        // First/last of the mapped and NAT64 classes, and just outside them
        // (still refused: outside 2000::/3 there is nothing public).
        assert!(!public("::ffff:0:0") && !public("::ffff:ffff:ffff"));
        assert!(!public("64:ff9b::") && !public("64:ff9b::ffff:ffff"));
        assert!(!public("::fffe:ffff:ffff") && !public("::1:0:0:0"));
        assert!(!public("64:ff9b::1:0:0") && !public("64:ff9a:ffff:ffff:ffff:ffff:ffff:ffff"));
        // 6to4: 2002:AABB:CCDD:: embeds AA.BB.CC.DD.
        assert!(!public("2002:7f00:1::"));
        assert!(!public("2002:a9fe:a9fe::1"));
        assert!(!public("2002::"), "2002:0000:0000:: embeds 0.0.0.0");
        assert!(!public("2002:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), "embeds 255.255.255.255");
        assert!(public("2002:101:101::1"), "6to4 of 1.1.1.1");
        assert!(public("2001:ffff:ffff:ffff:ffff:ffff:ffff:ffff") && public("2003::"), "neighbours of 2002::/16");
    }
}
