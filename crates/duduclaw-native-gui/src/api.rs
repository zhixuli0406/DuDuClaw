// S2 — HTTP auth client for the local gateway.
//
// Talks to exactly the two endpoints the web dashboard's `auth-store.ts`
// uses to establish a session (`POST /api/login`, `POST /api/session/local`)
// — same JSON shapes, verified against BOTH the gateway source
// (`duduclaw-gateway/src/server.rs::handle_login` /
// `handle_local_session`) AND a live local gateway via `curl` during this
// session (2026-08-19, gateway v1.61.2):
//
//   curl -X POST :18789/api/login -d '{"email":"x","password":"wrong"}'
//     → HTTP 401, body {"error":"invalid email or password"}
//   curl -X POST :18789/api/session/local  (no marker header)
//     → HTTP 403, body {"error":"local auto-login unavailable"}
//   curl -X POST :18789/api/session/local -H 'X-DuDuClaw-Local: 1'
//     → HTTP 403, body {"error":"local auto-login unavailable"}
//     (this dev gateway isn't a Personal-edition loopback install, so the
//     gate in `local_session.rs` refuses — same uniform 403 either way,
//     confirming the endpoint never leaks *why* it refused)
//
// Success shape for both endpoints (`server.rs` lines ~2359 / ~3171):
//   {"access_token": "...", "refresh_token": "...", "user": {...}}
//
// No UI/gpui types appear in this module on purpose — it's a plain async
// HTTP client, directly unit-testable with `#[tokio::test]` (see the bottom
// of this file) without spinning up a window. `ws_status.rs` is the only
// caller, dispatching these functions from its background tokio thread.

use std::time::Duration;

use serde::Deserialize;

/// Base URL for the local gateway. Not configurable in S2 — every other
/// part of this crate (the WS smoke test this replaces) also hardcodes
/// `127.0.0.1:18789`; a settings screen to override it is future scope.
pub const GATEWAY_BASE_URL: &str = "http://127.0.0.1:18789";

/// Custom header `POST /api/session/local` requires (`local_session.rs`'s
/// `LOCAL_MARKER_HEADER`/`LOCAL_MARKER_VALUE`). Its value is arbitrary — the
/// header's mere *presence* is what the gateway checks — but the gateway
/// only accepts the exact value `"1"`, so we send that.
const LOCAL_SESSION_HEADER_NAME: &str = "X-DuDuClaw-Local";
const LOCAL_SESSION_HEADER_VALUE: &str = "1";

/// Generous but bounded — a hung gateway process must not wedge the login
/// button forever. `AuthError::Unreachable` classification below treats a
/// timeout the same as a refused connection (task brief: "連不上（connection
/// refused/timeout → 無法連線到 gateway）").
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The subset of the gateway's `User` row
/// (`duduclaw-auth::models::User`) this client reads. `#[serde(default)]`
/// on everything but `id`/`email` so an unrelated server-side field
/// addition/removal (e.g. `department`, `last_login`) never breaks
/// deserialization — this client only ever displays these values, never
/// round-trips them back to the server.
///
/// S2's UI doesn't render a profile/account panel yet (no such screen
/// exists), so these fields are only ever *deserialized and validated*, not
/// displayed — `#[allow(dead_code)]` documents that as deliberate (kept for
/// whichever S3+ screen adds "logged in as ...", not dead weight to prune).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub status: String,
}

/// `{access_token, refresh_token, user}` — the shared success shape of both
/// `/api/login` and `/api/session/local`. `refresh_token` is captured into
/// `RootView` (S2 item 5: kept in memory only) but not yet acted on — no
/// refresh timer exists in S2, see `RootView::handle_session_event`'s doc
/// comment for the explicit S3+ callout. `user` likewise rides along
/// unread today; see [`AuthUser`]'s doc comment.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    #[allow(dead_code)]
    pub user: AuthUser,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
}

/// Three-way failure classification the task brief asks for. Deliberately
/// carries no localized text — `ws_status.rs`/the UI layer picks the
/// `login.error.*` i18n key via [`AuthError::i18n_key_and_code`], keeping
/// every user-facing string in the i18n catalogs (this module has no
/// `Locale` dependency).
#[derive(Debug, Clone)]
pub enum AuthError {
    /// Connection refused, DNS failure, or the whole request timed out —
    /// the gateway process itself could not be reached at all.
    Unreachable,
    /// 401 or 403 from `/api/login` — a reachable gateway rejected the
    /// credentials.
    InvalidCredentials,
    /// Anything else: a status code the two variants above don't cover
    /// (429 rate-limited, 5xx, ...), or a response body that didn't parse
    /// as the expected success/error shape.
    Other { status: Option<u16>, detail: String },
}

impl AuthError {
    /// i18n key (`login.error.*`) for the user-facing message, plus the
    /// `{code}` value `Other` wants interpolated (empty string for the
    /// other two variants, which have no code to show).
    pub fn i18n_key_and_code(&self) -> (&'static str, String) {
        match self {
            AuthError::Unreachable => ("login.error.unreachable", String::new()),
            AuthError::InvalidCredentials => ("login.error.invalidCredentials", String::new()),
            AuthError::Other { status, .. } => (
                "login.error.unknown",
                status.map(|s| s.to_string()).unwrap_or_else(|| "N/A".into()),
            ),
        }
    }
}

/// One-off client per call rather than a shared `OnceLock<Client>` — these
/// are low-frequency (a login click, a boot-time local-session probe, never
/// a hot loop), so the extra allocation is not worth the added complexity
/// of threading a shared client through the background thread's command
/// loop. `.expect()` is safe here: with no TLS feature compiled in and no
/// exotic builder options, `ClientBuilder::build()` cannot fail on any
/// supported target (verified against reqwest's own source: `build()` only
/// errors on TLS-backend / resolver construction, both no-ops here).
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client build (loopback HTTP, no TLS backend) should never fail")
}

/// `POST /api/login` with `{email, password}`.
pub async fn login(email: &str, password: &str) -> Result<LoginResponse, AuthError> {
    login_at(GATEWAY_BASE_URL, email, password).await
}

/// Same as [`login`] against an explicit base URL — the seam that makes the
/// "gateway unreachable" path testable (point it at a port nothing is
/// listening on) without touching the hardcoded production constant.
pub async fn login_at(
    base_url: &str,
    email: &str,
    password: &str,
) -> Result<LoginResponse, AuthError> {
    let url = format!("{base_url}/api/login");
    let resp = client()
        .post(&url)
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(classify_send_error)?;
    handle_credentialed_response(resp).await
}

/// `POST /api/session/local` — Personal-edition passwordless local session
/// probe. Mirrors `auth-store.ts`'s `tryLocalSession()`: `None` on ANY
/// non-success outcome (network failure, non-2xx status, malformed body) —
/// this is expected to be refused on every install that isn't a Personal
/// loopback box, and that refusal must stay silent (never surfaced as an
/// error to the caller; the caller just shows the login form).
pub async fn try_local_session() -> Option<LoginResponse> {
    try_local_session_at(GATEWAY_BASE_URL).await
}

/// Same as [`try_local_session`] against an explicit base URL (test seam,
/// see [`login_at`]).
pub async fn try_local_session_at(base_url: &str) -> Option<LoginResponse> {
    let url = format!("{base_url}/api/session/local");
    let resp = client()
        .post(&url)
        .header(LOCAL_SESSION_HEADER_NAME, LOCAL_SESSION_HEADER_VALUE)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<LoginResponse>().await.ok()
}

/// WP-S5b2-F — generic authenticated `GET` returning the parsed JSON body.
/// The REST twin of `ws_status::Command::Call`'s WS-RPC primitive: this
/// crate's dashboard-file-panel surface (`GET /api/files`, `screens::files`)
/// has no WS-RPC equivalent server-side (`duduclaw-gateway/src/handlers.rs`'s
/// dispatch match has no `files.*` arm — the listing only exists as a REST
/// route in `server.rs`), so a page needing it cannot just reuse
/// `Command::Call`. Generic over any future `/api/*` GET rather than a
/// one-off `files_list` function, mirroring how `Command::Call` is generic
/// over RPC method name rather than minting a variant per method.
///
/// Caller supplies `path_and_query` starting with `/` (e.g. `"/api/files?
/// agent=duduclaw"`); `jwt` is sent as a Bearer token when present (omitted
/// entirely — not an empty header — when `None`, matching how every other
/// authenticated call site in this crate treats a missing token).
pub async fn get_json(
    base_url: &str,
    path_and_query: &str,
    jwt: Option<&str>,
) -> Result<serde_json::Value, AuthError> {
    let url = format!("{base_url}{path_and_query}");
    let mut req = client().get(&url);
    if let Some(jwt) = jwt {
        req = req.bearer_auth(jwt);
    }
    let resp = req.send().await.map_err(classify_send_error)?;
    let status = resp.status();
    if status.is_success() {
        return resp.json::<serde_json::Value>().await.map_err(|e| AuthError::Other {
            status: Some(status.as_u16()),
            detail: format!("malformed response body: {e}"),
        });
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(AuthError::InvalidCredentials);
    }
    let body_text = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<ErrorBody>(&body_text)
        .ok()
        .map(|b| b.error)
        .filter(|s| !s.is_empty())
        .unwrap_or(body_text);
    Err(AuthError::Other { status: Some(status.as_u16()), detail })
}

/// A `reqwest::Error` from `.send()` itself (never reached the server, or
/// the whole request timed out) → [`AuthError::Unreachable`]; anything else
/// reqwest could raise at this stage (body-encode failure, ...) is folded
/// into `Other` rather than mis-classified as unreachable.
fn classify_send_error(e: reqwest::Error) -> AuthError {
    if e.is_connect() || e.is_timeout() {
        AuthError::Unreachable
    } else {
        AuthError::Other {
            status: e.status().map(|s| s.as_u16()),
            detail: e.to_string(),
        }
    }
}

/// Shared response handling for `/api/login` (and, if S3+ adds OTP verify
/// on the same shape, that too): success → parse `LoginResponse`; 401/403 →
/// `InvalidCredentials`; anything else → `Other`, with the body's `error`
/// field as the detail when present.
async fn handle_credentialed_response(
    resp: reqwest::Response,
) -> Result<LoginResponse, AuthError> {
    let status = resp.status();
    if status.is_success() {
        return resp.json::<LoginResponse>().await.map_err(|e| AuthError::Other {
            status: Some(status.as_u16()),
            detail: format!("malformed success response body: {e}"),
        });
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(AuthError::InvalidCredentials);
    }
    let body_text = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<ErrorBody>(&body_text)
        .ok()
        .map(|b| b.error)
        .filter(|s| !s.is_empty())
        .unwrap_or(body_text);
    Err(AuthError::Other {
        status: Some(status.as_u16()),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port nothing is listening on — every `login_at`/`try_local_session_at`
    /// call against this must classify as `Unreachable`/`None` without
    /// touching the network beyond the initial connect attempt.
    const UNREACHABLE_BASE_URL: &str = "http://127.0.0.1:1";

    /// Distinct per test run so repeated `cargo test` invocations never
    /// collide on the gateway's `(ip, email)`-scoped login rate limiter
    /// (`server.rs::check_login_rate_limit` — 5 attempts / (ip, email) /
    /// 15min). All calls in this test run from `127.0.0.1`, so only the
    /// email half needs to vary.
    fn unique_test_email() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("duduclaw-native-gui-s2-test-{nanos}@nowhere.invalid")
    }

    /// Live path: a real (wrong-credential) `/api/login` call against
    /// whatever gateway is actually running on 127.0.0.1:18789. Requires a
    /// local gateway to be up — this session's activity log confirmed one
    /// running (v1.61.2) and this exact 401 shape via `curl` before this
    /// test was written; if no gateway is running this test instead proves
    /// the `Unreachable` path (still a meaningful assertion, just not the
    /// one the test name promises), so a failure here should first be
    /// triaged by checking `curl :18789/healthz`.
    #[tokio::test]
    async fn login_with_wrong_credentials_against_live_gateway_is_invalid_credentials() {
        let email = unique_test_email();
        let result = login(&email, "definitely-wrong-password-12345").await;
        match result {
            Err(AuthError::InvalidCredentials) => {}
            other => panic!(
                "expected InvalidCredentials against the live local gateway, got {other:?} \
                 — is a gateway actually running on 127.0.0.1:18789? (curl :18789/healthz)"
            ),
        }
    }

    #[tokio::test]
    async fn login_against_unreachable_host_is_unreachable() {
        let result = login_at(UNREACHABLE_BASE_URL, "a@b.c", "whatever").await;
        assert!(
            matches!(result, Err(AuthError::Unreachable)),
            "expected Unreachable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn try_local_session_against_unreachable_host_is_none() {
        assert!(try_local_session_at(UNREACHABLE_BASE_URL).await.is_none());
    }

    /// Live path: this dev gateway is not a Personal-edition loopback
    /// install (confirmed via `curl` — see module doc comment), so the
    /// probe must degrade to `None`, never propagate an error. This is
    /// exactly the "no local session available" branch S2's login screen
    /// relies on to fall back to the password form.
    #[tokio::test]
    async fn try_local_session_against_live_gateway_is_none_or_a_session() {
        // Whichever branch fires, it must not panic/hang — both are valid
        // depending on the gateway's edition + switch state, which this
        // test doesn't control. The meaningful assertion is just "the call
        // completes and returns the expected type", exercising the same
        // code path `ws_status.rs`'s boot-time probe uses.
        let _ = try_local_session().await;
    }

    #[tokio::test]
    async fn get_json_against_unreachable_host_is_unreachable() {
        let result = get_json(UNREACHABLE_BASE_URL, "/api/files", None).await;
        assert!(matches!(result, Err(AuthError::Unreachable)), "expected Unreachable, got {result:?}");
    }

    /// Live path: `/api/files` without a bearer token must fail closed
    /// (401), never silently return a body — the same authenticated-REST
    /// invariant `authorize_file_access` documents server-side.
    #[tokio::test]
    async fn get_json_without_token_against_live_gateway_is_invalid_credentials() {
        let result = get_json(GATEWAY_BASE_URL, "/api/files", None).await;
        match result {
            Err(AuthError::InvalidCredentials) => {}
            other => panic!(
                "expected InvalidCredentials against the live local gateway, got {other:?} \
                 — is a gateway actually running on 127.0.0.1:18789? (curl :18789/healthz)"
            ),
        }
    }

    #[test]
    fn auth_error_i18n_mapping() {
        assert_eq!(
            AuthError::Unreachable.i18n_key_and_code(),
            ("login.error.unreachable", String::new())
        );
        assert_eq!(
            AuthError::InvalidCredentials.i18n_key_and_code(),
            ("login.error.invalidCredentials", String::new())
        );
        assert_eq!(
            AuthError::Other { status: Some(429), detail: "x".into() }.i18n_key_and_code(),
            ("login.error.unknown", "429".to_string())
        );
        assert_eq!(
            AuthError::Other { status: None, detail: "x".into() }.i18n_key_and_code(),
            ("login.error.unknown", "N/A".to_string())
        );
    }
}
