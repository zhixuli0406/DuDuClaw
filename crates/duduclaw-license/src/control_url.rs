//! Outbound URL policy. The license's control_url is NOT signed or trusted.
//! DNS validation and connection pinning must also run at the transport layer.

use url::{Host, Url};

/// The production endpoint used by the gateway and CLI. No wildcard suffixes.
pub const DEFAULT_CONTROL_URL: &str = "https://api.duduclaw.dudustudio.monster";
pub const OFFICIAL_CONTROL_HOSTS: &[&str] = &["api.duduclaw.dudustudio.monster"];

/// Validate an endpoint before resolving or contacting it. An operator override
/// must come from local process configuration, never from a license/response.
/// It authorizes only its exact origin; all other URL restrictions still apply.
pub fn validate_control_url(endpoint: &str, operator_override: Option<&str>) -> Result<Url, &'static str> {
    let url = parse_endpoint(endpoint)?;
    let host = url.host_str().ok_or("missing control-plane host")?;
    let locally_allowed = operator_override
        .filter(|value| !value.trim().is_empty())
        .map(|value| parse_endpoint(value.trim()))
        .transpose()?
        .is_some_and(|configured| configured.origin() == url.origin());
    if !OFFICIAL_CONTROL_HOSTS.contains(&host) && !locally_allowed {
        return Err("control-plane host is not allowed; configure DUDUCLAW_CONTROL_URL locally for a custom issuer");
    }
    Ok(url)
}

fn parse_endpoint(value: &str) -> Result<Url, &'static str> {
    // Reject parser normalization tricks, including empty userinfo and slashes
    // that URL parsers can reinterpret as an authority separator.
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) || value.contains('\\') {
        return Err("invalid control-plane URL characters");
    }
    let authority = value.strip_prefix("https://")
        .ok_or("control-plane URL must use HTTPS")?
        .split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("control-plane URL must not contain userinfo");
    }
    let url = Url::parse(value).map_err(|_| "invalid control-plane URL")?;
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return Err("control-plane URL must use HTTPS port 443");
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("control-plane URL must not contain credentials, query or fragment");
    }
    let Some(Host::Domain(host)) = url.host() else {
        return Err("control-plane IP literals are forbidden");
    };
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with('.') || !host.contains('.') {
        return Err("control-plane requires a public DNS hostname");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_host_and_explicit_local_override_only() {
        for host in OFFICIAL_CONTROL_HOSTS {
            assert!(validate_control_url(&format!("https://{host}/v1/license/refresh"), None).is_ok());
        }
        let custom = "https://issuer.example.com/prefix/v1/license/refresh";
        assert!(validate_control_url(custom, None).is_err());
        assert!(validate_control_url(custom, Some("https://issuer.example.com/prefix")).is_ok());
        assert!(validate_control_url("https://other.example.com", Some("https://issuer.example.com")).is_err());
        assert!(validate_control_url("https://api.duduclaw.dudustudio.monster.attacker.com", None).is_err());
        assert!(validate_control_url("https://sub.api.duduclaw.dudustudio.monster", None).is_err());
    }

    #[test]
    fn unsafe_urls_fail_even_as_local_overrides() {
        for value in [
            "http://api.duduclaw.dudustudio.monster", "ftp://issuer.example.com",
            "https://issuer.example.com:8443", "https://user@issuer.example.com",
            "https://@issuer.example.com", "https://user:pass@issuer.example.com",
            "https://issuer.example.com?next=https://attacker.com", "https://issuer.example.com#fragment",
            "https://127.0.0.1", "https://2130706433", "https://0x7f000001", "https://8.8.8.8",
            "https://[::1]", "https://[::ffff:127.0.0.1]", "https://169.254.169.254",
            "https://localhost", "https://local.localhost", "https://issuer.example.com.",
            "https://issuer", "https://issuer.example.com\\@attacker.com", "https://issuer.example.com\n",
        ] {
            assert!(validate_control_url(value, None).is_err(), "{value}");
            assert!(validate_control_url(value, Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn terminal_output_uses_canonical_control_free_url() {
        let url = validate_control_url(
            "https://API.DUDUCLAW.DUDUSTUDIO.MONSTER:443/old/../v1/license/refresh", None).unwrap();
        assert_eq!(url.as_str(), "https://api.duduclaw.dudustudio.monster/v1/license/refresh");
        assert!(!url.as_str().chars().any(char::is_control));
        for control in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}', '\u{9b}'] {
            let endpoint = format!("{DEFAULT_CONTROL_URL}/{control}injected");
            let error = validate_control_url(&endpoint, None).unwrap_err();
            assert_eq!(error, "invalid control-plane URL characters");
            assert!(!error.chars().any(char::is_control));
        }
        let encoded = validate_control_url(
            &format!("{DEFAULT_CONTROL_URL}/%1B%5B2J"), None).unwrap();
        assert!(encoded.as_str().ends_with("/%1B%5B2J"));
        assert!(!encoded.as_str().chars().any(char::is_control));
    }

    #[test]
    fn tampered_unsigned_control_url_cannot_authorize_outbound_request() {
        use crate::{License, LicenseTier, PublicKeyRegistry};
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[42; 32]);
        let registry = PublicKeyRegistry::new().with_key("test", key.verifying_key().to_bytes());
        let mut license = License::new("sub", "customer", LicenseTier::SelfHostPro,
            "fingerprint", chrono::Duration::days(30), "test");
        license.control_url = Some(DEFAULT_CONTROL_URL.into());
        license.signature = key.sign(&license.canonical_payload().unwrap()).to_bytes().to_vec();
        assert!(registry.verify(&license).is_ok());
        for target in ["https://attacker.example.com", "http://169.254.169.254", "https://localhost"] {
            license.control_url = Some(target.into());
            // Compatibility is preserved: only outbound trust changes.
            assert!(registry.verify(&license).is_ok());
            assert!(validate_control_url(license.control_url.as_deref().unwrap(), None).is_err());
        }
    }
}
