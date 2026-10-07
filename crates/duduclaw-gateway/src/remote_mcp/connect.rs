//! Connecting, signing in, disconnecting, and handing the bridge a token.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use super::oauth::{self, ClientCredentials, TokenError};
use super::store::{self, AuthKind, ConnStatus, OAuthSecrets, RemoteSecrets, RemoteServerRecord};
use super::url_policy::{OutboundPolicy, is_loopback_host, validate_remote_url};

/// Path of the gateway route the authorization server redirects back to.
pub const CALLBACK_PATH: &str = "/oauth/mcp/callback";
/// How long a started sign-in stays valid.
pub const PENDING_TTL: Duration = Duration::from_secs(600);
/// At most this many sign-ins wait at once (oldest dropped first).
const MAX_PENDING: usize = 64;
/// A token this close to expiry is refreshed before use.
pub const REFRESH_SKEW_SECS: i64 = 60;

/// A sign-in waiting for its callback. Lives only in gateway memory.
struct PendingConnect {
    agent_id: String,
    server: String,
    mcp_url: String,
    redirect_uri: String,
    redirect_origin: String,
    code_verifier: String,
    token_endpoint: Url,
    issuer: String,
    creds: ClientCredentials,
    resource: String,
    scope: Option<String>,
    created: Instant,
}

fn pending() -> &'static Mutex<HashMap<String, PendingConnect>> {
    static P: OnceLock<Mutex<HashMap<String, PendingConnect>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prune(map: &mut HashMap<String, PendingConnect>) {
    map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
    while map.len() >= MAX_PENDING {
        let oldest = map
            .iter()
            .min_by_key(|(_, p)| p.created)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                map.remove(&k);
            }
            None => break,
        }
    }
}

/// Take (single use) a pending sign-in by state. Expired ⇒ `None`.
fn take_pending(state: &str) -> Option<PendingConnect> {
    let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
    let p = map.remove(state)?;
    if p.created.elapsed() >= PENDING_TTL {
        return None;
    }
    Some(p)
}

/// Validate the dashboard origin the browser should come back to, and
/// return it normalized (`scheme://host[:port]`).
///
/// Allowed: a loopback origin (`localhost`, `127.0.0.0/8`, `[::1]`, http or
/// https), or an `https` origin whose authority is in the gateway's allowed
/// origins (`[gateway] allowed_origins` + `DUDUCLAW_ALLOWED_ORIGINS`, matched
/// with `duduclaw_core::origin_host_matches`). Plain http to a non-loopback
/// origin is refused: OAuth 2.1 requires https redirect URIs except loopback.
pub fn validate_redirect_origin(raw: &str, allowed: &[String]) -> Result<String, String> {
    let raw = raw.trim();
    let url = Url::parse(raw).map_err(|e| format!("redirect_origin is not a valid origin: {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("redirect_origin must not contain a user name or password".into());
    }
    if !(url.path() == "/" || url.path().is_empty()) || url.query().is_some() || url.fragment().is_some() {
        return Err("redirect_origin must be an origin (scheme, host and port only)".into());
    }
    let host = url.host().ok_or_else(|| "redirect_origin has no host".to_string())?;
    let origin = url.origin().ascii_serialization();
    match url.scheme() {
        "http" | "https" if is_loopback_host(&host) => Ok(origin),
        "https" => {
            let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
            if duduclaw_core::origin_host_matches(&origin, &allowed) {
                Ok(origin)
            } else {
                Err("redirect_origin is not one of the gateway's allowed origins ([gateway] allowed_origins)".into())
            }
        }
        "http" => Err("a non-loopback dashboard origin must use https to receive an OAuth redirect".into()),
        other => Err(format!("unsupported redirect_origin scheme {other}")),
    }
}

/// What the operator asked for.
pub struct ConnectRequest {
    pub agent_id: String,
    pub server: String,
    /// `None` ⇒ reuse the URL already recorded for (agent, server).
    pub url: Option<String>,
    pub auth: AuthKind,
    pub bearer: Option<String>,
    pub redirect_origin: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// The gateway's allowed dashboard origins.
    pub allowed_origins: Vec<String>,
}

/// Result of [`start_connect`].
#[derive(Debug)]
pub enum ConnectOutcome {
    /// Stored and usable now (`none` / `bearer`).
    Connected,
    /// The dashboard must open this URL; the callback finishes the job.
    Authorize { authorize_url: String },
}

fn resolve_url(home: &Path, req: &ConnectRequest) -> Result<Url, String> {
    match req.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => validate_remote_url(raw),
        None => {
            let rec = store::get(home, &req.agent_id, &req.server)?.ok_or_else(|| {
                "no URL given and none is recorded for this server".to_string()
            })?;
            validate_remote_url(&store::open(home, &rec)?.url)
        }
    }
}

fn save_connected(
    home: &Path,
    agent_id: &str,
    server: &str,
    url: &Url,
    auth: AuthKind,
    secrets: RemoteSecrets,
) -> Result<(), String> {
    let now = store::now_rfc3339();
    let created_at = store::get(home, agent_id, server)?
        .map(|r| r.created_at)
        .unwrap_or_else(|| now.clone());
    let (expires, has_refresh) = match &secrets.oauth {
        Some(o) => (o.expires_at, o.refresh_token.is_some()),
        None => (None, false),
    };
    store::upsert(
        home,
        RemoteServerRecord {
            agent_id: agent_id.to_string(),
            server: server.to_string(),
            auth,
            host: super::http::host_label(url),
            status: ConnStatus::Connected,
            created_at,
            updated_at: now,
            access_expires_at: expires,
            has_refresh_token: has_refresh,
            secret_enc: store::seal(home, &secrets)?,
        },
    )
}

/// Start a connection. `none` / `bearer` are probed and stored at once;
/// `oauth` runs discovery and registration and returns the authorize URL.
pub async fn start_connect(home: &Path, req: ConnectRequest) -> Result<ConnectOutcome, String> {
    store::validate_ids(&req.agent_id, &req.server)?;
    let url = resolve_url(home, &req)?;
    let policy = OutboundPolicy::for_mcp_url(&url);
    match req.auth {
        AuthKind::None | AuthKind::Bearer => {
            let bearer = match req.auth {
                AuthKind::Bearer => {
                    let b = req
                        .bearer
                        .as_deref()
                        .map(str::trim)
                        .filter(|b| !b.is_empty())
                        .ok_or_else(|| "a bearer token is required for auth = bearer".to_string())?;
                    if b.len() > 8192 || b.chars().any(|c| c.is_control()) {
                        return Err("the bearer token is too long or contains control characters".into());
                    }
                    // Accept a pasted "Bearer xyz" as well as "xyz".
                    let b = match (b.get(..7), b.get(7..)) {
                        (Some(p), Some(rest)) if p.eq_ignore_ascii_case("bearer ") => rest.trim(),
                        _ => b,
                    };
                    Some(b.to_string())
                }
                _ => None,
            };
            probe_static(&url, policy, bearer.as_deref()).await?;
            save_connected(
                home,
                &req.agent_id,
                &req.server,
                &url,
                req.auth,
                RemoteSecrets { url: url.to_string(), bearer, oauth: None },
            )?;
            Ok(ConnectOutcome::Connected)
        }
        AuthKind::Oauth => {
            let origin = validate_redirect_origin(
                req.redirect_origin.as_deref().unwrap_or(""),
                &req.allowed_origins,
            )?;
            let redirect_uri = format!("{origin}{CALLBACK_PATH}");
            let disc = oauth::discover(&url, policy).await?;
            let creds = match req.client_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(cid) => {
                    let secret = req
                        .client_secret
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    ClientCredentials {
                        client_id: cid.to_string(),
                        token_endpoint_auth: oauth::auth_method_for_supplied(&disc.metadata, secret.is_some()),
                        client_secret: secret,
                    }
                }
                None => {
                    oauth::register_client(&disc.metadata, &redirect_uri, disc.scope.as_deref(), policy)
                        .await?
                }
            };
            let (verifier, challenge) = oauth::pkce_pair();
            let state = oauth::random_state();
            let authorize_url = oauth::build_authorize_url(
                &disc.metadata,
                &creds.client_id,
                &redirect_uri,
                &challenge,
                &state,
                &disc.resource,
                disc.scope.as_deref(),
            );
            // Record the URL so the dashboard can show the pending server.
            if store::get(home, &req.agent_id, &req.server)?.is_none() {
                let now = store::now_rfc3339();
                store::upsert(
                    home,
                    RemoteServerRecord {
                        agent_id: req.agent_id.clone(),
                        server: req.server.clone(),
                        auth: AuthKind::Oauth,
                        host: super::http::host_label(&url),
                        status: ConnStatus::NotConnected,
                        created_at: now.clone(),
                        updated_at: now,
                        access_expires_at: None,
                        has_refresh_token: false,
                        secret_enc: store::seal(
                            home,
                            &RemoteSecrets { url: url.to_string(), bearer: None, oauth: None },
                        )?,
                    },
                )?;
            }
            let entry = PendingConnect {
                agent_id: req.agent_id,
                server: req.server,
                mcp_url: url.to_string(),
                redirect_uri,
                redirect_origin: origin,
                code_verifier: verifier,
                token_endpoint: disc.metadata.token_endpoint.clone(),
                issuer: disc.metadata.issuer.clone(),
                creds,
                resource: disc.resource,
                scope: disc.scope,
                created: Instant::now(),
            };
            {
                let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
                prune(&mut map);
                map.insert(state, entry);
            }
            Ok(ConnectOutcome::Authorize { authorize_url: authorize_url.to_string() })
        }
    }
}

/// Probe a server with `initialize` using a static credential (or none).
async fn probe_static(url: &Url, policy: OutboundPolicy, bearer: Option<&str>) -> Result<(), String> {
    let client = super::url_policy::pinned_client(url, policy, super::http::REQUEST_TIMEOUT).await?;
    let mut req = client
        .post(url.clone())
        .header("Accept", "application/json, text/event-stream")
        .json(&oauth::initialize_probe_frame());
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("cannot reach {}: {e}", super::http::host_label(url)))?;
    let status = resp.status().as_u16();
    let session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // Close the probe session politely; failures are irrelevant.
    if let Some(sid) = session {
        let mut del = client.delete(url.clone()).header("Mcp-Session-Id", sid);
        if let Some(b) = bearer {
            del = del.bearer_auth(b);
        }
        let _ = del.send().await;
    }
    match status {
        200..=299 => Ok(()),
        401 | 403 if bearer.is_none() => Err(
            "the server requires sign-in (HTTP 401/403); connect with auth = oauth or a bearer token".into(),
        ),
        401 | 403 => Err("the server refused the bearer token (HTTP 401/403)".into()),
        s => Err(format!("the server answered HTTP {s} to initialize")),
    }
}

/// What a finished sign-in produced, for the callback page and the audit.
#[derive(Debug)]
pub struct Completed {
    pub agent_id: String,
    pub server: String,
    pub redirect_origin: String,
}

/// Abandon a pending sign-in (the authorization server answered with an
/// error). Returns the origin to send the browser back to.
pub fn cancel_pending(state: &str) -> Option<Completed> {
    take_pending(state).map(|p| Completed {
        agent_id: p.agent_id,
        server: p.server,
        redirect_origin: p.redirect_origin,
    })
}

/// Finish a sign-in: the state must match a pending, unexpired sign-in
/// (single use — a replayed callback finds nothing), then the code is
/// exchanged and the tokens stored encrypted.
pub async fn complete_callback(home: &Path, state: &str, code: &str) -> Result<Completed, String> {
    if state.is_empty() || state.len() > 256 {
        return Err("missing or invalid state".into());
    }
    if code.is_empty() || code.len() > 4096 {
        return Err("missing or invalid authorization code".into());
    }
    let p = take_pending(state)
        .ok_or_else(|| "unknown or expired sign-in; start again from the dashboard".to_string())?;
    let url = validate_remote_url(&p.mcp_url)?;
    let policy = OutboundPolicy::for_mcp_url(&url);
    let tokens = oauth::exchange_code(
        &p.token_endpoint,
        policy,
        &p.creds,
        code,
        &p.code_verifier,
        &p.redirect_uri,
        &p.resource,
    )
    .await
    .map_err(|e| e.to_string())?;
    let oauth_secrets = OAuthSecrets {
        issuer: p.issuer.clone(),
        token_endpoint: p.token_endpoint.to_string(),
        client_id: p.creds.client_id.clone(),
        client_secret: p.creds.client_secret.clone(),
        token_endpoint_auth: p.creds.token_endpoint_auth.clone(),
        resource: p.resource.clone(),
        scope: tokens.scope.clone().or(p.scope.clone()),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
    };
    save_connected(
        home,
        &p.agent_id,
        &p.server,
        &url,
        AuthKind::Oauth,
        RemoteSecrets { url: url.to_string(), bearer: None, oauth: Some(oauth_secrets) },
    )?;
    Ok(Completed { agent_id: p.agent_id, server: p.server, redirect_origin: p.redirect_origin })
}

/// Forget the credentials of a connection. `forget = false` keeps the record
/// (URL) as `not_connected` so it can be connected again; `forget = true`
/// removes it. Returns whether a record existed. Local only: no revocation
/// request is sent to the authorization server.
pub fn disconnect(home: &Path, agent_id: &str, server: &str, forget: bool) -> Result<bool, String> {
    store::validate_ids(agent_id, server)?;
    if forget {
        return store::remove(home, agent_id, server);
    }
    let Some(rec) = store::get(home, agent_id, server)? else {
        return Ok(false);
    };
    // Keep only the URL; a record whose secrets cannot be opened is removed.
    let url = match store::open(home, &rec) {
        Ok(s) => s.url,
        Err(_) => return store::remove(home, agent_id, server),
    };
    let sealed = store::seal(home, &RemoteSecrets { url, bearer: None, oauth: None })?;
    store::update(home, agent_id, server, move |r| {
        r.status = ConnStatus::NotConnected;
        r.access_expires_at = None;
        r.has_refresh_token = false;
        r.secret_enc = sealed;
        Ok(true)
    })
}

/// Non-secret view of every record (optionally one employee's).
pub fn status_list(home: &Path, agent_filter: Option<&str>) -> Result<Vec<Value>, String> {
    let now = chrono::Utc::now().timestamp();
    Ok(store::load_all(home)?
        .into_iter()
        .filter(|r| agent_filter.is_none_or(|a| r.agent_id == a))
        .map(|r| {
            let installed = duduclaw_agent::mcp_template::read_mcp_config(&home.join("agents").join(&r.agent_id))
                .ok()
                .and_then(|c| c.mcp_servers.get(&r.server).cloned())
                .is_some_and(|d| super::bridge_def::is_bridge_def(&d));
            json!({
                "agent_id": r.agent_id,
                "server": r.server,
                "auth": r.auth.as_str(),
                "host": r.host,
                "status": r.status.as_str(),
                "access_expires_at": r.access_expires_at,
                "access_token_expired": r.access_expires_at.is_some_and(|e| e <= now),
                "has_refresh_token": r.has_refresh_token,
                "installed": installed,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
            })
        })
        .collect())
}

/// Why the bridge has no usable credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Not connected / needs sign-in; the message tells the operator what to do.
    NotConnected(String),
    /// A transient failure (network, token endpoint down).
    Transient(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected(s) | Self::Transient(s) => f.write_str(s),
        }
    }
}

/// What the bridge sends upstream: the URL and the `Authorization` value.
#[derive(Clone)]
pub struct UpstreamAuth {
    pub url: Url,
    /// Full header value (`Bearer …`), or `None` for `auth = none`.
    pub authorization: Option<String>,
    /// Unix seconds the access token expires, if known.
    pub expires_at: Option<i64>,
}

fn not_connected(agent_id: &str, server: &str, why: &str) -> AuthError {
    AuthError::NotConnected(format!(
        "remote MCP server '{server}' for employee '{agent_id}' {why}; \
         an administrator must connect it in the dashboard (MCP → Remote servers)"
    ))
}

fn load_usable(home: &Path, agent_id: &str, server: &str) -> Result<(RemoteServerRecord, RemoteSecrets), AuthError> {
    let rec = store::get(home, agent_id, server)
        .map_err(AuthError::Transient)?
        .ok_or_else(|| not_connected(agent_id, server, "is not set up"))?;
    match rec.status {
        ConnStatus::Connected => {}
        ConnStatus::NotConnected => return Err(not_connected(agent_id, server, "is not connected")),
        ConnStatus::NeedsReauth => return Err(not_connected(agent_id, server, "needs to sign in again")),
    }
    let secrets = store::open(home, &rec).map_err(|e| AuthError::NotConnected(e))?;
    Ok((rec, secrets))
}

fn to_upstream(secrets: &RemoteSecrets) -> Result<UpstreamAuth, AuthError> {
    let url = validate_remote_url(&secrets.url).map_err(AuthError::NotConnected)?;
    let (authorization, expires_at) = match (&secrets.bearer, &secrets.oauth) {
        (Some(b), _) => (Some(format!("Bearer {b}")), None),
        (None, Some(o)) => (Some(format!("Bearer {}", o.access_token)), o.expires_at),
        (None, None) => (None, None),
    };
    Ok(UpstreamAuth { url, authorization, expires_at })
}

/// Take the per-record refresh lock (a separate lock file, held across the
/// network call so two bridge processes never spend the same rotating
/// refresh token).
async fn refresh_lock(home: &Path, agent_id: &str, server: &str) -> Result<std::fs::File, AuthError> {
    use fs2::FileExt;
    let dir = store::lock_dir(home);
    let path = dir.join(format!("{agent_id}__{server}.lock"));
    tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create lock dir: {e}"))?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("cannot open refresh lock: {e}"))?;
        f.lock_exclusive().map_err(|e| format!("cannot take refresh lock: {e}"))?;
        Ok(f)
    })
    .await
    .map_err(|e| AuthError::Transient(format!("lock task failed: {e}")))?
    .map_err(AuthError::Transient)
}

/// The credential to use now. With `force_refresh` (the server answered 401)
/// or when the access token is within [`REFRESH_SKEW_SECS`] of expiry, the
/// token is refreshed under the refresh lock, re-reading the record first so a
/// refresh another process just did is reused instead of repeated. A refused
/// refresh token (`invalid_grant`) marks the record `needs_reauth`.
///
/// `seen` is the `Authorization` value the caller last used: when forcing a
/// refresh and the stored token already differs, the stored one is returned
/// without a network call.
pub async fn upstream_auth(
    home: &Path,
    agent_id: &str,
    server: &str,
    force_refresh: bool,
    seen: Option<&str>,
) -> Result<UpstreamAuth, AuthError> {
    let (rec, secrets) = load_usable(home, agent_id, server)?;
    let now = chrono::Utc::now().timestamp();
    let Some(o) = &secrets.oauth else {
        if force_refresh && rec.auth != AuthKind::None {
            return Err(AuthError::NotConnected(format!(
                "remote MCP server '{server}' refused the stored bearer token (HTTP 401); \
                 an administrator must enter a new one in the dashboard"
            )));
        }
        return to_upstream(&secrets);
    };
    let expiring = o.expires_at.is_some_and(|e| e - REFRESH_SKEW_SECS <= now);
    if !force_refresh && !expiring {
        return to_upstream(&secrets);
    }

    let _lock = refresh_lock(home, agent_id, server).await?;
    // Re-read under the lock.
    let (_, mut secrets) = load_usable(home, agent_id, server)?;
    let current = to_upstream(&secrets)?;
    let now = chrono::Utc::now().timestamp();
    {
        let o = secrets.oauth.as_ref().ok_or_else(|| not_connected(agent_id, server, "has no OAuth tokens"))?;
        let still_expiring = o.expires_at.is_some_and(|e| e - REFRESH_SKEW_SECS <= now);
        let changed_since_seen = seen.is_some() && current.authorization.as_deref() != seen;
        if (force_refresh && changed_since_seen) || (!force_refresh && !still_expiring) {
            return Ok(current);
        }
    }
    let o = secrets.oauth.as_mut().expect("checked above");
    let Some(rt) = o.refresh_token.clone() else {
        // No refresh token: an expired token cannot be renewed.
        let _ = store::update(home, agent_id, server, |r| {
            r.status = ConnStatus::NeedsReauth;
            Ok(true)
        });
        return Err(not_connected(agent_id, server, "has an expired sign-in and no refresh token"));
    };
    let url = current.url.clone();
    let policy = OutboundPolicy::for_mcp_url(&url);
    let token_endpoint = Url::parse(&o.token_endpoint)
        .map_err(|e| AuthError::NotConnected(format!("stored token endpoint is invalid: {e}")))?;
    let creds = ClientCredentials {
        client_id: o.client_id.clone(),
        client_secret: o.client_secret.clone(),
        token_endpoint_auth: o.token_endpoint_auth.clone(),
    };
    match oauth::refresh(&token_endpoint, policy, &creds, &rt, &o.resource, o.scope.as_deref()).await {
        Ok(fresh) => {
            oauth::merge_refreshed(o, fresh);
            let expires = o.expires_at;
            let has_rt = o.refresh_token.is_some();
            let sealed = store::seal(home, &secrets).map_err(AuthError::Transient)?;
            store::update(home, agent_id, server, move |r| {
                r.secret_enc = sealed;
                r.access_expires_at = expires;
                r.has_refresh_token = has_rt;
                r.status = ConnStatus::Connected;
                Ok(true)
            })
            .map_err(AuthError::Transient)?;
            to_upstream(&secrets)
        }
        Err(TokenError::InvalidGrant(msg)) => {
            let _ = store::update(home, agent_id, server, |r| {
                r.status = ConnStatus::NeedsReauth;
                Ok(true)
            });
            Err(not_connected(agent_id, server, &format!("could not renew its sign-in ({msg})")))
        }
        Err(TokenError::Other(msg)) => Err(AuthError::Transient(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_origin_validation() {
        let allowed = vec!["dash.example.com".to_string(), "other.example.com:8443".to_string()];
        assert_eq!(validate_redirect_origin("http://localhost:18789", &allowed).unwrap(), "http://localhost:18789");
        assert_eq!(validate_redirect_origin("http://127.0.0.1:5173/", &allowed).unwrap(), "http://127.0.0.1:5173");
        assert!(validate_redirect_origin("http://[::1]:18789", &allowed).is_ok());
        assert_eq!(validate_redirect_origin("https://dash.example.com", &allowed).unwrap(), "https://dash.example.com");
        assert!(validate_redirect_origin("https://other.example.com:8443", &allowed).is_ok());
        // Port-qualified allow entry must match the port exactly.
        assert!(validate_redirect_origin("https://other.example.com:9443", &allowed).is_err());
        // Suffix / prefix tricks.
        assert!(validate_redirect_origin("https://dash.example.com.evil.com", &allowed).is_err());
        assert!(validate_redirect_origin("http://localhost.evil.com", &allowed).is_err());
        assert!(validate_redirect_origin("https://evil.com", &allowed).is_err());
        // Plain http to an allowed non-loopback origin: refused.
        assert!(validate_redirect_origin("http://dash.example.com", &allowed).is_err());
        // Not an origin.
        assert!(validate_redirect_origin("https://dash.example.com/app", &allowed).is_err());
        assert!(validate_redirect_origin("https://dash.example.com/?x=1", &allowed).is_err());
        assert!(validate_redirect_origin("https://u:p@dash.example.com", &allowed).is_err());
        assert!(validate_redirect_origin("javascript:alert(1)", &allowed).is_err());
        assert!(validate_redirect_origin("", &allowed).is_err());
    }

    #[tokio::test]
    async fn callback_state_is_single_use_and_unknown_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(complete_callback(dir.path(), "nope", "code").await.is_err());
        assert!(complete_callback(dir.path(), "", "code").await.is_err());
        // Insert a pending entry by hand, then take it twice.
        {
            let mut map = pending().lock().unwrap();
            map.insert(
                "st-test-1".into(),
                PendingConnect {
                    agent_id: "a1".into(),
                    server: "s".into(),
                    mcp_url: "https://mcp.example.com/mcp".into(),
                    redirect_uri: "http://localhost:1/oauth/mcp/callback".into(),
                    redirect_origin: "http://localhost:1".into(),
                    code_verifier: "v".into(),
                    token_endpoint: Url::parse("https://auth.example.com/token").unwrap(),
                    issuer: "https://auth.example.com".into(),
                    creds: ClientCredentials { client_id: "c".into(), client_secret: None, token_endpoint_auth: "none".into() },
                    resource: "https://mcp.example.com/mcp".into(),
                    scope: None,
                    created: Instant::now() - PENDING_TTL - Duration::from_secs(1),
                },
            );
        }
        // Expired ⇒ refused, and removed.
        assert!(cancel_pending("st-test-1").is_none());
        assert!(complete_callback(dir.path(), "st-test-1", "code").await.is_err());
    }

    #[test]
    fn disconnect_keeps_the_url_and_forget_removes_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let url = Url::parse("https://mcp.example.com/mcp").unwrap();
        save_connected(home, "a1", "s1", &url, AuthKind::Bearer, RemoteSecrets {
            url: url.to_string(),
            bearer: Some("tok".into()),
            oauth: None,
        })
        .unwrap();
        assert!(disconnect(home, "a1", "s1", false).unwrap());
        let rec = store::get(home, "a1", "s1").unwrap().unwrap();
        assert_eq!(rec.status, ConnStatus::NotConnected);
        let s = store::open(home, &rec).unwrap();
        assert!(s.bearer.is_none());
        assert_eq!(s.url, "https://mcp.example.com/mcp");
        assert!(disconnect(home, "a1", "s1", true).unwrap());
        assert!(store::get(home, "a1", "s1").unwrap().is_none());
    }

    #[tokio::test]
    async fn bridge_auth_refuses_records_that_are_not_connected() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let e = upstream_auth(home, "a1", "missing", false, None).await.err().unwrap();
        assert!(matches!(e, AuthError::NotConnected(_)));
        super::super::bridge_def::prepare_for_install(home, "a1", "s1", &super::super::bridge_def::candidate_def("https://mcp.example.com/mcp")).unwrap();
        let e = upstream_auth(home, "a1", "s1", false, None).await.err().unwrap();
        assert!(e.to_string().contains("is not connected"), "{e}");
    }
}
