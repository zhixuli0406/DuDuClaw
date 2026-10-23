//! SSRF / scheme validation for Odoo server URLs and database names.
//!
//! Single implementation shared by the gateway write path (`odoo.configure`,
//! `odoo.agent_config_set`), the connectivity tests, the per-agent runtime
//! merge in [`crate::AgentOdooConfig::apply_to`] and the connector itself, so
//! a URL that the dashboard refuses can never be dialed at runtime and vice
//! versa.
//!
//! Two modes, selected by the admin-only `[odoo] allow_private_network`:
//!
//! * **off (default)** — HTTPS to a public host; plain `http://` only for
//!   strict localhost.
//! * **on** — internal (RFC 1918 / ULA / CGNAT …) addresses are allowed over
//!   HTTPS or HTTP, but loopback (except the strict-localhost HTTP exception),
//!   link-local, unspecified/multicast and cloud-metadata addresses and names
//!   stay refused.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Why a URL was refused, as text an operator can act on.
const MSG_PRIVATE: &str = "這個 Odoo 網址指向內網位址，預設不允許。請管理員在本頁開啟「允許連到內網的 Odoo」後再試";
const MSG_UNSAFE: &str = "Odoo 網址不被允許：必須是 HTTPS（只有 localhost／127.0.0.1 可用 http://），不能帶帳號密碼，也不能指向本機、保留位址或雲端 metadata 位址";

/// Names that are never allowed in any mode (compared without a trailing dot).
fn is_always_denied_name(lower: &str) -> bool {
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
        || lower == "metadata.azure.internal"
}

/// Addresses refused in every mode: loopback, unspecified, multicast,
/// broadcast, link-local (covers 169.254.169.254) and known metadata
/// endpoints. IPv4-mapped IPv6 is judged by the embedded IPv4.
fn is_always_denied_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_always_denied_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_always_denied_v4(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || v6 == "fd00:ec2::254".parse::<Ipv6Addr>().unwrap_or(Ipv6Addr::UNSPECIFIED)
        }
    }
}

fn is_always_denied_v4(v4: Ipv4Addr) -> bool {
    v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_link_local()
        || v4 == Ipv4Addr::new(100, 100, 100, 200) // Alibaba Cloud metadata
}

/// Validate an Odoo URL. `Err` carries a sentence an operator can act on.
pub fn check_url(url: &str, allow_private: bool) -> Result<(), String> {
    if url.len() > 512 {
        return Err(MSG_UNSAFE.into());
    }
    let parsed = url::Url::parse(url).map_err(|_| MSG_UNSAFE.to_string())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MSG_UNSAFE.into());
    }
    let host = parsed.host_str().ok_or_else(|| MSG_UNSAFE.to_string())?;
    if is_local_http(&parsed) {
        return Ok(());
    }
    match parsed.scheme() {
        "https" => {}
        // Plain HTTP is only for strict localhost (handled above) or, with
        // the admin switch on, an internal host.
        "http" if allow_private => {}
        // Plain HTTP to an internal address is also a "switch" matter.
        "http" if is_private_literal(host) => return Err(MSG_PRIVATE.into()),
        "http" => return Err(MSG_UNSAFE.into()),
        _ => return Err(MSG_UNSAFE.into()),
    }
    let raw = host.trim_start_matches('[').trim_end_matches(']');
    // Bare IPv6 without brackets is ambiguous.
    if !host.starts_with('[') && raw.contains(':') {
        return Err(MSG_UNSAFE.into());
    }
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    if is_always_denied_name(&lower) {
        return Err(MSG_UNSAFE.into());
    }
    if let Ok(ip) = raw.parse::<IpAddr>() {
        if is_always_denied_ip(ip) {
            return Err(MSG_UNSAFE.into());
        }
        if !allow_private && is_private_ip(ip) {
            return Err(MSG_PRIVATE.into());
        }
        // Cleartext HTTP is only for internal hosts, never a public address.
        if parsed.scheme() == "http" && !is_private_ip(ip) {
            return Err(MSG_UNSAFE.into());
        }
    }
    Ok(())
}

/// A private (non-public, not always-denied) IP literal host.
fn is_private_literal(host: &str) -> bool {
    let raw = host.trim_start_matches('[').trim_end_matches(']');
    raw.parse::<IpAddr>()
        .is_ok_and(|ip| !is_always_denied_ip(ip) && is_private_ip(ip))
}

/// Boolean form of [`check_url`] with the admin switch **off** (the strict
/// default).
pub fn is_safe_odoo_url(url: &str) -> bool {
    check_url(url, false).is_ok()
}

/// Check if a hostname is a private/reserved IP or a known metadata endpoint
/// (strict mode).
pub fn is_private_host(host: &str) -> bool {
    let raw = host.trim_start_matches('[').trim_end_matches(']');

    // Bare IPv6 without brackets (contains ':' but no '[') is ambiguous.
    if !host.starts_with('[') && raw.contains(':') {
        return true;
    }

    if let Ok(ip) = raw.parse::<IpAddr>() {
        return is_private_ip(ip);
    }

    // A trailing dot is the same DNS name ("localhost." == "localhost").
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    is_always_denied_name(&lower)
}

/// Private, loopback, link-local or otherwise non-public address
/// ([`duduclaw_core::net_addr::is_public_ip`]).
pub fn is_private_ip(ip: IpAddr) -> bool {
    !duduclaw_core::net_addr::is_public_ip(&ip)
}

/// Host (no scheme, path, query or port) of a URL, for log lines that must
/// not carry a query string. Falls back to a fixed placeholder.
pub fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "<invalid-url>".to_string())
}

/// Plain-HTTP strict-localhost URL (the only non-HTTPS shape accepted with the
/// admin switch off).
fn is_local_http(parsed: &url::Url) -> bool {
    parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// Check every resolved address. Strict mode: all must be public. Switch on:
/// none may be loopback/link-local/metadata (private ranges are fine).
pub fn check_resolved_addrs(addrs: &[SocketAddr], allow_private: bool) -> Result<(), String> {
    if addrs.is_empty() {
        return Err("Odoo 主機名稱解析不到任何位址".into());
    }
    for a in addrs {
        if is_always_denied_ip(a.ip()) {
            return Err("Odoo 主機名稱解析到本機、link-local 或雲端 metadata 位址，已拒絕連線".into());
        }
        if !allow_private && is_private_ip(a.ip()) {
            return Err(MSG_PRIVATE.into());
        }
    }
    Ok(())
}

/// Resolve `host:port` and check every address (see [`check_resolved_addrs`]).
/// Returns the checked addresses so the caller can pin them (no second lookup,
/// which would reopen a DNS-rebinding window).
pub async fn resolve_checked(
    host: &str,
    port: u16,
    allow_private: bool,
) -> Result<Vec<SocketAddr>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|_| "無法解析 Odoo 主機名稱".to_string())?
        .collect();
    check_resolved_addrs(&addrs, allow_private)?;
    Ok(addrs)
}

/// Resolved-address plan for dialing `url`: `Ok(None)` for strict-localhost
/// HTTP (nothing to pin), `Ok(Some((host, addrs)))` otherwise after the
/// literal check *and* a resolution check. Fail closed on anything else.
pub async fn plan_connection(
    url: &str,
    allow_private: bool,
) -> Result<Option<(String, Vec<SocketAddr>)>, String> {
    check_url(url, allow_private)?;
    let parsed = url::Url::parse(url).map_err(|_| MSG_UNSAFE.to_string())?;
    if is_local_http(&parsed) {
        return Ok(None);
    }
    let host = parsed.host_str().ok_or_else(|| MSG_UNSAFE.to_string())?.to_string();
    let port = parsed.port_or_known_default().ok_or_else(|| MSG_UNSAFE.to_string())?;
    let addrs = resolve_checked(&host, port, allow_private).await?;
    // Cleartext HTTP (only reachable with the admin switch on) must resolve to
    // internal addresses exclusively.
    if parsed.scheme() == "http" && addrs.iter().any(|a| !is_private_ip(a.ip())) {
        return Err(MSG_UNSAFE.into());
    }
    Ok(Some((host.trim_start_matches('[').trim_end_matches(']').to_string(), addrs)))
}

/// Odoo database name: at most 63 characters of `[a-zA-Z0-9_-]`.
pub fn is_valid_odoo_db(db: &str) -> bool {
    !db.is_empty()
        && db.len() < 64
        && db
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unsafe_urls() {
        for url in [
            "http://localhost:3000@evil.com",
            "http://localhost.evil.com",
            "https://user@10.0.0.1/",
            "https://169.254.169.254/",
            "https://[::1]/",
            "http://odoo.example.com",
            "ftp://localhost",
            "not a url",
        ] {
            assert!(!is_safe_odoo_url(url), "{url} must be refused");
        }
    }

    #[test]
    fn accepts_safe_urls() {
        for url in [
            "http://localhost:8069",
            "http://127.0.0.1:8069/web",
            "https://odoo.example.com:8443/odoo",
        ] {
            assert!(is_safe_odoo_url(url), "{url} must be accepted");
        }
    }

    #[test]
    fn trailing_dot_hosts_do_not_bypass_the_name_list() {
        for url in [
            "https://localhost./",
            "https://metadata.google.internal./",
            "https://foo.localhost./",
        ] {
            assert!(!is_safe_odoo_url(url), "{url} must be refused");
        }
    }

    #[test]
    fn resolved_addresses_must_all_be_public() {
        let a = |s: &str| s.parse::<std::net::SocketAddr>().unwrap();
        assert!(check_resolved_addrs(&[a("8.8.8.8:443")], false).is_ok());
        assert!(check_resolved_addrs(&[], false).is_err());
        assert!(check_resolved_addrs(&[a("127.0.0.1:443")], false).is_err());
        assert!(check_resolved_addrs(&[a("8.8.8.8:443"), a("10.0.0.1:443")], false).is_err());
        assert!(check_resolved_addrs(&[a("[::1]:443")], false).is_err());
    }

    #[tokio::test]
    async fn name_resolving_to_loopback_is_refused() {
        // A public-looking name whose lookup lands on 127.0.0.1 (the
        // nip.io / rebinding shape). "localhost" resolves locally without
        // network, so it exercises the resolution check on its own.
        let err = resolve_checked("localhost", 443, false).await.unwrap_err();
        assert!(err.contains("本機") || err.contains("內網"), "{err}");
    }

    #[tokio::test]
    async fn plan_connection_refuses_literal_and_passes_local_http() {
        assert!(plan_connection("https://10.0.0.1/", false).await.is_err());
        assert!(plan_connection("http://example.com", false).await.is_err());
        assert!(plan_connection("http://127.0.0.1:8069", false).await.unwrap().is_none());
    }

    #[test]
    fn host_of_drops_query_and_path() {
        assert_eq!(host_of("https://erp.example.com/odoo?token=abc"), "erp.example.com");
        assert_eq!(host_of("nonsense"), "<invalid-url>");
    }

    #[test]
    fn db_name_rules() {
        assert!(is_valid_odoo_db("prod_db-1"));
        assert!(!is_valid_odoo_db(""));
        assert!(!is_valid_odoo_db("a b"));
        assert!(!is_valid_odoo_db(&"x".repeat(64)));
    }

    #[test]
    fn switch_off_private_literals_refused_with_how_to_enable() {
        for url in ["https://10.0.0.5/", "https://192.168.1.9:8443", "https://172.16.0.1"] {
            let err = check_url(url, false).unwrap_err();
            assert!(err.contains("允許連到內網的 Odoo"), "{url}: {err}");
        }
    }

    #[test]
    fn refusal_text_has_no_internal_key_and_covers_plain_http() {
        for url in ["https://10.0.0.5/", "http://192.168.1.9:8069"] {
            let err = check_url(url, false).unwrap_err();
            assert!(err.contains("允許連到內網的 Odoo"), "{url}: {err}");
            assert!(!err.contains("allow_private_network") && !err.contains("config.toml"), "{err}");
        }
    }

    #[test]
    fn switch_on_allows_private_https_and_http() {
        for url in [
            "https://10.0.0.5/",
            "http://192.168.1.9:8069",
            "https://[fd12:3456::1]/",
            "https://erp.corp.example",
        ] {
            assert!(check_url(url, true).is_ok(), "{url} must be accepted with the switch on");
        }
        // Cleartext HTTP to a public literal stays refused.
        assert!(check_url("http://8.8.8.8/", true).is_err());
    }

    #[test]
    fn metadata_and_loopback_refused_even_with_switch_on() {
        for url in [
            "https://169.254.169.254/",
            "http://169.254.169.254/latest",
            "https://metadata.google.internal/",
            "https://metadata.google.internal./",
            "https://metadata.azure.internal/",
            "https://[fd00:ec2::254]/",
            "https://100.100.100.200/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://[::ffff:127.0.0.1]/",
            "https://[::ffff:169.254.169.254]/",
            "https://localhost/",
            "https://foo.localhost/",
            "https://0.0.0.0/",
            "https://u:p@10.0.0.1/",
        ] {
            assert!(check_url(url, true).is_err(), "{url} must stay refused");
        }
        // strict-localhost http exception survives in both modes
        assert!(check_url("http://127.0.0.1:8069", true).is_ok());
        assert!(check_url("http://localhost:8069", false).is_ok());
    }

    #[test]
    fn resolved_private_addrs_follow_the_switch_but_metadata_never() {
        let a = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert!(check_resolved_addrs(&[a("10.0.0.5:443")], false).is_err());
        assert!(check_resolved_addrs(&[a("10.0.0.5:443")], true).is_ok());
        assert!(check_resolved_addrs(&[a("8.8.8.8:443"), a("192.168.1.2:443")], true).is_ok());
        for bad in ["127.0.0.1:443", "169.254.169.254:80", "[::1]:443", "[fe80::1]:443", "0.0.0.0:1"] {
            assert!(check_resolved_addrs(&[a(bad)], true).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn plan_connection_private_literal_follows_switch() {
        assert!(plan_connection("https://192.168.1.9/", false).await.is_err());
        let plan = plan_connection("https://192.168.1.9/", true).await.unwrap().unwrap();
        assert_eq!(plan.0, "192.168.1.9");
        assert!(plan_connection("https://169.254.169.254/", true).await.is_err());
        assert!(plan_connection("https://localhost/", true).await.is_err());
    }
}
