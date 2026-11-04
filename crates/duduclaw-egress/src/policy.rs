//! IANA special-purpose registries (2025-10-09 snapshot), narrowed by design r7.
//! Translation prefixes are denied even when globally reachable in the registry.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use duduclaw_isolation::egress::{ConnectTarget, EgressError, EgressProfile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host { Ip(IpAddr), Dns(String) }

pub fn normalize_host(host: &str) -> Result<Host, EgressError> {
    if let Ok(ip) = host.parse::<IpAddr>() { return Ok(Host::Ip(ip)); }
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || name.len() > 253 || !name.is_ascii() { return Err(EgressError::InvalidRequest); }
    let labels: Vec<_> = name.split('.').collect();
    for label in &labels {
        if label.is_empty() || label.len() > 63
            || !label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            || !label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(EgressError::InvalidRequest);
        }
    }
    // Never pass inet_aton-style octal/hex/short forms to a resolver.
    let last = labels.last().ok_or(EgressError::InvalidRequest)?;
    if last.bytes().all(|b| b.is_ascii_digit())
        || last.strip_prefix("0x").or_else(|| last.strip_prefix("0X"))
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(EgressError::InvalidRequest);
    }
    Ok(Host::Dns(name.to_ascii_lowercase()))
}

fn v4_in(ip: Ipv4Addr, base: [u8; 4], prefix: u32) -> bool {
    let mask = u32::MAX << (32 - prefix);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(base)) & mask
}
fn v6_in(ip: Ipv6Addr, base: [u16; 8], prefix: u32) -> bool {
    let mask = u128::MAX << (128 - prefix);
    u128::from(ip) & mask == u128::from(Ipv6Addr::from(base)) & mask
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            if matches!(ip.octets(), [192, 0, 0, 9 | 10]) { return true; }
            ![
                ([0,0,0,0],8), ([10,0,0,0],8), ([100,64,0,0],10),
                ([127,0,0,0],8), ([169,254,0,0],16), ([172,16,0,0],12),
                ([192,0,0,0],24), ([192,0,2,0],24), ([192,88,99,0],24),
                ([192,168,0,0],16), ([198,18,0,0],15), ([198,51,100,0],24),
                ([203,0,113,0],24), ([224,0,0,0],4), ([240,0,0,0],4),
            ].into_iter().any(|(base, prefix)| v4_in(ip, base, prefix))
        }
        IpAddr::V6(ip) => {
            // All currently public unicast allocations are within 2000::/3.
            // This also rejects mapped/compatible, NAT64, ULA, site/link-local,
            // multicast, discard-only and unallocated future address space.
            if !v6_in(ip, [0x2000,0,0,0,0,0,0,0], 3) { return false; }
            if v6_in(ip, [0x2001,0,0,0,0,0,0,0], 23) {
                let s = ip.segments();
                return matches!(s, [0x2001,1,0,0,0,0,0,1..=3])
                    || v6_in(ip, [0x2001,3,0,0,0,0,0,0],32)
                    || v6_in(ip, [0x2001,4,0x112,0,0,0,0,0],48)
                    || v6_in(ip, [0x2001,0x20,0,0,0,0,0,0],28)
                    || v6_in(ip, [0x2001,0x30,0,0,0,0,0,0],28);
            }
            !v6_in(ip, [0x2001,0xdb8,0,0,0,0,0,0],32)
                && !v6_in(ip, [0x2002,0,0,0,0,0,0,0],16)
                && !v6_in(ip, [0x3fff,0,0,0,0,0,0,0],20)
        }
    }
}

/// The caller connects these numeric SocketAddrs directly, never a hostname.
pub fn authorize_answers(target: &ConnectTarget, profile: &EgressProfile, answers: Vec<IpAddr>)
    -> Result<Vec<SocketAddr>, EgressError> {
    profile.validate().map_err(|_| EgressError::Unavailable)?;
    target.validate().map_err(|_| EgressError::InvalidRequest)?;
    if !profile.allowed_connect_ports.as_slice().contains(&target.port) { return Err(EgressError::PortDenied); }
    if answers.is_empty() || answers.len() > 64 { return Err(EgressError::Unavailable); }
    if answers.iter().any(|ip| !is_public_ip(*ip)) { return Err(EgressError::DestinationDenied); }
    let mut pinned: Vec<_> = answers.into_iter().map(|ip| SocketAddr::new(ip, target.port.get())).collect();
    pinned.sort_unstable();
    pinned.dedup();
    Ok(pinned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_classification_table() {
        for text in ["0.0.0.0", "0.255.255.255", "10.0.0.0", "10.255.255.255", "100.64.0.0", "100.127.255.255",
            "127.0.0.1", "127.255.255.255", "169.254.169.254", "172.16.0.0", "172.31.255.255",
            "192.0.0.0", "192.0.0.8", "192.0.0.11", "192.0.0.170", "192.0.0.171", "192.0.0.255",
            "192.0.2.1", "192.88.99.1", "192.88.99.2", "192.168.1.1", "198.18.0.0", "198.19.255.255",
            "198.51.100.1", "203.0.113.1", "224.0.0.0", "239.255.255.255", "240.0.0.0", "255.255.255.255",
            "::", "::1", "::127.0.0.1", "::ffff:8.8.8.8", "::ffff:127.0.0.1", "64:ff9b::808:808",
            "64:ff9b:1::1", "100::1", "100:0:0:1::1", "2001::1", "2001:2::1", "2001:10::1",
            "2001:1::4", "2001:1ff:ffff::1", "2001:db8::1", "2002:808:808::1", "3fff::1", "3fff:fff::1",
            "5f00::1", "fc00::1", "fd00:ec2::254", "fe80::1", "fec0::1", "ff02::1"] {
            assert!(!is_public_ip(text.parse().unwrap()), "{text}");
        }
        for text in ["1.1.1.1", "8.8.8.8", "100.63.255.255", "100.128.0.0", "172.15.255.255", "172.32.0.0",
            "192.0.0.9", "192.0.0.10", "192.31.196.1", "192.52.193.1", "192.175.48.1", "198.17.255.255",
            "198.20.0.0", "223.255.255.255", "2001:1::1", "2001:1::2", "2001:1::3", "2001:3::1",
            "2001:4:112::1", "2001:20::1", "2001:30::1", "2001:200::1", "2001:4860:4860::8888",
            "2606:4700:4700::1111", "2620:4f:8000::1", "3fff:1000::1"] {
            assert!(is_public_ip(text.parse().unwrap()), "{text}");
        }
    }
    #[test]
    fn normalize_refuses_ambiguous_and_injected_hosts() {
        for host in ["127.1", "2130706433", "0177.0.0.1", "0x7f000001", "0x7f.0.0.1", "1.2.3.999",
            "127.0.0.1.", "[::1]", "fe80::1%eth0", "user@example.com", "a/b", "a\\b", "a\r\n", "a..b", "-a.com", "a_.com", "例子.com", "vsock:3:80"] {
            assert!(normalize_host(host).is_err(), "{host}");
        }
        assert_eq!(normalize_host("Example.COM."), Ok(Host::Dns("example.com".into())));
        assert!(matches!(normalize_host("2001:4860::1"), Ok(Host::Ip(_))));
    }
}
