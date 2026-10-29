//! Signed loopback client shared by built-in MCP and third-party bridges.
//! Only tainted states are cached; entries stay tainted until their TTL expires.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use crate::egress_guard::Taint;
use duduclaw_core::egress_guard::{RESPONSE_DOMAIN, RESPONSE_SIGNATURE_HEADER};

type Key = (PathBuf, String, String);

const MAX_CACHE_ENTRIES: usize = 65_536;
const MAX_CACHED_SOURCES: usize = 512;
const ADDITIONAL_PRIVATE_SOURCES: &str = "additional_private_sources";

struct CachedTaint {
    recorded_at: Instant,
    sources: BTreeSet<String>,
}

#[derive(Default)]
struct TaintCache {
    entries: HashMap<Key, CachedTaint>,
    // A dropped taint cannot subsequently be mistaken for a clean cache miss.
    overflowed: bool,
}

impl TaintCache {
    fn merge(&mut self, key: Key, state: Taint, now: Instant) -> Taint {
        // Clean up even after overflow: fail-closed state does not need to keep
        // expired source names in memory. Expired turns are unknown at gateway.
        self.entries.retain(|_, entry|
            now.saturating_duration_since(entry.recorded_at) < crate::egress_guard::TTL);
        if self.overflowed { return Taint::unknown(); }
        if state.tainted {
            if self.entries.len() >= MAX_CACHE_ENTRIES && !self.entries.contains_key(&key) {
                self.overflowed = true;
                return Taint::unknown();
            }
            let entry = self.entries.entry(key.clone()).or_insert_with(|| CachedTaint {
                recorded_at: now,
                sources: BTreeSet::new(),
            });
            for source in state.sources {
                if entry.sources.contains(&source) { continue; }
                if entry.sources.len() < MAX_CACHED_SOURCES {
                    entry.sources.insert(source);
                } else if !entry.sources.contains(ADDITIONAL_PRIVATE_SOURCES) {
                    // The summary marker counts toward the same 512-entry cap.
                    entry.sources.pop_last();
                    entry.sources.insert(ADDITIONAL_PRIVATE_SOURCES.into());
                }
            }
        }
        match self.entries.get(&key) {
            Some(entry) => Taint {
                tainted: true,
                sources: entry.sources.iter().cloned().collect(),
            },
            None => Taint { tainted: false, sources: Vec::new() },
        }
    }
}

fn cache() -> &'static Mutex<TaintCache> {
    static CACHE: OnceLock<Mutex<TaintCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TaintCache::default()))
}

fn checked_turn_id() -> Result<String, &'static str> {
    checked_identity(
        std::env::var_os(duduclaw_core::ENV_TRUST_TURN_ID),
        std::env::var_os(duduclaw_core::egress_guard::ENV_TURN_ALIAS),
    )
}

fn checked_identity(primary: Option<std::ffi::OsString>, alias: Option<std::ffi::OsString>) -> Result<String, &'static str> {
    if primary.is_none() && alias.is_none() { return Err(crate::egress_guard::UNTRACKED_TURN); }
    resolve_turn_id(primary, alias).ok_or("invalid turn identity")
}

fn turn_id() -> Option<String> {
    resolve_turn_id(
        std::env::var_os(duduclaw_core::ENV_TRUST_TURN_ID),
        std::env::var_os(duduclaw_core::egress_guard::ENV_TURN_ALIAS),
    )
}

fn resolve_turn_id(
    primary: Option<std::ffi::OsString>,
    alias: Option<std::ffi::OsString>,
) -> Option<String> {
    // A present malformed primary remains a refusal; the alias is only for
    // runtimes that do not declare the original variable at all.
    primary
        .or(alias)?
        .into_string()
        .ok()
        .filter(|turn| crate::computer_use_sessions::turns::valid_turn_id(turn))
}

fn merge(key: Key, state: Taint) -> Taint {
    let Ok(mut cache) = cache().lock() else {
        return Taint::unknown();
    };
    cache.merge(key, state, Instant::now())
}

pub async fn query(home: &Path, agent: &str) -> Taint {
    let Some(turn) = turn_id() else {
        return merge((home.into(), agent.into(), String::new()), Taint::unknown());
    };
    let result = exchange(home, agent, &turn, "query", None, None, None)
        .await
        .unwrap_or_else(|reason| {
            tracing::warn!(%reason, "egress guard query failed; treating turn as tainted");
            Taint::unknown()
        });
    merge((home.into(), agent.into(), turn), result)
}

/// Callers in enforcing modes must withhold private results on an error:
/// otherwise another MCP process could still observe a clean gateway turn.
pub async fn mark(home: &Path, agent: &str, tool: &str) -> Result<(), String> {
    let turn = match checked_turn_id() {
        Ok(turn) => turn,
        Err(reason) => {
            // Unattributed results remain tainted. Read admission already
            // checked the authenticated agent fence through reserve().
            let mut state = Taint::unknown();
            state.sources.push(tool.into());
            merge((home.into(), agent.into(), String::new()), state);
            return reservation_failure(home, agent, tool, reason);
        }
    };
    let key = (home.into(), agent.into(), turn.clone());
    merge(
        key.clone(),
        Taint {
            tainted: true,
            sources: vec![tool.into()],
        },
    );
    match exchange(home, agent, &turn, "mark", Some(tool), None, None).await {
        Ok(state) if state.tainted => {
            merge(key, state);
            Ok(())
        }
        Ok(_) => Err("gateway did not record private read".into()),
        Err(reason) => {
            tracing::warn!(%reason, "egress guard mark failed; treating turn as tainted");
            merge(key, Taint::unknown());
            Err(reason)
        }
    }
}

/// The identity is captured once, so completion cannot affect a different turn.
pub struct PrivateRead {
    home: PathBuf,
    agent: String,
    turn: String,
    id: String,
}

impl PrivateRead {
    pub(crate) fn id(&self) -> &str { &self.id }

    pub async fn finish(self, payload: bool) -> Result<(), String> {
        let state = exchange(&self.home, &self.agent, &self.turn, "finish", None, Some(&self.id), Some(payload)).await?;
        if payload {
            merge((self.home, self.agent, self.turn), state);
        }
        Ok(())
    }
}

// An absent turn must reach the authenticated gateway, which checks the agent's
// active sinks and restart fence. A malformed supplied identity still fails closed.
fn read_turn_id(identity: Result<String, &'static str>) -> Result<String, String> {
    match identity {
        Ok(turn) => Ok(turn),
        Err(crate::egress_guard::UNTRACKED_TURN) => Ok(String::new()),
        Err(reason) => Err(reason.into()),
    }
}

pub async fn reserve(home: &Path, agent: &str, tool: &str) -> Result<(PrivateRead, Taint), String> {
    let turn = read_turn_id(checked_turn_id())?;
    let id = uuid::Uuid::new_v4().to_string();
    let state = exchange(home, agent, &turn, "reserve", Some(tool), Some(&id), None).await?;
    let state = merge((home.into(), agent.into(), turn.clone()), state);
    Ok((PrivateRead { home: home.into(), agent: agent.into(), turn, id }, state))
}

/// A failed reservation cannot let another process receive untracked private data.
/// Shadow mode records the failure while preserving its observational behavior.
pub fn reservation_failure(home: &Path, agent: &str, tool: &str, reason: &str) -> Result<(), String> {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let mode = duduclaw_core::egress_guard::effective_mode_checked(home, Some(&dir))?;
    duduclaw_security::audit::append_tool_call_with_extras(
        home, agent, tool, "egress guard private-read reservation unavailable", reason == crate::egress_guard::UNTRACKED_TURN,
        &[("egress_guard", serde_json::json!(if reason == crate::egress_guard::UNTRACKED_TURN { "read_without_turn" } else { "reservation_failed" })),
            ("sources", serde_json::json!([tool])), ("reason", serde_json::json!(reason))],
    );
    if reason == crate::egress_guard::UNTRACKED_TURN {
        return Ok(());
    }
    if mode >= duduclaw_core::egress_guard::Mode::Ask {
        Err(if reason == crate::egress_guard::READ_DURING_SINK {
            reason.into()
        } else {
            "無法確認本回合的外送防護狀態，已暫停呼叫；請稍後重試。".into()
        })
    } else { Ok(()) }
}

/// Two-phase transport lease. Uncertain dispatched calls retain the gateway
/// fence until lease expiry, even when every local owner is dropped.
pub struct SinkLease {
    home: PathBuf,
    agent: String,
    turn: String,
    id: String,
    direct: bool,
    released: bool,
    phase: AtomicU8,
}

const NOT_DISPATCHED: u8 = 0;
const DISPATCHED: u8 = 1;
const TERMINAL: u8 = 2;

impl SinkLease {
    pub(crate) fn direct(home: &Path, agent: &str, turn: Option<&str>, own_read: Option<&str>) -> Result<(Self, Taint), String> {
        let id = uuid::Uuid::new_v4().to_string();
        let taint = crate::egress_guard::acquire_sink_turn(home, agent, turn, &id, own_read)?;
        Ok((Self { home: home.into(), agent: agent.into(), turn: turn.unwrap_or("").into(), id, direct: true, released: false, phase: AtomicU8::new(NOT_DISPATCHED) }, taint))
    }

    pub async fn acquire(home: &Path, agent: &str, read: Option<&PrivateRead>) -> Result<(Self, Taint), String> {
        let turn = read_turn_id(checked_turn_id())?;
        if read.is_some_and(|read| read.turn != turn || read.agent != agent || read.home != home) {
            return Err("private read identity mismatch".into());
        }
        Self::acquire_id(home, agent, read.map(PrivateRead::id)).await
    }

    pub(crate) async fn acquire_id(home: &Path, agent: &str, own_read: Option<&str>) -> Result<(Self, Taint), String> {
        let turn = read_turn_id(checked_turn_id())?;
        // Construct the guard before awaiting: cancellation after gateway
        // admission must still attempt release using this exact lease id.
        let lease = Self { home: home.into(), agent: agent.into(), turn, id: uuid::Uuid::new_v4().to_string(), direct: false, released: false, phase: AtomicU8::new(NOT_DISPATCHED) };
        let taint = exchange_with_read(home, agent, &lease.turn, "acquire_sink", None, Some(&lease.id), None, own_read).await?;
        Ok((lease, taint))
    }

    /// Mark before the first potentially partial write, with no intervening await.
    /// A local cancellation/error cannot prove that the upstream stopped.
    pub fn dispatched(&self) {
        let _ = self.phase.compare_exchange(NOT_DISPATCHED, DISPATCHED, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Only a correlated terminal reply (or confirmed upstream cancellation)
    /// permits release after dispatch. A local timeout/error is not a reply.
    pub fn terminal_response(&self) {
        self.phase.store(TERMINAL, Ordering::Release);
    }

    pub async fn release(mut self) {
        if self.phase.load(Ordering::Acquire) == DISPATCHED { return; }
        // Drop retries only a safe release if this future is cancelled.
        if !self.direct {
            self.released = exchange(&self.home, &self.agent, &self.turn, "release_sink", None, Some(&self.id), None).await.is_ok();
        }
    }

}

impl duduclaw_llm::ClientCallLease for SinkLease {
    fn dispatched(&self) { SinkLease::dispatched(self); }
    fn terminal_response(&self) { SinkLease::terminal_response(self); }
}

impl Drop for SinkLease {
    fn drop(&mut self) {
        if self.released || self.phase.load(Ordering::Acquire) == DISPATCHED { return; }
        if self.direct {
            crate::egress_guard::release_sink_turn(&self.agent, &self.turn, &self.id);
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (home, agent, turn, id) = (self.home.clone(), self.agent.clone(), self.turn.clone(), self.id.clone());
            runtime.spawn(async move {
                let _ = exchange(&home, &agent, &turn, "release_sink", None, Some(&id), None).await;
            });
        }
    }
}

async fn exchange(
    home: &Path, agent: &str, turn: &str, op: &str,
    source: Option<&str>, reservation: Option<&str>, payload: Option<bool>,
) -> Result<Taint, String> {
    exchange_with_read(home, agent, turn, op, source, reservation, payload, None).await
}

async fn exchange_with_read(
    home: &Path,
    agent: &str,
    turn: &str,
    op: &str,
    source: Option<&str>,
    reservation: Option<&str>,
    payload: Option<bool>,
    own_read: Option<&str>,
) -> Result<Taint, String> {
    let key = std::env::var(duduclaw_core::ENV_MCP_API_KEY).map_err(|_| "missing gateway key")?;
    let token =
        std::env::var(duduclaw_core::ENV_AGENT_TOKEN).map_err(|_| "missing employee token")?;
    if key.is_empty() || token.is_empty() || !duduclaw_core::is_valid_agent_id(agent) {
        return Err("missing or invalid identity".into());
    }
    let route = match op {
        "mark" => crate::egress_guard::MARK_ROUTE,
        "reserve" => crate::egress_guard::RESERVE_ROUTE,
        "finish" => crate::egress_guard::FINISH_ROUTE,
        "acquire_sink" => crate::egress_guard::ACQUIRE_SINK_ROUTE,
        "release_sink" => crate::egress_guard::RELEASE_SINK_ROUTE,
        _ => crate::egress_guard::QUERY_ROUTE,
    };
    let body = serde_json::to_vec(&serde_json::json!({
        "op": op, "turn_id": turn, "source": source, "reservation": reservation, "payload": payload, "own_read": own_read
    })).map_err(|_| "bad request")?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    let signature = duduclaw_core::internal_request_signature(
        key.as_bytes(),
        agent,
        &token,
        &timestamp,
        &nonce,
        &body,
    );
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "client unavailable")?;
    let mut response = client
        .post(format!("http://127.0.0.1:{port}{route}"))
        .header("Content-Type", "application/json")
        .header("X-Duduclaw-Agent-Id", agent)
        .header("X-Duduclaw-Timestamp", timestamp)
        .header("X-Duduclaw-Nonce", &nonce)
        .header("X-Duduclaw-Signature", signature)
        .body(body)
        .send()
        .await
        .map_err(|_| "gateway unreachable")?;
    if !response.status().is_success() {
        return Err("gateway refused taint request".into());
    }
    let signature = response
        .headers()
        .get(RESPONSE_SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or("unsigned gateway response")?
        .to_string();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "response unreadable")? {
        if bytes.len() + chunk.len() > 256 * 1024 {
            return Err("response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    interpret(&key, agent, &token, &nonce, &signature, &bytes)
}

fn interpret(
    key: &str,
    agent: &str,
    token: &str,
    nonce: &str,
    signature: &str,
    bytes: &[u8],
) -> Result<Taint, String> {
    if !duduclaw_core::verify_internal_request_signature(
        key.as_bytes(),
        agent,
        token,
        RESPONSE_DOMAIN,
        nonce,
        bytes,
        signature,
    ) {
        return Err("unverifiable gateway response".into());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "invalid gateway response")?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(duduclaw_core::truncate_bytes(error, 1024).into());
    }
    let state: Taint = serde_json::from_slice(bytes).map_err(|_| "invalid gateway response")?;
    if state.tainted == state.sources.is_empty()
        || state.sources.len() > 514
        || state
            .sources
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err("inconsistent gateway response".into());
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn egress_valid_turn_unreachable_gateway_refuses_read() {
        // Process isolation keeps credential/turn env changes out of parallel tests.
        const CHILD: &str = "DUDUCLAW_EGRESS_UNREACHABLE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            for missing in [false, true] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", "egress_client::tests::egress_valid_turn_unreachable_gateway_refuses_read", "--nocapture"])
                    .env(CHILD, "1").env("DUDUCLAW_HOME", home.path())
                    .env(duduclaw_core::ENV_TRUST_TURN_ID, "isolated-valid-turn")
                    .env(duduclaw_core::ENV_MCP_API_KEY, "test-key")
                    .env(duduclaw_core::ENV_AGENT_TOKEN, "test-token")
                    .env("DUDUCLAW_PORT", "1")
                    .env_remove(duduclaw_core::egress_guard::ENV_TURN_ALIAS);
                if missing { command.env_remove(duduclaw_core::ENV_TRUST_TURN_ID); }
                assert!(command.status().unwrap().success());
            }
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        let reason = reserve(home.path(), "bot", "memory_search").await.err().unwrap();
        assert_eq!(reason, "gateway unreachable");
        for mode in ["block", "ask", "shadow"] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
            assert_eq!(reservation_failure(home.path(), "bot", "memory_search", &reason).is_err(), mode != "shadow");
        }
    }

    #[test]
    fn egress_absent_identity_is_distinct_from_malformed_or_unverifiable() {
        assert_eq!(checked_identity(None, None).unwrap_err(), crate::egress_guard::UNTRACKED_TURN);
        assert_eq!(read_turn_id(checked_identity(None, None)).unwrap(), "");
        assert!(read_turn_id(checked_identity(Some("".into()), None)).is_err());
        assert_eq!(checked_identity(Some("valid-turn".into()), None).unwrap(), "valid-turn");
        assert_ne!(checked_identity(Some("".into()), None).unwrap_err(), crate::egress_guard::UNTRACKED_TURN);
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        let body = serde_json::to_vec(&serde_json::json!({"error": crate::egress_guard::UNTRACKED_TURN})).unwrap();
        let sig = duduclaw_core::internal_request_signature(b"key", "bot", "token", RESPONSE_DOMAIN, "nonce", &body);
        let verified = interpret("key", "bot", "token", "nonce", &sig, &body).unwrap_err();
        assert_eq!(verified, crate::egress_guard::UNTRACKED_TURN);
        let unverified = interpret("key", "bot", "token", "wrong-nonce", &sig, &body).unwrap_err();
        for mode in ["ask", "block", "shadow"] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
            assert!(reservation_failure(home.path(), "bot", "memory_search", &verified).is_ok());
            for reason in [&unverified, "gateway unreachable", "invalid turn identity"] {
                assert_eq!(reservation_failure(home.path(), "bot", "memory_search", reason).is_err(), mode != "shadow");
            }
        }
    }

    #[test]
    fn egress_reservation_failure_refuses_enforcing_modes_and_audits_shadow() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/bot");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "").unwrap();
        for (mode, refused) in [("shadow", false), ("ask", true), ("block", true)] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
            assert_eq!(reservation_failure(home.path(), "bot", "memory_read", "gateway unreachable").is_err(), refused);
        }
        let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
        assert_eq!(row["egress_guard"], "reservation_failed");
    }

    #[test]
    fn turn_alias_only_fills_an_absent_primary() {
        assert_eq!(
            resolve_turn_id(None, Some("turn-alias".into())),
            Some("turn-alias".into())
        );
        assert_eq!(
            resolve_turn_id(Some("turn-primary".into()), Some("turn-alias".into())),
            Some("turn-primary".into())
        );
        assert_eq!(
            resolve_turn_id(Some("".into()), Some("turn-alias".into())),
            None
        );
        assert_eq!(
            resolve_turn_id(Some("bad\nturn".into()), Some("turn-alias".into())),
            None
        );
        assert_eq!(resolve_turn_id(None, Some("".into())), None);
    }

    #[test]
    fn response_is_bound_to_nonce_agent_and_body() {
        let body = br#"{"tainted":false,"sources":[]}"#;
        let sig = duduclaw_core::internal_request_signature(
            b"key",
            "agent",
            "token",
            RESPONSE_DOMAIN,
            "nonce",
            body,
        );
        assert!(
            !interpret("key", "agent", "token", "nonce", &sig, body)
                .unwrap()
                .tainted
        );
        assert!(interpret("key", "other", "token", "nonce", &sig, body).is_err());
        assert!(interpret("key", "agent", "token", "other", &sig, body).is_err());
        assert!(interpret("key", "agent", "token", "nonce", &sig, b"{}").is_err());
        let forged = duduclaw_core::internal_request_signature(
            b"key", "agent", "token", "1234", "nonce", body,
        );
        assert!(interpret("key", "agent", "token", "nonce", &forged, body).is_err());
    }

    #[test]
    fn local_taint_never_downgrades() {
        let tmp = tempfile::tempdir().unwrap();
        let key = (tmp.path().into(), "agent".into(), "turn".into());
        let state = Taint {
            tainted: true,
            sources: vec!["memory_search".into()],
        };
        assert!(merge(key.clone(), state).tainted);
        let result = merge(
            key,
            Taint {
                tainted: false,
                sources: Vec::new(),
            },
        );
        assert!(result.tainted);
        assert_eq!(result.sources, ["memory_search"]);
    }

    #[test]
    fn egress_local_cache_caps_sources_including_overflow_marker() {
        let mut cache = TaintCache::default();
        let now = Instant::now();
        // Missing-turn callers share this key for the life of the process.
        let key = (PathBuf::from("isolated-home"), "agent".into(), String::new());
        let sources: Vec<_> = (0..MAX_CACHED_SOURCES).map(|n| format!("private_tool_{n:04}")).collect();
        let state = cache.merge(key.clone(), Taint { tainted: true, sources: sources.clone() }, now);
        assert_eq!(state.sources.len(), MAX_CACHED_SOURCES);
        assert!(!state.sources.iter().any(|source| source == ADDITIONAL_PRIVATE_SOURCES));
        // Duplicates do not consume capacity or imply additional sources.
        let state = cache.merge(key.clone(), Taint { tainted: true, sources }, now);
        assert!(!state.sources.iter().any(|source| source == ADDITIONAL_PRIVATE_SOURCES));
        for n in 0..1024 {
            let state = cache.merge(key.clone(), Taint {
                tainted: true, sources: vec![format!("another_private_tool_{n}")],
            }, now);
            assert!(state.tainted);
            assert_eq!(state.sources.len(), MAX_CACHED_SOURCES);
            assert!(state.sources.iter().any(|source| source == ADDITIONAL_PRIVATE_SOURCES));
        }
        let clean = Taint { tainted: false, sources: Vec::new() };
        assert!(cache.merge(key, clean, now).tainted);
    }

    #[test]
    fn egress_local_cache_cleans_expired_entries_and_keeps_live_taint() {
        let mut cache = TaintCache::default();
        let now = Instant::now();
        let old = (PathBuf::from("isolated-home"), "agent".into(), "old-turn".into());
        let live = (PathBuf::from("isolated-home"), "agent".into(), "live-turn".into());
        cache.merge(old.clone(), Taint { tainted: true, sources: vec!["old_source".into()] }, now);
        cache.merge(live.clone(), Taint { tainted: true, sources: vec!["live_source".into()] }, now + Duration::from_secs(1));
        let clean = Taint { tainted: false, sources: Vec::new() };
        assert!(cache.merge(old.clone(), clean.clone(), now + crate::egress_guard::TTL - Duration::from_nanos(1)).tainted);
        let result = cache.merge(live, clean, now + crate::egress_guard::TTL);
        assert!(result.tainted);
        assert_eq!(result.sources, ["live_source"]);
        assert!(!cache.entries.contains_key(&old));
        assert_eq!(cache.entries.len(), 1);
        // An expired gateway turn is reintroduced as unknown, never clean.
        let result = cache.merge(old, Taint::unknown(), now + crate::egress_guard::TTL);
        assert_eq!(result, Taint::unknown());
    }

    #[test]
    fn egress_local_cache_overflow_stays_closed_while_expired_sources_are_cleaned() {
        let mut cache = TaintCache::default();
        let now = Instant::now();
        let key = (PathBuf::from("isolated-home"), "agent".into(), "turn".into());
        cache.merge(key.clone(), Taint { tainted: true, sources: vec!["private_source".into()] }, now);
        cache.overflowed = true;
        let state = cache.merge(key, Taint { tainted: false, sources: Vec::new() }, now + crate::egress_guard::TTL);
        assert_eq!(state, Taint::unknown());
        assert!(cache.entries.is_empty());
    }
}
