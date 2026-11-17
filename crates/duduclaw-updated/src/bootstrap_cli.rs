//! `duduclaw-bootstrap` argv (plan R1). Pure and platform-neutral so it is tested
//! everywhere. Arguments are compared as `OsStr` against fixed literals: non-UTF-8
//! input is a usage error, never a panic, and nothing is ever converted lossily.
use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};

/// The whole command line after argv[0]. Exactly one fixed word is accepted;
/// only `install` may be followed by the typed options of [`Forwarded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `--print-root-pin`: read-only, prints the compiled pin kind and digest.
    PrintRootPin,
    /// `--version`: read-only, prints [`version_line`].
    Version,
    Install,
    Upgrade,
    Rollback,
    /// Anything else, including extra, empty or non-UTF-8 arguments.
    Usage,
}

impl Command {
    /// Install/upgrade/rollback change root state; the caller hardens before any
    /// other work once one of them is selected.
    pub fn changes_state(self) -> bool { matches!(self, Self::Install | Self::Upgrade | Self::Rollback) }
}

/// Upper bound of `--host-public-address` values (the egress service's own
/// bound, `duduclaw_egress::policy::MAX_CONFIGURED_HOST_ADDRESSES`).
pub const MAX_HOST_PUBLIC_ADDRESSES: usize = 64;

/// The three typed values `duduclaw-bootstrap install` hands to the verified
/// gateway's `service bootstrap-continue` (plan §4.3). Never a path: each value
/// is parsed into an address type here and rendered again from that type, so
/// the forwarded argv never contains the caller's text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forwarded {
    /// The single DNS upstream of the dedicated DNS broker (hosts without systemd-resolved).
    pub dns_upstream: Option<SocketAddr>,
    /// Public addresses of this host that are on no interface (cloud 1:1 NAT).
    pub host_public_addresses: Vec<IpAddr>,
    /// Start the gateway after the install instead of only enabling it.
    pub enable_now: bool,
}

impl Forwarded {
    /// The canonical argv tail for `service bootstrap-continue`.
    pub fn args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(upstream) = self.dns_upstream {
            out.push("--dns-upstream".to_owned());
            out.push(upstream.to_string());
        }
        for address in &self.host_public_addresses {
            out.push("--host-public-address".to_owned());
            out.push(address.to_string());
        }
        if self.enable_now { out.push("--enable-now".to_owned()); }
        out
    }
}

/// Parse `std::env::args_os().skip(1)`; only the command (see [`parse_invocation`]).
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Command { parse_invocation(args).0 }

/// Parse `std::env::args_os().skip(1)` into the command and, for `install` only,
/// its forwarded options. Any unknown, repeated, empty, non-UTF-8 or malformed
/// item makes the whole invocation a usage error (with no options). At most
/// `2 * MAX_HOST_PUBLIC_ADDRESSES + 4` items are pulled, so an oversized argv is
/// not collected and an unbounded iterator terminates.
pub fn parse_invocation<I: IntoIterator<Item = OsString>>(args: I) -> (Command, Forwarded) {
    let usage = (Command::Usage, Forwarded::default());
    let mut args = args.into_iter();
    let Some(word) = args.next() else { return usage };
    let command = if word == *"--print-root-pin" {
        Command::PrintRootPin
    } else if word == *"--version" {
        Command::Version
    } else if word == *"install" {
        Command::Install
    } else if word == *"upgrade" {
        Command::Upgrade
    } else if word == *"rollback" {
        Command::Rollback
    } else {
        return usage;
    };
    if command != Command::Install {
        return if args.next().is_none() { (command, Forwarded::default()) } else { usage };
    }
    let mut forwarded = Forwarded::default();
    let mut pulled = 1usize;
    while let Some(flag) = args.next() {
        pulled += 1;
        if pulled > 2 * MAX_HOST_PUBLIC_ADDRESSES + 4 { return usage; }
        if flag == *"--enable-now" {
            if forwarded.enable_now { return usage; }
            forwarded.enable_now = true;
            continue;
        }
        let Some(value) = args.next() else { return usage };
        pulled += 1;
        let Some(value) = value.to_str() else { return usage };
        if flag == *"--dns-upstream" {
            if forwarded.dns_upstream.is_some() { return usage; }
            let Ok(upstream) = value.parse::<SocketAddr>() else { return usage };
            forwarded.dns_upstream = Some(upstream);
        } else if flag == *"--host-public-address" {
            let Ok(address) = value.parse::<IpAddr>() else { return usage };
            if forwarded.host_public_addresses.contains(&address)
                || forwarded.host_public_addresses.len() >= MAX_HOST_PUBLIC_ADDRESSES { return usage; }
            forwarded.host_public_addresses.push(address);
        } else {
            return usage;
        }
    }
    (command, forwarded)
}

/// `--version` output: the crate (= workspace release) version. The packaging
/// script requires it to equal the version being packaged.
pub fn version_line() -> String { format!("duduclaw-bootstrap {}", env!("CARGO_PKG_VERSION")) }

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> { list.iter().map(OsString::from).collect() }

    #[test]
    fn argv_is_fixed_words_only() {
        assert_eq!(parse(args(&["--print-root-pin"])), Command::PrintRootPin);
        assert_eq!(parse(args(&["--version"])), Command::Version);
        assert_eq!(parse(args(&["install"])), Command::Install);
        assert_eq!(parse(args(&["upgrade"])), Command::Upgrade);
        assert_eq!(parse(args(&["rollback"])), Command::Rollback);
        let usage: Vec<Vec<&str>> = vec![vec![], vec![""], vec!["INSTALL"], vec!["install "], vec![" install"],
            vec!["install\0"], vec!["--install"], vec!["install", "upgrade"], vec!["install", ""],
            vec!["--print-root-pin", "x"], vec!["--version", "--version"], vec!["-V"], vec!["--print-root-pin=1"],
            vec!["ｉnstall"], vec!["help"]];
        for bad in &usage {
            assert_eq!(parse(args(bad)), Command::Usage, "{bad:?}");
        }
        assert!(Command::Install.changes_state() && Command::Upgrade.changes_state() && Command::Rollback.changes_state());
        for read_only in [Command::PrintRootPin, Command::Version, Command::Usage] { assert!(!read_only.changes_state()); }
        assert_eq!(version_line(), format!("duduclaw-bootstrap {}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn install_forwards_only_typed_options_rendered_canonically() {
        let (command, forwarded) = parse_invocation(args(&["install", "--dns-upstream", "192.0.2.53:53",
            "--host-public-address", "203.0.113.7", "--host-public-address", "2001:db8::7", "--enable-now"]));
        assert_eq!(command, Command::Install);
        assert_eq!(forwarded.args(), ["--dns-upstream", "192.0.2.53:53", "--host-public-address", "203.0.113.7",
            "--host-public-address", "2001:db8::7", "--enable-now"]);
        assert_eq!(parse_invocation(args(&["install"])).1, Forwarded::default());
        assert!(Forwarded::default().args().is_empty());
        // Non-canonical but valid input is rendered from the parsed type.
        let (_, forwarded) = parse_invocation(args(&["install", "--host-public-address", "2001:0db8:0:0:0:0:0:0007"]));
        assert_eq!(forwarded.args(), ["--host-public-address", "2001:db8::7"]);
        let bad: Vec<Vec<&str>> = vec![
            vec!["install", "--dns-upstream"], vec!["install", "--dns-upstream", "resolver.example:53"],
            vec!["install", "--dns-upstream", "192.0.2.53"], vec!["install", "--dns-upstream", "/etc/resolv.conf"],
            vec!["install", "--dns-upstream", "192.0.2.53:53", "--dns-upstream", "192.0.2.54:53"],
            vec!["install", "--host-public-address", "203.0.113.7", "--host-public-address", "203.0.113.7"],
            vec!["install", "--host-public-address", "203.0.113.0/24"], vec!["install", "--enable-now", "--enable-now"],
            vec!["install", "--enable-now=yes"], vec!["install", "--path", "/tmp"],
            vec!["upgrade", "--enable-now"], vec!["rollback", "--dns-upstream", "192.0.2.53:53"],
            vec!["--version", "--enable-now"]];
        for bad in &bad {
            assert_eq!(parse_invocation(args(bad)), (Command::Usage, Forwarded::default()), "{bad:?}");
        }
        let mut many = vec!["install".to_owned()];
        for index in 0..=MAX_HOST_PUBLIC_ADDRESSES {
            many.push("--host-public-address".to_owned());
            many.push(format!("198.51.100.{index}"));
        }
        assert_eq!(parse_invocation(many.into_iter().map(OsString::from)).0, Command::Usage);
        // An unbounded option stream terminates.
        assert_eq!(parse_invocation(std::iter::repeat(OsString::from("--enable-now"))).0, Command::Usage);
        let endless = std::iter::once(OsString::from("install")).chain(std::iter::repeat(OsString::from("--enable-now")));
        assert_eq!(parse_invocation(endless).0, Command::Usage);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_argv_is_a_usage_error_not_a_panic() {
        use std::os::unix::ffi::OsStringExt;
        for bytes in [&b"\xff"[..], b"install\xff", b"\xc3\x28", b"--version\x80"] {
            assert_eq!(parse(vec![OsString::from_vec(bytes.to_vec())]), Command::Usage);
            assert_eq!(parse(vec![OsString::from("install"), OsString::from_vec(bytes.to_vec())]), Command::Usage);
            assert_eq!(parse(vec![OsString::from_vec(bytes.to_vec()), OsString::from("install")]), Command::Usage);
            assert_eq!(parse(vec![OsString::from("install"), OsString::from("--dns-upstream"),
                OsString::from_vec(bytes.to_vec())]), Command::Usage);
        }
        // Only a bounded number of items is ever pulled, so an unbounded iterator terminates.
        assert_eq!(parse(std::iter::repeat(OsString::from("install"))), Command::Usage);
    }
}
