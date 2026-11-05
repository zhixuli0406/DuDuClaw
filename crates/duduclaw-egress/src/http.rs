//! Bounded CONNECT-only parser. No request/body/header is ever audited.
use std::num::NonZeroU16;
use duduclaw_isolation::{bounded::Text, egress::{ConnectTarget, EgressLimits}};
use crate::policy::normalize_host;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError { BadRequest, MethodNotAllowed, TooLarge }
impl HttpError {
    pub fn response(self) -> &'static [u8] {
        match self {
            Self::BadRequest => b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            Self::MethodNotAllowed => b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            Self::TooLarge => b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        }
    }
}
pub fn parse_connect(bytes: &[u8], limits: &EgressLimits) -> Result<ConnectTarget, HttpError> {
    if limits.validate().is_err() || bytes.len() > limits.max_header_bytes.get() as usize { return Err(HttpError::TooLarge); }
    let text = std::str::from_utf8(bytes).map_err(|_| HttpError::BadRequest)?;
    let content = text.strip_suffix("\r\n\r\n").ok_or(HttpError::BadRequest)?;
    let mut lines = content.split("\r\n");
    let mut words = lines.next().ok_or(HttpError::BadRequest)?.split(' ');
    let method = words.next().ok_or(HttpError::BadRequest)?;
    if method != "CONNECT" { return Err(HttpError::MethodNotAllowed); }
    let authority = words.next().ok_or(HttpError::BadRequest)?;
    if !matches!(words.next(), Some("HTTP/1.1" | "HTTP/1.0")) || words.next().is_some() { return Err(HttpError::BadRequest); }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']').ok_or(HttpError::BadRequest)?;
        host.parse::<std::net::Ipv6Addr>().map_err(|_| HttpError::BadRequest)?;
        (host, rest.strip_prefix(':').ok_or(HttpError::BadRequest)?)
    } else {
        let (host, port) = authority.split_once(':').ok_or(HttpError::BadRequest)?;
        (host, port)
    };
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) { return Err(HttpError::BadRequest); }
    let port = port.parse::<NonZeroU16>().map_err(|_| HttpError::BadRequest)?;
    normalize_host(host).map_err(|_| HttpError::BadRequest)?;
    for (index, line) in lines.enumerate() {
        if index >= limits.max_headers.get() as usize { return Err(HttpError::TooLarge); }
        let (name, value) = line.split_once(':').ok_or(HttpError::BadRequest)?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127) { return Err(HttpError::BadRequest); }
        // Reject request bodies/upgrade framing; the next bytes are tunnel data.
        if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-length") && value.trim() != "0" { return Err(HttpError::BadRequest); }
    }
    Ok(ConnectTarget { host: Text::new(host.to_owned()).map_err(|_| HttpError::BadRequest)?, port })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::default_limits;
    #[test]
    fn connect_parser_only_accepts_bounded_authority_requests() {
        let limits = default_limits();
        for request in ["CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n", "CONNECT [2606:4700::1111]:443 HTTP/1.0\r\n\r\n"] {
            assert!(parse_connect(request.as_bytes(), &limits).is_ok());
        }
        for request in ["GET http://example.com/ HTTP/1.1\r\n\r\n", "POST / HTTP/1.1\r\n\r\n"] {
            assert_eq!(parse_connect(request.as_bytes(), &limits), Err(HttpError::MethodNotAllowed));
        }
        for request in ["CONNECT example.com:0 HTTP/1.1\r\n\r\n", "CONNECT user@example.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT example.com:443 HTTP/1.1 extra\r\n\r\n", "CONNECT example.com:443 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "CONNECT example.com:443 HTTP/1.1\r\nContent-Length: 1\r\n\r\n", "CONNECT example.com:443 HTTP/1.1\r\n folded: nope\r\n\r\n",
            "CONNECT example.com:443 HTTP/1.1\r\n\r\nTRAILER", "CONNECT example.com:443 HTTP/1.1\r\nX: a\nb\r\n\r\n"] {
            assert!(parse_connect(request.as_bytes(), &limits).is_err(), "{request:?}");
        }
        let large = format!("CONNECT example.com:443 HTTP/1.1\r\n{}\r\n", "X: a\r\n".repeat(33));
        assert_eq!(parse_connect(large.as_bytes(), &limits), Err(HttpError::TooLarge));
        assert_eq!(parse_connect(&vec![b'a'; 8193], &limits), Err(HttpError::TooLarge));
    }
}
