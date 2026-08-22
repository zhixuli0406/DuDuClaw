// Local session bootstrap — Shell-S4 (2026-08-22). `POST /api/session/local`
// (`duduclaw-gateway/src/server.rs::handle_local_session`, read-only
// reference, not modified from here): issues a real JWT for the Personal
// edition's `admin@local` user to a caller that has proven it's sitting at
// this machine — no password prompt, matching this shell's own "殼與
// gateway 同機＝本機特權面" posture (task brief).
//
// Same hand-rolled HTTP/1.1 client SHAPE as `oobe/claim.rs` (see that
// module's own header comment for why no `reqwest` — a raw `TcpStream`
// round trip against a `127.0.0.1` peer is a few dozen lines, not worth a
// new HTTP stack for). Deliberately a SEPARATE, self-contained module
// rather than sharing code with `claim.rs`: the two endpoints' request
// shapes are close but not identical (this one is POST-only, no `GET`
// pre-check, and needs one extra header — see below), and this crate's own
// established convention (`Cargo.toml`'s `zbus`/`serde` comments) is small
// independent modules over cross-module coupling. `tokio` is now a
// dependency of this crate as of `gateway_client::ws_rpc`, but this
// particular call stays a plain blocking `TcpStream` round trip on purpose —
// pulling `reqwest`'s async client in for one small POST would mean this
// call needs a tokio runtime too, for no benefit over `claim.rs`'s already-
// proven pattern.
//
// The one thing this endpoint needs beyond `claim.rs`'s baseline: the
// `x-duduclaw-local: 1` marker header. Its mere PRESENCE is what the
// server's `local_session::evaluate()` gate checks (`local_marker_ok` —
// "forces the preflight", per that fn's own doc comment); the value is
// pinned to `"1"` to match the server's literal `== LOCAL_MARKER_VALUE`
// check. No `Origin` header is sent at all: `local_session::
// origin_is_allowed_with` treats an ABSENT `Origin` as allowed (`None =>
// true` — the same rule the `/ws` upgrade path uses for non-browser
// clients), and this is a native TCP client, not a browser, so there is
// nothing honest to put in one.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:18789";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_MARKER_HEADER: &str = "x-duduclaw-local";
const LOCAL_MARKER_VALUE: &str = "1";

/// Why a local-session bootstrap did not resolve to a fresh JWT. Deliberately
/// distinct variants for the same reason `oobe::claim::ClaimError` gives —
/// worth having for a stderr diagnostic and for tests — even though
/// `overlay::notifications_feed` collapses all of them to one Offline
/// presentation (see that module's own doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// Couldn't complete the TCP round trip — DNS/connect/read/write
    /// failure, or a timeout tripped.
    Unreachable(String),
    /// The gateway refused with 403 — disabled switch / non-Personal
    /// edition / off-loopback / behind a proxy / foreign origin. One
    /// collapsed reason: `handle_local_session` deliberately never reports
    /// which condition failed (see that fn's own doc comment — "an
    /// off-loopback prober must not be able to learn ... which condition it
    /// tripped"), so there is nothing more specific for this client to
    /// surface even if it wanted to.
    Refused,
    /// An HTTP status this module has no specific handling for.
    Http(u16),
    /// The response didn't parse as expected — bad HTTP framing, non-UTF8
    /// headers, non-JSON body, or JSON missing `access_token`.
    Malformed(String),
    /// The configured base URL failed the scheme/host safety gate BEFORE
    /// any connection was attempted — same "refuse fail-closed before any
    /// I/O" rule `oobe/claim.rs`'s own `NonLoopback` variant documents.
    NonLoopback,
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Real entry point: reads `DUDUCLAW_SHELL_GATEWAY_URL` (same env var, same
/// fallback, as `oobe::claim::create_account`) and returns the freshly
/// issued access token (JWT) on success.
pub fn bootstrap_local_session() -> Result<String, SessionError> {
    bootstrap_local_session_at(&gateway_base_url())
}

fn gateway_base_url() -> String {
    std::env::var("DUDUCLAW_SHELL_GATEWAY_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GATEWAY_URL.to_string())
}

/// The testable core — every test below calls this directly against a local
/// mock server, same split `oobe/claim.rs`'s own
/// `create_account`/`create_account_at` establish.
pub(crate) fn bootstrap_local_session_at(base_url: &str) -> Result<String, SessionError> {
    let (host, port) = parse_loopback_base_url(base_url)?;
    let resp = http_post_local(&host, port, "/api/session/local")?;
    match resp.status {
        200 => {
            let json: serde_json::Value = serde_json::from_slice(&resp.body)
                .map_err(|e| SessionError::Malformed(format!("local session response was not valid JSON: {e}")))?;
            json.get("access_token")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| SessionError::Malformed("local session response missing `access_token`".to_string()))
        }
        403 => Err(SessionError::Refused),
        other => Err(SessionError::Http(other)),
    }
}

fn parse_loopback_base_url(url: &str) -> Result<(String, u16), SessionError> {
    let rest = url.strip_prefix("http://").ok_or(SessionError::NonLoopback)?;
    let authority = rest.split('/').next().unwrap_or(rest);

    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let mut parts = after_bracket.splitn(2, ']');
        let host = parts.next().ok_or(SessionError::NonLoopback)?.to_string();
        let port = match parts.next() {
            Some(p) if !p.is_empty() => p.strip_prefix(':').and_then(|d| d.parse::<u16>().ok()).ok_or(SessionError::NonLoopback)?,
            _ => 80,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| SessionError::NonLoopback)?),
            None => (authority.to_string(), 80),
        }
    };

    let host_lower = host.to_ascii_lowercase();
    let is_loopback = host_lower == "127.0.0.1" || host_lower == "::1" || host_lower == "localhost";
    if !is_loopback {
        return Err(SessionError::NonLoopback);
    }
    Ok((host_lower, port))
}

fn connect(host: &str, port: u16) -> Result<TcpStream, SessionError> {
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut addrs = target.to_socket_addrs().map_err(|e| SessionError::Unreachable(e.to_string()))?;
    let sock_addr = addrs.next().ok_or_else(|| SessionError::Unreachable("could not resolve any address".to_string()))?;
    let stream = TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT).map_err(|e| SessionError::Unreachable(e.to_string()))?;
    let _ = stream.set_read_timeout(Some(READ_WRITE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_WRITE_TIMEOUT));
    Ok(stream)
}

fn http_post_local(host: &str, port: u16, path: &str) -> Result<HttpResponse, SessionError> {
    let mut stream = connect(host, port)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 0\r\n{LOCAL_MARKER_HEADER}: {LOCAL_MARKER_VALUE}\r\nAccept: application/json\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(|e| SessionError::Unreachable(e.to_string()))?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| SessionError::Unreachable(e.to_string()))?;
    parse_response(&raw)
}

/// Same status-line -> headers -> body parse, and the same `Content-Length`
/// vs. read-to-EOF fallback, as `oobe/claim.rs::parse_response` — see that
/// fn's own doc comment for the exact rule.
fn parse_response(raw: &[u8]) -> Result<HttpResponse, SessionError> {
    let split_at =
        raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| SessionError::Malformed("no header/body separator in HTTP response".to_string()))?;
    let header_text = std::str::from_utf8(&raw[..split_at]).map_err(|_| SessionError::Malformed("response headers were not valid UTF-8".to_string()))?;
    let mut body = raw[split_at + 4..].to_vec();

    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| SessionError::Malformed("empty HTTP response".to_string()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| SessionError::Malformed(format!("could not parse HTTP status line: {status_line:?}")))?;

    if let Some(len) = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
    }) {
        if len <= body.len() {
            body.truncate(len);
        }
    }

    Ok(HttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Same one-connection-per-canned-response mock server shape as
    /// `oobe/claim.rs`'s own `start_mock_server` — this endpoint only ever
    /// sends ONE request per call (no status pre-check like the first-run
    /// claim flow has), so every test here only needs a single-element
    /// `responses` list.
    fn start_mock_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut sink = [0u8; 4096];
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.read(&mut sink);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn success_returns_the_access_token() {
        let base = start_mock_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 47\r\n\r\n{\"access_token\":\"jwt-abc\",\"refresh_token\":\"r\"}",
        );
        assert_eq!(bootstrap_local_session_at(&base), Ok("jwt-abc".to_string()));
    }

    #[test]
    fn forbidden_maps_to_refused() {
        let base = start_mock_server("HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\n\r\n{\"x\":1}\n");
        assert_eq!(bootstrap_local_session_at(&base), Err(SessionError::Refused));
    }

    #[test]
    fn missing_access_token_field_is_malformed_not_a_panic() {
        let base = start_mock_server("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"ok\":true}");
        assert!(matches!(bootstrap_local_session_at(&base), Err(SessionError::Malformed(_))));
    }

    #[test]
    fn non_json_body_is_malformed_not_a_panic() {
        // No `Content-Length` — exercises the read-to-EOF fallback path.
        let base = start_mock_server("HTTP/1.1 200 OK\r\n\r\nnot json");
        assert!(matches!(bootstrap_local_session_at(&base), Err(SessionError::Malformed(_))));
    }

    #[test]
    fn unexpected_status_maps_to_http() {
        let base = start_mock_server("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\n\r\n{}");
        assert_eq!(bootstrap_local_session_at(&base), Err(SessionError::Http(500)));
    }

    #[test]
    fn connection_refused_maps_to_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let result = bootstrap_local_session_at(&format!("http://{addr}"));
        assert!(matches!(result, Err(SessionError::Unreachable(_))), "{result:?}");
    }

    #[test]
    fn non_loopback_url_is_refused_without_any_connection_attempt() {
        let started = std::time::Instant::now();
        let result = bootstrap_local_session_at("http://example.com:18789");
        assert_eq!(result, Err(SessionError::NonLoopback));
        assert!(started.elapsed() < Duration::from_millis(500), "must fail before ever dialing out, elapsed={:?}", started.elapsed());
    }

    #[test]
    fn parse_loopback_base_url_accepts_all_three_loopback_spellings() {
        assert_eq!(parse_loopback_base_url("http://127.0.0.1:18789"), Ok(("127.0.0.1".to_string(), 18789)));
        assert_eq!(parse_loopback_base_url("http://localhost:18789"), Ok(("localhost".to_string(), 18789)));
        assert_eq!(parse_loopback_base_url("http://[::1]:18789"), Ok(("::1".to_string(), 18789)));
    }
}
