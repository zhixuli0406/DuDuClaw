//! Where a remote MCP connection may reach.
//!
//! Every URL this feature dials — the MCP endpoint itself, the protected
//! resource metadata, the authorization server's metadata, registration,
//! authorize and token endpoints — passes [`check_outbound`] first and is then
//! dialled at addresses resolved and screened by [`resolve_pinned`], so the
//! address that was checked is the address that is used (no DNS rebinding
//! between check and connect).
//!
//! Rules:
//! - `https` only; plain `http` only when the host is loopback.
//! - No userinfo (`https://user:pass@host`), no fragment.
//! - Every resolved address must pass `duduclaw_core::net_addr::is_public_ip`
//!   (the single workspace-wide classifier). Private, link-local, CGNAT,
//!   metadata and similar addresses are refused, and so is a name that
//!   resolves to a mix of public and private addresses.
//! - Loopback (`localhost` exactly, `127.0.0.0/8`, `::1`) is allowed only when
//!   [`OutboundPolicy::allow_loopback`] is set, which happens only when the
//!   MCP URL the operator typed is itself a loopback URL (a server on this
//!   machine). A public server can therefore never steer the gateway to a
//!   loopback address through its metadata.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use url::{Host, Url};

/// Longest URL accepted anywhere in this feature.
pub const MAX_URL_LEN: usize = 2048;

/// What a connection derived from one MCP URL may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundPolicy {
    /// Loopback hosts allowed (the MCP server runs on this machine).
    pub allow_loopback: bool,
}

impl OutboundPolicy {
    /// Public internet only.
    pub const PUBLIC_ONLY: OutboundPolicy = OutboundPolicy { allow_loopback: false };

    /// The policy for every URL discovered from `mcp_url`: loopback only when
    /// `mcp_url` itself is loopback.
    pub fn for_mcp_url(mcp_url: &Url) -> Self {
        Self { allow_loopback: is_loopback_url(mcp_url) }
    }
}

/// `localhost` (exact, case-insensitive), `127.0.0.0/8` or `::1`.
pub fn is_loopback_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// Whether the URL's host is loopback.
pub fn is_loopback_url(url: &Url) -> bool {
    url.host().as_ref().is_some_and(is_loopback_host)
}

/// Parse and validate a URL an operator typed for a remote MCP server.
/// Same rules as [`check_outbound`] with loopback allowed (a local server is a
/// legitimate target when the operator names it).
pub fn validate_remote_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("URL is empty".into());
    }
    if raw.len() > MAX_URL_LEN {
        return Err(format!("URL is longer than {MAX_URL_LEN} bytes"));
    }
    let url = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    let policy = OutboundPolicy::for_mcp_url(&url);
    check_outbound(&url, policy)?;
    Ok(url)
}

/// Refuse a URL this connection may not dial (pattern level; addresses are
/// screened again by [`resolve_pinned`]).
pub fn check_outbound(url: &Url, policy: OutboundPolicy) -> Result<(), String> {
    if url.as_str().len() > MAX_URL_LEN {
        return Err(format!("URL is longer than {MAX_URL_LEN} bytes"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL must not contain a user name or password".into());
    }
    if url.fragment().is_some() {
        return Err("URL must not contain a fragment".into());
    }
    let host = url.host().ok_or_else(|| "URL has no host".to_string())?;
    let loopback = is_loopback_host(&host);
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => return Err("plain http is allowed only for a server on this machine; use https".into()),
        other => return Err(format!("unsupported URL scheme {other}://")),
    }
    if loopback {
        if !policy.allow_loopback {
            return Err("this address is on the gateway machine and is not allowed here".into());
        }
        return Ok(());
    }
    match host {
        Host::Ipv4(ip) if !duduclaw_core::net_addr::is_public_ip(&IpAddr::V4(ip)) => {
            Err(format!("address {ip} is not a public internet address"))
        }
        Host::Ipv6(ip) if !duduclaw_core::net_addr::is_public_ip(&IpAddr::V6(ip)) => {
            Err(format!("address {ip} is not a public internet address"))
        }
        Host::Domain(d) => {
            let lower = d.to_ascii_lowercase();
            // Names that are internal by definition; the resolver check would
            // refuse them too, this just gives a clearer message.
            if lower.ends_with(".localhost")
                || lower.ends_with(".local")
                || lower.ends_with(".internal")
                || lower == "metadata.google.internal"
            {
                return Err(format!("host {d} is not a public internet host"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The screening half of [`resolve_pinned`], pure so it can be tested
/// without a resolver: every address must be public, or (when allowed) every
/// address loopback for a loopback host. All-or-nothing.
pub fn vet_addrs(
    host_is_loopback: bool,
    policy: OutboundPolicy,
    addrs: &[SocketAddr],
) -> Result<(), String> {
    if addrs.is_empty() {
        return Err("the host name resolved to no address".into());
    }
    for a in addrs {
        let ip = a.ip();
        let ok = if host_is_loopback {
            policy.allow_loopback && ip.is_loopback()
        } else {
            duduclaw_core::net_addr::is_public_ip(&ip)
        };
        if !ok {
            return Err(format!(
                "the host name resolved to {ip}, which is not a public internet address"
            ));
        }
    }
    Ok(())
}

/// Resolve the URL's host now and return the screened addresses to pin.
/// An IP literal is returned as is (already screened by [`check_outbound`]).
pub async fn resolve_pinned(url: &Url, policy: OutboundPolicy) -> Result<Vec<SocketAddr>, String> {
    check_outbound(url, policy)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no port".to_string())?;
    let host = url.host().ok_or_else(|| "URL has no host".to_string())?;
    let loopback = is_loopback_host(&host);
    let addrs: Vec<SocketAddr> = match host {
        Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Host::Domain(d) => {
            let target = format!("{d}:{port}");
            tokio::task::spawn_blocking(move || {
                target
                    .to_socket_addrs()
                    .map(|it| it.collect::<Vec<_>>())
                    .map_err(|e| format!("could not resolve the host name: {e}"))
            })
            .await
            .map_err(|e| format!("resolver task failed: {e}"))??
        }
    };
    vet_addrs(loopback, policy, &addrs)?;
    Ok(addrs)
}

/// A client that dials exactly the screened addresses of `url`'s host, never
/// follows redirects (callers that accept a redirect re-check and re-pin the
/// next hop themselves) and gives up after `timeout`.
pub async fn pinned_client(
    url: &Url,
    policy: OutboundPolicy,
    timeout: std::time::Duration,
) -> Result<reqwest::Client, String> {
    let addrs = resolve_pinned(url, policy).await?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("DuDuClaw/", env!("CARGO_PKG_VERSION")));
    if let Some(Host::Domain(d)) = url.host() {
        builder = builder.resolve_to_addrs(d, &addrs);
    }
    builder.build().map_err(|e| format!("http client init failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn https_public_host_passes_and_plain_http_remote_is_refused() {
        assert!(validate_remote_url("https://mcp.example.com/mcp").is_ok());
        assert!(validate_remote_url("http://mcp.example.com/mcp").is_err());
        assert!(validate_remote_url("ftp://mcp.example.com/").is_err());
        assert!(validate_remote_url("").is_err());
    }

    #[test]
    fn userinfo_and_fragment_are_refused() {
        assert!(validate_remote_url("https://user:pw@mcp.example.com/mcp").is_err());
        assert!(validate_remote_url("https://user@mcp.example.com/mcp").is_err());
        assert!(validate_remote_url("https://mcp.example.com/mcp#x").is_err());
    }

    #[test]
    fn private_and_special_ip_literals_are_refused() {
        for bad in [
            "https://10.0.0.1/mcp",
            "https://192.168.1.10/mcp",
            "https://169.254.169.254/latest",
            "https://100.64.0.1/mcp",
            "https://[fd00::1]/mcp",
            "https://[::ffff:127.0.0.1]/mcp",
            "https://[::ffff:10.0.0.1]/mcp",
            "https://0.0.0.0/mcp",
            "https://metadata.google.internal/x",
            "https://printer.local/x",
        ] {
            assert!(validate_remote_url(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn loopback_is_allowed_only_under_a_loopback_policy() {
        // Operator-typed loopback URL: allowed (local server).
        assert!(validate_remote_url("http://127.0.0.1:9000/mcp").is_ok());
        assert!(validate_remote_url("http://localhost:9000/mcp").is_ok());
        assert!(validate_remote_url("http://[::1]:9000/mcp").is_ok());
        // A look-alike is a remote host and needs https.
        assert!(validate_remote_url("http://localhost.evil.com/mcp").is_err());
        // Discovered URL under a public policy: refused.
        let public = OutboundPolicy::PUBLIC_ONLY;
        assert!(check_outbound(&u("http://127.0.0.1:9000/token"), public).is_err());
        assert!(check_outbound(&u("https://localhost/token"), public).is_err());
        let local = OutboundPolicy::for_mcp_url(&u("http://127.0.0.1:9000/mcp"));
        assert!(local.allow_loopback);
        assert!(check_outbound(&u("http://127.0.0.1:9001/token"), local).is_ok());
        // Loopback policy never opens private ranges.
        assert!(check_outbound(&u("https://10.1.2.3/token"), local).is_err());
    }

    #[test]
    fn resolved_address_sets_are_all_or_nothing() {
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let private: SocketAddr = "10.0.0.5:443".parse().unwrap();
        let lo: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let p = OutboundPolicy::PUBLIC_ONLY;
        assert!(vet_addrs(false, p, &[public]).is_ok());
        assert!(vet_addrs(false, p, &[public, private]).is_err());
        assert!(vet_addrs(false, p, &[lo]).is_err());
        assert!(vet_addrs(false, p, &[]).is_err());
        // `localhost` resolving to loopback: only with the loopback policy.
        assert!(vet_addrs(true, p, &[lo]).is_err());
        let l = OutboundPolicy { allow_loopback: true };
        assert!(vet_addrs(true, l, &[lo]).is_ok());
        // A non-loopback name resolving to loopback is refused even then.
        assert!(vet_addrs(false, l, &[lo]).is_err());
    }
}
