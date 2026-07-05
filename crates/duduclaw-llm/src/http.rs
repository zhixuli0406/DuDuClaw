//! Shared HTTP client singleton + header helpers.

use std::time::Duration;

/// Shared HTTP client — avoids rebuilding the connection pool per request
/// (same policy as the gateway's `direct_api.rs`: 120s total / 10s connect).
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

pub(crate) fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

/// Parse a `Retry-After` header (delta-seconds form only — HTTP-date form is
/// rare on LLM APIs and safely ignored → default cooldown applies).
pub(crate) fn retry_after_of(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    #[test]
    fn retry_after_parses_seconds_and_ignores_dates() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("30"));
        assert_eq!(retry_after_of(&h), Some(Duration::from_secs(30)));

        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"));
        assert_eq!(retry_after_of(&h), None);

        assert_eq!(retry_after_of(&HeaderMap::new()), None);
    }
}
