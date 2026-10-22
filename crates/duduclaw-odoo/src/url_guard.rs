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

    let lower = host.to_ascii_lowercase();
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
    fn db_name_rules() {
        assert!(is_valid_odoo_db("prod_db-1"));
        assert!(!is_valid_odoo_db(""));
        assert!(!is_valid_odoo_db("a b"));
        assert!(!is_valid_odoo_db(&"x".repeat(64)));
    }
}
