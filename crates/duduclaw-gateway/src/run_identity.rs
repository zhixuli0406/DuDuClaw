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
use duduclaw_core::run_credential::{ENV_IDENTITY_URL, ENV_RUN_CREDENTIAL, RunCredential, SpawnBinding};
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

struct Entry { binding: SpawnBinding, record: RunRecord, deadline: Instant, rate: (Instant, usize) }

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
    binding: SpawnBinding,
}

impl std::fmt::Debug for RunGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunGuard([REDACTED])")
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.binding.revoke();
        // A poisoned registry already denies every lookup and issuance.
        if let Ok(mut runs) = self.registry.runs.lock() { runs.remove(&self.hash); }
    }
}

mod issuer;
pub use issuer::RunIssuer;

impl RunGuard {
    pub fn spawn_binding(&self) -> SpawnBinding { self.binding.clone() }

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
    fn spawn_binding(&self) -> SpawnBinding { self.binding.clone() }
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
    fn issue(self: &Arc<Self>, record: RunRecord, timeout: u64) -> Result<RunGuard, String> {
        self.issue_until(record, timeout, None)
    }

    fn issue_until(self: &Arc<Self>, mut record: RunRecord, timeout: u64, limit: Option<Instant>) -> Result<RunGuard, String> {
        let now = unix_now()?;
        let ttl = timeout.saturating_add(60).min(MAX_TTL);
        record.issued_at = now;
        record.issued_at_exact = chrono::Utc::now();
        record.expires_at = if limit.is_some() { record.expires_at.min(now.saturating_add(ttl)) } else { now.saturating_add(ttl) };
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
        let deadline = Instant::now() + Duration::from_secs(ttl);
        let deadline = limit.map_or(deadline, |limit| limit.min(deadline));
        let binding = SpawnBinding::pending(deadline);
        runs.insert(hash, Entry { binding: binding.clone(), record, deadline, rate: (Instant::now(), 0) });
        Ok(RunGuard { registry: self.clone(), hash, credential, identity_url, binding })
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
            entry.binding.revoke();
            runs.remove(&hash);
            return Ok(None);
        }
        if !well_formed || entry.binding.root().is_none() { return Ok(None); }
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
    if duduclaw_core::execution_gate::isolation_required() {
        return Err("Protected execution does not issue host run credentials".into());
    }
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
pub(crate) fn assert_command_identity(command: &tokio::process::Command, issued: bool) -> RunCredential {
    let value = command.as_std().get_envs()
        .find(|(name, _)| *name == ENV_RUN_CREDENTIAL)
        .and_then(|(_, value)| value).and_then(|value| value.to_str()).expect("run credential env missing");
    let credential = RunCredential::parse(value.to_string()).expect("malformed run credential");
    assert_eq!(registry().runs.lock().unwrap().contains_key(&digest(&credential)), issued);
    assert!(registry().lookup(&credential).is_none(), "unspawned command must remain inactive");
    assert!(!command.as_std().get_args().any(|arg| arg.to_string_lossy().contains(value)));
    credential
}

#[cfg(test)]
mod tests;
