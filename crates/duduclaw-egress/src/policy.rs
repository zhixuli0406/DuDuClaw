//! IANA special-purpose registries (2025-10-09 snapshot), narrowed by design r7.
//! Translation prefixes are denied even when globally reachable in the registry.
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
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
///
/// SR5 item 7: an answer equal to one of the host's own interface addresses
/// (a public address of this machine reaches its own listeners) is denied
/// too ([`host_addresses`]; an unknown host set refuses).
pub fn authorize_answers(target: &ConnectTarget, profile: &EgressProfile, answers: Vec<IpAddr>)
    -> Result<Vec<SocketAddr>, EgressError> {
    let own = host_addresses().current(std::time::Instant::now())?;
    authorize_answers_with(target, profile, answers, &own)
}

/// [`authorize_answers`] against an explicit set of the host's own addresses.
pub fn authorize_answers_with(target: &ConnectTarget, profile: &EgressProfile, answers: Vec<IpAddr>,
    own: &BTreeSet<IpAddr>) -> Result<Vec<SocketAddr>, EgressError> {
    profile.validate().map_err(|_| EgressError::Unavailable)?;
    target.validate().map_err(|_| EgressError::InvalidRequest)?;
    if !profile.allowed_connect_ports.as_slice().contains(&target.port) { return Err(EgressError::PortDenied); }
    if answers.is_empty() || answers.len() > 64 { return Err(EgressError::Unavailable); }
    if answers.iter().any(|ip| !is_public_ip(*ip) || own.contains(&canonical(*ip))) {
        return Err(EgressError::DestinationDenied);
    }
    let mut pinned: Vec<_> = answers.into_iter().map(|ip| SocketAddr::new(ip, target.port.get())).collect();
    pinned.sort_unstable();
    pinned.dedup();
    Ok(pinned)
}

// ── SR5 item 7: the host's own interface addresses ───────────────────────

/// An IPv4-mapped IPv6 address compares as its IPv4 address.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

/// Where the host's interface addresses come from (tests inject a stub).
pub trait HostAddressSource: Send + Sync {
    fn addresses(&self) -> Result<Vec<IpAddr>, EgressError>;
}

/// How often the set is re-enumerated.
pub const HOST_ADDRESS_REFRESH: Duration = Duration::from_secs(30);
/// The longest a snapshot is served while re-enumeration keeps failing;
/// after that every connect is refused (fail closed).
pub const HOST_ADDRESS_MAX_STALE: Duration = Duration::from_secs(600);

/// The host's own addresses, enumerated at start ([`init_host_addresses`],
/// before the seccomp filter) and refreshed every [`HOST_ADDRESS_REFRESH`].
pub struct HostAddresses<S: HostAddressSource> {
    source: S,
    refresh: Duration,
    max_stale: Duration,
    /// `(enumerated at, addresses)` of the last successful enumeration.
    state: Mutex<Option<(Instant, Arc<BTreeSet<IpAddr>>)>>,
}

impl<S: HostAddressSource> HostAddresses<S> {
    pub fn new(source: S, refresh: Duration, max_stale: Duration) -> Self {
        Self { source, refresh, max_stale, state: Mutex::new(None) }
    }

    /// Enumerate now. A failure leaves the previous snapshot in place.
    pub fn refresh_at(&self, now: Instant) -> Result<Arc<BTreeSet<IpAddr>>, EgressError> {
        let addresses: BTreeSet<IpAddr> = self.source.addresses()?.into_iter().map(canonical).collect();
        let addresses = Arc::new(addresses);
        *self.state.lock().map_err(|_| EgressError::Unavailable)? = Some((now, addresses.clone()));
        Ok(addresses)
    }

    /// The current set: re-enumerated when older than the refresh interval;
    /// if that fails, the last snapshot until it is `max_stale` old, then
    /// `Unavailable`. Never enumerated successfully: `Unavailable`.
    pub fn current(&self, now: Instant) -> Result<Arc<BTreeSet<IpAddr>>, EgressError> {
        let last = self.state.lock().map_err(|_| EgressError::Unavailable)?.clone();
        match last {
            Some((at, addresses)) if now.saturating_duration_since(at) < self.refresh => Ok(addresses),
            Some((at, addresses)) => match self.refresh_at(now) {
                Ok(fresh) => Ok(fresh),
                Err(_) if now.saturating_duration_since(at) < self.max_stale => Ok(addresses),
                Err(_) => Err(EgressError::Unavailable),
            },
            None => self.refresh_at(now).map_err(|_| EgressError::Unavailable),
        }
    }
}

/// The production source: IPv4 through `SIOCGIFCONF` on an `AF_INET`
/// socket and IPv6 from `/proc/net/if_inet6` — both work under the egress
/// seccomp filter (no netlink). Linux only; elsewhere (development hosts,
/// no egress daemon) the set is empty.
pub struct SystemInterfaces;

impl HostAddressSource for SystemInterfaces {
    fn addresses(&self) -> Result<Vec<IpAddr>, EgressError> {
        #[cfg(target_os = "linux")]
        {
            let mut out = interfaces::ipv4()?;
            out.extend(interfaces::ipv6()?);
            Ok(out)
        }
        #[cfg(not(target_os = "linux"))]
        { Ok(Vec::new()) }
    }
}

/// The process-wide registry used by [`authorize_answers`].
pub fn host_addresses() -> &'static HostAddresses<SystemInterfaces> {
    static HOST: OnceLock<HostAddresses<SystemInterfaces>> = OnceLock::new();
    HOST.get_or_init(|| HostAddresses::new(SystemInterfaces, HOST_ADDRESS_REFRESH, HOST_ADDRESS_MAX_STALE))
}

/// Enumerate the host's addresses at daemon start (before seccomp). A host
/// whose interfaces cannot be read refuses to start.
pub fn init_host_addresses() -> Result<(), EgressError> {
    host_addresses().refresh_at(Instant::now()).map(|_| ())
}

#[cfg(target_os = "linux")]
mod interfaces {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// `struct ifreq`: `IFNAMSIZ` name + the largest union member
    /// (`struct ifmap`: two `unsigned long` + 5 bytes, padded).
    fn ifreq_size() -> usize {
        let word = std::mem::size_of::<libc::c_ulong>();
        let ifmap = (2 * word + 5).div_ceil(word) * word;
        16 + ifmap.max(16)
    }

    #[repr(C)]
    struct IfConf {
        len: libc::c_int,
        buf: *mut u8,
    }

    pub(super) fn ipv4() -> Result<Vec<IpAddr>, EgressError> {
        // SAFETY: plain syscall; the descriptor is owned right away.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if raw < 0 { return Err(EgressError::Unavailable); }
        // SAFETY: `raw` is a fresh descriptor we own.
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let size = ifreq_size();
        let mut buffer = vec![0u8; size * 256];
        let mut conf = IfConf { len: libc::c_int::try_from(buffer.len()).map_err(|_| EgressError::Unavailable)?,
            buf: buffer.as_mut_ptr() };
        // SAFETY: `conf` points at `buffer`, which outlives the call; the
        // kernel writes at most `conf.len` bytes and updates `len`.
        if unsafe { libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFCONF as _, &mut conf as *mut IfConf) } < 0 {
            return Err(EgressError::Unavailable);
        }
        let used = usize::try_from(conf.len).map_err(|_| EgressError::Unavailable)?.min(buffer.len());
        let mut out = Vec::new();
        for entry in buffer[..used].chunks_exact(size) {
            // sockaddr at offset 16: family (native u16), port, then sin_addr.
            let family = u16::from_ne_bytes([entry[16], entry[17]]);
            if i32::from(family) == libc::AF_INET {
                out.push(IpAddr::V4(Ipv4Addr::new(entry[20], entry[21], entry[22], entry[23])));
            }
        }
        Ok(out)
    }

    pub(super) fn ipv6() -> Result<Vec<IpAddr>, EgressError> {
        let text = match std::fs::read_to_string("/proc/net/if_inet6") {
            Ok(text) => text,
            // No IPv6 on this host.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(EgressError::Unavailable),
        };
        parse_if_inet6(&text)
    }
}

/// `/proc/net/if_inet6` lines: 32 hex digits, then index, prefix, scope,
/// flags and name. A malformed line refuses (never a partial set).
pub fn parse_if_inet6(text: &str) -> Result<Vec<IpAddr>, EgressError> {
    text.lines().filter(|line| !line.trim().is_empty()).map(|line| {
        let hex = line.split_whitespace().next().ok_or(EgressError::Unavailable)?;
        if hex.len() != 32 { return Err(EgressError::Unavailable); }
        let value = u128::from_str_radix(hex, 16).map_err(|_| EgressError::Unavailable)?;
        Ok(IpAddr::V6(Ipv6Addr::from(value)))
    }).collect()
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
    /// A stub interface source: the next answers, or a failure when empty.
    struct Stub(Mutex<Vec<Result<Vec<IpAddr>, EgressError>>>);
    impl HostAddressSource for Stub {
        fn addresses(&self) -> Result<Vec<IpAddr>, EgressError> {
            let mut queue = self.0.lock().unwrap();
            if queue.is_empty() { Err(EgressError::Unavailable) } else { queue.remove(0) }
        }
    }
    fn ip(text: &str) -> IpAddr { text.parse().unwrap() }
    fn profile() -> EgressProfile {
        use duduclaw_isolation::{bounded::List, egress::{DnsRoute, ProfileId}, ids::Sha256Digest};
        EgressProfile { id: ProfileId(Sha256Digest([1; 32])), allowed_connect_ports: List::new(vec![443.try_into().unwrap()]).unwrap(),
            dns_route: DnsRoute::ResolvedVarlink, limits: crate::budget::default_limits() }
    }
    fn target() -> ConnectTarget {
        ConnectTarget { host: duduclaw_isolation::bounded::Text::new("example.com".into()).unwrap(), port: 443.try_into().unwrap() }
    }

    #[test]
    fn sr5_the_hosts_own_public_addresses_are_denied() {
        let own: BTreeSet<IpAddr> = [ip("203.0.114.7"), ip("2606:4700::7")].into();
        for answer in ["203.0.114.7", "::ffff:203.0.114.7", "2606:4700::7"] {
            assert_eq!(authorize_answers_with(&target(), &profile(), vec![ip("8.8.8.8"), ip(answer)], &own),
                Err(EgressError::DestinationDenied), "{answer}");
        }
        assert_eq!(authorize_answers_with(&target(), &profile(), vec![ip("8.8.8.8")], &own),
            Ok(vec!["8.8.8.8:443".parse().unwrap()]));
    }

    #[test]
    fn sr5_host_addresses_refresh_and_fail_closed() {
        let start = Instant::now();
        let refresh = Duration::from_secs(30);
        let stale = Duration::from_secs(600);
        // Never enumerated: refused.
        let never = HostAddresses::new(Stub(Mutex::new(vec![])), refresh, stale);
        assert_eq!(never.current(start).err(), Some(EgressError::Unavailable));
        // Enumerated at start, refreshed after the interval (a new address is seen).
        let host = HostAddresses::new(Stub(Mutex::new(vec![Ok(vec![ip("203.0.114.7")]),
            Ok(vec![ip("203.0.114.7"), ip("::ffff:198.51.101.9")])])), refresh, stale);
        assert!(host.refresh_at(start).unwrap().contains(&ip("203.0.114.7")));
        assert_eq!(host.current(start + Duration::from_secs(5)).unwrap().len(), 1, "cached inside the interval");
        let later = host.current(start + Duration::from_secs(31)).unwrap();
        assert!(later.contains(&ip("198.51.101.9")), "mapped addresses compare as IPv4");
        // Re-enumeration now fails: the last snapshot is served until it is too old.
        assert!(host.current(start + Duration::from_secs(120)).unwrap().contains(&ip("198.51.101.9")));
        assert_eq!(host.current(start + Duration::from_secs(31) + stale).err(), Some(EgressError::Unavailable));
    }

    #[test]
    fn sr5_if_inet6_parsing_is_strict() {
        let text = "20010db8000000000000000000000001 02 40 00 80 eth0\nfe800000000000000000000000000001 02 40 20 80 eth0\n";
        assert_eq!(parse_if_inet6(text).unwrap(), vec![ip("2001:db8::1"), ip("fe80::1")]);
        assert_eq!(parse_if_inet6("").unwrap(), vec![]);
        assert!(parse_if_inet6("zz 02 40 00 80 eth0").is_err());
        assert!(parse_if_inet6("2001 02").is_err());
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
