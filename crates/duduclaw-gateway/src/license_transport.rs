//! Shared gateway/CLI transport for unsigned license endpoint hints.

use std::net::SocketAddr;
use std::time::Duration;

/// Validate before DNS or sending data, then pin the checked DNS answers to
/// this request's client. A fresh client prevents stale connection/DNS reuse.
pub async fn request(
    method: reqwest::Method,
    endpoint: &str,
    timeout: Duration,
) -> Result<reqwest::RequestBuilder, String> {
    let local_override = std::env::var("DUDUCLAW_CONTROL_URL").ok();
    let url = duduclaw_license::control_url::validate_control_url(endpoint, local_override.as_deref())
        .map_err(str::to_string)?;
    let host = url.host_str().ok_or("missing control-plane host")?;
    let addresses: Vec<SocketAddr> = tokio::time::timeout(timeout, tokio::net::lookup_host((host, 443)))
        .await.map_err(|_| "control-plane DNS timed out")?
        .map_err(|_| "control-plane DNS lookup failed")?
        .collect();
    let client = pinned_client(host, &addresses, timeout)?;
    Ok(client.request(method, url))
}

fn pinned_client(host: &str, addresses: &[SocketAddr], timeout: Duration) -> Result<reqwest::Client, String> {
    // Reject the entire result when even one address is private/special-use.
    // Checking first and letting reqwest resolve again permits DNS rebinding.
    if addresses.is_empty() || addresses.iter().any(|addr| !duduclaw_core::net_addr::is_public_ip(&addr.ip())) {
        return Err("control-plane DNS must resolve exclusively to public addresses".into());
    }
    reqwest::Client::builder()
        .timeout(timeout)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        // A proxy could resolve the hostname again and bypass address pinning.
        .no_proxy()
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|_| "could not build control-plane client".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_empty_private_or_mixed_dns_answers() {
        let timeout = Duration::from_secs(1);
        let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
        assert!(pinned_client("issuer.example.com", &[], timeout).is_err());
        assert!(pinned_client("issuer.example.com", &[public], timeout).is_ok());
        for private in ["127.0.0.1:443", "10.0.0.1:443", "192.168.1.1:443",
            "169.254.169.254:443", "100.64.0.1:443", "[::1]:443", "[fc00::1]:443",
            "[fe80::1]:443", "[::ffff:127.0.0.1]:443", "[64:ff9b::a9fe:a9fe]:443"] {
            let address = private.parse().unwrap();
            assert!(pinned_client("issuer.example.com", &[address], timeout).is_err(), "{private}");
            assert!(pinned_client("issuer.example.com", &[public, address], timeout).is_err(), "{private}");
        }
    }

    #[tokio::test]
    async fn refuses_tampered_endpoint_before_resolving_or_sending() {
        // No server is necessary: these URLs fail before any DNS/network work,
        // even when an operator has configured a custom issuer.
        for endpoint in ["http://169.254.169.254/v1/license/refresh",
            "https://127.0.0.1/v1/license/refresh", "https://localhost/v1/license/refresh"] {
            assert!(request(reqwest::Method::POST, endpoint, Duration::from_secs(1)).await.is_err());
        }
    }
}
