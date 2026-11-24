//! RM-208: gateway-owned, memory-only run registry. No issuance HTTP API.
//! An AI cannot claim another identity by changing environment variables,
//! parameters, or its working directory. Processes sharing an OS user may
//! borrow another active run's credential by reading its environment; the
//! credential stops working when that run ends. Native Linux protected mode
//! isolates employees as separate users and uses its own capability identity;
//! it issues no run credential. Child environments only transport the proof.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{Json, Router, body::to_bytes, extract::{ConnectInfo, Request, State}, routing::post};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use duduclaw_core::run_credential::{ENV_IDENTITY_URL, ENV_RUN_CREDENTIAL, RunCredential};
use duduclaw_core::run_credential::discovery::DiscoveryDirectory;
#[cfg(test)]
use duduclaw_core::run_credential::IDENTITY_URL_FILE;
use duduclaw_knowledge::requester::RequesterIdentity;

pub const TOOL_REFS_ROUTE: &str = "/api/internal/identity/tool-reference";
pub const ROUTE: &str = "/api/internal/identity/introspect";
const MAX_RUNS: usize = 8192;
const MAX_TTL: u64 = 6 * 60 * 60;
const RATE_PER_MINUTE: usize = 600;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Principal {
    Human { user_id: String },
    Background { kind: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub agent_id: String,
    #[serde(skip_serializing)]
    pub employee_id: Option<String>,
    #[serde(skip_serializing)]
    pub incarnation: Option<u64>,
    pub principal: Principal,
    pub session_id: Option<String>,
    pub turn_id: String,
    pub task_id: Option<String>,
    pub runtime: String,
    pub issued_at: u64,
    #[serde(skip_serializing)]
    pub(crate) issued_at_exact: chrono::DateTime<chrono::Utc>,
    pub expires_at: u64,
}

struct Entry { record: RunRecord, deadline: Instant, rate: (Instant, usize) }

pub struct Registry {
    // The only credential material retained here is its SHA-256 digest.
    runs: Mutex<HashMap<[u8; 32], Entry>>,
    invalid_rate: Mutex<(Instant, usize)>,
    endpoint: Mutex<Option<String>>,
    #[cfg(test)]
    lookups: std::sync::atomic::AtomicUsize,
}

impl Default for Registry {
    fn default() -> Self {
        Self { runs: Mutex::new(HashMap::new()), invalid_rate: Mutex::new((Instant::now(), 0)),
            endpoint: Mutex::new(None),
            #[cfg(test)]
            lookups: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

fn registry() -> &'static Arc<Registry> {
    static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(Registry::default()))
}

fn unix_now() -> Result<u64, String> {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs())
        .map_err(|_| "Run identity clock unavailable".to_string())
}

fn digest(credential: &RunCredential) -> [u8; 32] {
    Sha256::digest(credential.expose().as_bytes()).into()
}

/// Held by the same future that owns the child: every return, error, panic
/// unwind or cancellation removes the digest. Detached tools lose validity.
pub struct RunGuard {
    registry: Arc<Registry>,
    hash: [u8; 32],
    credential: RunCredential,
    identity_url: Option<String>,
}

impl std::fmt::Debug for RunGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunGuard([REDACTED])")
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // A poisoned registry already denies every lookup and issuance.
        if let Ok(mut runs) = self.registry.runs.lock() { runs.remove(&self.hash); }
    }
}

impl RunGuard {
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        cmd.env(ENV_RUN_CREDENTIAL, self.credential.expose());
        cmd.env_remove(ENV_IDENTITY_URL);
        if let Some(url) = &self.identity_url { cmd.env(ENV_IDENTITY_URL, url); }
    }

    pub fn apply_pty(&self, env: &mut HashMap<String, String>) {
        env.insert(ENV_RUN_CREDENTIAL.to_string(), self.credential.expose().to_string());
        env.remove(ENV_IDENTITY_URL);
        if let Some(url) = &self.identity_url { env.insert(ENV_IDENTITY_URL.into(), url.clone()); }
    }
}

impl duduclaw_core::run_credential::RunLease for RunGuard {
    fn credential(&self) -> &RunCredential { &self.credential }
    fn identity_url(&self) -> Option<&str> { self.identity_url.as_deref() }
}

pub fn install_background_issuer() -> Result<(), String> {
    duduclaw_core::run_credential::install_background_issuer(Box::new(|home, agent, runtime, kind, timeout| {
        if !matches!(kind, "heartbeat") { return Err("Unknown background identity kind".into()); }
        // A lower-layer scheduler has no channel task-locals. Establish an
        // explicit gateway registration and scope it for synchronous issuance.
        let run = uuid::Uuid::new_v4().to_string();
        let _requester = crate::knowledge::turn_requesters::register_run(agent, &run, kind)
            .ok_or("Cannot register background identity")?;
        let guard = crate::memory_provenance::DISPATCH_RUN.sync_scope(
            Some((format!("{kind}:{agent}"), run)), || issue(home, agent, runtime, timeout))?;
        Ok(Box::new(guard))
    }))
}

impl Registry {
    fn issue(self: &Arc<Self>, mut record: RunRecord, timeout: u64) -> Result<RunGuard, String> {
        let now = unix_now()?;
        let ttl = timeout.saturating_add(60).min(MAX_TTL);
        record.issued_at = now;
        record.issued_at_exact = chrono::Utc::now();
        record.expires_at = now.saturating_add(ttl);
        let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.try_fill_bytes(&mut *bytes)
            .map_err(|_| "Run identity randomness unavailable".to_string())?;
        let credential = RunCredential::from_bytes(&bytes);
        let hash = digest(&credential);
        let mut runs = self.runs.lock().map_err(|_| "Run identity registry unavailable".to_string())?;
        runs.retain(|_, entry| entry.record.expires_at > now && entry.deadline > Instant::now());
        if runs.len() >= MAX_RUNS || runs.contains_key(&hash) {
            return Err("Run identity registry full".into());
        }
        let identity_url = self.endpoint.lock().map_err(|_| "Run identity listener unavailable")?.clone();
        runs.insert(hash, Entry { record, deadline: Instant::now() + Duration::from_secs(ttl), rate: (Instant::now(), 0) });
        Ok(RunGuard { registry: self.clone(), hash, credential, identity_url })
    }

    #[cfg(test)]
    fn lookup(&self, credential: &RunCredential) -> Option<RunRecord> {
        self.lookup_hash(digest(credential), true, false).ok().flatten()
    }

    fn lookup_hash(&self, hash: [u8; 32], well_formed: bool, rate_limit: bool) -> Result<Option<RunRecord>, ()> {
        #[cfg(test)]
        self.lookups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let now = unix_now().map_err(|_| ())?;
        let mut runs = self.runs.lock().map_err(|_| ())?;
        // Malformed and unknown requests both reach this fixed-size digest
        // lookup. No plaintext prefix participates in credential comparison.
        let Some((key, entry)) = runs.get_key_value(&hash) else { return Ok(None); };
        ring::constant_time::verify_slices_are_equal(key, &hash).map_err(|_| ())?;
        if entry.record.expires_at <= now || entry.deadline <= Instant::now() {
            runs.remove(&hash);
            return Ok(None);
        }
        if !well_formed { return Ok(None); }
        let entry = runs.get_mut(&hash).ok_or(())?;
        if rate_limit && !allow_bucket(&mut entry.rate) { return Err(()); }
        Ok(Some(entry.record.clone()))
    }

    fn query(&self, credential: Option<&RunCredential>) -> Option<RunRecord> {
        // Always hash and look up one credential, even if parsing failed.
        let dummy = RunCredential::from_bytes(&[0; 32]);
        let hash = digest(credential.unwrap_or(&dummy));
        match self.lookup_hash(hash, credential.is_some(), true) {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                let Ok(mut rate) = self.invalid_rate.lock() else { return None; };
                if !allow_bucket(&mut rate) { return None; }
                None
            }
            // An exhausted valid bucket never consumes the junk bucket.
            Err(()) => None,
        }
    }
}

fn allow_bucket(rate: &mut (Instant, usize)) -> bool {
    if rate.0.elapsed() >= Duration::from_secs(60) { *rate = (Instant::now(), 0); }
    if rate.1 >= RATE_PER_MINUTE { return false; }
    rate.1 += 1;
    true
}

fn principal(who: RequesterIdentity, fallback: &str) -> Principal {
    match who {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser { user_id: Some(user_id), .. }
            if !user_id.is_empty() => Principal::Human { user_id },
        // Scheduled work remains background, even if an older delegation
        // registration records an on_behalf_of hint. It grants no admin role.
        RequesterIdentity::System { kind, .. } if !kind.is_empty() => Principal::Background { kind },
        _ => Principal::Background { kind: fallback.to_string() },
    }
}

/// Capture only gateway task-locals and its live registration. Neither the
/// process environment, model text, tool arguments nor cwd is consulted.
pub fn issue(home: &Path, agent: &str, runtime: &str, timeout: u64) -> Result<RunGuard, String> {
    if !duduclaw_core::is_valid_agent_id(agent) || runtime.is_empty() {
        return Err("Run identity has no valid agent or runtime".into());
    }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten();
    let dispatch = crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten();
    let who = dispatch.as_ref().and_then(|(_, run)| crate::knowledge::turn_requesters::lookup(agent, run))
        .or_else(|| turn.as_deref().and_then(|turn| crate::knowledge::turn_requesters::lookup(agent, turn)))
        .unwrap_or_else(crate::knowledge::turn_requesters::current_requester);
    let (employee_id, incarnation) = match duduclaw_core::identity_token::token_binding(home, agent, false)
        .map_err(|_| "Run identity employee registry unavailable".to_string())? {
        duduclaw_core::identity_token::TokenBinding::V1 => (None, None),
        duduclaw_core::identity_token::TokenBinding::V2(binding) =>
            (Some(binding.employee.iter().map(|b| format!("{b:02x}")).collect()), Some(binding.incarnation)),
        duduclaw_core::identity_token::TokenBinding::Refused => return Err("Run identity employee inactive".into()),
    };
    let record = RunRecord {
        agent_id: agent.to_string(), employee_id, incarnation,
        principal: principal(who, crate::runtime_dispatch::identity_background_kind()),
        session_id: duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone).ok().flatten()
            .or_else(|| dispatch.as_ref().map(|(session, _)| session.clone())),
        turn_id: turn.or_else(|| dispatch.map(|(_, run)| run)).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        task_id: crate::runtime::round_task_env().map(|(_, task)| task),
        runtime: runtime.to_string(), issued_at: 0, issued_at_exact: chrono::Utc::now(), expires_at: 0,
    };
    registry().issue(record, timeout)
}

/// Owned by gateway startup, after acquiring this home's gateway.lock.
/// Dropping it stops the listener and removes the discovery file.
pub struct IdentityListener {
    task: tokio::task::JoinHandle<()>,
    discovery: DiscoveryDirectory,
    registry: Arc<Registry>,
    #[cfg(test)]
    address: std::net::SocketAddr,
}

impl Drop for IdentityListener {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.discovery.remove();
        if let Ok(mut endpoint) = self.registry.endpoint.lock() { *endpoint = None; }
    }
}

pub async fn start_listener(home: &Path) -> Result<IdentityListener, String> {
    start_listener_with(home, registry().clone()).await
}

async fn start_listener_with(home: &Path, registry: Arc<Registry>) -> Result<IdentityListener, String> {
    if !duduclaw_core::gateway_instance::held(home) {
        return Err("Run identity listener requires the gateway lock".into());
    }
    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db"))
        .map_err(|_| "Cannot open conversation reference store")?);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
        .map_err(|_| "Cannot bind run identity listener")?;
    let address = listener.local_addr().map_err(|_| "Cannot inspect run identity listener")?;
    let url = format!("http://{address}{ROUTE}");
    let mut endpoint = registry.endpoint.lock().map_err(|_| "Run identity listener unavailable")?;
    if endpoint.is_some() { return Err("Run identity listener already started".into()); }
    let discovery = DiscoveryDirectory::open(home, true).map_err(|_| "Unsafe identity run directory")?;
    discovery.write(&url).map_err(|_| "Cannot publish identity discovery file")?;
    *endpoint = Some(url);
    drop(endpoint);
    let app = router_with(registry.clone()).merge(tool_refs_router(registry.clone(), sessions));
    let task = tokio::spawn(async move {
        if axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.is_err() {
            tracing::error!("Run identity listener stopped");
        }
    });
    Ok(IdentityListener { task, discovery, registry, #[cfg(test)] address })
}

fn router_with(registry: Arc<Registry>) -> Router {
    Router::new().route(ROUTE, post(handle)).with_state(registry)
}

fn tool_refs_router(registry: Arc<Registry>, sessions: Arc<crate::session::SessionManager>) -> Router {
    Router::new().route(TOOL_REFS_ROUTE, post(handle_tool_ref))
        .layer(axum::Extension(sessions)).with_state(registry)
}

fn local_request(request: &Request) -> bool {
    request.extensions().get::<ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|peer| crate::computer_use_sessions::auth::check_peer(peer.0).is_ok())
        && !request.headers().keys().any(|name| {
            let name = name.as_str().to_ascii_lowercase();
            name.starts_with("x-forwarded-") || matches!(name.as_str(), "forwarded" | "x-real-ip" | "via" | "origin")
        })
}

async fn handle_tool_ref(
    State(registry): State<Arc<Registry>>,
    axum::Extension(sessions): axum::Extension<Arc<crate::session::SessionManager>>,
    request: Request,
) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    if !local_request(&request) { return StatusCode::FORBIDDEN; }
    let mut value = match tokio::time::timeout(Duration::from_secs(2), to_bytes(request.into_body(), 4096)).await {
        Ok(Ok(body)) => serde_json::from_slice::<serde_json::Value>(&body).ok(),
        _ => None,
    };
    let credential = value.as_mut().and_then(|v| v.get_mut("credential"))
        .map(serde_json::Value::take).and_then(|v| match v {
            serde_json::Value::String(raw) => RunCredential::parse(raw), _ => None,
        });
    let Some(run) = registry.query(credential.as_ref()) else { return StatusCode::FORBIDDEN; };
    let reference = value.as_mut().and_then(|v| v.get_mut("reference"))
        .map(serde_json::Value::take).and_then(|v| serde_json::from_value(v).ok());
    let Some(reference) = reference else { return StatusCode::BAD_REQUEST; };
    // Session/agent/issue-time claims in the request are never consulted.
    match sessions.record_run_tool_reference(&run, reference).await {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::FORBIDDEN,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn handle(State(registry): State<Arc<Registry>>, request: Request) -> Json<serde_json::Value> {
    let inactive = || Json(serde_json::json!({"active": false}));
    if !local_request(&request) { return inactive(); }
    // No body/error logging. Every body/parse failure still hashes a dummy
    // proof and reaches the same registry lookup as an unknown credential.
    let credential = match tokio::time::timeout(Duration::from_secs(2), to_bytes(request.into_body(), 1024)).await {
        Ok(Ok(body)) => serde_json::from_slice::<serde_json::Value>(&body).ok()
            .and_then(|mut value| value.get_mut("credential").map(serde_json::Value::take))
            .and_then(|raw| match raw { serde_json::Value::String(raw) => RunCredential::parse(raw), _ => None }),
        _ => None,
    };
    let Some(record) = registry.query(credential.as_ref()) else { return inactive(); };
    let Ok(mut response) = serde_json::to_value(record) else { return inactive(); };
    response["active"] = serde_json::json!(true);
    Json(response)
}

#[cfg(test)]
pub(crate) fn assert_command_identity(command: &tokio::process::Command, active: bool) -> RunCredential {
    let value = command.as_std().get_envs()
        .find(|(name, _)| *name == ENV_RUN_CREDENTIAL)
        .and_then(|(_, value)| value).and_then(|value| value.to_str()).expect("run credential env missing");
    let credential = RunCredential::parse(value.to_string()).expect("malformed run credential");
    assert_eq!(registry().lookup(&credential).is_some(), active);
    assert!(!command.as_std().get_args().any(|arg| arg.to_string_lossy().contains(value)));
    credential
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn record() -> RunRecord {
        RunRecord { agent_id: "alice".into(), employee_id: None, incarnation: None,
            principal: Principal::Human { user_id: "user-1".into() }, session_id: Some("session-1".into()),
            turn_id: "turn-1".into(), task_id: None, runtime: "claude".into(), issued_at: 0, issued_at_exact: chrono::Utc::now(), expires_at: 0 }
    }

    async fn query(registry: &Arc<Registry>, peer: Option<&str>, body: String) -> (u16, Vec<u8>) {
        let mut request = axum::http::Request::post(ROUTE).body(Body::from(body)).unwrap();
        if let Some(peer) = peer {
            request.extensions_mut().insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        }
        let response = router_with(registry.clone()).oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        (status, to_bytes(response.into_body(), 16384).await.unwrap().to_vec())
    }

    fn body(credential: &RunCredential) -> String {
        serde_json::json!({"credential": credential.expose()}).to_string()
    }

    async fn write_ref(registry: &Arc<Registry>, sessions: &Arc<crate::session::SessionManager>, credential: Option<&RunCredential>) -> axum::http::StatusCode {
        let payload = serde_json::json!({
            "credential": credential.map(RunCredential::expose),
            "DUDUCLAW_SESSION_ID": "other-session", "session_id": "other-session",
            "agent_id": "other-agent", "issued_at": u64::MAX,
            "reference": { "tool_name": "docs_create", "ref_kind": "google_doc", "ref_id": "doc-1", "label": "Document" }
        });
        let mut request = axum::http::Request::post(TOOL_REFS_ROUTE).body(Body::from(payload.to_string())).unwrap();
        request.extensions_mut().insert(ConnectInfo("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap()));
        tool_refs_router(registry.clone(), sessions.clone()).oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn tool_ref_route_uses_only_live_run_session_and_rejects_pre_reset_runs() {
        use axum::http::StatusCode;
        let file = tempfile::NamedTempFile::new().unwrap();
        let sessions = Arc::new(crate::session::SessionManager::new(file.path()).unwrap());
        sessions.get_or_create("session-1", "alice").await.unwrap();
        sessions.get_or_create("other-session", "alice").await.unwrap();
        let registry = Arc::new(Registry::default());
        let lease = registry.issue(record(), 60).unwrap();
        assert_eq!(write_ref(&registry, &sessions, None).await, StatusCode::FORBIDDEN);
        let unknown = RunCredential::from_bytes(&[42; 32]);
        assert_eq!(write_ref(&registry, &sessions, Some(&unknown)).await, StatusCode::FORBIDDEN);
        assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::NO_CONTENT);
        assert!(!sessions.tool_refs_context("session-1").await.unwrap().is_empty());
        assert!(sessions.tool_refs_context("other-session").await.unwrap().is_empty());
        sessions.clear_for_user("session-1").await.unwrap();
        sessions.get_or_create("session-1", "alice").await.unwrap();
        assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::FORBIDDEN);
        assert!(sessions.tool_refs_context("session-1").await.unwrap().is_empty());
        // Deterministic sub-second reset: comparing integer seconds would allow it.
        let issued = registry.lookup(&lease.credential).unwrap().issued_at_exact;
        let generation = issued + chrono::Duration::nanoseconds(1);
        sessions.acquire().await.execute("UPDATE sessions SET created_at = ?1 WHERE id = 'session-1'",
            [generation.to_rfc3339()]).unwrap();
        assert_eq!(write_ref(&registry, &sessions, Some(&lease.credential)).await, StatusCode::FORBIDDEN);
        let fresh = registry.issue(record(), 60).unwrap();
        assert_eq!(write_ref(&registry, &sessions, Some(&fresh.credential)).await, StatusCode::NO_CONTENT);
        let proof = RunCredential::parse(fresh.credential.expose().into()).unwrap();
        drop(fresh);
        assert_eq!(write_ref(&registry, &sessions, Some(&proof)).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn tool_ref_route_rejects_nonlocal_and_forwarded_requests() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let sessions = Arc::new(crate::session::SessionManager::new(file.path()).unwrap());
        let registry = Arc::new(Registry::default());
        let lease = registry.issue(record(), 60).unwrap();
        for (peer, header) in [("192.0.2.1:1234", None), ("127.0.0.1:1234", Some("origin")),
            ("127.0.0.1:1234", Some("x-forwarded-for"))] {
            let mut request = axum::http::Request::post(TOOL_REFS_ROUTE);
            if let Some(header) = header { request = request.header(header, "untrusted"); }
            let mut request = request.body(Body::from(body(&lease.credential))).unwrap();
            request.extensions_mut().insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
            let response = tool_refs_router(registry.clone(), sessions.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        }
        assert_eq!(registry.lookups.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn proxy_headers_are_uniformly_inactive() {
        let registry = Arc::new(Registry::default());
        let lease = registry.issue(record(), 100).unwrap();
        for header in ["Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Real-IP", "Via", "Origin",
            "X-Forwarded-Proto", "X-Forwarded-Port", "X-Forwarded-Prefix", "x-FoRwArDeD-pRoTo",
            "X-fOrWaRdEd-PoRt", "x-FORWARDED-pReFiX", "X-Forwarded-Unknown"] {
            let mut request = axum::http::Request::post(ROUTE)
                .header(header, "untrusted").body(Body::from(body(&lease.credential))).unwrap();
            request.extensions_mut().insert(ConnectInfo("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap()));
            let response = router_with(registry.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap().as_ref(), br#"{"active":false}"#);
        }
        assert_eq!(registry.lookups.load(std::sync::atomic::Ordering::Relaxed), 0);
        // Only the anchored prefix is forbidden; unrelated header names work.
        let mut request = axum::http::Request::post(ROUTE)
            .header("custom-x-forwarded-proto", "untrusted").body(Body::from(body(&lease.credential))).unwrap();
        request.extensions_mut().insert(ConnectInfo("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap()));
        let response = router_with(registry.clone()).oneshot(request).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
        assert_eq!(value["active"], true);
    }

    #[tokio::test]
    async fn dedicated_listener_requires_lock_and_binds_only_ipv4_loopback() {
        let home = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::default());
        assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
        assert!(!home.path().join(IDENTITY_URL_FILE).exists());
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let listener = start_listener_with(home.path(), registry.clone()).await.unwrap();
        assert_eq!(listener.address.ip(), std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert_ne!(listener.address.port(), 0);
        let url = std::fs::read_to_string(home.path().join(IDENTITY_URL_FILE)).unwrap();
        assert_eq!(url, format!("http://{}{}", listener.address, ROUTE));
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(home.path().join("run")).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(home.path().join(IDENTITY_URL_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let lease = registry.issue(record(), 30).unwrap();
        let mut command = tokio::process::Command::new("never-spawned");
        lease.apply(&mut command);
        assert!(command.as_std().get_envs().any(|(key, value)| key == ENV_IDENTITY_URL
            && value.and_then(|v| v.to_str()) == Some(url.as_str())));
        assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
        drop(listener);
        assert!(!home.path().join(IDENTITY_URL_FILE).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn listener_refuses_symlinked_or_group_writable_run_directory() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::default());
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let run = home.path().join("run");
        symlink(outside.path(), &run).unwrap();
        assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
        assert!(!outside.path().join("identity-url").exists());
        std::fs::remove_file(&run).unwrap();
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(start_listener_with(home.path(), registry.clone()).await.is_err());
        assert!(registry.endpoint.lock().unwrap().is_none());
        assert!(!home.path().join(IDENTITY_URL_FILE).exists());
    }

    #[test]
    fn protected_branches_do_not_issue_or_pass_run_credentials() {
        // Source-contract guard: protected launch implementations must remain
        // ahead of host command preparation and must not receive its env.
        let channel = include_str!("channel_reply/cli_env.rs");
        assert!(channel.contains("let _run_identity = if protected { None } else {"));
        let protected = channel.split_once("let mut child = if protected {").unwrap().1
            .split_once("} else {").unwrap().0;
        assert!(!protected.contains("cmd"));
        assert!(!protected.contains("run_identity"));
        assert!(!protected.contains("env"));

        let claude = include_str!("claude_runner.rs");
        let branch = claude.split_once("if duduclaw_core::execution_gate::isolation_required() {").unwrap().1
            .split_once("let claude = duduclaw_core::which_claude()").unwrap().0;
        assert!(branch.contains("return call_claude_streaming"));
        assert!(!branch.contains("prepare_claude_cmd"));
        assert!(!branch.contains("run_identity::issue"));
        assert!(!branch.contains(".env("));
        for source in [include_str!("runtime/codex.rs"), include_str!("runtime/grok.rs"),
            include_str!("runtime/generic_cli.rs")] {
            let execute = source.split_once("async fn execute(").unwrap().1;
            let branch = execute.split_once("if duduclaw_core::execution_gate::isolation_required() {").unwrap().1
                .split_once("\n        }\n").unwrap().0;
            assert!(branch.contains("return "));
            assert!(!branch.contains("run_identity::issue"));
            assert!(!branch.contains(".env("));
            assert!(!branch.contains("prepare_run_command"));
        }
    }

    #[test]
    fn dashboard_does_not_mount_identity_introspection() {
        // The identity router is private to this module; pin the dashboard
        // assembly too, including accidental literal-path registration.
        let server = include_str!("server.rs");
        assert!(!server.contains("run_identity::router"));
        assert!(!server.contains(ROUTE));
    }

    #[test]
    fn junk_and_other_runs_cannot_exhaust_a_valid_runs_bucket() {
        let registry = Arc::new(Registry::default());
        let first = registry.issue(record(), 100).unwrap();
        let second = registry.issue(record(), 100).unwrap();
        let unknown = RunCredential::from_bytes(&[42; 32]);
        for _ in 0..RATE_PER_MINUTE {
            assert!(registry.query(Some(&first.credential)).is_some());
            assert!(registry.query(Some(&unknown)).is_none());
        }
        assert!(registry.query(Some(&first.credential)).is_none());
        assert!(registry.query(None).is_none());
        assert!(registry.query(Some(&second.credential)).is_some());
        assert_eq!(registry.invalid_rate.lock().unwrap().1, RATE_PER_MINUTE);
        assert_eq!(registry.runs.lock().unwrap().get(&second.hash).unwrap().rate.1, 1);
        registry.runs.lock().unwrap().get_mut(&first.hash).unwrap().rate.0 =
            Instant::now() - Duration::from_secs(61);
        assert!(registry.query(Some(&first.credential)).is_some());
        // Valid buckets live inside the bounded registry, not an unbounded
        // attacker-keyed map. Unknown requests share exactly one bucket.
        assert_eq!(registry.runs.lock().unwrap().len(), 2);
        drop(first);
        assert_eq!(registry.runs.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn malformed_and_unknown_requests_do_the_same_digest_lookup_work() {
        let registry = Arc::new(Registry::default());
        for raw in ["not json".to_string(), "{}".into(), "{\"credential\":0}".into(),
            "{\"credential\":\"bad\"}".into(), "x".repeat(2048),
            body(&RunCredential::from_bytes(&[42; 32]))] {
            let before = registry.lookups.load(std::sync::atomic::Ordering::Relaxed);
            let response = query(&registry, Some("127.0.0.1:1234"), raw).await;
            assert_eq!(response, (200, br#"{"active":false}"#.to_vec()));
            assert_eq!(registry.lookups.load(std::sync::atomic::Ordering::Relaxed), before + 1);
        }
    }

    #[test]
    fn registry_retains_only_hashes_and_drop_revokes() {
        let registry = Arc::new(Registry::default());
        let lease = registry.issue(record(), 100).unwrap();
        assert_eq!(format!("{lease:?}"), "RunGuard([REDACTED])");
        let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
        {
            let runs = registry.runs.lock().unwrap();
            assert_eq!(runs.len(), 1);
            assert!(runs.contains_key(&digest(&proof)));
            let entry = runs.get(&digest(&proof)).unwrap();
            assert!(!serde_json::to_string(&entry.record).unwrap().contains(proof.expose()));
        }
        assert!(registry.lookup(&proof).is_some());
        drop(lease);
        assert!(registry.lookup(&proof).is_none());
        assert!(registry.runs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn errors_cancellation_expiry_and_restart_revoke() {
        let registry = Arc::new(Registry::default());
        let lease = registry.issue(record(), u64::MAX).unwrap();
        let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
        let entry = registry.lookup(&proof).unwrap();
        assert_eq!(entry.expires_at - entry.issued_at, MAX_TTL);
        assert!(Registry::default().lookup(&proof).is_none());
        let result: Result<(), ()> = async move { let _lease = lease; Err(()) }.await;
        assert!(result.is_err());
        assert!(registry.lookup(&proof).is_none());

        let lease = registry.issue(record(), 100).unwrap();
        let proof = RunCredential::parse(lease.credential.expose().to_owned()).unwrap();
        let (ready, observed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _lease = lease;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        observed.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(registry.lookup(&proof).is_none());

        let lease = registry.issue(record(), 100).unwrap();
        registry.runs.lock().unwrap().get_mut(&lease.hash).unwrap().deadline = Instant::now();
        assert!(registry.lookup(&lease.credential).is_none());
    }

    #[tokio::test]
    async fn all_inactive_http_responses_are_identical_and_peer_is_real() {
        let registry = Arc::new(Registry::default());
        let active = registry.issue(record(), 100).unwrap();
        let expired = registry.issue(record(), 100).unwrap();
        registry.runs.lock().unwrap().get_mut(&expired.hash).unwrap().record.expires_at = 0;
        let revoked = registry.issue(record(), 100).unwrap();
        let revoked_body = body(&revoked.credential);
        drop(revoked);
        let baseline = (200, br#"{"active":false}"#.to_vec());
        for raw in [String::new(), "not json".into(), "{}".into(), "{\"credential\":0}".into(),
            "{\"credential\":\"ddc_run_bad\"}".into(), body(&RunCredential::from_bytes(&[42; 32])),
            body(&expired.credential), revoked_body, "x".repeat(2048)] {
            assert_eq!(query(&registry, Some("127.0.0.1:1234"), raw).await, baseline);
        }
        for peer in [None, Some("192.0.2.1:1234"), Some("[2001:db8::1]:1234")] {
            assert_eq!(query(&registry, peer, body(&active.credential)).await, baseline);
        }
        for peer in ["127.0.0.1:1234", "[::1]:1234", "[::ffff:127.0.0.1]:1234"] {
            let (status, bytes) = query(&registry, Some(peer), body(&active.credential)).await;
            assert_eq!(status, 200);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["active"], true);
            assert_eq!(value["agent_id"], "alice");
            assert_eq!(value["principal"]["user_id"], "user-1");
            assert!(value.get("credential").is_none());
            assert!(!String::from_utf8(bytes).unwrap().contains(active.credential.expose()));
        }
        registry.runs.lock().unwrap().get_mut(&active.hash).unwrap().rate = (Instant::now(), RATE_PER_MINUTE);
        assert_eq!(query(&registry, Some("127.0.0.1:1234"), body(&active.credential)).await, baseline);
    }

    #[tokio::test]
    async fn gateway_registration_controls_human_and_background_claims() {
        let home = tempfile::tempdir().unwrap();
        for kind in ["cron", "heartbeat", "task"] {
            let run = uuid::Uuid::new_v4().to_string();
            let _registered = crate::knowledge::turn_requesters::register_run("alice", &run, kind).unwrap();
            crate::memory_provenance::DISPATCH_RUN.scope(Some((format!("{kind}:alice"), run)), async {
                let lease = issue(home.path(), "alice", "grok", 30).unwrap();
                assert_eq!(registry().lookup(&lease.credential).unwrap().principal,
                    Principal::Background { kind: kind.into() });
            }).await;
        }
        crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
            RequesterIdentity::DashboardUser { user_id: "verified-user".into() }, async {
                let lease = issue(home.path(), "alice", "codex", 30).unwrap();
                assert_eq!(registry().lookup(&lease.credential).unwrap().principal,
                    Principal::Human { user_id: "verified-user".into() });
            }).await;
    }

    #[test]
    fn capacity_is_bounded_and_expired_entries_are_reclaimed() {
        let registry = Arc::new(Registry::default());
        {
            let mut runs = registry.runs.lock().unwrap();
            for n in 0..MAX_RUNS {
                let hash = Sha256::digest(n.to_be_bytes()).into();
                let mut record = record();
                record.expires_at = u64::MAX;
                runs.insert(hash, Entry { record, deadline: Instant::now() + Duration::from_secs(60), rate: (Instant::now(), 0) });
            }
        }
        assert!(registry.issue(record(), 10).is_err());
        for entry in registry.runs.lock().unwrap().values_mut() { entry.record.expires_at = 0; }
        assert!(registry.issue(record(), 10).is_ok());
    }

    #[test]
    fn runtime_env_and_pty_proofs_are_distinct_and_revoked() {
        let home = tempfile::tempdir().unwrap();
        for runtime in ["claude", "codex", "grok", "copilot", "qwen", "kimi", "opencode", "vibe"] {
            let lease = issue(home.path(), "alice", runtime, 30).unwrap();
            let mut cmd = tokio::process::Command::new("never-spawned");
            lease.apply(&mut cmd);
            let proof = assert_command_identity(&cmd, true);
            assert_eq!(registry().lookup(&proof).unwrap().runtime, runtime);
            let mut env = HashMap::new();
            lease.apply_pty(&mut env);
            assert!(env.get(ENV_RUN_CREDENTIAL).is_some_and(|value| value == proof.expose()));
            drop(lease);
            assert!(registry().lookup(&proof).is_none());
        }
    }
}
