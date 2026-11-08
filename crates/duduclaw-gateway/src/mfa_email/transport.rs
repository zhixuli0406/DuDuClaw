//! This transport has no operator-selectable authority, including in tests.
use std::{net::SocketAddr, time::Duration};

pub(super) const OFFICIAL_ORIGIN: &str = "https://api.duduclaw.dudustudio.monster";
const OFFICIAL_HOST: &str = "api.duduclaw.dudustudio.monster";
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MAX_RESPONSE_BYTES: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Endpoint { Challenge, Enroll, Send }
impl Endpoint {
    pub(super) fn path(self) -> &'static str {
        match self {
            Self::Challenge => "/api/v1/relay/enroll/challenge",
            Self::Enroll => "/api/v1/relay/enroll",
            Self::Send => "/api/v1/relay/mfa-email",
        }
    }
    fn url(self) -> String { format!("{OFFICIAL_ORIGIN}{}", self.path()) }
}

// No Debug: bodies and headers contain credentials or OTPs. Bodies are
// cleared on drop. Residual risk: hyper/rustls copy request and response
// bytes into internal `Bytes` buffers that this crate cannot clear.
pub(crate) struct Call {
    pub endpoint: Endpoint,
    pub body: zeroize::Zeroizing<Vec<u8>>,
    pub headers: reqwest::header::HeaderMap,
}
pub(crate) struct Reply { pub status: u16, pub body: zeroize::Zeroizing<Vec<u8>> }
impl Reply {
    /// Fixed-capacity, zeroize-on-drop body buffer. Callers never exceed
    /// `MAX_RESPONSE_BYTES`, so the buffer is never reallocated (a realloc would
    /// leave an uncleared copy of a credential secret behind).
    pub(super) fn buffer() -> zeroize::Zeroizing<Vec<u8>> {
        zeroize::Zeroizing::new(Vec::with_capacity(MAX_RESPONSE_BYTES))
    }
    /// Copy into a fixed-capacity buffer. Oversize input (tests only; the
    /// reader rejects it) becomes a zero-filled over-limit marker instead, so
    /// `sent`/enroll still reject it without copying remote bytes.
    pub(super) fn new(status: u16, bytes: &[u8]) -> Self {
        if bytes.len() > MAX_RESPONSE_BYTES { return Self { status, body: oversized_marker() }; }
        let mut body = Self::buffer();
        body.extend_from_slice(bytes);
        Self { status, body }
    }
}
/// A body that is recognisably over the limit, without holding remote bytes.
fn oversized_marker() -> zeroize::Zeroizing<Vec<u8>> {
    zeroize::Zeroizing::new(vec![0u8; MAX_RESPONSE_BYTES + 1])
}

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
        // reqwest takes ownership of a body copy; our Zeroizing copy is cleared
        // when `call` drops at the end of this scope.
        let Call { endpoint, body, headers } = call;
        read_response(client.post(endpoint.url()).headers(headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json").body(body.to_vec())).await
    }).await.map_err(|_| super::DELIVERY_FAILED)?
}

pub(super) async fn read_response(request: reqwest::RequestBuilder) -> Result<Reply, &'static str> {
    let mut response = request.send().await.map_err(|_| super::DELIVERY_FAILED)?;
    let status = response.status().as_u16();
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        return Err(super::DELIVERY_FAILED);
    }
    let mut body = Reply::buffer();
    while let Some(chunk) = response.chunk().await.map_err(|_| super::DELIVERY_FAILED)? {
        // Checked before extending: the fixed capacity is never exceeded.
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
            for endpoint in [Endpoint::Challenge, Endpoint::Enroll, Endpoint::Send] {
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
    fn reply_bodies_are_zeroizing_with_fixed_capacity() {
        // Type-level: a plain Vec here would not compile.
        let reply = Reply::new(201, br#"{"credential_id":"id","secret":"00"}"#);
        let body: &zeroize::Zeroizing<Vec<u8>> = &reply.body;
        assert!(body.capacity() >= MAX_RESPONSE_BYTES);
        let mut buffer = Reply::buffer();
        let capacity = buffer.capacity();
        let pointer = buffer.as_ptr();
        buffer.extend_from_slice(&[7u8; MAX_RESPONSE_BYTES]);
        assert_eq!((buffer.capacity(), buffer.as_ptr()), (capacity, pointer), "no reallocation up to the limit");
        assert!(Reply::new(202, &[b' '; MAX_RESPONSE_BYTES + 1]).body.len() > MAX_RESPONSE_BYTES);
        let call = Call { endpoint: Endpoint::Send, body: zeroize::Zeroizing::new(b"otp".to_vec()),
            headers: reqwest::header::HeaderMap::new() };
        let _: &zeroize::Zeroizing<Vec<u8>> = &call.body;
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
