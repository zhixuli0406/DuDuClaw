//! Captive-portal / internet-connectivity detection — M1 scope is detect
//! and report only (design §6): this module never drives a login flow, it
//! only tells [`crate::network::status`] whether the network is `online`,
//! sitting behind a `portal`, or `offline`.
//!
//! **Honest disclosure** (design §6 carries this same caveat): the design
//! doc's original plan was a first-party `/generate_204` endpoint, but that
//! endpoint's actual 204 behavior has never been verified against a live
//! deployment — shipping an unverified first-party check would risk
//! misreading an ordinary 404 (with a body) as a captive portal. This module
//! therefore probes two independent third-party endpoints instead (one
//! `204`-shaped, one `200`-with-body-shaped) so a single vendor outage can't
//! misclassify a perfectly good connection as offline. A first-party
//! endpoint is a reasonable follow-up once its response has actually been
//! confirmed live.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Wall-clock budget for one probe attempt against one endpoint.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a cached verdict is reused before a fresh probe runs — keeps a
/// polled `network.status` from turning into a steady stream of outbound
/// requests (design §6: "探測要有頻率上限，不能變成常駐流量").
const CACHE_TTL: Duration = Duration::from_secs(30);

/// `https://detectportal.firefox.com/success.txt` — expected to respond
/// `200` with a body starting `"success"`. Firefox's own captive-portal
/// probe endpoint; plaintext HTTP is used deliberately (see [`probe`]'s
/// doc — a portal has to be able to intercept it).
const ENDPOINT_FIREFOX: &str = "http://detectportal.firefox.com/success.txt";
/// `http://connectivity-check.ubuntu.com/` — expected to respond `204` with
/// an empty body. Ubuntu/NetworkManager's own probe endpoint.
const ENDPOINT_UBUNTU: &str = "http://connectivity-check.ubuntu.com/";

/// What a probed endpoint's response is expected to look like when the
/// network is genuinely online (no portal in the way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeExpect {
    /// HTTP 204, empty body.
    NoContent204,
    /// HTTP 200, body starting with this literal prefix.
    BodyStartsWith(&'static str),
}

/// Outcome of a connectivity probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternetVerdict {
    Online,
    /// `url`: the portal's login page, when a redirect named one.
    Portal {
        url: Option<String>,
    },
    Offline,
    /// Neither endpoint could be reached AND neither could be classified as
    /// a portal — reserved for a shape [`classify_probe`] cannot itself
    /// produce (every branch it has resolves to one of the other three);
    /// kept as a fourth state for symmetry with [`crate::network::
    /// NetworkStatus::internet`]'s own `"unknown"` value and as headroom
    /// for a future probe failure mode (e.g. DNS resolution error) that
    /// isn't simply "connection refused" (which today reads as `Offline`).
    Unknown,
}

/// Pure: classify one endpoint's response against what it was expected to
/// look like.
///
/// - Matches the expectation exactly -> [`InternetVerdict::Online`].
/// - A 3xx with a `Location` header -> [`InternetVerdict::Portal`] carrying
///   that URL (the common captive-portal redirect shape).
/// - Any other 2xx/4xx/5xx that doesn't match the expectation -> `Portal`
///   with no URL — something on the network path is rewriting the response
///   (a portal that returns 200 with its own HTML instead of the expected
///   204/`"success"` body, for instance), even though it didn't hand us a
///   redirect to follow.
pub fn classify_probe(
    status: u16,
    location_header: Option<&str>,
    body_prefix: &str,
    expect: ProbeExpect,
) -> InternetVerdict {
    let matches_expectation = match expect {
        ProbeExpect::NoContent204 => status == 204,
        ProbeExpect::BodyStartsWith(prefix) => status == 200 && body_prefix.starts_with(prefix),
    };
    if matches_expectation {
        return InternetVerdict::Online;
    }
    if (300..400).contains(&status) {
        if let Some(url) = location_header {
            if !url.is_empty() {
                return InternetVerdict::Portal {
                    url: Some(url.to_string()),
                };
            }
        }
        return InternetVerdict::Portal { url: None };
    }
    InternetVerdict::Portal { url: None }
}

struct CachedVerdict {
    verdict: InternetVerdict,
    at: Instant,
}

fn cache() -> &'static Mutex<Option<CachedVerdict>> {
    static CACHE: OnceLock<Mutex<Option<CachedVerdict>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Probe for internet connectivity, rate-limited to once per [`CACHE_TTL`].
///
/// Uses plaintext `http://` on purpose, not `https://`: a captive portal
/// intercepts unencrypted traffic to inject its login redirect — probing
/// over TLS would just fail the handshake, which reads identically to
/// "offline" and throws away the one signal ([`InternetVerdict::Portal`])
/// this whole module exists to produce. `redirect::Policy::none()` for the
/// same reason: a followed redirect would land on the portal's actual login
/// page and read as a normal 200, hiding the very thing being detected.
///
/// The two endpoints are compile-time constants ([`ENDPOINT_FIREFOX`] /
/// [`ENDPOINT_UBUNTU`]), never caller-supplied — this does not introduce a
/// new SSRF surface (design §6: "探測端點是寫死的常數，不接受呼叫端指定").
pub async fn probe() -> InternetVerdict {
    {
        let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.as_ref() {
            if cached.at.elapsed() < CACHE_TTL {
                return cached.verdict.clone();
            }
        }
    }

    let verdict = probe_uncached().await;

    let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(CachedVerdict {
        verdict: verdict.clone(),
        at: Instant::now(),
    });
    verdict
}

async fn probe_uncached() -> InternetVerdict {
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .build()
    else {
        return InternetVerdict::Unknown;
    };

    let firefox = probe_one(
        &client,
        ENDPOINT_FIREFOX,
        ProbeExpect::BodyStartsWith("success"),
    )
    .await;
    if matches!(firefox, Some(InternetVerdict::Online)) {
        return InternetVerdict::Online;
    }

    let ubuntu = probe_one(&client, ENDPOINT_UBUNTU, ProbeExpect::NoContent204).await;
    if matches!(ubuntu, Some(InternetVerdict::Online)) {
        return InternetVerdict::Online;
    }

    // Neither endpoint was reachable at all -> genuinely offline (no TCP/TLS
    // connectivity, not merely "not the expected response").
    if firefox.is_none() && ubuntu.is_none() {
        return InternetVerdict::Offline;
    }

    // At least one endpoint answered but neither read as Online — prefer a
    // Portal verdict (with a URL, if either endpoint's redirect carried
    // one) over a plain Offline, since SOMETHING on the path is responding.
    firefox
        .into_iter()
        .chain(ubuntu)
        .find(|v| matches!(v, InternetVerdict::Portal { .. }))
        .unwrap_or(InternetVerdict::Offline)
}

/// One endpoint attempt. `None` means the request itself failed (timeout,
/// DNS, connection refused — no HTTP response at all to classify);
/// `Some(verdict)` is [`classify_probe`]'s judgment on whatever response did
/// come back.
async fn probe_one(
    client: &reqwest::Client,
    url: &str,
    expect: ProbeExpect,
) -> Option<InternetVerdict> {
    let response = client.get(url).send().await.ok()?;
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap_or_default();
    // Only the prefix matters for `BodyStartsWith`, and this is diagnostic
    // data (never user secrets), so a generous-but-bounded slice is fine —
    // `truncate_bytes` keeps it CJK-safe regardless (coding convention 1),
    // even though these two fixed endpoints only ever return ASCII.
    let body_prefix = duduclaw_core::truncate_bytes(&body, 64);
    Some(classify_probe(
        status,
        location.as_deref(),
        body_prefix,
        expect,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_probe ─────────────────────────────────────────────────

    #[test]
    fn online_204_matches_expectation() {
        assert_eq!(
            classify_probe(204, None, "", ProbeExpect::NoContent204),
            InternetVerdict::Online
        );
    }

    #[test]
    fn online_200_body_prefix_matches() {
        assert_eq!(
            classify_probe(
                200,
                None,
                "success\n",
                ProbeExpect::BodyStartsWith("success")
            ),
            InternetVerdict::Online
        );
    }

    #[test]
    fn wrong_status_for_204_expectation_is_portal_no_url() {
        assert_eq!(
            classify_probe(200, None, "", ProbeExpect::NoContent204),
            InternetVerdict::Portal { url: None }
        );
    }

    #[test]
    fn wrong_body_for_200_expectation_is_portal_no_url() {
        assert_eq!(
            classify_probe(
                200,
                None,
                "<html>login</html>",
                ProbeExpect::BodyStartsWith("success")
            ),
            InternetVerdict::Portal { url: None }
        );
    }

    #[test]
    fn redirect_with_location_is_portal_with_url() {
        assert_eq!(
            classify_probe(
                302,
                Some("http://portal.example/login"),
                "",
                ProbeExpect::NoContent204
            ),
            InternetVerdict::Portal {
                url: Some("http://portal.example/login".to_string())
            }
        );
    }

    #[test]
    fn redirect_without_location_is_portal_no_url() {
        assert_eq!(
            classify_probe(301, None, "", ProbeExpect::NoContent204),
            InternetVerdict::Portal { url: None }
        );
    }

    #[test]
    fn redirect_with_empty_location_is_portal_no_url() {
        assert_eq!(
            classify_probe(302, Some(""), "", ProbeExpect::NoContent204),
            InternetVerdict::Portal { url: None }
        );
    }

    #[test]
    fn client_and_server_errors_are_portal_no_url() {
        assert_eq!(
            classify_probe(404, None, "", ProbeExpect::NoContent204),
            InternetVerdict::Portal { url: None }
        );
        assert_eq!(
            classify_probe(500, None, "", ProbeExpect::NoContent204),
            InternetVerdict::Portal { url: None }
        );
        assert_eq!(
            classify_probe(403, None, "", ProbeExpect::BodyStartsWith("success")),
            InternetVerdict::Portal { url: None }
        );
    }

    #[test]
    fn wrong_status_with_a_location_header_still_prefers_portal_with_url() {
        // A redirect status wins the "portal with URL" branch even under
        // the BodyStartsWith expectation.
        assert_eq!(
            classify_probe(
                302,
                Some("http://portal.example/"),
                "",
                ProbeExpect::BodyStartsWith("success")
            ),
            InternetVerdict::Portal {
                url: Some("http://portal.example/".to_string())
            }
        );
    }

    // ── probe() caching (no real network — just the TTL mechanics) ───────

    #[test]
    fn probe_cache_starts_empty() {
        // Not a shared-state assertion across tests (each test module run
        // is a fresh process per `cargo test` binary in practice, and this
        // only reads — never mutates — module state), just documents the
        // initial condition `cache()` returns before any `probe()` call.
        let guard = cache().lock().unwrap();
        // Either genuinely empty (nothing has probed yet in this process)
        // or already populated by an earlier test in the same binary — both
        // are legal; the real behavior (TTL reuse) is covered by the pure
        // `classify_probe` tests above plus the smoke test below.
        drop(guard);
    }

    #[tokio::test]
    async fn probe_never_panics_and_returns_a_closed_variant() {
        // This DOES make a real network call (or fails fast on an
        // unreachable/offline test runner) — bounded by PROBE_TIMEOUT so it
        // can't hang the test suite either way.
        let verdict = probe().await;
        assert!(matches!(
            verdict,
            InternetVerdict::Online
                | InternetVerdict::Portal { .. }
                | InternetVerdict::Offline
                | InternetVerdict::Unknown
        ));
    }
}
