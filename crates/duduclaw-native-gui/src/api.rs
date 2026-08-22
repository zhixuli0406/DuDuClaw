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

use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use serde::Deserialize;

/// Runtime-resolved gateway base URL (WP-C-M2). Was `pub const GATEWAY_BASE_
/// URL: &str = "http://127.0.0.1:18789"` through S2 — every part of this
/// crate hardcoded the same literal, with a doc comment calling a settings
/// screen to override it "future scope". This is that future scope: an
/// `OnceLock<RwLock<String>>` so `main.rs` can seed it once at startup with
/// whatever `sidecar::plan_gateway`/`config::load_gateway_selection`/
/// `DUDUCLAW_GATEWAY_URL` resolve to, AND `screens::gateway_picker` can
/// change it again later at runtime (switching gateways without a process
/// restart — see that screen's connect handler).
///
/// The lazy default (used only if [`init_gateway_base_url`] is never
/// called, e.g. every existing unit test in this module/crate that talks to
/// `login()`/`try_local_session()` directly) is the exact same literal S2
/// hardcoded, so none of those tests needed to change.
static GATEWAY_BASE: OnceLock<RwLock<String>> = OnceLock::new();

fn gateway_base_cell() -> &'static RwLock<String> {
    GATEWAY_BASE.get_or_init(|| RwLock::new("http://127.0.0.1:18789".to_string()))
}

/// Seed the resolved base URL once at startup, BEFORE any other call in
/// this module runs (`main.rs`'s very first lines, ahead of spawning
/// `ws_status`/`chat_ws`/`sidebar_rpc`). A no-op if something already read
/// the lazy default first — startup order guarantees that never happens in
/// practice, but silently losing an explicit seed would be worse than a
/// same-value overwrite, so [`set_gateway_base_url`] (not `.set()`) is used
/// here too.
pub fn init_gateway_base_url(url: String) {
    set_gateway_base_url(url);
}

/// The gateway base URL currently in effect. Read fresh on every call
/// (never cached by a caller) so a runtime gateway switch takes effect
/// immediately for the very next request.
pub fn gateway_base_url() -> String {
    gateway_base_cell().read().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone())
}

/// Point every future call in this crate at a different gateway. Called
/// exactly twice today: once at startup (via [`init_gateway_base_url`]) and
/// once per successful `screens::gateway_picker` connect/switch action.
pub fn set_gateway_base_url(url: String) {
    match gateway_base_cell().write() {
        Ok(mut g) => *g = url,
        Err(e) => *e.into_inner() = url,
    }
}

/// Derive a `ws://`/`wss://` URL for `path` from the current base URL —
/// the WS twin of [`gateway_base_url`], used by `ws_status.rs`/`chat_ws.rs`/
/// `screens::chat::sidebar_rpc.rs` instead of each hardcoding its own `ws://
/// 127.0.0.1:18789/...` constant. `path` should start with `/`.
pub fn gateway_ws_url(path: &str) -> String {
    derive_ws_url(&gateway_base_url(), path)
}

/// Pure half of [`gateway_ws_url`] — split out so the http→ws / https→wss
/// scheme swap is unit-testable without touching the process-global
/// [`GATEWAY_BASE`] cell (which `login()`/`try_local_session()`'s own live-
/// gateway tests below also read; a test that mutated it via
/// [`set_gateway_base_url`] would race with those running concurrently in
/// the same `cargo test` binary).
fn derive_ws_url(base: &str, path: &str) -> String {
    let (scheme, rest) = if let Some(r) = base.strip_prefix("https://") {
        ("wss", r)
    } else if let Some(r) = base.strip_prefix("http://") {
        ("ws", r)
    } else {
        // Already schemeless or unrecognized — pass through as `ws://`
        // rather than producing a malformed URL silently.
        ("ws", base)
    };
    let rest = rest.trim_end_matches('/');
    format!("{scheme}://{rest}{path}")
}

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
    login_at(&gateway_base_url(), email, password).await
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
    try_local_session_at(&gateway_base_url()).await
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

/// WP-C-M2 — validate a gateway URL a user typed into `screens::
/// gateway_picker`'s manual-entry field. Scheme MUST be `http`/`https`
/// (fail-closed — anything else, including `file:`/`javascript:`/`data:`
/// or an unparsable string, is rejected). Returns the normalized
/// `scheme://host[:port]` origin (path/query/fragment stripped) on success.
/// Hand-rolled rather than pulling in the `url` crate (this crate has no
/// existing URL-parsing dependency and the shape needed here — scheme +
/// host + optional port, nothing else — doesn't warrant one).
pub fn validate_gateway_url(input: &str) -> Result<String, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("empty URL".to_string());
    }
    let (scheme, rest) = if let Some(r) = raw.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        ("http", r)
    } else {
        return Err(format!("unsupported scheme in '{raw}' (only http/https)"));
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host_port.is_empty() {
        return Err("missing host".to_string());
    }
    Ok(format!("{scheme}://{host_port}"))
}

/// True when `url`'s host is a loopback address — used to decide whether a
/// gateway-picker selection is "local" (persisted as `GatewayMode::Local`,
/// never releases the sidecar) or "remote" (persisted as `Remote`, releases
/// a sidecar this app spawned). Best-effort string matching, not full URL
/// parsing (consistent with [`validate_gateway_url`]'s own scope) — a host
/// this can't recognize as loopback is treated as remote, the safe default
/// (never silently keeps a sidecar alive for a target this can't confirm is
/// actually local).
pub fn is_local_gateway_url(url: &str) -> bool {
    let without_scheme = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    // Bracketed IPv6 (`[::1]:18789`) needs its own split — the naive
    // `split(['/', ':'])` below would stop at the FIRST colon inside the
    // brackets and see only `[`.
    let host = if let Some(rest) = without_scheme.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        without_scheme.split(['/', ':']).next().unwrap_or("").to_string()
    };
    let host = host.to_ascii_lowercase();
    host == "127.0.0.1" || host == "localhost" || host == "::1"
}

/// WP-C-M2 — unauthenticated `GET <base_url>/healthz` probe, used by
/// `screens::gateway_picker` to validate a candidate URL (local sidecar or
/// manually-entered remote) before persisting/switching to it. Short
/// timeout — this is a UI-blocking "does anything answer here at all"
/// check, not a normal API call, so it does not reuse [`REQUEST_TIMEOUT`].
/// Reports `(ok, error_detail)` rather than an `AuthError`: an unhealthy
/// gateway is an expected, common outcome here (the whole point of the
/// probe), not a failure mode worth the three-way `AuthError` taxonomy.
pub async fn health_probe(base_url: &str) -> (bool, Option<String>) {
    const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
    let url = format!("{}/healthz", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return (false, Some(format!("http client: {e}"))),
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => (true, None),
        Ok(resp) => (false, Some(format!("HTTP {}", resp.status().as_u16()))),
        Err(e) => {
            let detail = if e.is_timeout() {
                "connection timed out".to_string()
            } else if e.is_connect() {
                "could not connect".to_string()
            } else {
                e.to_string()
            };
            (false, Some(detail))
        }
    }
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
        let result = get_json(&gateway_base_url(), "/api/files", None).await;
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

    #[test]
    fn derive_ws_url_swaps_scheme_and_strips_trailing_slash() {
        assert_eq!(derive_ws_url("http://127.0.0.1:18789", "/ws"), "ws://127.0.0.1:18789/ws");
        assert_eq!(derive_ws_url("http://127.0.0.1:18789/", "/ws"), "ws://127.0.0.1:18789/ws");
        assert_eq!(derive_ws_url("https://gw.example.com", "/ws/chat"), "wss://gw.example.com/ws/chat");
        // Schemeless/unrecognized input degrades to ws:// rather than
        // producing a malformed string.
        assert_eq!(derive_ws_url("127.0.0.1:18789", "/ws"), "ws://127.0.0.1:18789/ws");
    }

    #[tokio::test]
    async fn health_probe_against_unreachable_host_is_not_ok() {
        let (ok, detail) = health_probe(UNREACHABLE_BASE_URL).await;
        assert!(!ok);
        assert!(detail.is_some());
    }

    /// Live path: this is the exact round trip `RootView::begin_manual_
    /// connect` performs before switching gateways — proves the health-
    /// check half of the manual-entry flow really works against a real
    /// gateway, not just the unreachable-host failure path above. Same
    /// live-gateway caveat as every other `_against_live_gateway_` test in
    /// this file (requires `curl :18789/healthz` to be reachable).
    #[tokio::test]
    async fn health_probe_against_live_gateway_is_ok() {
        let (ok, detail) = health_probe("http://127.0.0.1:18789").await;
        assert!(ok, "expected the live local gateway to answer /healthz, got detail={detail:?}");
    }

    #[test]
    fn validate_gateway_url_accepts_http_and_https_and_strips_path() {
        assert_eq!(validate_gateway_url("http://192.168.1.5:18789").unwrap(), "http://192.168.1.5:18789");
        assert_eq!(validate_gateway_url("https://gw.example.com").unwrap(), "https://gw.example.com");
        assert_eq!(validate_gateway_url("  http://h:1/login?x=1#frag  ").unwrap(), "http://h:1");
    }

    #[test]
    fn validate_gateway_url_rejects_dangerous_schemes_fail_closed() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ftp://host/x",
            "not a url",
            "",
            "   ",
        ] {
            assert!(validate_gateway_url(bad).is_err(), "should reject: {bad:?}");
        }
    }

    #[test]
    fn is_local_gateway_url_detects_loopback() {
        assert!(is_local_gateway_url("http://127.0.0.1:18789"));
        assert!(is_local_gateway_url("http://localhost:18789"));
        assert!(is_local_gateway_url("http://[::1]:18789"));
        assert!(!is_local_gateway_url("http://192.168.1.5:18789"));
        assert!(!is_local_gateway_url("https://gw.example.com"));
    }
}
