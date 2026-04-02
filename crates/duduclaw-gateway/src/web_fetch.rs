//! HTTP fetch with caching, rate limiting, and SSRF prevention.
//!
//! Implements the L1 layer of the browser automation pipeline — plain HTTP
//! fetch before escalating to headless browsers or computer use.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum response body size (5 MB).
const MAX_RESPONSE_SIZE: u64 = 5 * 1024 * 1024;

/// Default cache TTL in seconds (24 hours).
const DEFAULT_TTL_SECONDS: u64 = 86_400;

/// HTTP client timeout in seconds.
const CLIENT_TIMEOUT_SECONDS: u64 = 30;

/// Default rate limit: requests per minute per agent.
const DEFAULT_RATE_LIMIT: usize = 10;

/// Rate limit sliding window duration.
const RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors returned by the web fetch layer.
#[derive(Debug)]
pub enum FetchError {
    /// The URL targets an internal/dangerous resource.
    SsrfBlocked(String),
    /// The agent has exceeded its rate limit.
    RateLimited,
    /// The response body exceeds `MAX_RESPONSE_SIZE`.
    TooLarge,
    /// The request timed out.
    Timeout,
    /// Non-2xx HTTP response.
    HttpError(u16, String),
    /// Filesystem or serialization I/O error.
    IoError(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SsrfBlocked(reason) => write!(f, "SSRF blocked: {reason}"),
            Self::RateLimited => write!(f, "rate limit exceeded"),
            Self::TooLarge => write!(f, "response exceeds {MAX_RESPONSE_SIZE} bytes"),
            Self::Timeout => write!(f, "request timed out"),
            Self::HttpError(code, msg) => write!(f, "HTTP {code}: {msg}"),
            Self::IoError(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

/// Successful fetch result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub url: String,
    pub status_code: u16,
    pub content_type: String,
    pub body: String,
    pub cached: bool,
    pub fetched_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// SSRF prevention
// ---------------------------------------------------------------------------

/// Validate that a URL is safe to fetch (not targeting internal resources).
fn validate_url(url: &str) -> Result<reqwest::Url, FetchError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| FetchError::SsrfBlocked(format!("invalid URL: {e}")))?;

    // Block dangerous schemes.
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(FetchError::SsrfBlocked(format!(
            "blocked scheme: {scheme}://"
        )));
    }

    // Resolve the host.
    let host = parsed
        .host_str()
        .ok_or_else(|| FetchError::SsrfBlocked("missing host".into()))?;

    // Block well-known internal hostnames.
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower == "0.0.0.0" {
        return Err(FetchError::SsrfBlocked(format!(
            "blocked host: {host_lower}"
        )));
    }

    // Block cloud metadata endpoints (GCP, AWS, Azure).
    // Note: DNS rebinding is a known limitation — a hostname that resolves to
    // a public IP at validation time may re-resolve to a metadata IP during the
    // actual request. This is mitigated by adding the Metadata-Flavor header on
    // outgoing requests and by blocking the well-known metadata hostnames here.
    let blocked_metadata_hosts = [
        "metadata.google.internal",
        "169.254.169.254",
        "metadata.azure.com",
        "169.254.170.2",
    ];
    if blocked_metadata_hosts.iter().any(|h| host_lower == *h) {
        return Err(FetchError::SsrfBlocked(format!(
            "blocked metadata endpoint: {host_lower}"
        )));
    }

    // Try to parse as IP and check for internal ranges.
    // Strip brackets for IPv6 addresses (host_str returns "[::1]" for IPv6).
    let host_bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_bare.parse::<IpAddr>() {
        if is_internal_ip(&ip) {
            return Err(FetchError::SsrfBlocked(format!(
                "blocked internal IP: {ip}"
            )));
        }
    }

    Ok(parsed)
}

/// Returns `true` if the IP address belongs to a private/reserved range.
fn is_internal_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8
            octets[0] == 127
            // 10.0.0.0/8
            || octets[0] == 10
            // 172.16.0.0/12
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 (link-local)
            || (octets[0] == 169 && octets[1] == 254)
            // 0.0.0.0
            || (octets[0] == 0 && octets[1] == 0 && octets[2] == 0 && octets[3] == 0)
        }
        IpAddr::V6(v6) => {
            // ::1
            v6.is_loopback()
            // fc00::/7 (unique local)
            || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

// ---------------------------------------------------------------------------
// Cache helpers
// ---------------------------------------------------------------------------

/// Normalize a URL for consistent cache key derivation (R4-M1).
///
/// Lowercases the host, removes trailing slashes from the path, and sorts
/// query parameters so that semantically equivalent URLs map to the same key.
fn normalize_cache_url(url: &str) -> String {
    if let Ok(mut parsed) = reqwest::Url::parse(url) {
        // Lowercase the host
        if let Some(host) = parsed.host_str() {
            let lower_host = host.to_lowercase();
            let _ = parsed.set_host(Some(&lower_host));
        }
        // Remove trailing slash from non-root path
        let path = parsed.path().trim_end_matches('/').to_string();
        parsed.set_path(if path.is_empty() { "/" } else { &path });
        // Sort query parameters
        if let Some(query) = parsed.query() {
            if !query.is_empty() {
                let mut params: Vec<&str> = query.split('&').collect();
                params.sort_unstable();
                parsed.set_query(Some(&params.join("&")));
            }
        }
        parsed.to_string()
    } else {
        url.to_string()
    }
}

/// Derive a cache file path from the URL.
fn cache_path(url: &str, cache_dir: &Path) -> PathBuf {
    let normalized = normalize_cache_url(url);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = hex::encode(hasher.finalize());
    cache_dir.join(hash)
}

/// Metadata stored alongside the cached body.
#[derive(Debug, Serialize, Deserialize)]
struct CacheMeta {
    url: String,
    status_code: u16,
    content_type: String,
    fetched_at: DateTime<Utc>,
}

/// Try to load a cached response if it exists and hasn't expired.
fn load_cache(url: &str, ttl_seconds: u64, cache_dir: &Path) -> Option<FetchResult> {
    let body_path = cache_path(url, cache_dir);
    let meta_path = body_path.with_extension("meta.json");

    let meta_bytes = std::fs::read(&meta_path).ok()?;
    let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;

    // Check TTL.
    let age = Utc::now().signed_duration_since(meta.fetched_at);
    if age.num_seconds() as u64 > ttl_seconds {
        return None;
    }

    let body = std::fs::read_to_string(&body_path).ok()?;

    Some(FetchResult {
        url: meta.url,
        status_code: meta.status_code,
        content_type: meta.content_type,
        body,
        cached: true,
        fetched_at: meta.fetched_at,
    })
}

/// Persist a fetch result to the cache directory.
fn save_cache(result: &FetchResult, cache_dir: &Path) -> Result<(), FetchError> {
    std::fs::create_dir_all(cache_dir).map_err(|e| FetchError::IoError(e.to_string()))?;

    let body_path = cache_path(&result.url, cache_dir);
    let meta_path = body_path.with_extension("meta.json");

    let meta = CacheMeta {
        url: result.url.clone(),
        status_code: result.status_code,
        content_type: result.content_type.clone(),
        fetched_at: result.fetched_at,
    };

    let meta_json =
        serde_json::to_vec_pretty(&meta).map_err(|e| FetchError::IoError(e.to_string()))?;

    std::fs::write(&body_path, &result.body).map_err(|e| FetchError::IoError(e.to_string()))?;
    std::fs::write(&meta_path, &meta_json).map_err(|e| FetchError::IoError(e.to_string()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

/// Sliding-window rate limiter keyed by agent ID.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    max_requests: usize,
}

impl RateLimiter {
    /// Create a new rate limiter with the default limit.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_requests: DEFAULT_RATE_LIMIT,
        }
    }

    /// Create a rate limiter with a custom per-agent limit.
    pub fn with_limit(max_requests: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, agent_id: &str) -> bool {
        let mut map = self.inner.lock().expect("rate limiter lock poisoned");
        let window = map.entry(agent_id.to_string()).or_default();
        let now = Instant::now();

        // Discard entries older than the sliding window.
        while let Some(&front) = window.front() {
            if now.duration_since(front) > RATE_WINDOW {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() >= self.max_requests {
            return false;
        }

        window.push_back(now);
        true
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch a URL with disk caching and SSRF validation.
///
/// Checks the local cache first; on miss, performs an HTTP GET and stores the
/// response. The cache key is the SHA-256 of the URL.
pub async fn web_fetch_cached(
    url: &str,
    ttl_seconds: u64,
    cache_dir: &Path,
) -> Result<FetchResult, FetchError> {
    // 1. SSRF check.
    let validated = validate_url(url)?;

    // 2. Cache lookup.
    let ttl = if ttl_seconds == 0 {
        DEFAULT_TTL_SECONDS
    } else {
        ttl_seconds
    };

    if let Some(hit) = load_cache(url, ttl, cache_dir) {
        info!(url, "web_fetch cache hit");
        return Ok(hit);
    }

    // 3. Live fetch.
    info!(url, "web_fetch cache miss — fetching");

    // DNS rebinding defence: resolve the hostname now, verify the IP is not
    // internal, then pin the resolved address into the per-request client so
    // the same IP is used for the connection even if the DNS record changes
    // between validation and the actual TCP handshake.
    let host = validated
        .host_str()
        .ok_or_else(|| FetchError::SsrfBlocked("missing host".into()))?
        .to_string();
    let port = validated.port_or_known_default().unwrap_or(443);

    // Synchronous DNS resolution — runs on the blocking thread pool via
    // spawn_blocking to avoid blocking the async executor.
    let host_for_dns = host.clone();
    let resolved_addr = tokio::task::spawn_blocking(move || {
        format!("{host_for_dns}:{port}")
            .to_socket_addrs()
            .map_err(|e| FetchError::SsrfBlocked(format!("DNS resolution failed: {e}")))?
            .next()
            .ok_or_else(|| FetchError::SsrfBlocked("DNS returned no results".into()))
    })
    .await
    .map_err(|e| FetchError::SsrfBlocked(format!("DNS task failed: {e}")))??;

    // Re-check the resolved IP — catches DNS rebinding to internal ranges.
    if is_internal_ip(&resolved_addr.ip()) {
        warn!(host = %host, ip = %resolved_addr.ip(), "DNS rebinding detected: resolved to private IP");
        return Err(FetchError::SsrfBlocked(format!(
            "DNS rebinding detected: {host} resolved to private IP {}",
            resolved_addr.ip()
        )));
    }

    // Build a one-shot client with the IP pinned so reqwest dials the
    // same address we just validated, regardless of later DNS changes.
    //
    // SEC2-M12: Use a custom redirect policy that re-validates each redirect
    // target for SSRF. The default `limited(5)` follows redirects blindly and
    // can be used to bypass the initial SSRF check (e.g. open-redirect to
    // http://169.254.169.254/).
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        let url = attempt.url();
        // Block non-http(s) schemes in redirects.
        if url.scheme() != "http" && url.scheme() != "https" {
            return attempt.stop();
        }
        // Block internal hosts in redirect targets.
        if let Some(host) = url.host_str() {
            let h = host.to_ascii_lowercase();
            if h == "localhost"
                || h == "0.0.0.0"
                || h == "169.254.169.254"
                || h == "169.254.170.2"
                || h == "metadata.google.internal"
                || h == "metadata.azure.com"
            {
                return attempt.stop();
            }
            // Block bare IP redirects into private ranges.
            let bare = h.trim_start_matches('[').trim_end_matches(']');
            if let Ok(ip) = bare.parse::<IpAddr>() {
                if is_internal_ip(&ip) {
                    return attempt.stop();
                }
            }
        }
        // NOTE: redirect targets are validated for internal IPs but DNS is not re-pinned.
        // Full DNS re-pinning on redirect requires per-redirect client rebuilding,
        // which is not practical. The IP validation in the redirect policy provides
        // reasonable protection against known internal ranges.
        if attempt.previous().len() >= 5 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });

    let pinned_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(CLIENT_TIMEOUT_SECONDS))
        .redirect(redirect_policy)
        .resolve(&host, resolved_addr)
        .build()
        .map_err(|e| FetchError::IoError(format!("failed to build pinned client: {e}")))?;

    let response = pinned_client
        .get(validated.as_str())
        .header("User-Agent", "DuDuClaw/1.0")
        // Blocks GCP metadata server from responding to SSRF requests.
        // See: https://cloud.google.com/compute/docs/metadata/querying-metadata
        .header("Metadata-Flavor", "none")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::HttpError(0, e.to_string())
            }
        })?;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // Check Content-Length before downloading.
    if let Some(len) = response.content_length() {
        if len > MAX_RESPONSE_SIZE {
            warn!(url, len, "response too large");
            return Err(FetchError::TooLarge);
        }
    }

    let bytes = response.bytes().await.map_err(|e| {
        if e.is_timeout() {
            FetchError::Timeout
        } else {
            FetchError::IoError(e.to_string())
        }
    })?;

    if bytes.len() as u64 > MAX_RESPONSE_SIZE {
        return Err(FetchError::TooLarge);
    }

    if !(200..300).contains(&status) {
        return Err(FetchError::HttpError(
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
    }

    let body = String::from_utf8_lossy(&bytes).into_owned();

    let result = FetchResult {
        url: url.to_string(),
        status_code: status,
        content_type,
        body,
        cached: false,
        fetched_at: Utc::now(),
    };

    // 4. Persist to cache (best-effort).
    if let Err(e) = save_cache(&result, cache_dir) {
        warn!(%e, "failed to save web_fetch cache");
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- SSRF validation tests --

    #[test]
    fn blocks_file_scheme() {
        let err = validate_url("file:///etc/passwd").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_javascript_scheme() {
        let err = validate_url("javascript:alert(1)").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_data_scheme() {
        let err = validate_url("data:text/html,<h1>hi</h1>").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_localhost() {
        let err = validate_url("http://localhost/secret").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_zero_address() {
        let err = validate_url("http://0.0.0.0/").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_loopback_ipv4() {
        let err = validate_url("http://127.0.0.1/admin").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_private_10() {
        let err = validate_url("http://10.0.0.1/").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_private_172() {
        let err = validate_url("http://172.16.0.1/").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_private_192() {
        let err = validate_url("http://192.168.1.1/").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_link_local() {
        let err = validate_url("http://169.254.169.254/metadata").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn blocks_ipv6_loopback() {
        let err = validate_url("http://[::1]/").unwrap_err();
        assert!(matches!(err, FetchError::SsrfBlocked(_)));
    }

    #[test]
    fn allows_public_https() {
        assert!(validate_url("https://example.com/page").is_ok());
    }

    #[test]
    fn allows_public_http() {
        assert!(validate_url("http://example.com").is_ok());
    }

    // -- Cache tests --

    #[test]
    fn cache_miss_then_hit() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/test";

        // Initially no cache.
        assert!(load_cache(url, DEFAULT_TTL_SECONDS, dir.path()).is_none());

        // Save a result.
        let result = FetchResult {
            url: url.to_string(),
            status_code: 200,
            content_type: "text/html".to_string(),
            body: "<h1>Hello</h1>".to_string(),
            cached: false,
            fetched_at: Utc::now(),
        };
        save_cache(&result, dir.path()).unwrap();

        // Now it should hit.
        let hit = load_cache(url, DEFAULT_TTL_SECONDS, dir.path()).unwrap();
        assert!(hit.cached);
        assert_eq!(hit.body, "<h1>Hello</h1>");
        assert_eq!(hit.status_code, 200);
    }

    #[test]
    fn cache_expired() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/expire";

        let result = FetchResult {
            url: url.to_string(),
            status_code: 200,
            content_type: "text/plain".to_string(),
            body: "old".to_string(),
            cached: false,
            fetched_at: Utc::now() - chrono::Duration::hours(25),
        };
        save_cache(&result, dir.path()).unwrap();

        // With 24h TTL the entry is stale.
        assert!(load_cache(url, DEFAULT_TTL_SECONDS, dir.path()).is_none());
    }

    // -- Rate limiter tests --

    #[test]
    fn rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::with_limit(3);
        assert!(limiter.check("agent-1"));
        assert!(limiter.check("agent-1"));
        assert!(limiter.check("agent-1"));
        // Fourth should be denied.
        assert!(!limiter.check("agent-1"));
    }

    #[test]
    fn rate_limiter_isolates_agents() {
        let limiter = RateLimiter::with_limit(1);
        assert!(limiter.check("agent-a"));
        assert!(!limiter.check("agent-a"));
        // Different agent is unaffected.
        assert!(limiter.check("agent-b"));
    }

    #[test]
    fn rate_limiter_default_is_ten() {
        let limiter = RateLimiter::new();
        for _ in 0..10 {
            assert!(limiter.check("agent-x"));
        }
        assert!(!limiter.check("agent-x"));
    }
}
