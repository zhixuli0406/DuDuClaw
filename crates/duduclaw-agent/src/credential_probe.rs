//! Zero-cost credential probing against the Anthropic API.
//!
//! ## Why this exists (2026-09-08 incident)
//!
//! The rotator's health probe used to "verify" an OAuth account by running
//! `claude auth status` and treating `loggedIn: true` as proof the token was
//! good. That signal is a lie in two directions:
//!
//! - it is `true` whenever *any* `CLAUDE_CODE_OAUTH_TOKEN` exists in the
//!   ambient environment, regardless of whether it still authenticates; and
//! - it says nothing about **which** account's token was checked — a pool of
//!   five accounts all "verify" against the same host session.
//!
//! On 2026-09-08 a setup-token started returning
//! `403 oauth_not_allowed_for_organization`. The rotator marked the account
//! unhealthy after three errors and the fake probe resurrected it 60 seconds
//! later, every 60 seconds, for 18 hours — each resurrection burning one more
//! scheduled dispatch.
//!
//! ## The real probe
//!
//! `GET https://api.anthropic.com/v1/models` with `anthropic-version:
//! 2023-06-01` answers the question for the *specific secret we hand it*,
//! consumes no tokens and costs nothing. Measured 2026-09-08 with real
//! credentials:
//!
//! | credential | headers | result |
//! |---|---|---|
//! | valid setup-token | `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20` | 200 |
//! | org-disabled setup-token | same | 403 |
//! | invalid / short-lived access token | same | 401 |
//!
//! API keys use `x-api-key` instead (Anthropic documents 401 =
//! `authentication_error` for a bad key).
//!
//! ## Secret hygiene
//!
//! The secret is never logged, never returned, and never embedded in a
//! [`CredentialProbe::Unknown`] payload — transport error text is redacted
//! against the secret before it is truncated (defence in depth: reqwest's
//! `Display` carries the URL, not the headers).

use std::time::Duration;

use duduclaw_core::truncate_bytes;

/// Production Anthropic API base. Split out so tests can point the probe at a
/// local listener without a network round-trip.
pub const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";

/// Anthropic API version header value the probe pins.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta header required for OAuth (setup-token) bearer authentication.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Probe timeout. A credential check must never become a stall — an
/// unreachable API is `Unknown`, which leaves the account untouched.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Byte budget for the free-form detail carried by [`CredentialProbe::Unknown`].
const UNKNOWN_DETAIL_MAX_BYTES: usize = 200;

/// Prefix of a short-lived Claude **access** token (`claude auth token`).
/// These expire in hours and must never be pasted in as an account credential.
pub const ACCESS_TOKEN_PREFIX: &str = "sk-ant-at01-";

/// Prefix of a long-lived Claude **setup** token (`claude setup-token`) —
/// the credential shape the rotator actually wants for an OAuth account.
pub const SETUP_TOKEN_PREFIX: &str = "sk-ant-oat01-";

/// Which authentication header the probed secret belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// Claude subscription OAuth token (`setup-token`) → `Authorization: Bearer`
    /// plus the OAuth beta header.
    OAuthToken,
    /// Anthropic API key → `x-api-key`.
    ApiKey,
}

/// Outcome of a single credential probe.
///
/// The three-way split between "the credential is bad" ([`InvalidCredential`],
/// [`OrgDisabled`]) and "we could not tell" ([`RateLimited`], [`Unknown`]) is
/// load-bearing: only the former may be used to keep an account dead, and only
/// the latter must leave account state untouched.
///
/// [`InvalidCredential`]: CredentialProbe::InvalidCredential
/// [`OrgDisabled`]: CredentialProbe::OrgDisabled
/// [`RateLimited`]: CredentialProbe::RateLimited
/// [`Unknown`]: CredentialProbe::Unknown
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialProbe {
    /// 200 — the credential authenticates right now.
    Valid,
    /// 401 — the credential is rejected (expired, revoked, malformed, or a
    /// short-lived access token pasted where a setup-token belongs).
    InvalidCredential,
    /// 403 — authentication succeeded but the organization has disabled this
    /// access path (`oauth_not_allowed_for_organization`). Waiting does not fix
    /// it; only an operator can.
    OrgDisabled,
    /// 429 — the probe itself was rate-limited. Says nothing about the
    /// credential.
    RateLimited,
    /// Anything else (5xx, an unexpected status, a transport failure). Carries
    /// a short, secret-free detail for logs. Says nothing about the credential.
    Unknown(String),
}

impl CredentialProbe {
    /// Whether this outcome proves the credential itself is unusable.
    ///
    /// `Valid` is not "conclusive" in this sense — it proves the opposite.
    /// `RateLimited` / `Unknown` are explicitly inconclusive so a network blip
    /// can never be mistaken for a dead token.
    pub fn is_conclusive_failure(&self) -> bool {
        matches!(self, Self::InvalidCredential | Self::OrgDisabled)
    }
}

/// Map an HTTP status code onto a probe outcome.
///
/// Pure and unit-tested so the classification can be reasoned about without a
/// network.
pub fn classify_probe_status(status: u16) -> CredentialProbe {
    match status {
        200 => CredentialProbe::Valid,
        401 => CredentialProbe::InvalidCredential,
        403 => CredentialProbe::OrgDisabled,
        429 => CredentialProbe::RateLimited,
        other => CredentialProbe::Unknown(format!("HTTP {other}")),
    }
}

/// Whether `s` looks like a short-lived Claude access token (`sk-ant-at01-…`).
///
/// Prefix test on the trimmed string — deliberately anchored (project coding
/// convention 2: no unanchored `contains` for a routing/security decision).
pub fn looks_like_access_token(s: &str) -> bool {
    s.trim().starts_with(ACCESS_TOKEN_PREFIX)
}

/// Whether `s` looks like a long-lived Claude setup token (`sk-ant-oat01-…`).
pub fn looks_like_setup_token(s: &str) -> bool {
    s.trim().starts_with(SETUP_TOKEN_PREFIX)
}

/// Probe a credential against the production Anthropic API.
///
/// See [`probe_anthropic_credential_at`] for the full semantics.
pub async fn probe_anthropic_credential(kind: CredentialKind, secret: &str) -> CredentialProbe {
    probe_anthropic_credential_at(ANTHROPIC_API_BASE, kind, secret).await
}

/// Probe a credential against an arbitrary API base (tests inject a local
/// listener; production passes [`ANTHROPIC_API_BASE`]).
///
/// An empty / whitespace-only secret short-circuits to
/// [`CredentialProbe::InvalidCredential`] without a network call — that is
/// exactly the "decrypted to an empty string" trap that used to spawn
/// credential-less children.
pub async fn probe_anthropic_credential_at(
    base_url: &str,
    kind: CredentialKind,
    secret: &str,
) -> CredentialProbe {
    if secret.trim().is_empty() {
        return CredentialProbe::InvalidCredential;
    }

    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return CredentialProbe::Unknown(unknown_detail(
                &format!("client build failed: {e}"),
                secret,
            ));
        }
    };

    let mut req = client
        .get(&url)
        .header("anthropic-version", ANTHROPIC_VERSION);
    match kind {
        CredentialKind::OAuthToken => {
            req = req
                .header("authorization", format!("Bearer {secret}"))
                .header("anthropic-beta", OAUTH_BETA);
        }
        CredentialKind::ApiKey => {
            req = req.header("x-api-key", secret);
        }
    }

    match req.send().await {
        Ok(resp) => classify_probe_status(resp.status().as_u16()),
        Err(e) => CredentialProbe::Unknown(unknown_detail(&e.to_string(), secret)),
    }
}

/// Build the free-form detail for [`CredentialProbe::Unknown`]: redact any
/// literal occurrence of the secret, then truncate on a char boundary.
///
/// Redaction happens **before** truncation so a cut can never expose a partial
/// secret that the redactor would otherwise have caught.
fn unknown_detail(raw: &str, secret: &str) -> String {
    let redacted = if secret.trim().is_empty() {
        raw.to_string()
    } else {
        raw.replace(secret, "***")
    };
    truncate_bytes(&redacted, UNKNOWN_DETAIL_MAX_BYTES).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── status classification ────────────────────────────────────────

    #[test]
    fn status_200_is_valid() {
        assert_eq!(classify_probe_status(200), CredentialProbe::Valid);
    }

    #[test]
    fn status_401_is_invalid_credential() {
        assert_eq!(
            classify_probe_status(401),
            CredentialProbe::InvalidCredential
        );
    }

    #[test]
    fn status_403_is_org_disabled() {
        assert_eq!(classify_probe_status(403), CredentialProbe::OrgDisabled);
    }

    #[test]
    fn status_429_is_rate_limited() {
        assert_eq!(classify_probe_status(429), CredentialProbe::RateLimited);
    }

    /// Anything unmapped stays inconclusive — a 500 or a 404 must never be
    /// read as "the credential is dead".
    #[test]
    fn other_statuses_are_unknown_and_inconclusive() {
        for status in [201u16, 400, 404, 418, 500, 502, 503] {
            let got = classify_probe_status(status);
            assert_eq!(
                got,
                CredentialProbe::Unknown(format!("HTTP {status}")),
                "status {status} should classify as Unknown"
            );
            assert!(
                !got.is_conclusive_failure(),
                "status {status} must not be treated as a conclusive credential failure"
            );
        }
    }

    #[test]
    fn only_401_and_403_are_conclusive_failures() {
        assert!(CredentialProbe::InvalidCredential.is_conclusive_failure());
        assert!(CredentialProbe::OrgDisabled.is_conclusive_failure());
        assert!(!CredentialProbe::Valid.is_conclusive_failure());
        assert!(!CredentialProbe::RateLimited.is_conclusive_failure());
        assert!(!CredentialProbe::Unknown("boom".into()).is_conclusive_failure());
    }

    // ── token-shape helpers ──────────────────────────────────────────

    #[test]
    fn recognizes_short_lived_access_token() {
        assert!(looks_like_access_token("sk-ant-at01-abcdef"));
        assert!(looks_like_access_token("  sk-ant-at01-abcdef\n"));
        assert!(!looks_like_access_token("sk-ant-oat01-abcdef"));
        assert!(!looks_like_access_token("sk-ant-api03-abcdef"));
        assert!(!looks_like_access_token(""));
    }

    #[test]
    fn recognizes_setup_token() {
        assert!(looks_like_setup_token("sk-ant-oat01-abcdef"));
        assert!(looks_like_setup_token(" sk-ant-oat01-abcdef "));
        assert!(!looks_like_setup_token("sk-ant-at01-abcdef"));
        assert!(!looks_like_setup_token(""));
    }

    /// The prefixes must not overlap in either direction — the whole point of
    /// D6 is telling an `at01` paste apart from an `oat01` one.
    #[test]
    fn access_and_setup_prefixes_are_disjoint() {
        assert!(!ACCESS_TOKEN_PREFIX.starts_with(SETUP_TOKEN_PREFIX));
        assert!(!SETUP_TOKEN_PREFIX.starts_with(ACCESS_TOKEN_PREFIX));
    }

    // ── empty-secret short circuit ───────────────────────────────────

    /// An empty secret is invalid by construction and must not cost a request.
    /// The base URL below is unroutable on purpose: if a request were made,
    /// this would take the 10 s timeout and come back `Unknown`.
    #[tokio::test]
    async fn empty_secret_is_invalid_without_a_request() {
        for secret in ["", "   ", "\t\n"] {
            assert_eq!(
                probe_anthropic_credential_at(
                    "http://192.0.2.1:9",
                    CredentialKind::OAuthToken,
                    secret
                )
                .await,
                CredentialProbe::InvalidCredential
            );
            assert_eq!(
                probe_anthropic_credential_at("http://192.0.2.1:9", CredentialKind::ApiKey, secret)
                    .await,
                CredentialProbe::InvalidCredential
            );
        }
    }

    // ── detail redaction ─────────────────────────────────────────────

    #[test]
    fn unknown_detail_redacts_the_secret_and_caps_length() {
        let detail = unknown_detail(
            "error sending request for sk-ant-oat01-hunter2",
            "sk-ant-oat01-hunter2",
        );
        assert!(
            !detail.contains("hunter2"),
            "secret leaked into detail: {detail}"
        );
        assert!(detail.contains("***"));

        let long = "x".repeat(500);
        assert!(unknown_detail(&long, "unused").len() <= UNKNOWN_DETAIL_MAX_BYTES);
        // CJK must not panic and must stay on a char boundary.
        let cjk = "憑證".repeat(200);
        let cut = unknown_detail(&cjk, "unused");
        assert!(cut.len() <= UNKNOWN_DETAIL_MAX_BYTES);
        assert!(cut.chars().all(|c| c == '憑' || c == '證'));
    }

    // ── end-to-end over a local listener ─────────────────────────────

    /// Minimal fixed-response HTTP server: accepts one connection, records the
    /// request head, replies with `response`. Dependency-free on purpose —
    /// this crate has no test HTTP-server dependency and does not need one.
    async fn spawn_fixed_response_server(
        response: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return String::new(),
            };
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
            head
        });
        (format!("http://{addr}"), handle)
    }

    /// An OAuth probe must send `Authorization: Bearer` + the OAuth beta
    /// header (never `x-api-key`), and a 403 must map to `OrgDisabled` — the
    /// exact shape of the 2026-09-08 incident.
    #[tokio::test]
    async fn oauth_probe_sends_bearer_and_maps_403_to_org_disabled() {
        let (base, server) = spawn_fixed_response_server(
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;

        let got = probe_anthropic_credential_at(
            &base,
            CredentialKind::OAuthToken,
            "sk-ant-oat01-probe-test",
        )
        .await;
        assert_eq!(got, CredentialProbe::OrgDisabled);

        let head = server.await.expect("server task");
        let lower = head.to_lowercase();
        assert!(lower.contains("get /v1/models"), "wrong path: {head}");
        assert!(
            lower.contains("authorization: bearer sk-ant-oat01-probe-test"),
            "missing bearer header: {head}"
        );
        assert!(
            lower.contains("anthropic-beta: oauth-2025-04-20"),
            "missing oauth beta header: {head}"
        );
        assert!(
            lower.contains("anthropic-version: 2023-06-01"),
            "missing api version header: {head}"
        );
        assert!(
            !lower.contains("x-api-key"),
            "an OAuth probe must not send x-api-key: {head}"
        );
    }

    /// An API-key probe must send `x-api-key` (never a bearer header), and a
    /// 403 maps the same way.
    #[tokio::test]
    async fn api_key_probe_sends_x_api_key_header() {
        let (base, server) = spawn_fixed_response_server(
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;

        let got =
            probe_anthropic_credential_at(&base, CredentialKind::ApiKey, "sk-ant-api03-probe")
                .await;
        assert_eq!(got, CredentialProbe::OrgDisabled);

        let head = server.await.expect("server task");
        let lower = head.to_lowercase();
        assert!(
            lower.contains("x-api-key: sk-ant-api03-probe"),
            "missing x-api-key header: {head}"
        );
        assert!(
            !lower.contains("authorization:"),
            "an API-key probe must not send an Authorization header: {head}"
        );
        assert!(
            !lower.contains("anthropic-beta"),
            "an API-key probe must not send the OAuth beta header: {head}"
        );
    }

    /// 200 → Valid and 401 → InvalidCredential, end to end over the wire.
    #[tokio::test]
    async fn end_to_end_maps_200_and_401() {
        let (base, server) = spawn_fixed_response_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        )
        .await;
        assert_eq!(
            probe_anthropic_credential_at(&base, CredentialKind::OAuthToken, "tok").await,
            CredentialProbe::Valid
        );
        let _ = server.await;

        let (base, server) = spawn_fixed_response_server(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(
            probe_anthropic_credential_at(&base, CredentialKind::OAuthToken, "tok").await,
            CredentialProbe::InvalidCredential
        );
        let _ = server.await;
    }

    /// A 500 stays `Unknown` end to end — the caller must leave the account
    /// alone rather than reading a server outage as a dead credential.
    #[tokio::test]
    async fn end_to_end_500_is_unknown() {
        let (base, server) = spawn_fixed_response_server(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        let got = probe_anthropic_credential_at(&base, CredentialKind::ApiKey, "k").await;
        assert_eq!(got, CredentialProbe::Unknown("HTTP 500".to_string()));
        assert!(!got.is_conclusive_failure());
        let _ = server.await;
    }

    // ── live fire (opt-in, never in CI) ──────────────────────────────

    /// The one thing a local listener cannot prove: that the headers and the
    /// endpoint this module pins are the ones Anthropic actually honours.
    ///
    /// `#[ignore]` by design — it needs a real credential and a network.
    /// Run it by hand after any change to the request shape:
    ///
    /// ```text
    /// DUDUCLAW_LIVE_OAUTH_TOKEN=sk-ant-oat01-… \
    ///   cargo test -p duduclaw-agent live_probe_real_api -- --ignored --nocapture
    /// ```
    ///
    /// With the variable unset it returns early rather than failing: a missing
    /// credential is "not run", not "broken" (and never a reason to invent a
    /// pass).
    #[tokio::test]
    #[ignore = "live: needs DUDUCLAW_LIVE_OAUTH_TOKEN and network access"]
    async fn live_probe_real_api() {
        let Ok(token) = std::env::var("DUDUCLAW_LIVE_OAUTH_TOKEN") else {
            eprintln!("DUDUCLAW_LIVE_OAUTH_TOKEN unset — skipping live probe");
            return;
        };
        if token.trim().is_empty() {
            eprintln!("DUDUCLAW_LIVE_OAUTH_TOKEN is empty — skipping live probe");
            return;
        }

        let got = probe_anthropic_credential(CredentialKind::OAuthToken, &token).await;
        assert_eq!(
            got,
            CredentialProbe::Valid,
            "a real setup-token must probe Valid (403 ⇒ the org disabled subscription \
             access; 401 ⇒ the token is dead — re-issue it with `claude setup-token`)"
        );

        // …and the negative control, so a probe that answers `Valid` for
        // everything (e.g. a proxy swallowing the auth header) cannot pass.
        let got = probe_anthropic_credential(CredentialKind::OAuthToken, "sk-ant-oat01-bogus").await;
        assert_eq!(
            got,
            CredentialProbe::InvalidCredential,
            "a bogus token must be rejected 401"
        );
    }

    /// A transport failure (nothing listening) is `Unknown`, and the detail it
    /// carries never contains the secret.
    #[tokio::test]
    async fn transport_failure_is_unknown_without_the_secret() {
        // Bind then drop, so the port is (almost certainly) closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let secret = "sk-ant-oat01-never-log-me";
        let got = probe_anthropic_credential_at(
            &format!("http://{addr}"),
            CredentialKind::OAuthToken,
            secret,
        )
        .await;
        match got {
            CredentialProbe::Unknown(detail) => {
                assert!(
                    !detail.contains("never-log-me"),
                    "secret leaked into Unknown detail: {detail}"
                );
                assert!(detail.len() <= UNKNOWN_DETAIL_MAX_BYTES);
            }
            other => panic!("expected Unknown for a closed port, got {other:?}"),
        }
    }
}
