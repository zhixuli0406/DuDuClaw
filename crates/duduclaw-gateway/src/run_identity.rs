//! RM-208: gateway-owned, memory-only run registry. No issuance HTTP API.
//! Child environments are transport, never a source of identity claims.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{Json, Router, body::to_bytes, extract::{ConnectInfo, Request, State}, routing::post};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use duduclaw_core::run_credential::{ENV_RUN_CREDENTIAL, RunCredential};
use duduclaw_knowledge::requester::RequesterIdentity;

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
    pub expires_at: u64,
}

struct Entry { record: RunRecord, deadline: Instant }

pub struct Registry {
    // The only credential material retained here is its SHA-256 digest.
    runs: Mutex<HashMap<[u8; 32], Entry>>,
    rate: Mutex<(Instant, usize)>,
}

impl Default for Registry {
    fn default() -> Self {
        Self { runs: Mutex::new(HashMap::new()), rate: Mutex::new((Instant::now(), 0)) }
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
    port: Option<u16>,
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
        if let Some(port) = self.port {
            cmd.env(duduclaw_core::run_credential::ENV_RUN_GATEWAY_PORT, port.to_string());
        }
    }

    pub fn apply_pty(&self, env: &mut HashMap<String, String>) {
        env.insert(ENV_RUN_CREDENTIAL.to_string(), self.credential.expose().to_string());
        if let Some(port) = self.port {
            env.insert(duduclaw_core::run_credential::ENV_RUN_GATEWAY_PORT.to_string(), port.to_string());
        }
    }
}

impl duduclaw_core::run_credential::RunLease for RunGuard {
    fn credential(&self) -> &RunCredential { &self.credential }
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
        runs.insert(hash, Entry { record, deadline: Instant::now() + Duration::from_secs(ttl) });
        Ok(RunGuard { registry: self.clone(), hash, credential, port: None })
    }

    fn lookup(&self, credential: &RunCredential) -> Option<RunRecord> {
        let now = unix_now().ok()?;
        let hash = digest(credential);
        let mut runs = self.runs.lock().ok()?;
        // Hash-map lookup compares fixed-size cryptographic digests, never
        // plaintext prefixes. Confirm the selected key in constant time.
        let (key, entry) = runs.get_key_value(&hash)?;
        ring::constant_time::verify_slices_are_equal(key, &hash).ok()?;
        if entry.record.expires_at <= now || entry.deadline <= Instant::now() {
            runs.remove(&hash);
            return None;
        }
        Some(entry.record.clone())
    }

    fn allow_query(&self) -> bool {
        let Ok(mut rate) = self.rate.lock() else { return false; };
        if rate.0.elapsed() >= Duration::from_secs(60) { *rate = (Instant::now(), 0); }
        if rate.1 >= RATE_PER_MINUTE { return false; }
        rate.1 += 1;
        true
    }
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
        runtime: runtime.to_string(), issued_at: 0, expires_at: 0,
    };
    let mut guard = registry().issue(record, timeout)?;
    guard.port = Some(duduclaw_core::gateway_port_for_home(home).0);
    Ok(guard)
}

pub fn router() -> Router { router_with(registry().clone()) }

fn router_with(registry: Arc<Registry>) -> Router {
    Router::new().route(ROUTE, post(handle)).with_state(registry)
}

async fn handle(State(registry): State<Arc<Registry>>, request: Request) -> Json<serde_json::Value> {
    let inactive = || Json(serde_json::json!({"active": false}));
    let Some(peer) = request.extensions().get::<ConnectInfo<std::net::SocketAddr>>() else { return inactive(); };
    if crate::computer_use_sessions::auth::check_peer(peer.0).is_err() || !registry.allow_query() {
        return inactive();
    }
    // Read only after checking the actual TCP peer. No body/error logging.
    let Ok(Ok(body)) = tokio::time::timeout(Duration::from_secs(2), to_bytes(request.into_body(), 1024)).await
        else { return inactive(); };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else { return inactive(); };
    let Some(raw) = value.get_mut("credential").map(serde_json::Value::take) else { return inactive(); };
    let serde_json::Value::String(raw) = raw else { return inactive(); };
    let Some(credential) = RunCredential::parse(raw) else { return inactive(); };
    let Some(record) = registry.lookup(&credential) else { return inactive(); };
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
            turn_id: "turn-1".into(), task_id: None, runtime: "claude".into(), issued_at: 0, expires_at: 0 }
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
        *registry.rate.lock().unwrap() = (Instant::now(), RATE_PER_MINUTE);
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
                runs.insert(hash, Entry { record, deadline: Instant::now() + Duration::from_secs(60) });
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
