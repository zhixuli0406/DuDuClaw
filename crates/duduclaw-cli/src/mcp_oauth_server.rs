//! OAuth 2.1 authorization surface for the remote MCP endpoint — WP3.1-T2.
//!
//! Makes `duduclaw http-server` connectable from OAuth-only MCP clients
//! (claude.ai custom connectors, Claude mobile). DuDuClaw is single-tenant
//! self-hosted, so this is deliberately the *smallest* spec-conforming
//! authorization server: the operator is the only "user", and consent is
//! proven by pasting an existing **internal** MCP API key.
//!
//! Spec surface (verified against the MCP 2025-06-18 authorization spec +
//! the RFCs it cites):
//!
//! - RFC 9728 Protected Resource Metadata: `GET /.well-known/oauth-protected-resource`
//!   (the MCP endpoint also stamps `WWW-Authenticate: Bearer resource_metadata=…`
//!   on its 401s — that's how clients discover this document)
//! - RFC 8414 AS Metadata: `GET /.well-known/oauth-authorization-server`
//! - RFC 7591 Dynamic Client Registration: `POST /oauth/register`
//!   (public clients only, `token_endpoint_auth_method: "none"`)
//! - Authorization endpoint: `GET /oauth/authorize` (code flow, PKCE S256
//!   REQUIRED) → consent page → `POST /oauth/decision` → 302 back with code
//! - Token endpoint: `POST /oauth/token` (`authorization_code` +
//!   `refresh_token` grants; refresh rotation)
//!
//! Security posture (fail closed everywhere):
//! - Issued principals are ALWAYS `is_external = true` — the C4 external
//!   tool predicate applies. Scopes are the intersection of what the client
//!   requested and [`EXTERNALLY_GRANTABLE_SCOPES`]; connector/execute/admin
//!   surfaces can never be minted over OAuth, no matter what was requested.
//! - The consent credential must be an *internal* key (an external key
//!   cannot escalate itself into more grants).
//! - Tokens/refresh tokens/codes are random 192-bit values; only SHA-256
//!   digests are stored at rest. Codes are single-use with a 10-minute TTL;
//!   access tokens live 1 hour; refresh tokens 30 days and rotate on use.
//! - `redirect_uri` must exactly match a registered value (string equality),
//!   and registration only accepts `https://…` or loopback `http://…` URIs.
//! - Client-supplied text (client_name) is HTML-escaped before rendering.
//!
//! Client ID Metadata Documents (2026-10-08, [`cimd`]): a `client_id` that
//! is an `https://` URL with a path and is not a registered id is resolved by
//! fetching that URL (public addresses only, pinned resolution, no
//! redirects, 5 s including DNS, 5 KiB, cached 1 h; failures cached 30 s,
//! at most 64 cached). Four workers retain their slots through caller
//! cancellation; admission waits at most 100 ms, with 12 attempts/IP/minute.
//! The document must
//! be a JSON object whose `client_id` equals the URL exactly, whose
//! `redirect_uris` pass the same policy as registration, and whose
//! `token_endpoint_auth_method` is absent or `none` (public clients only).
//! The AS metadata advertises `client_id_metadata_document_supported`.
//! Written from the MCP 2025-11-25 authorization spec and the IETF draft
//! `draft-ietf-oauth-client-id-metadata-document`, not tested against a real
//! client that uses it.

use std::collections::HashMap;
use std::path::Path;

use axum::{Extension, Json};
use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::mcp_auth::{AuthError, EXTERNALLY_GRANTABLE_SCOPES, Principal, parse_scopes};
use crate::mcp_http_server::{HttpState, SourceIp, RegistrationBudget};

use duduclaw_core::mcp_oauth_store::{
    Consent, Grant, IssuedToken, OAuthStore, PendingCode, RegisteredClient,
    CODE_TTL_SECS, load_store, update_store, scope_subset,
};
#[cfg(test)]
use duduclaw_core::mcp_oauth_store::store_path;
const ACCESS_TOKEN_TTL_SECS: i64 = 3600;
const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;
use duduclaw_core::mcp_oauth_store::MAX_CLIENTS;
const MAX_REDIRECT_URIS: usize = 10;
const MAX_CLIENT_NAME_CHARS: usize = 100;

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn store_error() -> Response {
    oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "OAuth store unavailable")
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}

/// 192-bit random hex string (two v4 UUIDs' random payloads).
fn random_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

// ── Bearer validation hook (called from the auth path) ────────────────────────

/// Prefix distinguishing OAuth-issued access tokens from static `ddc_*` keys.
pub(crate) const OAUTH_TOKEN_PREFIX: &str = "ddc_oauth_";

pub(crate) fn is_oauth_access_token(raw: &str) -> bool {
    raw.strip_prefix(OAUTH_TOKEN_PREFIX).is_some_and(|secret| {
        secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Validate an OAuth-issued access token. Returns the external Principal it
/// was minted for, or an auth error (expired/unknown ⇒ the caller's 401).
pub(crate) fn validate_oauth_token(raw: &str, home: &Path, resource: &str) -> Result<Principal, AuthError> {
    let store = load_store(home).map_err(|_| AuthError::ReloadFailed)?;
    let entry = store.tokens.get(&sha256_hex(raw)).ok_or(AuthError::UnknownKey)?;
    if !store.valid_token(entry, home, resource) || filter_scope(&entry.scope) != entry.scope {
        return Err(AuthError::UnknownKey);
    }
    let grant = store.grants.get(&entry.family_id).ok_or(AuthError::UnknownKey)?;
    Ok(Principal {
        client_id: format!("oauth_{}", entry.client_id),
        scopes: parse_scopes(&entry.scope.replace(' ', ","))?,
        is_external: true,
        created_at: chrono::DateTime::from_timestamp(grant.issued_unix, 0).ok_or(AuthError::UnknownKey)?,
    })
}

/// Approximate telemetry, at most one persisted update per family per minute.
/// Re-read under the mutation lock; a stale observation never recreates a grant.
pub(crate) fn note_oauth_use(raw: &str, home: &Path) {
    let key = sha256_hex(raw);
    let Ok(snapshot) = load_store(home) else { return; };
    let Some(token) = snapshot.tokens.get(&key) else { return; };
    let Some(grant) = snapshot.grants.get(&token.family_id) else { return; };
    let now = now_unix();
    if grant.last_used_unix.is_some_and(|last| now - last < 60) { return; }
    let _ = update_store(home, |store| {
        if let Some(token) = store.tokens.get(&key)
            && token.expires_unix > now
            && let Some(grant) = store.grants.get_mut(&token.family_id)
            && grant.last_used_unix.is_none_or(|last| now - last >= 60) {
            grant.last_used_unix = Some(now);
        }
    });
}

// ── Canonical resource binding ───────────────────────────────────────────────

/// Omitted indicators default only to this server; explicit empty, multiple
/// different audiences and all other targets are refused (RFC 8707).
fn requested_resource<'a>(requested: Option<&str>, canonical: &'a str) -> Result<&'a str, Response> {
    if canonical.is_empty() || requested.is_some_and(|r| r != canonical) {
        return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_target", "resource must equal this server's canonical MCP URI"));
    }
    Ok(canonical)
}

fn resource_params(params: &[(String, String)], canonical: &str) -> Result<(), Response> {
    requested_resource(None, canonical)?;
    for (_, value) in params.iter().filter(|(key, _)| key == "resource") {
        requested_resource(Some(value), canonical)?;
    }
    Ok(())
}

/// RFC 9728 challenge uses configuration, never request authority headers.
pub(crate) fn www_authenticate_value(external_origin: &str) -> String {
    format!("Bearer resource_metadata=\"{external_origin}/.well-known/oauth-protected-resource\"")
}

// ── Metadata endpoints ────────────────────────────────────────────────────────

fn grantable_scope_strings() -> Vec<String> {
    EXTERNALLY_GRANTABLE_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub(crate) async fn protected_resource_metadata(State(state): State<HttpState>) -> Response {
    let base = &state.external_origin;
    Json(json!({
        "resource": state.resource,
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": grantable_scope_strings(),
    }))
    .into_response()
}

pub(crate) async fn authorization_server_metadata(State(state): State<HttpState>) -> Response {
    authorization_metadata(&state.external_origin)
}

fn authorization_metadata(base: &str) -> Response {
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "revocation_endpoint_auth_methods_supported": ["none"],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": grantable_scope_strings(),
        "client_id_metadata_document_supported": true,
    }))
    .into_response()
}

// ── Client ID Metadata Documents ─────────────────────────────────────────────

pub(crate) mod cimd {
    use super::{RegisteredClient, redirect_uri_acceptable, sanitize_client_name};
    use duduclaw_gateway::remote_mcp::url_policy::{OutboundPolicy, check_outbound, pinned_client};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    pub const MAX_DOC_BYTES: usize = 5 * 1024;
    const CACHE_TTL: Duration = Duration::from_secs(3600);
    const CACHE_MAX: usize = 64;
    const NEGATIVE_TTL: Duration = Duration::from_secs(30);
    const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(100);
    const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Clone, Debug)]
    pub enum ResolveError {
        Invalid(String),
        Temporary,
    }

    impl ResolveError {
        pub fn response(self) -> axum::response::Response {
            match self {
                Self::Invalid(message) => super::html_error(axum::http::StatusCode::BAD_REQUEST, &message),
                Self::Temporary => super::oauth_error(axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily_unavailable", "retry client metadata resolution later"),
            }
        }
    }

    type CachedResult = Result<RegisteredClient, ResolveError>;

    struct FetchGuard {
        slots: Arc<tokio::sync::Semaphore>,
        cache: Mutex<HashMap<String, (CachedResult, Instant)>>,
        attempts: Mutex<crate::mcp_http_server::Admission>,
    }

    impl Default for FetchGuard {
        fn default() -> Self {
            Self {
                slots: Arc::new(tokio::sync::Semaphore::new(4)),
                cache: Mutex::new(HashMap::new()),
                attempts: Mutex::new(Default::default()),
            }
        }
    }

    impl FetchGuard {
        fn cached(&self, id: &str) -> Result<Option<CachedResult>, ResolveError> {
            let mut cache = self.cache.lock().map_err(|_| ResolveError::Temporary)?;
            cache.retain(|_, (_, until)| *until > Instant::now());
            Ok(cache.get(id).map(|(value, _)| value.clone()))
        }

        fn remember(&self, id: &str, result: CachedResult) {
            if let Ok(mut cache) = self.cache.lock() {
                cache.retain(|_, (_, until)| *until > Instant::now());
                if !cache.contains_key(id) && cache.len() >= CACHE_MAX {
                    if let Some(oldest) = cache.iter().min_by_key(|(_, (_, until))| *until).map(|(key, _)| key.clone()) {
                        cache.remove(&oldest);
                    }
                }
                let ttl = if result.is_ok() { CACHE_TTL } else { NEGATIVE_TTL };
                cache.insert(id.to_owned(), (result, Instant::now() + ttl));
            }
        }

        async fn resolve<F>(&self, id: &str, source: &str, fetch: F, timeout: Duration) -> CachedResult
        where F: std::future::Future<Output = Result<RegisteredClient, String>> + Send + 'static {
            if let Some(cached) = self.cached(id)? { return cached; }
            if !self.attempts.lock().map(|mut t| t.allow(source.to_owned(), 12)).unwrap_or(false) {
                return Err(ResolveError::Temporary);
            }
            let permit = match tokio::time::timeout(ACQUIRE_TIMEOUT, self.slots.clone().acquire_owned()).await {
                Ok(Ok(permit)) => permit,
                _ => {
                    self.remember(id, Err(ResolveError::Temporary));
                    return Err(ResolveError::Temporary);
                }
            };
            // Serialize the cache recheck and in-flight marker to coalesce URL probes.
            {
                let mut cache = self.cache.lock().map_err(|_| ResolveError::Temporary)?;
                cache.retain(|_, (_, until)| *until > Instant::now());
                if let Some((cached, _)) = cache.get(id) { return cached.clone(); }
                if cache.len() >= CACHE_MAX {
                    if let Some(oldest) = cache.iter().min_by_key(|(_, (_, until))| *until).map(|(key, _)| key.clone()) {
                        cache.remove(&oldest);
                    }
                }
                cache.insert(id.to_owned(), (Err(ResolveError::Temporary), Instant::now() + NEGATIVE_TTL));
            }
            // Keep the permit in a detached worker if the HTTP caller cancels or
            // times out. spawn_blocking DNS cannot be cancelled: releasing here
            // would let slow resolvers accumulate beyond the four-slot bound.
            let work = tokio::spawn(async move {
                let _permit = permit;
                fetch.await
            });
            let result = match tokio::time::timeout(timeout, work).await {
                Ok(Ok(Ok(client))) => Ok(client),
                Ok(Ok(Err(message))) => Err(ResolveError::Invalid(message)),
                _ => Err(ResolveError::Temporary),
            };
            self.remember(id, result.clone());
            result
        }
    }

    fn guard() -> &'static FetchGuard {
        static GUARD: OnceLock<FetchGuard> = OnceLock::new();
        GUARD.get_or_init(Default::default)
    }

    /// Is `client_id` shaped like a metadata-document URL (https, has a
    /// path other than `/`, no fragment)?
    pub fn is_cimd_client_id(client_id: &str) -> bool {
        client_id.len() <= 2000
            && url::Url::parse(client_id).is_ok_and(|u| {
                u.scheme() == "https" && u.path() != "/" && !u.path().is_empty() && u.fragment().is_none()
            })
    }

    /// Validate a fetched document for `client_id`. Pure.
    pub fn parse_document(doc: &Value, client_id: &str) -> Result<RegisteredClient, String> {
        let obj = doc.as_object().ok_or("the client metadata document is not a JSON object")?;
        if obj.get("client_id").and_then(|v| v.as_str()) != Some(client_id) {
            return Err("the client metadata document names a different client_id".into());
        }
        match obj.get("token_endpoint_auth_method").and_then(|v| v.as_str()) {
            None | Some("none") => {}
            Some(_) => return Err("only public clients (token_endpoint_auth_method none) are supported".into()),
        }
        let uris: Vec<String> = obj
            .get("redirect_uris")
            .and_then(|v| v.as_array())
            .ok_or("the client metadata document has no redirect_uris")?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or("redirect_uris must be strings")?;
        if uris.is_empty() || uris.len() > super::MAX_REDIRECT_URIS || !uris.iter().all(|u| redirect_uri_acceptable(u)) {
            return Err("redirect_uris must be 1..=10 https:// or loopback http:// URLs".into());
        }
        Ok(RegisteredClient {
            client_name: sanitize_client_name(obj.get("client_name").and_then(|v| v.as_str())),
            redirect_uris: uris,
            created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
        })
    }

    /// Fetch attempts use process-wide slots, bounded positive/negative caches
    /// and a dedicated per-source budget, independent of bearer hints.
    pub async fn resolve(client_id: &str, source: &str) -> Result<RegisteredClient, ResolveError> {
        resolve_from(client_id, source, OutboundPolicy::PUBLIC_ONLY).await
    }

    #[cfg(test)]
    pub async fn resolve_with(client_id: &str, policy: OutboundPolicy) -> Result<RegisteredClient, ResolveError> {
        resolve_from(client_id, "test-source", policy).await
    }

    async fn resolve_from(client_id: &str, source: &str, policy: OutboundPolicy) -> Result<RegisteredClient, ResolveError> {
        let id = client_id.to_owned();
        guard().resolve(client_id, source, async move { fetch(&id, policy).await }, FETCH_TIMEOUT).await
    }

    async fn fetch(client_id: &str, policy: OutboundPolicy) -> Result<RegisteredClient, String> {
        let url = url::Url::parse(client_id).map_err(|_| "client_id is not a URL".to_string())?;
        check_outbound(&url, policy)?;
        let http = pinned_client(&url, policy, Duration::from_secs(5)).await?;
        let resp = http
            .get(url.clone())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| "the client metadata document could not be fetched".to_string())?;
        if resp.status().as_u16() != 200 {
            return Err(format!("the client metadata document answered HTTP {}", resp.status().as_u16()));
        }
        let bytes = duduclaw_gateway::remote_mcp::http::read_capped(resp, MAX_DOC_BYTES).await?;
        let doc: Value = serde_json::from_slice(&bytes).map_err(|_| "the client metadata document is not JSON".to_string())?;
        let client = parse_document(&doc, client_id)?;
        Ok(client)
    }

    #[cfg(test)]
    mod guard_tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn client() -> RegisteredClient {
            RegisteredClient { client_name: "Example".into(), redirect_uris: vec![],
                created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None }
        }

        #[tokio::test]
        async fn four_fetches_bound_global_work_even_when_callers_cancel() {
            let guard = Arc::new(FetchGuard::default());
            let release = Arc::new(tokio::sync::Semaphore::new(0));
            let (started, mut ready) = tokio::sync::mpsc::channel(4);
            let mut callers = Vec::new();
            for n in 0..4 {
                let guard = guard.clone(); let release = release.clone(); let started = started.clone();
                callers.push(tokio::spawn(async move {
                    guard.resolve(&format!("https://example.com/{n}"), &format!("source-{n}"), async move {
                        started.send(()).await.unwrap();
                        let _permit = release.acquire().await.unwrap();
                        Ok(client())
                    }, Duration::from_secs(30)).await
                }));
            }
            for _ in 0..4 { tokio::time::timeout(Duration::from_secs(2), ready.recv()).await.unwrap().unwrap(); }
            assert_eq!(guard.slots.available_permits(), 0);
            for caller in callers { caller.abort(); let _ = caller.await; }
            assert_eq!(guard.slots.available_permits(), 0);
            let result = guard.resolve("https://example.com/overflow", "another-source", async {
                panic!("fifth fetch must not start");
            }, FETCH_TIMEOUT).await;
            assert!(matches!(result, Err(ResolveError::Temporary)));
            // Acquire failure is also negatively cached across source addresses.
            assert!(matches!(guard.cached("https://example.com/overflow").unwrap(), Some(Err(ResolveError::Temporary))));
            release.add_permits(4);
            tokio::time::timeout(Duration::from_secs(2), async {
                while guard.slots.available_permits() != 4 { tokio::task::yield_now().await; }
            }).await.unwrap();
        }

        #[tokio::test]
        async fn slow_fetch_is_negatively_cached_without_releasing_running_worker() {
            let guard = FetchGuard::default();
            let (release, wait) = tokio::sync::oneshot::channel::<()>();
            let id = "https://example.com/slow";
            assert!(matches!(guard.resolve(id, "source", async {
                wait.await.unwrap(); Ok(client())
            }, Duration::from_millis(10)).await, Err(ResolveError::Temporary)));
            assert_eq!(guard.slots.available_permits(), 3);
            assert!(matches!(guard.resolve(id, "different-source", async {
                panic!("negative cache must prevent another slow fetch");
            }, FETCH_TIMEOUT).await, Err(ResolveError::Temporary)));
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while guard.slots.available_permits() != 4 { tokio::task::yield_now().await; }
            }).await.unwrap();
            assert!(matches!(guard.cached(id).unwrap(), Some(Err(ResolveError::Temporary))));
        }

        #[tokio::test]
        async fn failed_fetch_cache_expires_is_bounded_and_success_is_cached() {
            let guard = FetchGuard::default();
            let id = "https://example.com/failure";
            assert!(matches!(guard.resolve(id, "source", async { Err("bad JSON".into()) }, FETCH_TIMEOUT).await,
                Err(ResolveError::Invalid(_))));
            assert!(matches!(guard.resolve(id, "other-source", async {
                panic!("failed fetch must be cached");
            }, FETCH_TIMEOUT).await, Err(ResolveError::Invalid(_))));
            guard.cache.lock().unwrap().get_mut(id).unwrap().1 = Instant::now() - Duration::from_secs(1);
            assert!(guard.resolve(id, "source", async { Ok(client()) }, FETCH_TIMEOUT).await.is_ok());
            assert!(guard.resolve(id, "source", async { panic!("success must be cached"); }, FETCH_TIMEOUT).await.is_ok());
            for n in 0..CACHE_MAX + 10 {
                guard.remember(&format!("https://example.com/failure-{n}"), Err(ResolveError::Temporary));
            }
            assert_eq!(guard.cache.lock().unwrap().len(), CACHE_MAX);
        }

        #[tokio::test]
        async fn cimd_attempt_budget_is_per_source_even_for_distinct_urls() {
            let guard = FetchGuard::default();
            let calls = Arc::new(AtomicUsize::new(0));
            for n in 0..13 {
                let calls = calls.clone();
                let result = guard.resolve(&format!("https://example.com/{n}"), "attacker", async move {
                    calls.fetch_add(1, Ordering::SeqCst); Err("TLS failure".into())
                }, FETCH_TIMEOUT).await;
                if n == 12 { assert!(matches!(result, Err(ResolveError::Temporary))); }
                else { assert!(matches!(result, Err(ResolveError::Invalid(_)))); }
            }
            assert_eq!(calls.load(Ordering::SeqCst), 12);
            assert!(guard.resolve("https://example.com/12", "legitimate", async { Ok(client()) }, FETCH_TIMEOUT).await.is_ok());
            let response = ResolveError::Temporary.response();
            assert_eq!(response.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
            let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["error"], "temporarily_unavailable");
        }
    }

}

/// A registered client, or — for an unregistered `https://` client id — its
/// Client ID Metadata Document.
async fn resolve_client(store: &OAuthStore, client_id: &str, source: &str) -> Result<RegisteredClient, Response> {
    if let Some(c) = store.clients.get(client_id) {
        if !c.active_at(now_unix()) { return Err(html_error(StatusCode::BAD_REQUEST, "client registration expired; register again")); }
        return Ok(c.clone());
    }
    if cimd::is_cimd_client_id(client_id) {
        return cimd::resolve(client_id, source).await.map_err(cimd::ResolveError::response);
    }
    Err(html_error(StatusCode::BAD_REQUEST, "unknown client_id — register first via /oauth/register"))
}

// ── Dynamic client registration (RFC 7591) ────────────────────────────────────

/// Anchored redirect-URI policy: `https://…` anywhere, plain `http://…` only
/// on loopback hosts (native-app callbacks). No custom schemes.
fn redirect_uri_acceptable(uri: &str) -> bool {
    if uri.len() > 2000 || uri.chars().any(|c| c.is_control()) {
        return false;
    }
    if uri.starts_with("https://") {
        return uri.len() > "https://".len();
    }
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let prefix = format!("http://{host}");
        if let Some(rest) = uri.strip_prefix(&prefix) {
            // Anchor the host: what follows must be a port, a path, or the
            // end — `http://localhost.evil.com/` must not pass.
            return rest.is_empty() || rest.starts_with(':') || rest.starts_with('/');
        }
    }
    false
}

fn sanitize_client_name(raw: Option<&str>) -> String {
    let name: String = raw
        .unwrap_or("Unnamed MCP client")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_NAME_CHARS)
        .collect();
    if name.trim().is_empty() {
        "Unnamed MCP client".to_string()
    } else {
        name
    }
}

pub(crate) async fn register_handler(
    State(state): State<HttpState>,
    Extension(budget): Extension<RegistrationBudget>,
    Json(body): Json<Value>,
) -> Response {
    crate::mcp_http_server::blocking_http(move || register_response(&state.home_dir, body, &budget)).await
}

fn register_response(home: &Path, body: Value, budget: &RegistrationBudget) -> Response {
    if body.get("token_endpoint_auth_method").is_some_and(|method| method.as_str() != Some("none")) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "only public clients are supported");
    }
    let Some(uris) = body.get("redirect_uris").and_then(|v| v.as_array()) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "redirect_uris is required",
        );
    };
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "redirect_uris must contain 1..=10 entries",
        );
    }
    let mut redirect_uris = Vec::new();
    for u in uris {
        match u.as_str() {
            Some(s) if redirect_uri_acceptable(s) => redirect_uris.push(s.to_string()),
            _ => {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_redirect_uri",
                    "redirect_uris must be https:// or loopback http:// URLs",
                );
            }
        }
    }
    let client_name = sanitize_client_name(body.get("client_name").and_then(|v| v.as_str()));

    let client_id = format!("mcp_{}", uuid::Uuid::new_v4().simple());
    let result = update_store(home, |store| {
        let victim = if store.clients.len() >= MAX_CLIENTS {
            match store.registration_eviction_candidate(now_unix()) {
                Some(id) if store.clients.len() == MAX_CLIENTS => Some(id),
                _ => return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "registration limit reached")),
            }
        } else { None };
        // Invalid metadata and a full protected registry never charge this budget.
        if !budget.admit_creation() {
            return Err(oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable", "retry registration later"));
        }
        if let Some(id) = victim { store.clients.remove(&id); }
        store.clients.insert(client_id.clone(), RegisteredClient {
            client_name: client_name.clone(), redirect_uris: redirect_uris.clone(),
            created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
        });
        Ok(())
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(response)) => return response,
        Err(_) => return store_error(),
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": client_name,
            "redirect_uris": redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    )
        .into_response()
}

// ── Authorization endpoint + consent ──────────────────────────────────────────

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Filter a space-delimited OAuth scope string down to the externally
/// grantable set (unknown scopes are dropped — OAuth allows narrowing).
fn filter_scope(requested: &str) -> String {
    let grantable = grantable_scope_strings();
    requested
        .split_whitespace()
        .filter(|s| grantable.iter().any(|g| g == s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The C1 brand cat mark (navy ink + amber tail), inlined into the OAuth pages
/// so they render with no extra request. The file is a copy of
/// `docs/assets/brand/duduclaw-mark.svg` kept inside the crate because the
/// Docker build contexts copy `crates/` but not `docs/assets/`;
/// `brand_mark_matches_brand_kit` fails if the two copies drift apart.
const BRAND_MARK_SVG: &str = include_str!("../assets/duduclaw-mark.svg");

/// Shared head + page chrome for the OAuth pages: C1 palette (cream page, navy
/// text, navy primary button) and a heading row of mark + title. Callers
/// escape anything user-supplied in `body`; the page escapes the identity.
fn oauth_page(title: &str, heading: &str, body: &str) -> String {
    let brand = duduclaw_gateway::distributor_brand::effective();
    oauth_page_with_brand(title, heading, body, brand.as_ref())
}

fn oauth_page_with_brand(
    title: &str,
    heading: &str,
    body: &str,
    brand: Option<&duduclaw_gateway::distributor_brand::Brand>,
) -> String {
    let (title, heading, mark) = match brand {
        Some(brand) => (
            escape_html(&title.replace("DuDuClaw", &brand.product_name)),
            escape_html(&heading.replace("DuDuClaw", &brand.product_name)),
            format!(
                "<img src=\"{}\" alt=\"\" aria-hidden=\"true\" style=\"width:120px;height:40px;object-fit:contain;flex:none\">",
                escape_html(&brand.logo_light),
            ),
        ),
        None => (
            title.to_string(),
            heading.to_string(),
            BRAND_MARK_SVG.trim().replacen(
                "<svg ",
                "<svg aria-hidden=\"true\" focusable=\"false\" ",
                1,
            ),
        ),
    };
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{title}</title>\
<style>\
body{{font-family:\"Avenir Next\",\"Nunito Sans\",system-ui,-apple-system,\"PingFang TC\",sans-serif;\
max-width:34rem;margin:4rem auto;padding:0 1rem;line-height:1.6;background:#F5F4F0;color:#102B48}}\
.brand{{display:flex;align-items:center;gap:.75rem;margin-bottom:1rem}}\
.brand svg{{width:40px;height:40px;flex:none}}\
.brand h2{{margin:0;font-weight:600;letter-spacing:-.01em}}\
code{{background:#FFFFFF;border:1px solid #D4D7D8;border-radius:4px;padding:0 .25rem}}\
input{{border:1px solid #D4D7D8;border-radius:8px;background:#FFFFFF;color:#102B48}}\
button{{border-radius:8px;font:inherit;cursor:pointer}}\
.primary{{background:#102B48;color:#F5F4F0;border:1px solid #102B48}}\
.secondary{{background:transparent;color:#102B48;border:1px solid #102B48}}\
:focus-visible{{outline:2px solid #CC8B2D;outline-offset:2px}}\
.note{{color:#5B6470;font-size:.85rem}}\
</style>\
<body><div class=\"brand\">{mark}<h2>{heading}</h2></div>{body}</body>",
    )
}

fn html_error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Html(oauth_page(
            "DuDuClaw OAuth",
            "DuDuClaw",
            &format!("<p>{}</p>", escape_html(msg)),
        )),
    )
        .into_response()
}

pub(crate) async fn authorize_handler(
    State(state): State<HttpState>,
    Extension(source): Extension<SourceIp>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if let Err(response) = resource_params(&params, &state.resource) { return response; }
    let q: HashMap<String, String> = params.into_iter().collect();
    let home = state.home_dir.clone();
    let store = match crate::mcp_http_server::blocking_io(move || load_store(&home)).await {
        Ok(Ok(store)) => store, Ok(Err(_)) => return store_error(), Err(response) => return response,
    };
    // Client + redirect_uri must validate BEFORE any redirect is possible
    // (never bounce a browser to an unverified URI).
    let Some(client_id) = q.get("client_id") else {
        return html_error(StatusCode::BAD_REQUEST, "missing client_id");
    };
    let client = match resolve_client(&store, client_id, &source.0).await {
        Ok(c) => c,
        Err(response) => return response,
    };
    let Some(redirect_uri) = q.get("redirect_uri") else {
        return html_error(StatusCode::BAD_REQUEST, "missing redirect_uri");
    };
    if !client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return html_error(
            StatusCode::BAD_REQUEST,
            "redirect_uri is not registered for this client",
        );
    }

    let state_param = q.get("state").cloned().unwrap_or_default();
    let bounce = |err: &str| {
        let sep = if redirect_uri.contains('?') { '&' } else { '?' };
        let mut url = format!("{redirect_uri}{sep}error={err}");
        if !state_param.is_empty() {
            url.push_str(&format!("&state={}", urlencoding::encode(&state_param)));
        }
        Redirect::to(&url).into_response()
    };

    if q.get("response_type").map(String::as_str) != Some("code") {
        return bounce("unsupported_response_type");
    }
    let challenge = q.get("code_challenge").cloned().unwrap_or_default();
    let challenge_ok = (43..=128).contains(&challenge.len())
        && challenge
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c));
    if !challenge_ok || q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        // PKCE S256 is REQUIRED (OAuth 2.1 / MCP spec).
        return bounce("invalid_request");
    }

    let scope = filter_scope(q.get("scope").map(String::as_str).unwrap_or(""));
    let scope_note = if scope.is_empty() {
        "基本工具面（7 個基礎工具）".to_string()
    } else {
        format!("基本工具面＋<code>{}</code>", escape_html(&scope))
    };

    let body = format!(
        "<p>應用程式 <b>{name}</b> 想連接你的 {product}（remote MCP）。</p>\
<p>授權範圍：{scope_note}</p>\
<p>貼上一把<b>內部 MCP API key</b>（<code>config.toml [mcp_keys]</code> 中 \
<code>is_external = false</code> 的 key）以確認你是操作者本人：</p>\
<form method=\"post\" action=\"/oauth/decision\">\
<input type=\"hidden\" name=\"client_id\" value=\"{client_id}\">\
<input type=\"hidden\" name=\"redirect_uri\" value=\"{redirect_uri}\">\
<input type=\"hidden\" name=\"state\" value=\"{state_v}\">\
<input type=\"hidden\" name=\"code_challenge\" value=\"{challenge}\">\
<input type=\"hidden\" name=\"scope\" value=\"{scope}\">\
<input type=\"hidden\" name=\"resource\" value=\"{resource}\">\
<p><input type=\"password\" name=\"operator_key\" placeholder=\"ddc_…\" \
style=\"width:100%;padding:.5rem\" autocomplete=\"off\" required></p>\
<p><button class=\"primary\" name=\"action\" value=\"approve\" style=\"padding:.5rem 1.5rem\">同意連線</button> \
<button class=\"secondary\" name=\"action\" value=\"deny\" style=\"padding:.5rem 1.5rem\">拒絕</button></p>\
</form>\
<p class=\"note\">OAuth 簽發的存取權杖永遠是「外部客戶端」等級：\
只能使用基礎工具面與你在 key 簽發時明示授與的範圍，連接器／執行類／管理工具永不開放。</p>",
        name = escape_html(&client.client_name),
        product = escape_html(&duduclaw_gateway::distributor_brand::product_name()),
        scope_note = scope_note,
        client_id = escape_html(client_id),
        redirect_uri = escape_html(redirect_uri),
        state_v = escape_html(&state_param),
        challenge = escape_html(&challenge),
        scope = escape_html(&scope),
        resource = escape_html(&state.resource),
    );
    Html(oauth_page("DuDuClaw 授權", "DuDuClaw 連線授權", &body)).into_response()
}

#[derive(Deserialize)]
pub(crate) struct DecisionForm {
    resource: Option<String>,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: String,
    code_challenge: String,
    #[serde(default)]
    scope: String,
    operator_key: String,
    action: String,
}

pub(crate) async fn decision_handler(
    State(http_state): State<HttpState>,
    Extension(source): Extension<SourceIp>,
    Form(f): Form<DecisionForm>,
) -> Response {
    if let Err(response) = requested_resource(f.resource.as_deref(), &http_state.resource) { return response; }
    let home = http_state.home_dir.clone();
    let store = match crate::mcp_http_server::blocking_io(move || load_store(&home)).await {
        Ok(Ok(store)) => store, Ok(Err(_)) => return store_error(), Err(response) => return response,
    };
    // Re-validate against the store — hidden form fields are client-supplied.
    let client = match resolve_client(&store, &f.client_id, &source.0).await {
        Ok(c) => c,
        Err(response) => return response,
    };
    crate::mcp_http_server::blocking_http(move || decision_response(&http_state.home_dir, client, f, &http_state.resource)).await
}

fn decision_response(home: &Path, client: RegisteredClient, f: DecisionForm, resource: &str) -> Response {
    if let Err(response) = requested_resource(f.resource.as_deref(), resource) { return response; }
    if !client.redirect_uris.iter().any(|u| u == &f.redirect_uri) {
        return html_error(
            StatusCode::BAD_REQUEST,
            "redirect_uri is not registered for this client",
        );
    }
    let sep = if f.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let state_suffix = if f.state.is_empty() {
        String::new()
    } else {
        format!("&state={}", urlencoding::encode(&f.state))
    };

    if f.action != "approve" {
        return Redirect::to(&format!(
            "{}{sep}error=access_denied{state_suffix}",
            f.redirect_uri
        ))
        .into_response();
    }

    // Operator proof: a valid INTERNAL key. External keys cannot mint grants
    // (no self-escalation), and failures never redirect a code anywhere.
    match crate::mcp_auth::authenticate_with_key(&f.operator_key, home) {
        Ok(p) if !p.is_external => {}
        _ => {
            return html_error(
                StatusCode::UNAUTHORIZED,
                "operator key rejected — paste an internal (is_external = false) MCP API key",
            );
        }
    }

    let scope = filter_scope(&f.scope);
    let Some(consent) = Consent::capture(home, &f.operator_key, &scope) else {
        return html_error(StatusCode::UNAUTHORIZED, "operator key does not authorize these scopes");
    };
    let code = random_secret();
    if update_store(home, |store| {
        if let Some(client) = store.clients.get_mut(&f.client_id) { client.last_used_unix = Some(now_unix()); }
        store.codes.insert(sha256_hex(&code), PendingCode {
            resource: resource.to_owned(),
            client_id: f.client_id.clone(), redirect_uri: f.redirect_uri.clone(),
            code_challenge: f.code_challenge.clone(), scope,
            issued_unix: now_unix(), client_name: client.client_name, consent: Some(consent),
        });
    }).is_err() { return store_error(); }
    Redirect::to(&format!(
        "{}{sep}code={}{state_suffix}",
        f.redirect_uri,
        urlencoding::encode(&code)
    ))
    .into_response()
}

// ── Token endpoint ────────────────────────────────────────────────────────────

fn oauth_error(status: StatusCode, code: &str, desc: &str) -> Response {
    let mut resp = (
        status,
        Json(json!({ "error": code, "error_description": desc })),
    )
        .into_response();
    resp.headers_mut()
        .insert("Cache-Control", "no-store".parse().unwrap());
    resp
}

/// PKCE S256: base64url-nopad(SHA-256(verifier)) must equal the stored
/// challenge. Constant-time comparison (subtle) as belt-and-suspenders.
fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    use subtle::ConstantTimeEq;
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

fn mint_tokens(store: &mut OAuthStore, client_id: &str, client_name: &str, scope: &str, family_id: &str, resource: &str) -> Value {
    let access = format!("{OAUTH_TOKEN_PREFIX}{}", random_secret());
    let refresh = format!("{OAUTH_TOKEN_PREFIX}r_{}", random_secret());
    let now = now_unix();
    store.tokens.insert(
        sha256_hex(&access),
        IssuedToken {
            resource: resource.to_owned(),            client_id: client_id.to_string(),
            client_name: client_name.to_string(),
            scope: scope.to_string(),
            expires_unix: now + ACCESS_TOKEN_TTL_SECS,
            family_id: family_id.to_string(), used: false,
        },
    );
    store.refresh.insert(
        sha256_hex(&refresh),
        IssuedToken {
            resource: resource.to_owned(),            client_id: client_id.to_string(),
            client_name: client_name.to_string(),
            scope: scope.to_string(),
            expires_unix: now + REFRESH_TOKEN_TTL_SECS,
            family_id: family_id.to_string(), used: false,
        },
    );
    json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TOKEN_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope,
    })
}

pub(crate) async fn token_handler(
    State(state): State<HttpState>,
    Extension(source): Extension<SourceIp>,
    Form(params): Form<Vec<(String, String)>>,
) -> Response {
    if let Err(response) = resource_params(&params, &state.resource) { return response; }
    let f: HashMap<String, String> = params.into_iter().collect();
    let client = f.get("client_id").map(String::as_str).unwrap_or("");
    if !crate::mcp_http_server::oauth_client_admitted(&state.home_dir, client, &source.0) {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable", "retry token exchange later");
    }
    crate::mcp_http_server::blocking_http(move || token_response(&state.home_dir, &f, &state.resource)).await
}

fn token_response(home: &Path, f: &HashMap<String, String>, resource: &str) -> Response {
    if let Err(response) = requested_resource(f.get("resource").map(String::as_str), resource) { return response; }
    update_store(home, |store| {
        match f.get("grant_type").map(String::as_str).unwrap_or("") {
            "authorization_code" => {
                let (Some(code), Some(client_id), Some(redirect_uri), Some(verifier)) = (
                    f.get("code"), f.get("client_id"), f.get("redirect_uri"), f.get("code_verifier"),
                ) else {
                    return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "code, client_id, redirect_uri and code_verifier are required");
                };
                let Some(pending) = store.codes.remove(&sha256_hex(code)) else {
                    return invalid_grant();
                };
                if pending.resource != resource { return invalid_grant(); }
                if now_unix() - pending.issued_unix > CODE_TTL_SECS
                    || &pending.client_id != client_id || &pending.redirect_uri != redirect_uri
                    || !pkce_matches(verifier, &pending.code_challenge) {
                    return invalid_grant();
                }
                let Some(consent) = pending.consent.as_ref().filter(|c| c.valid(home, &pending.scope)).cloned() else {
                    return invalid_grant();
                };
                if filter_scope(&pending.scope) != pending.scope { return invalid_grant(); }
                if !store.can_mint(None) {
                    store.codes.insert(sha256_hex(code), pending);
                    return capacity_response();
                }
                let family = uuid::Uuid::new_v4().to_string();
                store.grants.insert(family.clone(), Grant {
                    resource: resource.to_owned(),
                    client_id: client_id.clone(), client_name: pending.client_name.clone(),
                    scope: pending.scope.clone(), consent, issued_unix: now_unix(), last_used_unix: None,
                });
                token_success(mint_tokens(store, client_id, &pending.client_name, &pending.scope, &family, resource))
            }
            "refresh_token" => {
                let (Some(raw), Some(client_id)) = (f.get("refresh_token"), f.get("client_id")) else {
                    return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "refresh_token and client_id are required");
                };
                let key = sha256_hex(raw);
                let Some(entry) = store.refresh.get(&key).cloned() else { return invalid_grant(); };
                // A different public client cannot revoke or consume this grant.
                if &entry.client_id != client_id { return invalid_grant(); }
                if entry.resource != resource { return invalid_grant(); }
                if !store.valid_token(&entry, home, resource) || filter_scope(&entry.scope) != entry.scope {
                    store.revoke_family(&entry.family_id);
                    return invalid_grant();
                }
                let scope = f.get("scope").map(String::as_str).unwrap_or(&entry.scope);
                if !scope_subset(scope, &entry.scope) {
                    return oauth_error(StatusCode::BAD_REQUEST, "invalid_scope", "refresh scopes may only narrow the existing grant");
                }
                let scope = filter_scope(scope);
                if !store.can_mint(Some(&entry.family_id)) { return capacity_response(); }
                // Retain the spent digest until expiry to detect replay and let
                // revocation of a rotated token reach its entire family.
                store.refresh.get_mut(&key).unwrap().used = true;
                store.grants.get_mut(&entry.family_id).unwrap().last_used_unix = Some(now_unix());
                if let Some(client) = store.clients.get_mut(client_id) { client.last_used_unix = Some(now_unix()); }
                token_success(mint_tokens(store, client_id, &entry.client_name, &scope, &entry.family_id, resource))
            }
            _ => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", "unsupported grant_type"),
        }
    }).unwrap_or_else(|_| store_error())
}

fn capacity_response() -> Response {
    oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable", "OAuth capacity reached; retry later or revoke unused grants")
}

fn invalid_grant() -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "grant expired, revoked or invalid; authorize again")
}

fn token_success(body: Value) -> Response {
    let mut response = Json(body).into_response();
    response.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
    response
}

pub(crate) async fn revoke_handler(
    State(state): State<HttpState>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    crate::mcp_http_server::blocking_http(move || revoke_response(&state.home_dir, &f)).await
}

fn revoke_response(home: &Path, f: &HashMap<String, String>) -> Response {
    let Some(client_id) = f.get("client_id").filter(|s| !s.is_empty()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "public clients must supply client_id");
    };
    let Some(raw) = f.get("token").filter(|s| !s.is_empty()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token is required");
    };
    // As at /oauth/token, only public clients (auth method none) are supported.
    // Ignore token_type_hint and search both stores (RFC 7009 section 2.1).
    update_store(home, |store| {
        let key = sha256_hex(raw);
        if let Some(entry) = store.refresh.get(&key).or_else(|| store.tokens.get(&key)).cloned()
            && &entry.client_id == client_id {
            store.revoke_family(&entry.family_id);
        }
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert("Cache-Control", "no-store".parse().unwrap());
        response
    }).unwrap_or_else(|_| store_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    const RESOURCE: &str = "https://mcp.example.com/mcp";
    fn validate_oauth_token(raw: &str, home: &Path) -> Result<Principal, AuthError> {
        super::validate_oauth_token(raw, home, RESOURCE)
    }
    fn mint_tokens(store: &mut OAuthStore, id: &str, name: &str, scope: &str, family: &str) -> Value {
        super::mint_tokens(store, id, name, scope, family, RESOURCE)
    }

    fn test_consent(home: &Path) -> Consent {
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='operator'\nscopes=['memory:read','wiki:read']\nis_external=false\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        Consent::capture(home, key, "memory:read wiki:read").unwrap()
    }

    /// The crate-local mark must stay byte-identical to the brand kit copy.
    /// Skipped when the brand kit is absent (e.g. a source tree without docs/).
    #[test]
    fn brand_mark_matches_brand_kit() {
        let kit = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/assets/brand/duduclaw-mark.svg");
        let Ok(kit_svg) = std::fs::read_to_string(&kit) else {
            return;
        };
        assert_eq!(
            BRAND_MARK_SVG, kit_svg,
            "crates/duduclaw-cli/assets/duduclaw-mark.svg drifted from docs/assets/brand/duduclaw-mark.svg"
        );
    }

    #[test]
    fn cimd_ids_and_documents_are_validated() {
        assert!(cimd::is_cimd_client_id("https://chatgpt.com/oauth/client.json"));
        assert!(!cimd::is_cimd_client_id("https://chatgpt.com/"));
        assert!(!cimd::is_cimd_client_id("http://chatgpt.com/c.json"));
        assert!(!cimd::is_cimd_client_id("abc123"));
        let id = "https://app.example.com/client.json";
        let good = json!({
            "client_id": id, "client_name": "Example <b>",
            "redirect_uris": ["https://app.example.com/cb"]
        });
        let c = cimd::parse_document(&good, id).unwrap();
        assert_eq!(c.redirect_uris, vec!["https://app.example.com/cb"]);
        let mut other = good.clone();
        other["client_id"] = json!("https://evil.example/client.json");
        assert!(cimd::parse_document(&other, id).is_err());
        let mut private = good.clone();
        private["token_endpoint_auth_method"] = json!("private_key_jwt");
        assert!(cimd::parse_document(&private, id).is_err());
        let mut bad_uri = good.clone();
        bad_uri["redirect_uris"] = json!(["myapp://cb"]);
        assert!(cimd::parse_document(&bad_uri, id).is_err());
        assert!(cimd::parse_document(&json!([]), id).is_err());
    }

    #[tokio::test]
    async fn cimd_document_is_fetched_capped_and_cached() {
        use axum::routing::get;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let id = format!("{base}/client.json");
        let doc = json!({ "client_id": id, "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"] });
        let big = "x".repeat(cimd::MAX_DOC_BYTES + 10);
        let app = axum::Router::new()
            .route("/client.json", get(move || { let d = doc.clone(); async move { axum::Json(d) } }))
            .route("/big.json", get(move || { let b = big.clone(); async move { b } }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let loopback = duduclaw_gateway::remote_mcp::url_policy::OutboundPolicy { allow_loopback: true };
        let c = cimd::resolve_with(&id, loopback).await.unwrap();
        assert_eq!(c.redirect_uris, vec!["https://chatgpt.com/connector_platform_oauth_redirect"]);
        assert!(cimd::resolve_with(&format!("{base}/big.json"), loopback).await.is_err());
        // Production policy: loopback refused before any request.
        assert!(cimd::resolve_with(&format!("{base}/other.json"), duduclaw_gateway::remote_mcp::url_policy::OutboundPolicy::PUBLIC_ONLY).await.is_err());
    }

    #[test]
    fn redirect_uri_policy_is_anchored_and_fail_closed() {
        assert!(redirect_uri_acceptable(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_uri_acceptable("http://localhost:33418/callback"));
        assert!(redirect_uri_acceptable("http://127.0.0.1/cb"));
        assert!(redirect_uri_acceptable("http://[::1]:8080/cb"));
        // Anchoring: loopback lookalikes and other schemes are refused.
        assert!(!redirect_uri_acceptable("http://localhost.evil.com/cb"));
        assert!(!redirect_uri_acceptable("http://evil.com/cb"));
        assert!(!redirect_uri_acceptable("myapp://callback"));
        assert!(!redirect_uri_acceptable("https://"));
        assert!(!redirect_uri_acceptable("javascript:alert(1)"));
    }

    #[test]
    fn scope_filter_narrows_to_grantable_only() {
        assert_eq!(
            filter_scope("memory:read admin odoo:execute wiki:read"),
            "memory:read wiki:read"
        );
        assert_eq!(filter_scope(""), "");
        assert_eq!(filter_scope("bogus"), "");
    }

    #[test]
    fn pkce_s256_roundtrip() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert!(pkce_matches(verifier, &challenge));
        assert!(!pkce_matches(
            "wrong-verifier-wrong-verifier-wrong-verifier",
            &challenge
        ));
    }

    #[test]
    fn issued_tokens_validate_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OAuthStore::default();
        let consent = test_consent(dir.path());
        store.grants.insert("family".into(), Grant { resource: RESOURCE.into(), client_id: "mcp_abc".into(), client_name: "Test Client".into(), scope: "memory:read".into(), consent, issued_unix: now_unix(), last_used_unix: None });
        let body = mint_tokens(&mut store, "mcp_abc", "Test Client", "memory:read", "family");
        update_store(dir.path(), |s| *s = store).unwrap();
        let access = body["access_token"].as_str().unwrap();

        let p = validate_oauth_token(access, dir.path()).unwrap();
        assert!(p.is_external, "OAuth principals must be external");
        assert_eq!(p.client_id, "oauth_mcp_abc");
        assert!(p.scopes.contains(&crate::mcp_auth::Scope::MemoryRead));

        // Unknown token → rejected.
        assert!(validate_oauth_token("ddc_oauth_deadbeef", dir.path()).is_err());

        // Expired token → rejected.
        let mut store = load_store(dir.path()).unwrap();
        for t in store.tokens.values_mut() {
            t.expires_unix = now_unix() - 10;
        }
        // Bypass save_store's pruning to keep the expired row on disk.
        std::fs::write(
            store_path(dir.path()),
            serde_json::to_string(&store).unwrap(),
        )
        .unwrap();
        assert!(validate_oauth_token(access, dir.path()).is_err());
    }

    #[test]
    fn client_name_is_sanitized_and_escaped() {
        assert_eq!(
            sanitize_client_name(Some("Evil\u{7}<script>")),
            "Evil<script>"
        );
        assert_eq!(escape_html("Evil<script>"), "Evil&lt;script&gt;");
        assert_eq!(sanitize_client_name(None), "Unnamed MCP client");
        assert_eq!(sanitize_client_name(Some("   ")), "Unnamed MCP client");
    }

    #[test]
    fn default_oauth_pages_keep_the_inline_brand_mark() {
        let page = oauth_page_with_brand("DuDuClaw 授權", "DuDuClaw 連線授權", "<p>x</p>", None);
        // Mark is inlined (both color layers), decorative, and precedes the heading.
        assert!(page.contains("<svg aria-hidden=\"true\" focusable=\"false\" "));
        assert!(page.contains("class=\"ink\" fill=\"#102B48\""));
        assert!(page.contains("class=\"accent\" fill=\"#CC8B2D\""));
        let mark_at = page.find("<svg").unwrap();
        let heading_at = page.find("<h2>DuDuClaw 連線授權</h2>").unwrap();
        assert!(mark_at < heading_at);
        // The retired paw glyph is gone from both pages.
        assert!(!page.contains('🐾'));
        let err = oauth_page_with_brand("DuDuClaw OAuth", "DuDuClaw", "<p>missing client_id</p>", None);
        assert!(!err.contains('🐾'));
        assert!(err.contains("<h2>DuDuClaw</h2>"));
        assert!(page.contains("<title>DuDuClaw 授權</title>"));
        assert!(page.ends_with("<h2>DuDuClaw 連線授權</h2></div><p>x</p></body>"));
        assert!(!page.contains("<img"));
    }

    #[test]
    fn distributor_oauth_identity_is_escaped_and_logo_keeps_its_aspect_ratio() {
        let brand = duduclaw_gateway::distributor_brand::Brand {
            product_name: "A & \"B\" <tag>".into(),
            logo_light: "data:image/png;base64,test\" onerror=\"bad".into(),
            logo_dark: None,
        };
        let page = oauth_page_with_brand("DuDuClaw 授權", "DuDuClaw 連線授權", "<p>safe</p>", Some(&brand));
        assert!(page.contains("<title>A &amp; &quot;B&quot; &lt;tag&gt; 授權</title>"));
        assert!(page.contains("<h2>A &amp; &quot;B&quot; &lt;tag&gt; 連線授權</h2>"));
        assert!(page.contains("object-fit:contain"));
        assert!(page.contains("test&quot; onerror=&quot;bad\""));
        assert!(!page.contains(" onerror=\"bad"));
        assert!(!page.contains("<svg"));
    }
}

#[cfg(test)]
#[path = "mcp_oauth_server_tests.rs"]
mod lifecycle_tests;
