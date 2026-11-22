//! Shared gateway/CLI transport for unsigned license endpoint hints.
//!
//! Every licence-related response body is read through [`read_bounded`]:
//! nothing is parsed (let alone verified) before the whole body is known to
//! fit its cap.

use std::net::SocketAddr;
use std::time::Duration;

/// Cap for refresh / rebind / subscription-status responses (and their error
/// bodies). A signed refresh envelope is a few KiB at most — the complete
/// `tests/fixtures/license_response_v2.json` vector, with its two extra
/// status vectors, is about 4 KiB — so 256 KiB is ~60× headroom.
pub const MAX_LICENSE_RESPONSE_BYTES: usize = 256 * 1024;

/// Cap for the signed CRL. Unlike a licence it grows with every revoked
/// subscription (an opaque id of ~40 bytes plus JSON quoting); a cap that
/// is too small would silently stop revocations from applying, so 1 MiB
/// (~25 000 revoked ids) rather than the 256 KiB used for licences.
pub const MAX_CRL_RESPONSE_BYTES: usize = 1024 * 1024;

/// Why a response body was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    /// Declared or actual length above the cap; the rest is never read.
    TooLarge,
    /// The body could not be read (connection error, timeout, short body).
    Read,
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "response body too large",
            Self::Read => "response body could not be read",
        })
    }
}

impl std::error::Error for BodyError {}

/// Read a response body of at most `cap` bytes. A declared `Content-Length`
/// above the cap is refused before anything is read; otherwise chunks are
/// accumulated (with checked arithmetic) and the read stops as soon as the
/// total would pass the cap — whatever framing the server used.
pub async fn read_bounded(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, BodyError> {
    if response.content_length().is_some_and(|len| len > cap as u64) {
        return Err(BodyError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| BodyError::Read)? {
        let total = bytes.len().checked_add(chunk.len()).ok_or(BodyError::TooLarge)?;
        if total > cap {
            return Err(BodyError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// [`read_bounded`], then parse JSON.
pub async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    cap: usize,
) -> Result<T, String> {
    let bytes = read_bounded(response, cap).await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// A short, display-safe excerpt of a bounded error body: the JSON
/// `message` when there is one, else the first 200 characters.
pub async fn error_excerpt(response: reqwest::Response) -> String {
    let Ok(bytes) = read_bounded(response, MAX_LICENSE_RESPONSE_BYTES).await else {
        return "<unreadable body>".into();
    };
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| text.chars().take(200).collect())
}

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

/// A one-shot plain-HTTP server on loopback writing a raw, hand-framed
/// response, so body caps can be tested against declared / chunked / short
/// framing without a real control plane (the pinned transport above refuses
/// loopback and plain HTTP by design).
#[cfg(test)]
pub(crate) mod test_http {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve `raw` once; returns the URL. The server reads the full request
    /// first and keeps draining afterwards, so the client never sees a reset.
    pub(crate) async fn serve_once(raw: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 { return; }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let body_len = head.lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if request.len() >= end + 4 + body_len { break; }
                }
            }
            let _ = sock.write_all(&raw).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
            while matches!(sock.read(&mut buf).await, Ok(n) if n > 0) {}
        });
        format!("http://{addr}/")
    }

    /// `200` with `Content-Length: declared` followed by `body`.
    pub(crate) fn with_length(declared: usize, body: &[u8]) -> Vec<u8> {
        let mut raw = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\n\r\n")
            .into_bytes();
        raw.extend_from_slice(body);
        raw
    }

    /// `200` with `Transfer-Encoding: chunked`, `body` split into `chunk`-byte chunks.
    pub(crate) fn chunked(body: &[u8], chunk: usize) -> Vec<u8> {
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for part in body.chunks(chunk.max(1)) {
            raw.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            raw.extend_from_slice(part);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"0\r\n\r\n");
        raw
    }

    /// GET `url` with a plain client (no proxy, no redirects, short timeout).
    pub(crate) async fn get(url: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_http::{chunked, get, serve_once, with_length};

    const CAP: usize = 1024;

    #[tokio::test]
    async fn declared_oversize_content_length_is_refused_before_reading() {
        // Only the headers are sent: reading the body would wait for bytes
        // that never come, so a prompt TooLarge proves the header check.
        let url = serve_once(with_length(CAP + 1, b"")).await;
        assert_eq!(read_bounded(get(&url).await, CAP).await, Err(BodyError::TooLarge));
        let url = serve_once(with_length(MAX_LICENSE_RESPONSE_BYTES + 1, b"{}")).await;
        assert_eq!(read_bounded(get(&url).await, MAX_LICENSE_RESPONSE_BYTES).await, Err(BodyError::TooLarge));
    }

    #[tokio::test]
    async fn chunked_body_over_the_cap_is_refused() {
        for chunk in [1, 100, CAP, CAP + 1, 4 * CAP] {
            let url = serve_once(chunked(&vec![b'x'; CAP + 1], chunk)).await;
            assert_eq!(read_bounded(get(&url).await, CAP).await, Err(BodyError::TooLarge), "{chunk}");
        }
    }

    #[tokio::test]
    async fn body_exactly_at_the_cap_is_accepted() {
        let body = vec![b'y'; CAP];
        let url = serve_once(with_length(CAP, &body)).await;
        assert_eq!(read_bounded(get(&url).await, CAP).await.unwrap(), body);
        let url = serve_once(chunked(&body, 100)).await;
        assert_eq!(read_bounded(get(&url).await, CAP).await.unwrap(), body);
        let url = serve_once(with_length(0, b"")).await;
        assert!(read_bounded(get(&url).await, CAP).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn short_body_is_a_read_error_and_json_is_parsed_only_after_the_cap() {
        // Declares more than it sends, then closes: never accepted.
        let url = serve_once(with_length(100, b"{\"a\":1}")).await;
        assert_eq!(read_bounded(get(&url).await, CAP).await, Err(BodyError::Read));
        let url = serve_once(with_length(7, b"{\"a\":1}")).await;
        let value: serde_json::Value = read_bounded_json(get(&url).await, CAP).await.unwrap();
        assert_eq!(value, serde_json::json!({"a": 1}));
        let url = serve_once(chunked(format!("{{\"a\":\"{}\"}}", "z".repeat(CAP)).as_bytes(), 64)).await;
        assert!(read_bounded_json::<serde_json::Value>(get(&url).await, CAP).await.is_err());
        assert!(MAX_CRL_RESPONSE_BYTES >= MAX_LICENSE_RESPONSE_BYTES);
    }

    #[tokio::test]
    async fn error_excerpt_is_bounded_and_prefers_the_message() {
        let body = br#"{"message":"go away"}"#;
        let url = serve_once(with_length(body.len(), body)).await;
        assert_eq!(error_excerpt(get(&url).await).await, "go away");
        let url = serve_once(with_length(MAX_LICENSE_RESPONSE_BYTES + 1, b"")).await;
        assert_eq!(error_excerpt(get(&url).await).await, "<unreadable body>");
        let url = serve_once(chunked("e".repeat(500).as_bytes(), 50)).await;
        assert_eq!(error_excerpt(get(&url).await).await.chars().count(), 200);
    }

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
