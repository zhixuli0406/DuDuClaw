//! SSRF / scheme validation for Odoo server URLs and database names.
//!
//! Single implementation shared by the gateway write path (`odoo.configure`,
//! `odoo.agent_config_set`), the connectivity tests and the per-agent runtime
//! merge in [`crate::AgentOdooConfig::apply_to`], so a URL that the dashboard
//! refuses can never be dialed at runtime and vice versa.

/// Validate that a URL is safe for Odoo connections.
/// Requires HTTPS with a non-private host, except for strict localhost over
/// plain HTTP.
///
/// The host is taken from the parsed URL, never by prefix: with a prefix
/// check `http://localhost:3000@evil.com` passed as local (its host is
/// `evil.com`), and `https://user@10.0.0.1/` hid a private address behind
/// userinfo. Any userinfo is refused outright.
pub fn is_safe_odoo_url(url: &str) -> bool {
    if url.len() > 512 {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    match parsed.scheme() {
        "http" => matches!(host, "127.0.0.1" | "localhost" | "[::1]"),
        "https" => !is_private_host(host),
        _ => false,
    }
}

/// Check if a hostname is a private/reserved IP or a known metadata endpoint.
/// Uses `std::net::IpAddr` parsing so IPv4-mapped IPv6 and compressed forms
/// are handled.
pub fn is_private_host(host: &str) -> bool {
    let raw = host.trim_start_matches('[').trim_end_matches(']');

    // Bare IPv6 without brackets (contains ':' but no '[') is ambiguous.
    if !host.starts_with('[') && raw.contains(':') {
        return true;
    }

    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return is_private_ip(ip);
    }

    // A trailing dot is the same DNS name ("localhost." == "localhost").
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
        || lower == "metadata.azure.internal"
}

/// Private, loopback, link-local or otherwise non-public address
/// ([`duduclaw_core::net_addr::is_public_ip`]).
pub fn is_private_ip(ip: std::net::IpAddr) -> bool {
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

/// Plain-HTTP strict-localhost URL (the only non-HTTPS shape the guard accepts).
fn is_local_http(parsed: &url::Url) -> bool {
    parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// Every resolved address must be a public IP.
pub fn check_resolved_addrs(addrs: &[std::net::SocketAddr]) -> Result<(), String> {
    if addrs.is_empty() {
        return Err("Odoo host did not resolve to any address".into());
    }
    if addrs.iter().any(|a| is_private_ip(a.ip())) {
        return Err("Odoo host resolves to a private or reserved address; connection refused".into());
    }
    Ok(())
}

/// Resolve `host:port` and require every address to be public. Returns the
/// checked addresses so the caller can pin them (no second lookup, which
/// would reopen a DNS-rebinding window).
pub async fn resolve_public(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|_| "could not resolve the Odoo host".to_string())?
        .collect();
    check_resolved_addrs(&addrs)?;
    Ok(addrs)
}

/// Resolved-address plan for dialing `url`: `Ok(None)` for strict-localhost
/// HTTP (nothing to pin), `Ok(Some((host, addrs)))` for HTTPS after the
/// literal check *and* a resolution check. Fail closed on anything else.
pub async fn plan_connection(
    url: &str,
) -> Result<Option<(String, Vec<std::net::SocketAddr>)>, String> {
    if !is_safe_odoo_url(url) {
        return Err("Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1) and must not target a private/reserved host".into());
    }
    let parsed = url::Url::parse(url).map_err(|_| "Odoo URL is malformed".to_string())?;
    if is_local_http(&parsed) {
        return Ok(None);
    }
    let host = parsed.host_str().ok_or("Odoo URL has no host")?.to_string();
    let port = parsed.port_or_known_default().ok_or("Odoo URL has no port")?;
    let addrs = resolve_public(&host, port).await?;
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
        assert!(check_resolved_addrs(&[a("8.8.8.8:443")]).is_ok());
        assert!(check_resolved_addrs(&[]).is_err());
        assert!(check_resolved_addrs(&[a("127.0.0.1:443")]).is_err());
        assert!(check_resolved_addrs(&[a("8.8.8.8:443"), a("10.0.0.1:443")]).is_err());
        assert!(check_resolved_addrs(&[a("[::1]:443")]).is_err());
    }

    #[tokio::test]
    async fn name_resolving_to_loopback_is_refused() {
        // A public-looking name whose lookup lands on 127.0.0.1 (the
        // nip.io / rebinding shape). "localhost" resolves locally without
        // network, so it exercises the resolution check on its own.
        let err = resolve_public("localhost", 443).await.unwrap_err();
        assert!(err.contains("private or reserved"), "{err}");
    }

    #[tokio::test]
    async fn plan_connection_refuses_literal_and_passes_local_http() {
        assert!(plan_connection("https://10.0.0.1/").await.is_err());
        assert!(plan_connection("http://example.com").await.is_err());
        assert!(plan_connection("http://127.0.0.1:8069").await.unwrap().is_none());
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
}
