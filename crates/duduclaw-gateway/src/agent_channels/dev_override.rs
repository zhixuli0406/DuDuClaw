//! Debug-build-only API origin overrides for live tests against local fakes
//! (`DUDUCLAW_DEV_TELEGRAM_API_BASE`, `DUDUCLAW_DEV_GOOGLECHAT_API_BASE`,
//! `DUDUCLAW_DEV_GOOGLECHAT_JWKS_URL`). The whole module is compiled out of
//! release builds, and only a loopback `http://` origin is honoured.

/// The override in `var`, when it names a loopback `http://` URL. Trailing
/// slashes are trimmed. Anything else is ignored with a warning.
pub fn loopback_origin(var: &str) -> Option<String> {
    let raw = std::env::var(var).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if is_loopback_http(raw) {
        Some(raw.trim_end_matches('/').to_string())
    } else {
        tracing::warn!("{var} ignored: only a loopback http:// URL is accepted");
        None
    }
}

fn is_loopback_http(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_is_accepted() {
        assert!(is_loopback_http("http://127.0.0.1:9000"));
        assert!(is_loopback_http("http://localhost:9000/x"));
        assert!(!is_loopback_http("https://127.0.0.1:9000"));
        assert!(!is_loopback_http("http://api.telegram.org"));
        assert!(!is_loopback_http("http://127.0.0.1.evil.com"));
        assert!(!is_loopback_http("not a url"));
    }
}
