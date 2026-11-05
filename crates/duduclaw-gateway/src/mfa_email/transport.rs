//! This transport has no operator-selectable authority, including in tests.
use std::{net::SocketAddr, time::Duration};

pub(super) const OFFICIAL_ORIGIN: &str = "https://api.duduclaw.dudustudio.monster";
const OFFICIAL_HOST: &str = "api.duduclaw.dudustudio.monster";
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MAX_RESPONSE_BYTES: usize = 4096;

#[derive(Clone, Copy)]
pub(super) enum Endpoint { Enroll, Send }
impl Endpoint {
    pub(super) fn path(self) -> &'static str {
        match self { Self::Enroll => "/api/v1/relay/enroll", Self::Send => "/api/v1/relay/mfa-email" }
    }
    fn url(self) -> String { format!("{OFFICIAL_ORIGIN}{}", self.path()) }
}

// No Debug: bodies and headers contain credentials or OTPs.
pub(super) struct Call {
    pub endpoint: Endpoint,
    pub body: Vec<u8>,
    pub headers: reqwest::header::HeaderMap,
}
pub(super) struct Reply { pub status: u16, pub body: Vec<u8> }

fn pinned_client(addresses: &[SocketAddr]) -> Result<reqwest::Client, &'static str> {
    if addresses.is_empty() || addresses.iter().any(|a| a.port() != 443
        || !duduclaw_core::net_addr::is_public_ip(&a.ip())) {
        return Err(super::DELIVERY_FAILED);
    }
    reqwest::Client::builder().https_only(true).no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(TIMEOUT)
        .resolve_to_addrs(OFFICIAL_HOST, addresses).build().map_err(|_| super::DELIVERY_FAILED)
}

pub(super) async fn exchange(call: Call) -> Result<Reply, &'static str> {
    // Also bound DNS + connect + response together, rather than just each stage.
    tokio::time::timeout(TIMEOUT, async {
        let addresses: Vec<_> = tokio::net::lookup_host((OFFICIAL_HOST, 443)).await
            .map_err(|_| super::DELIVERY_FAILED)?.collect();
        let client = pinned_client(&addresses)?;
        read_response(client.post(call.endpoint.url()).headers(call.headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json").body(call.body)).await
    }).await.map_err(|_| super::DELIVERY_FAILED)?
}

pub(super) async fn read_response(request: reqwest::RequestBuilder) -> Result<Reply, &'static str> {
    let mut response = request.send().await.map_err(|_| super::DELIVERY_FAILED)?;
    let status = response.status().as_u16();
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        return Err(super::DELIVERY_FAILED);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| super::DELIVERY_FAILED)? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) { return Err(super::DELIVERY_FAILED); }
        body.extend_from_slice(&chunk);
    }
    Ok(Reply { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_ignores_environment_overrides() {
        // Use a subprocess: changing the environment in a multi-threaded test
        // runner is unsafe and could interfere with license-refresh tests.
        const CHILD: &str = "DUDUCLAW_MFA_RELAY_DESTINATION_TEST";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(std::env::var("DUDUCLAW_CONTROL_URL").unwrap(), "https://attacker.example.com");
            let client = pinned_client(&["8.8.8.8:443".parse().unwrap()]).unwrap();
            for endpoint in [Endpoint::Enroll, Endpoint::Send] {
                let request = client.post(endpoint.url()).build().unwrap();
                assert_eq!(request.url().scheme(), "https");
                assert_eq!(request.url().host_str(), Some(OFFICIAL_HOST));
                assert_eq!(request.url().port_or_known_default(), Some(443));
                assert_eq!(request.url().path(), endpoint.path());
            }
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["mfa_email::transport::tests::destination_ignores_environment_overrides", "--exact"])
            .env(CHILD, "1").env("DUDUCLAW_CONTROL_URL", "https://attacker.example.com")
            .env("HTTPS_PROXY", "http://127.0.0.1:1").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "child filter must execute the destination assertion");
    }
    #[test]
    fn rejects_non_public_mixed_empty_and_non_tls_port_dns() {
        let public = "8.8.8.8:443".parse().unwrap();
        assert!(pinned_client(&[public]).is_ok());
        assert!(pinned_client(&[]).is_err());
        for address in ["127.0.0.1:443", "10.0.0.1:443", "169.254.169.254:443",
            "[::1]:443", "[fc00::1]:443", "[::ffff:127.0.0.1]:443", "8.8.8.8:80"] {
            let address = address.parse().unwrap();
            assert!(pinned_client(&[address]).is_err());
            assert!(pinned_client(&[public, address]).is_err());
        }
    }
}
