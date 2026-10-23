//! Inbound webhook JWT verification (RS256 + remote JWKS).
//!
//! Shared by the Google Chat channel (tokens signed by
//! `chat@system.gserviceaccount.com`, JWKS at
//! `https://www.googleapis.com/service_accounts/v1/jwk/chat@system.gserviceaccount.com`)
//! and the Microsoft Teams channel (Bot Framework Connector tokens, JWKS at
//! `https://login.botframework.com/v1/.well-known/keys`).
//!
//! Security gate — FAILS CLOSED: any fetch/parse/signature/claim error
//! rejects the request. JWKS documents are cached for 24h per URL and
//! refreshed on unknown `kid` (key rotation).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use tokio::sync::RwLock;

/// JWKS cache TTL (Microsoft guidance: refresh at least every 24h).
const JWKS_TTL: Duration = Duration::from_secs(24 * 3600);
/// Least time between two forced refetches of one JWKS URL. An unknown `kid`
/// forces a refetch (key rotation); without a floor every unauthenticated
/// request with a random `kid` made an outbound fetch.
const FORCED_REFETCH_MIN: Duration = Duration::from_secs(60);

static LAST_FORCED: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();

/// Whether a forced refetch of `jwks_url` may run now (and record it).
fn forced_refetch_allowed(jwks_url: &str) -> bool {
    let mut map = LAST_FORCED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match map.get(jwks_url) {
        Some(at) if now.duration_since(*at) < FORCED_REFETCH_MIN => false,
        _ => {
            map.insert(jwks_url.to_string(), now);
            true
        }
    }
}

struct CachedJwks {
    /// kid → (n, e) RSA components (base64url).
    keys: HashMap<String, (String, String)>,
    fetched_at: Instant,
}

static JWKS_CACHE: OnceLock<RwLock<HashMap<String, CachedJwks>>> = OnceLock::new();

fn jwks_cache() -> &'static RwLock<HashMap<String, CachedJwks>> {
    JWKS_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Fetch (or reuse cached) JWKS and return the RSA components for `kid`.
async fn rsa_components_for_kid(
    http: &reqwest::Client,
    jwks_url: &str,
    kid: &str,
    force_refresh: bool,
) -> Result<(String, String), String> {
    let cached_fresh = {
        let cache = jwks_cache().read().await;
        match cache.get(jwks_url) {
            Some(entry) if entry.fetched_at.elapsed() < JWKS_TTL => {
                if !force_refresh {
                    // Unknown kid: the caller retries with a (rate-limited)
                    // forced refetch.
                    return entry
                        .keys
                        .get(kid)
                        .cloned()
                        .ok_or_else(|| format!("kid {kid} not in cached JWKS"));
                }
                true
            }
            _ => false,
        }
    };
    // Unknown kid on a fresh document: refetch at most once a minute.
    if force_refresh && cached_fresh && !forced_refetch_allowed(jwks_url) {
        return Err(format!("kid {kid} not found in JWKS"));
    }

    let resp = http
        .get(jwks_url)
        .send()
        .await
        .map_err(|e| format!("JWKS fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("JWKS fetch status {}", resp.status()));
    }
    let doc: serde_json::Value = resp.json().await.map_err(|e| format!("JWKS parse: {e}"))?;
    let mut keys: HashMap<String, (String, String)> = HashMap::new();
    for key in doc
        .get("keys")
        .and_then(|k| k.as_array())
        .unwrap_or(&vec![])
    {
        let (Some(kid), Some(n), Some(e)) = (
            key.get("kid").and_then(|v| v.as_str()),
            key.get("n").and_then(|v| v.as_str()),
            key.get("e").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if key.get("kty").and_then(|v| v.as_str()) != Some("RSA") {
            continue;
        }
        keys.insert(kid.to_string(), (n.to_string(), e.to_string()));
    }

    let found = keys.get(kid).cloned();
    jwks_cache().write().await.insert(
        jwks_url.to_string(),
        CachedJwks {
            keys,
            fetched_at: Instant::now(),
        },
    );
    found.ok_or_else(|| format!("kid {kid} not found in JWKS"))
}

/// Verify an RS256 JWT against a remote JWKS. Checks signature, `exp`,
/// `iss`, and `aud`. Returns the decoded claims on success.
///
/// Fail-closed: every error path rejects.
pub async fn verify_rs256(
    http: &reqwest::Client,
    token: &str,
    jwks_url: &str,
    expected_issuer: &str,
    expected_audience: &str,
) -> Result<serde_json::Value, String> {
    let header = decode_header(token).map_err(|e| format!("JWT header: {e}"))?;
    if header.alg != Algorithm::RS256 {
        return Err(format!("unexpected alg {:?}", header.alg));
    }
    let kid = header.kid.ok_or("JWT missing kid")?;

    // First try the cache; on unknown kid force one refresh (rotation).
    let (n, e) = match rsa_components_for_kid(http, jwks_url, &kid, false).await {
        Ok(k) => k,
        Err(_) => rsa_components_for_kid(http, jwks_url, &kid, true).await?,
    };
    let key = DecodingKey::from_rsa_components(&n, &e).map_err(|e| format!("JWKS key: {e}"))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[expected_issuer]);
    validation.set_audience(&[expected_audience]);
    validation.leeway = 300; // 5-min clock skew

    let data = decode::<serde_json::Value>(token, &key, &validation)
        .map_err(|e| format!("JWT verify: {e}"))?;
    Ok(data.claims)
}

/// Extract the Bearer token from an Authorization header value.
pub fn bearer_token(auth_header: &str) -> Option<&str> {
    auth_header
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_refetch_is_rate_limited_per_url() {
        let url = format!("http://127.0.0.1:1/jwks-{}", uuid::Uuid::new_v4());
        assert!(forced_refetch_allowed(&url));
        assert!(!forced_refetch_allowed(&url), "second forced refetch inside a minute is refused");
        assert!(forced_refetch_allowed(&format!("{url}-other")), "per URL");
    }

    #[test]
    fn bearer_extraction() {
        assert_eq!(bearer_token("Bearer abc.def.ghi"), Some("abc.def.ghi"));
        assert_eq!(bearer_token("Basic xyz"), None);
        assert_eq!(bearer_token("Bearer "), None);
    }
}
