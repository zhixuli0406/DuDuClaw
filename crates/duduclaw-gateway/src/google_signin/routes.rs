//! Public Google login: browser-bound, single-use state and completion tickets.
use super::{config, oidc};
use crate::google_personal::oauth;
use crate::server::{AppState, login_throttle_ip, server_mfa};
use axum::{
    Json,
    extract::{ConnectInfo, RawQuery, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use duduclaw_auth::{UserDb, UserStatus, mfa::FirstFactor,
    signin_members::{SigninApproval, SigninOutcome, SigninPolicy, VerifiedGoogleIdentity}};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, net::{IpAddr, SocketAddr}, path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex}, time::{Duration, Instant}};
use zeroize::Zeroize;

const CALLBACK_PATH: &str = "/api/login/google/callback";
const LOGIN_PATH: &str = "/login";
const STATE_TTL: Duration = Duration::from_secs(600);
const TICKET_TTL: Duration = Duration::from_secs(120);
const MAX_PENDING: usize = 128;
const MAX_PENDING_PER_IP: usize = 8;
const MAX_TICKETS: usize = 256;
const GOOGLE_RATE_WINDOW: Duration = Duration::from_secs(900);
const GOOGLE_RATE_LIMIT: u32 = 30;
const MAX_GOOGLE_RATE_ENTRIES: usize = 10000;
type Hash = [u8; 32];

static GOOGLE_RATE_LIMITER: LazyLock<Mutex<HashMap<(IpAddr, &'static str), (Instant, u32)>>> =
    LazyLock::new(Default::default);

fn check_google_rate_limit(ip: IpAddr, route: &'static str) -> bool {
    let ip = login_throttle_ip(ip);
    let Ok(mut map) = GOOGLE_RATE_LIMITER.lock() else { return false; };
    let now = Instant::now();
    if map.len() >= MAX_GOOGLE_RATE_ENTRIES {
        map.retain(|_, (at, _)| now.duration_since(*at) < GOOGLE_RATE_WINDOW);
        if map.len() >= MAX_GOOGLE_RATE_ENTRIES && !map.contains_key(&(ip, route)) {
            return false;
        }
    }
    let entry = map.entry((ip, route)).or_insert((now, 0));
    if now.duration_since(entry.0) >= GOOGLE_RATE_WINDOW { *entry = (now, 0); }
    if entry.1 >= GOOGLE_RATE_LIMIT { return false; }
    entry.1 += 1;
    true
}

fn hash(value: &str) -> Hash { Sha256::digest(value.as_bytes()).into() }
fn same_hash(a: &Hash, b: &Hash) -> bool {
    ring::constant_time::verify_slices_are_equal(a, b).is_ok()
}

#[path = "routes_me.rs"]
pub(crate) mod me;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionBinding {
    pub user_id: String,
    pub session_id: String,
    pub epoch: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Purpose { Login, Link(SessionBinding), Reauth(SessionBinding) }
impl Purpose {
    fn binding(&self) -> Option<&SessionBinding> {
        match self { Self::Login => None, Self::Link(b) | Self::Reauth(b) => Some(b) }
    }
    fn path(&self) -> &'static str {
        match self { Self::Login => LOGIN_PATH, Self::Link(_) => "/me/connections", Self::Reauth(_) => "/me/account" }
    }
}

struct Pending {
    credential_revision: Option<i64>,
    purpose: Purpose,
    home: PathBuf,
    ip: IpAddr,
    cookie_sha256: Hash,
    nonce_sha256: Hash,
    verifier: String,
    redirect_uri: String,
    client_id: String,
    settings_revision: String,
    secure: bool,
    created: Instant,
}
impl Drop for Pending {
    fn drop(&mut self) { self.verifier.zeroize(); }
}

enum LoginResult {
    Identity(oidc::VerifiedIdToken),
    Code(&'static str),
    Ok { user_id: String, credential_epoch: u64 },
}
struct Ticket {
    credential_revision: Option<i64>,
    purpose: Purpose,
    home: PathBuf,
    cookie_sha256: Hash,
    secure: bool,
    result: LoginResult,
    created: Instant,
}
static PENDING: LazyLock<Mutex<HashMap<Hash, Pending>>> = LazyLock::new(Default::default);
static TICKETS: LazyLock<Mutex<HashMap<Hash, Ticket>>> = LazyLock::new(Default::default);
// Request IDs (72-hour lifetime) and UTC calendar dates, scoped to a home.
static NOTIFIED: LazyLock<Mutex<HashMap<(PathBuf, String), Instant>>> = LazyLock::new(Default::default);

struct Available {
    settings: config::GoogleSigninSettings,
    client: crate::mcp_oauth::McpOAuthClientConfig,
    base: String,
    secure: bool,
}

pub(crate) fn public_base(home: &Path) -> Option<(String, bool)> {
    let root = std::fs::read_to_string(home.join("config.toml")).ok()?.parse::<toml::Table>().ok()?;
    let raw = root.get("dashboard")?.get("public_url")?.as_str()?;
    let (_, base) = crate::deep_link::valid_oauth_public_base(raw)?;
    let secure = url::Url::parse(&base).ok()?.scheme() == "https";
    Some((base, secure))
}

/// Shared by the public flow and the admin settings checks. Never serialize
/// the returned client configuration into dashboard responses or audit rows.
pub(crate) fn configured_client(home: &Path) -> Option<crate::mcp_oauth::McpOAuthClientConfig> {
    let client = crate::mcp_oauth::get_client_config(home, "google")?;
    if client.client_id.trim().is_empty() || client.client_secret.trim().is_empty() { return None; }
    Some(client)
}

pub(crate) fn redirect_uri(base: &str) -> String { format!("{base}{CALLBACK_PATH}") }

async fn availability(state: &AppState) -> Result<Available, &'static str> {
    if state.handler.resolve_edition_profile().await.is_personal() { return Err("off"); }
    let settings = config::read_strict(&state.home_dir);
    if !settings.enabled { return Err("off"); }
    let client = configured_client(&state.home_dir).ok_or("off")?;
    let (base, secure) = public_base(&state.home_dir).ok_or("needs_public_url")?;
    Ok(Available { settings, client, base, secure })
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
fn error(status: StatusCode) -> Response {
    no_store((status, Json(json!({"error":"登入沒有完成，請稍後再試。"}))).into_response())
}
fn result(code: &'static str) -> Response { no_store(Json(json!({"result":code})).into_response()) }
fn cookie_name(secure: bool) -> &'static str { if secure { "__Host-dd_gsi" } else { "dd_gsi" } }
fn cookie(value: &str, secure: bool, age: u32) -> String {
    format!("{}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={age}{}",
        cookie_name(secure), if secure { "; Secure" } else { "" })
}
fn cookie_hash(headers: &HeaderMap, secure: bool) -> Option<Hash> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        for pair in header.to_str().ok()?.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else { continue; };
            if name == cookie_name(secure) {
                // Reject ambiguous duplicate cookies and malformed random values.
                if found.is_some() || !random_shape(value) { return None; }
                found = Some(hash(value));
            }
        }
    }
    found
}
fn random_shape(value: &str) -> bool {
    value.len() == 43 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn clear_cookie(response: Response) -> Response {
    let mut response = no_store(response);
    // Clear both names even if the configured scheme changed or parsing failed.
    for secure in [true, false] {
        response.headers_mut().append(header::SET_COOKIE,
            HeaderValue::from_str(&cookie("", secure, 0)).expect("static cookie header"));
    }
    response
}
fn redirect(base: &str, fragment: &str) -> Response {
    redirect_to(base, LOGIN_PATH, fragment)
}
fn redirect_to(base: &str, path: &str, fragment: &str) -> Response {
    let mut response = no_store(Redirect::to(&format!("{base}{path}#{fragment}")).into_response());
    response.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}
fn expired(home: &Path) -> Response {
    // With no valid configured base, only the fixed local login path is safe.
    let base = public_base(home).map(|(base, _)| base).unwrap_or_default();
    redirect(&base, "gsi_error=expired")
}

pub(crate) async fn methods(State(state): State<Arc<AppState>>) -> Response {
    let mode = match availability(&state).await { Ok(_) => "available", Err(mode) => mode };
    no_store(Json(json!({"google":mode})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartBody {}

pub(crate) async fn start(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Result<Json<StartBody>, JsonRejection>,
) -> Response {
    if !check_google_rate_limit(addr.ip(), "start") { return error(StatusCode::TOO_MANY_REQUESTS); }
    if body.is_err() { return error(StatusCode::BAD_REQUEST); }
    let Ok(available) = availability(&state).await else { return error(StatusCode::NOT_FOUND); };
    let (Ok(state_value), Ok(nonce), Ok(verifier), Ok(cookie_value)) =
        (oidc::random_state(), oidc::random_nonce(), oidc::random_pkce_verifier(), oidc::random_state())
        else { return error(StatusCode::SERVICE_UNAVAILABLE); };
    let redirect_uri = redirect_uri(&available.base);
    let authorization_url = oidc::authorization_url(&available.client.client_id, &redirect_uri,
        &state_value, &nonce, &oidc::pkce_challenge_s256(&verifier), &available.settings.allowed_hd,
        oidc::SigninPurpose::Login, None);
    let ip = login_throttle_ip(addr.ip());
    let pending = Pending { credential_revision: None, purpose: Purpose::Login, home: state.home_dir.clone(), ip, cookie_sha256: hash(&cookie_value),
        nonce_sha256: hash(&nonce), verifier, redirect_uri, client_id: available.client.client_id,
        settings_revision: available.settings.revision, secure: available.secure, created: Instant::now() };
    {
        let Ok(mut map) = PENDING.lock() else { return error(StatusCode::SERVICE_UNAVAILABLE); };
        map.retain(|_, value| value.created.elapsed() < STATE_TTL);
        if map.len() >= MAX_PENDING || map.contains_key(&hash(&state_value)) {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        }
        if map.values().filter(|pending| pending.ip == ip).count() >= MAX_PENDING_PER_IP {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        }
        map.insert(hash(&state_value), pending);
    }
    let mut response = no_store(Json(json!({"authorization_url":authorization_url})).into_response());
    response.headers_mut().insert(header::SET_COOKIE,
        HeaderValue::from_str(&cookie(&cookie_value, available.secure, 600)).expect("random cookie header"));
    response
}

fn take_pending(value: &str) -> Option<Pending> {
    if !random_shape(value) { return None; }
    PENDING.lock().ok()?.remove(&hash(value))
        .filter(|pending| pending.created.elapsed() < STATE_TTL)
}
fn put_ticket(pending: &Pending, result: LoginResult) -> Option<String> {
    let value = oidc::random_state().ok()?;
    let mut map = TICKETS.lock().ok()?;
    map.retain(|_, ticket| ticket.created.elapsed() < TICKET_TTL);
    if map.len() >= MAX_TICKETS || map.contains_key(&hash(&value)) { return None; }
    map.insert(hash(&value), Ticket { credential_revision: pending.credential_revision, purpose: pending.purpose.clone(), home: pending.home.clone(), cookie_sha256: pending.cookie_sha256,
        secure: pending.secure, result, created: Instant::now() });
    Some(value)
}
fn take_ticket(value: &str) -> Option<Ticket> {
    if !random_shape(value) { return None; }
    TICKETS.lock().ok()?.remove(&hash(value)).filter(|ticket| ticket.created.elapsed() < TICKET_TTL)
}

// Ignore unknown query keys, including redirect/next; reject duplicate known keys.
fn callback_values(raw: &str) -> Option<(String, Option<String>, bool)> {
    if raw.len() > 8192 { return None; }
    let mut state = None;
    let mut code = None;
    let mut error = false;
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        match key.as_ref() {
            "state" => { if state.is_some() { return None; } state = Some(value.into_owned()); }
            "code" => { if code.is_some() { return None; } code = Some(value.into_owned()); }
            "error" => error = true,
            _ => {},
        }
    }
    Some((state?, code, error))
}

pub(crate) async fn callback(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if !check_google_rate_limit(addr.ip(), "callback") { return expired(&state.home_dir); }
    let Some((state_value, code, provider_error)) = callback_values(query.as_deref().unwrap_or_default())
        else { return expired(&state.home_dir); };
    // Consume before checking the cookie, settings, or contacting any provider.
    let Some(pending) = take_pending(&state_value) else { return expired(&state.home_dir); };
    let failed = || {
        let base = public_base(&state.home_dir).map(|(base, _)| base).unwrap_or_default();
        redirect_to(&base, pending.purpose.path(), "gsi_error=expired")
    };
    if pending.home != state.home_dir || provider_error
        || (pending.purpose == Purpose::Login && !cookie_hash(&headers, pending.secure).is_some_and(|h| same_hash(&h, &pending.cookie_sha256)))
    { return failed(); }
    let Some(code) = code.filter(|c| !c.is_empty() && c.len() <= 4096 && !c.chars().any(char::is_control))
        else { return failed(); };
    let Ok(available) = availability(&state).await else { return failed(); };
    if available.settings.revision != pending.settings_revision
        || available.client.client_id != pending.client_id
        || redirect_uri(&available.base) != pending.redirect_uri
    { return failed(); }
    if let Some(binding) = pending.purpose.binding().cloned() {
        let db = state.user_db.clone();
        if !matches!(tokio::task::spawn_blocking(move || me::live_user(&db, &binding)).await, Ok(Ok(_))) {
            return failed();
        }
    }
    let Ok(id_token) = oauth::exchange_for_id_token(&pending.client_id, &available.client.client_secret,
        &code, &pending.verifier, &pending.redirect_uri).await else { return failed(); };
    let Ok(http) = reqwest::Client::builder().timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none()).build() else { return failed(); };
    let now = chrono::Utc::now().timestamp();
    let decision = match oidc::verify_google_id_token(&http, &id_token, &pending.client_id,
        &pending.nonce_sha256, &available.settings.allowed_hd, now).await {
        Err(oidc::IdTokenError::Invalid) => return failed(),
        Err(oidc::IdTokenError::PersonalAccount) => LoginResult::Code("personal_account"),
        Err(oidc::IdTokenError::DomainNotAllowed) => LoginResult::Code("domain_not_allowed"),
        Ok(identity) if pending.purpose != Purpose::Login => LoginResult::Identity(identity),
        Ok(identity) => {
            let identity = VerifiedGoogleIdentity { sub: identity.sub, email: identity.email,
                hd: identity.hd, display_name: identity.name };
            let Some((ack_by, ack_at)) = available.settings.ack else { return failed(); };
            let policy = SigninPolicy {
                approval: match available.settings.approval {
                    config::Approval::Admin => SigninApproval::Admin,
                    config::Approval::Direct => SigninApproval::Direct,
                },
                daily_cap: available.settings.daily_new_accounts,
                settings_version: available.settings.revision, ack_by, ack_at,
            };
            let decision = decide_login(state.user_db.clone(), state.home_dir.clone(), identity, policy, now).await;
            match decision {
                Ok((result, notification)) => {
                    if let Some((key, text)) = notification { notify_admins(&state.home_dir, key, text).await; }
                    result
                }
                _ => LoginResult::Code("failed"),
            }
        }
    };
    match put_ticket(&pending, decision) {
        Some(ticket) => {
            let key = match pending.purpose { Purpose::Login => "gsi", Purpose::Link(_) => "gsi_link", Purpose::Reauth(_) => "gsi_reauth" };
            redirect_to(&available.base, pending.purpose.path(), &format!("{key}={ticket}"))
        },
        None => failed(),
    }
}

type Notification = Option<(String, &'static str)>;
async fn decide_login(db: Arc<UserDb>, home: PathBuf, identity: VerifiedGoogleIdentity,
    policy: SigninPolicy, now: i64) -> Result<(LoginResult, Notification), String>
{
    let probe_db = db.clone();
    let probe_id = identity.clone();
    let candidate = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        if probe_db.google_signin_link_by_sub(&probe_id.sub)?.is_some() { return Ok(false); }
        probe_db.signin_attestation_candidate_exists(&probe_id.email, now)
    }).await.map_err(|_| "signin_storage_unavailable")??;
    let lease = if candidate {
        Some(crate::google_personal::grants::acquire(&home).await
            .map_err(|_| "signin_storage_unavailable")?)
    } else { None };
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
        decide(&db, &identity, &policy, now, _lease.is_some(),
            &|user| me::personal_sub(&home, user).map_err(str::to_owned))
    }).await.map_err(|_| "signin_storage_unavailable")?
}

fn decide(db: &UserDb, identity: &VerifiedGoogleIdentity, policy: &SigninPolicy, now: i64,
    grant_lease_held: bool,
    personal_sub: &dyn Fn(&str) -> Result<Option<String>, String>)
    -> Result<(LoginResult, Notification), String>
{
    let previous_request = db.signin_request_by_sub(&identity.sub)?;
    let outcome = db.resolve_google_signin(grant_lease_held, identity, policy, now, personal_sub)?;
    let mut notification = None;
    let result = match outcome {
        SigninOutcome::Ok { user_id, credential_epoch, .. } => {
            match db.get_user(&user_id)? {
                Some(user) if user.status == UserStatus::Active && user.credential_epoch == credential_epoch =>
                    LoginResult::Ok { user_id, credential_epoch },
                _ => LoginResult::Code("expired"),
            }
        }
        SigninOutcome::AccountDisabled => LoginResult::Code("account_disabled"),
        SigninOutcome::ExistingAccount => LoginResult::Code("existing_account"),
        SigninOutcome::Rejected => LoginResult::Code("rejected"),
        SigninOutcome::DailyLimit => {
            notification = Some((format!("daily:{}", chrono::Utc::now().date_naive()),
                "今天的新帳號開通數已達上限，請到成員頁查看 Google 登入申請。"));
            LoginResult::Code("daily_limit")
        }
        SigninOutcome::PendingFull => LoginResult::Code("daily_limit"),
        SigninOutcome::PendingApproval { request_id } => {
            // Repeated pending submissions keep the same ID, including across
            // gateway restarts. The in-memory guard also coalesces concurrent starts.
            if previous_request.as_ref().map(|request| request.id.as_str()) != Some(request_id.as_str()) {
                notification = Some((format!("request:{request_id}"),
                    "有新的公司 Google 帳號開通申請，請到成員頁查看並核准。"));
            }
            LoginResult::Code("pending_approval")
        }
    };
    Ok((result, notification))
}

async fn notify_admins(home: &Path, key: String, text: &'static str) {
    let should_send = {
        let Ok(mut map) = NOTIFIED.lock() else { return; };
        map.retain(|_, at| at.elapsed() < Duration::from_secs(72 * 3600));
        let key = (home.to_path_buf(), key);
        if map.contains_key(&key) || map.len() >= 4096 { false }
        else { map.insert(key, Instant::now()); true }
    };
    if !should_send { return; }
    if let Ok(targets) = crate::notify_push::admin_targets(home) {
        crate::notify_push::push_plain_notice(home, "google.signin", text, targets).await;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteBody { ticket: String }

pub(crate) async fn complete(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<CompleteBody>, JsonRejection>,
) -> Response {
    clear_cookie(complete_inner(&state, addr, &headers, body).await)
}
async fn complete_inner(state: &Arc<AppState>, addr: SocketAddr, headers: &HeaderMap,
    body: Result<Json<CompleteBody>, JsonRejection>) -> Response
{
    if !check_google_rate_limit(addr.ip(), "complete") { return error(StatusCode::TOO_MANY_REQUESTS); }
    let Ok(Json(body)) = body else { return error(StatusCode::BAD_REQUEST); };
    let Some(ticket) = take_ticket(&body.ticket) else { return result("expired"); };
    if ticket.purpose != Purpose::Login || ticket.home != state.home_dir
        || !cookie_hash(headers, ticket.secure).is_some_and(|h| same_hash(&h, &ticket.cookie_sha256))
    { return result("expired"); }
    let (user_id, epoch) = match ticket.result {
        LoginResult::Code(code) => return result(code),
        LoginResult::Identity(_) => return result("expired"),
        LoginResult::Ok { user_id, credential_epoch } => (user_id, credential_epoch),
    };
    if availability(state).await.is_err() { return result("expired"); }
    let db = state.user_db.clone();
    let user = match tokio::task::spawn_blocking(move || db.get_user(&user_id)).await {
        Ok(Ok(Some(user))) if user.status == UserStatus::Active && user.credential_epoch == epoch => user,
        _ => return result("expired"),
    };
    if let Some(response) = server_mfa::after_first_factor(state, &user, FirstFactor::GoogleOidc).await {
        return response;
    }
    let policy_exempt = state.handler.resolve_edition_profile().await.is_personal();
    let (access_token, refresh_token) = match state.user_db.mfa_issue_if_unenrolled(
        &user.id, user.credential_epoch, policy_exempt,
        |current, access| state.jwt_config.issue_session_tokens(current, access)) {
        Ok(tokens) => tokens,
        Err(_) => return server_mfa::require_after_first_factor(state, &user, FirstFactor::GoogleOidc).await,
    };
    let _ = state.user_db.update_last_login(&user.id);
    let _ = state.user_db.log_action(Some(&user.id), "login", None,
        Some(r#"{"method":"google"}"#), Some(&addr.ip().to_string()));
    Json(json!({"access_token":access_token,"refresh_token":refresh_token,"user":user})).into_response()
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
