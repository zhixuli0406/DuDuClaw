//! Gateway-owned, monotonic turn taint. Unknown/expired turns are tainted.

mod lease_store;
pub mod recovery;
mod hook;
mod connectors;

pub const HOOK_CHECK_ROUTE: &str = "/api/internal/egress-guard/hook-check";

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::computer_use_sessions::auth::{
    authenticate, check_peer, RateLimiter, ReplayGuard, SIGNATURE_HEADER, TIMESTAMP_HEADER,
};

pub const MARK_ROUTE: &str = "/api/internal/egress-guard/mark";
pub const RECIPIENTS_ROUTE: &str = "/api/internal/recipient-guard/query";
pub const QUERY_ROUTE: &str = "/api/internal/egress-guard/query";
pub const RESERVE_ROUTE: &str = "/api/internal/egress-guard/reserve";
pub const FINISH_ROUTE: &str = "/api/internal/egress-guard/finish";
pub const ACQUIRE_SINK_ROUTE: &str = "/api/internal/egress-guard/acquire-sink";
pub const RELEASE_SINK_ROUTE: &str = "/api/internal/egress-guard/release-sink";
pub const READ_DURING_SINK: &str = "此員工正在執行對外動作，暫時無法讀取私人資料；請等動作結束後重試。";
/// Read admission returns this only after checking the authenticated agent fence.
/// An unknown turn remains tainted. Loopback clients accept it only after signature verification.
pub const UNTRACKED_TURN: &str = "egress_untracked_turn";
const MAX_SINK_LEASES: usize = 512;
const MAX_TOTAL_SINK_LEASES: usize = 65_536;
const STORE_RETRY: Duration = Duration::from_secs(5);
const MAX_BLOCKING_WORK: usize = 64;
pub const LEASE_STORE_UNAVAILABLE: &str = "外送防護紀錄無法讀取，暫停私人讀取及對外動作；請稍後重試。";
pub const TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_TURNS: usize = 65_536;
const MAX_AGENT_TURNS: usize = 4096;
const MAX_SOURCES: usize = 512;
// Permanent per-agent registration history: at most 32 MiB per gateway.
// Saturation only taints that agent's new turns; it cannot clear an old turn.
const MAX_REGISTRATION_AGENTS: usize = 1024;
const REGISTRATION_WORDS: usize = 4096;

type Key = (String, String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Taint {
    pub tainted: bool,
    pub sources: Vec<String>,
    #[serde(default = "unknown_connectors")]
    pub connector_refs: Vec<String>,
    #[serde(default)]
    pub dropped_connector_refs: u64,
    #[serde(default)]
    pub turn_started_at: Option<String>,
}

fn unknown_connectors() -> Vec<String> { vec!["unknown".into()] }

impl Taint {
    pub fn unknown() -> Self {
        Self {
            tainted: true,
            sources: vec!["turn_state_unavailable".into()],
            connector_refs: vec!["unknown".into()],
            dropped_connector_refs: 0,
            turn_started_at: None,
        }
    }
}

#[derive(Clone)]
pub struct ConnectorSnapshot {
    pub refs: Vec<String>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

impl ConnectorSnapshot {
    pub fn unknown() -> Self {
        Self { refs: vec!["unknown".into()], observed_at: chrono::DateTime::UNIX_EPOCH }
    }

    pub fn stamp(&self, provenance: duduclaw_memory::lineage::Provenance) -> duduclaw_memory::lineage::Provenance {
        provenance.with_connector_refs_at(self.refs.clone(), self.observed_at)
    }
}

impl Taint {
    pub(crate) fn connector_snapshot(&self) -> ConnectorSnapshot {
        match self.turn_started_at.as_deref().and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()) {
            Some(at) => ConnectorSnapshot { refs: self.connector_refs.clone(), observed_at: at.with_timezone(&chrono::Utc) },
            None => ConnectorSnapshot::unknown(),
        }
    }
}

struct Turn {
    created: Instant,
    started_at: String,
    sources: BTreeSet<String>,
    connector_refs: BTreeSet<String>,
    dropped_connector_refs: u64,
    pending: HashMap<String, String>,
}

struct Sink {
    acquired: Instant,
    home: Option<PathBuf>,
    restored: bool,
}

#[derive(Default)]
struct Persistence {
    store: Option<lease_store::Store>,
    attempted: Option<Instant>,
    logged: Option<Instant>,
}

struct Ledger {
    // Agent -> (turn, lease id). Never pruned with turns.
    sinks: HashMap<String, HashMap<Key, Sink>>,
    // Homes come only from gateway-owned configuration, never request fields.
    stores: HashMap<PathBuf, Persistence>,
    sink_count: usize,
    turns: HashMap<Key, Turn>,
    // Never rotate tombstones: an evicted/expired turn must not become clean
    // when re-registered. Separate filters prevent cross-agent saturation.
    seen: HashMap<String, Vec<u64>>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            stores: HashMap::new(), sinks: HashMap::new(), sink_count: 0,
            turns: HashMap::new(), seen: HashMap::new(),
        }
    }
}

impl Ledger {
    fn store_error(&mut self, home: &Path, now: Instant, reason: &str) {
        #[cfg(test)]
        eprintln!("egress store error at {}: {reason}", home.display());
        let state = self.stores.entry(home.into()).or_default();
        if state.store.is_some() { state.attempted = Some(now); }
        state.store = None;
        if state.logged.is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(60)) {
            tracing::error!(error = %reason, "egress lease store unavailable; private reads fenced");
            state.logged = Some(now);
        }
    }

    fn ensure_store(&mut self, home: &Path, now: Instant) -> bool {
        let state = self.stores.entry(home.into()).or_default();
        if state.store.is_some() { return true; }
        if state.attempted.is_some_and(|last| now.saturating_duration_since(last) < STORE_RETRY) { return false; }
        state.attempted = Some(now);
        let opened = lease_store::Store::open(home).and_then(|mut store| {
            let rows = store.restore()?;
            Ok((store, rows))
        });
        match opened {
            Ok((store, rows)) => {
                // Publish the readable state only after every row is restored.
                // Live entries surviving a transient store error keep their
                // terminal correlation; rows from a previous gateway do not.
                for row in rows {
                    self.sinks.entry(row.agent).or_default().entry((row.turn, row.id)).or_insert(Sink {
                        acquired: now, home: Some(home.into()), restored: true,
                    });
                }
                self.sink_count = self.sinks.values().map(HashMap::len).sum();
                self.stores.get_mut(home).unwrap().store = Some(store);
                true
            }
            Err(reason) => { self.store_error(home, now, &reason); false }
        }
    }

    fn delete_lease(&mut self, agent: &str, key: &Key, now: Instant) -> bool {
        let Some(sink) = self.sinks.get(agent).and_then(|leases| leases.get(key)) else { return true; };
        let Some(home) = sink.home.clone() else { return true; };
        let result = self.stores.get(&home).and_then(|s| s.store.as_ref())
            .ok_or_else(|| LEASE_STORE_UNAVAILABLE.to_string())
            .and_then(|store| store.delete(agent, &key.0, &key.1));
        match result {
            Ok(()) => true,
            Err(reason) => { self.store_error(&home, now, &reason); false }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.turns.retain(|_, turn| now.saturating_duration_since(turn.created) < TTL);
        let expired: Vec<_> = self.sinks.iter().flat_map(|(agent, leases)| {
            leases.iter().filter(move |(_, lease)| now.saturating_duration_since(lease.acquired) >= TTL)
                .map(move |(key, _)| (agent.clone(), key.clone()))
        }).collect();
        let mut by_home: HashMap<PathBuf, Vec<(String, Key)>> = HashMap::new();
        for (agent, key) in expired {
            match self.sinks[&agent][&key].home.clone() {
                Some(home) => by_home.entry(home).or_default().push((agent, key)),
                None => { self.sinks.get_mut(&agent).unwrap().remove(&key); }
            }
        }
        for (home, leases) in by_home {
            // One FULL-sync commit per home, not per row under the global mutex.
            let result = self.stores.get_mut(&home).and_then(|s| s.store.as_mut())
                .ok_or_else(|| LEASE_STORE_UNAVAILABLE.to_string())
                .and_then(|store| store.delete_batch(&leases));
            match result {
                Ok(()) => for (agent, key) in leases { self.sinks.get_mut(&agent).unwrap().remove(&key); },
                // Failed deletion retains every fence in memory and on disk.
                Err(reason) => self.store_error(&home, now, &reason),
            }
        }
        self.sinks.retain(|_, leases| !leases.is_empty());
        self.sink_count = self.sinks.values().map(HashMap::len).sum();
    }

    fn make_turn_room(&mut self, key: &Key) -> bool {
        if self.turns.contains_key(key) { return true; }
        let agent_full = self.turns.keys().filter(|k| k.0 == key.0).count() >= MAX_AGENT_TURNS;
        if !agent_full && self.turns.len() < MAX_TURNS { return true; }
        let candidate = self.turns.iter().filter(|(k, turn)| {
            (!agent_full || k.0 == key.0) && turn.sources.is_empty() && turn.pending.is_empty()
                && !self.sinks.get(&k.0).is_some_and(|leases| leases.keys().any(|lease| lease.0 == k.1))
        }).min_by_key(|(_, turn)| turn.created).map(|(k, _)| k.clone());
        if let Some(candidate) = candidate {
            // Registration already left a tombstone; re-registration can never
            // make an evicted turn clean again in this gateway process.
            self.turns.remove(&candidate);
            true
        } else { false }
    }

    fn note_registration(&mut self, key: &Key, _now: Instant) -> bool {
        if !self.seen.contains_key(&key.0) && self.seen.len() >= MAX_REGISTRATION_AGENTS {
            // No history slot: never claim a turn is clean. Existing agents'
            // filters stay intact, and private reads/sinks retain normal policy.
            return true;
        }
        let seen = self.seen.entry(key.0.clone()).or_insert_with(|| vec![0; REGISTRATION_WORDS]);
        let mut was_seen = true;
        for salt in 0..3u8 {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            (salt, &key.1).hash(&mut hash);
            let bit = (hash.finish() as usize) % (seen.len() * 64);
            let mask = 1u64 << (bit % 64);
            was_seen &= seen[bit / 64] & mask != 0;
            seen[bit / 64] |= mask;
        }
        was_seen
    }

    fn register(&mut self, key: Key, now: Instant) {
        self.prune(now);
        let seen = self.note_registration(&key, now);
        if self.make_turn_room(&key) {
            self.turns.entry(key).or_insert(Turn {
                created: now,
                started_at: duduclaw_memory::format_ts(chrono::Utc::now()),
                pending: HashMap::new(),
                connector_refs: if seen { BTreeSet::from(["unknown".into()]) } else { BTreeSet::new() },
                dropped_connector_refs: 0,
                sources: if seen {
                    BTreeSet::from(["turn_state_unavailable".into()])
                } else {
                    BTreeSet::new()
                },
            });
        }
    }

    fn query(&mut self, key: &Key, now: Instant) -> Taint {
        self.prune(now);
        match self.turns.get(key) {
            Some(turn) => Taint {
                connector_refs: turn.connector_refs.iter().cloned().collect(),
                dropped_connector_refs: turn.dropped_connector_refs,
                turn_started_at: Some(turn.started_at.clone()),
                tainted: !turn.sources.is_empty() || !turn.pending.is_empty(),
                sources: turn.sources.iter().cloned()
                    .chain((!turn.pending.is_empty()).then(|| "pending_private_read".into()))
                    .collect(),
            },
            None => Taint::unknown(),
        }
    }

    fn reserve(&mut self, key: &Key, id: &str, source: &str, now: Instant) -> Taint {
        let before = self.query(key, now);
        if let Some(turn) = self.turns.get_mut(key) {
            if turn.pending.len() >= MAX_SOURCES || turn.pending.contains_key(id) {
                turn.sources.insert("turn_state_unavailable".into());
            } else {
                turn.pending.insert(id.into(), source.into());
            }
        }
        before
    }

    fn reserve_with_policy(&mut self, home: &Path, key: &Key, id: &str, source: &str, now: Instant) -> Result<Taint, String> {
        let readable = self.ensure_store(home, now);
        self.prune(now);
        let mode = crate::egress_gate::mode(home, &key.0)?;
        // Check readiness after prune too: a failed expiry deletion fences all.
        let readable = readable && self.stores.get(home).is_some_and(|s| s.store.is_some());
        if !readable {
            if mode >= duduclaw_core::egress_guard::Mode::Ask { return Err(LEASE_STORE_UNAVAILABLE.into()); }
            if mode == duduclaw_core::egress_guard::Mode::Shadow {
                duduclaw_security::audit::append_tool_call_with_extras(
                    home, &key.0, source, LEASE_STORE_UNAVAILABLE, true,
                    &[("egress_guard", serde_json::json!("would_refuse_lease_store_unavailable"))],
                );
            }
        }
        if self.sinks.contains_key(&key.0) {
            if mode >= duduclaw_core::egress_guard::Mode::Ask {
                return Err(READ_DURING_SINK.into());
            }
            if mode == duduclaw_core::egress_guard::Mode::Shadow {
                duduclaw_security::audit::append_tool_call_with_extras(
                    home, &key.0, source, READ_DURING_SINK, true,
                    &[("egress_guard", serde_json::json!("would_refuse_read_during_sink"))],
                );
            }
        }
        if !self.turns.contains_key(key) {
            return Err(UNTRACKED_TURN.into());
        }
        Ok(self.reserve(key, id, source, now))
    }

    // Check taint and establish the fence under the same gateway mutex. Only
    // this call's own already-reserved read is excluded from the snapshot.
    fn acquire_sink(&mut self, key: &Key, id: &str, own_read: Option<&str>, now: Instant) -> Result<Taint, String> {
        self.prune(now);
        // Capacity pressure refuses new leases; it never evicts an active fence.
        if self.sink_count >= MAX_TOTAL_SINK_LEASES {
            return Err("外送防護紀錄已滿，請稍後重試。".into());
        }
        // SQLite lease IDs are globally unique. Reject caller reuse before an
        // INSERT constraint failure could mark the store unavailable for others.
        if self.sinks.values().any(|leases| leases.keys().any(|(_, held)| held == id)) {
            return Err("對外動作識別碼已使用，請重新發起呼叫。".into());
        }
        let turn = self.turns.get(key);
        if own_read.is_some_and(|read| !turn.is_some_and(|turn| turn.pending.contains_key(read))) {
            return Err("私人讀取登記已失效，請重新發起呼叫。".into());
        }
        let lease_key = (key.1.clone(), id.into());
        if self.sinks.get(&key.0).is_some_and(|leases|
            leases.len() >= MAX_SINK_LEASES || leases.contains_key(&lease_key)) {
            return Err("此員工的對外動作過多，請稍後重試。".into());
        }
        let state = match turn {
            Some(turn) => {
                let pending = turn.pending.keys().any(|read| Some(read.as_str()) != own_read);
                Taint {
                    connector_refs: turn.connector_refs.iter().cloned().collect(),
                    dropped_connector_refs: turn.dropped_connector_refs,
                    turn_started_at: Some(turn.started_at.clone()),
                    tainted: !turn.sources.is_empty() || pending,
                    sources: turn.sources.iter().cloned()
                        .chain(pending.then(|| "pending_private_read".into())).collect(),
                }
            }
            None => Taint::unknown(),
        };
        self.sinks.entry(key.0.clone()).or_default().insert(lease_key, Sink { acquired: now, home: None, restored: false });
        self.sink_count += 1;
        Ok(state)
    }

    fn acquire_persisted_sink(&mut self, home: &Path, key: &Key, id: &str, own_read: Option<&str>, now: Instant, wall: i64) -> Result<Taint, String> {
        if !self.ensure_store(home, now) { return Err(LEASE_STORE_UNAVAILABLE.into()); }
        let taint = self.acquire_sink(key, id, own_read, now)?;
        let lease_key = (key.1.clone(), id.into());
        let result = self.stores.get_mut(home).and_then(|s| s.store.as_mut())
            .ok_or_else(|| LEASE_STORE_UNAVAILABLE.to_string())
            .and_then(|store| store.insert(&key.0, &key.1, id, wall));
        if let Err(reason) = result {
            // Nothing has been granted. A commit with an uncertain outcome is
            // recovered from disk before ANY further read can be admitted.
            self.sinks.get_mut(&key.0).unwrap().remove(&lease_key);
            if self.sinks[&key.0].is_empty() { self.sinks.remove(&key.0); }
            self.sink_count -= 1;
            self.store_error(home, now, &reason);
            return Err(LEASE_STORE_UNAVAILABLE.into());
        }
        self.sinks.get_mut(&key.0).unwrap().get_mut(&lease_key).unwrap().home = Some(home.into());
        Ok(taint)
    }

    fn release_sink(&mut self, key: &Key, id: &str, now: Instant) -> Taint {
        self.prune(now);
        let lease_key = (key.1.clone(), id.into());
        if self.sinks.get(&key.0).and_then(|leases| leases.get(&lease_key)).is_some_and(|lease| lease.restored)
            || !self.delete_lease(&key.0, &lease_key, now) {
            return self.query(key, now);
        }
        if let Some(leases) = self.sinks.get_mut(&key.0) {
            if leases.remove(&lease_key).is_some() { self.sink_count -= 1; }
            if leases.is_empty() { self.sinks.remove(&key.0); }
        }
        self.query(key, now)
    }

    fn finish(&mut self, key: &Key, id: &str, payload: bool, now: Instant) -> Taint {
        self.prune(now);
        let source = self.turns.get_mut(key).and_then(|turn| turn.pending.remove(id));
        match source {
            Some(source) if payload => self.mark(key, &source, now),
            Some(_) => self.query(key, now),
            // Missing, expired or already completed reservations never clear taint.
            None => self.mark(key, "turn_state_unavailable", now),
        }
    }

    fn stamp(&mut self, key: &Key, connector: Option<&str>, rejected: bool, now: Instant) {
        self.prune(now);
        if let Some(turn) = self.turns.get_mut(key) {
            if rejected {
                turn.dropped_connector_refs = turn.dropped_connector_refs.saturating_add(1);
                turn.connector_refs.insert("unknown".into());
            }
            if let Some(reference) = connector {
                if turn.connector_refs.len() < MAX_SOURCES - 1 {
                    turn.connector_refs.insert(reference.into());
                } else {
                    turn.connector_refs.insert("unknown".into());
                }
            }
        }
    }

    fn mark(&mut self, key: &Key, source: &str, now: Instant) -> Taint {
        self.prune(now);
        self.note_registration(key, now);
        if !self.turns.contains_key(key) && self.make_turn_room(key) {
            self.turns.insert(
                key.clone(),
                Turn {
                    created: now,
                    started_at: duduclaw_memory::format_ts(chrono::Utc::now()),
                    pending: HashMap::new(),
                    connector_refs: BTreeSet::from(["unknown".into()]),
                    dropped_connector_refs: 0,
                    sources: BTreeSet::from(["turn_state_unavailable".into()]),
                },
            );
        }
        if let Some(turn) = self.turns.get_mut(key) {
            if turn.sources.len() < MAX_SOURCES {
                turn.sources.insert(source.into());
            } else {
                turn.sources.insert("additional_private_sources".into());
            }
        }
        self.query(key, now)
    }
}

fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(Ledger::default()))
}

/// Called only by the gateway when registering a live runtime turn.
/// Re-registration cannot clear previously recorded sources.
pub(crate) fn register_turn(agent: &str, turn: &str) {
    if let Ok(mut ledger) = ledger().lock() {
        let key = (agent.into(), turn.into());
        ledger.register(key.clone(), Instant::now());
        if let Ok(Some(session)) = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone) {
            if let Some(reference) = channel_connector(&session) {
                ledger.stamp(&key, Some(&reference), false, Instant::now());
            }
        }
    }
}

/// Direct gateway callers use the same ledger without a loopback round trip.
pub(crate) fn query_turn(agent: &str, turn: Option<&str>) -> Taint {
    let Some(turn) = turn.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t)) else {
        return Taint::unknown();
    };
    ledger()
        .lock()
        .map(|mut ledger| ledger.query(&(agent.into(), turn.into()), Instant::now()))
        .unwrap_or_else(|_| Taint::unknown())
}

/// Record a completed private read from a gateway-owned tool transport.
pub(crate) fn mark_turn(agent: &str, turn: Option<&str>, tool: &str) -> Taint {
    let Some(turn) = turn.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t)) else {
        return Taint {
            tainted: true,
            sources: vec!["turn_state_unavailable".into(), tool.into()],
            connector_refs: vec!["unknown".into()],
            dropped_connector_refs: 0,
            turn_started_at: None,
        };
    };
    if !valid_source(tool) {
        return Taint::unknown();
    }
    ledger()
        .lock()
        .map(|mut ledger| ledger.mark(&(agent.into(), turn.into()), tool, Instant::now()))
        .unwrap_or_else(|_| Taint::unknown())
}

pub(crate) fn channel_connector(session: &str) -> Option<String> {
    let (kind, _) = session.split_once(':')?;
    if !matches!(kind, "line" | "telegram" | "discord" | "slack" | "googlechat") { return None; }
    match session.split_once("#agent:") {
        Some((_, agent)) if duduclaw_core::is_valid_agent_id(agent) => Some(format!("channel:{kind}:{agent}")),
        Some(_) => None,
        None => Some(format!("channel:{kind}")),
    }
}

/// A reservation survives cancellation; only an explicit payload-less completion releases it.
pub(crate) fn reserve_turn(home: &Path, agent: &str, turn: Option<&str>, source: &str, id: &str) -> Result<Taint, String> {
    // Missing/invalid turns still pass the per-agent fence before admission.
    let turn = turn.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t)).unwrap_or("");
    if !valid_source(source) { return Err("invalid private source".into()); }
    let mut ledger = ledger().lock().map_err(|_| "ledger unavailable")?;
    ledger.reserve_with_policy(home, &(agent.into(), turn.into()), id, source, Instant::now())
}

pub(crate) fn finish_turn(agent: &str, turn: Option<&str>, id: &str, payload: bool) -> Result<(), String> {
    let turn = turn.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t))
        .ok_or(UNTRACKED_TURN)?;
    let mut ledger = ledger().lock().map_err(|_| "ledger unavailable")?;
    ledger.finish(&(agent.into(), turn.into()), id, payload, Instant::now());
    Ok(())
}

pub(crate) fn acquire_sink_turn(home: &Path, agent: &str, turn: Option<&str>, id: &str, own_read: Option<&str>) -> Result<Taint, String> {
    if turn.is_some_and(|t| !crate::computer_use_sessions::turns::valid_turn_id(t)) {
        return Err("invalid turn identity".into());
    }
    let turn = turn.unwrap_or("");
    ledger().lock().map_err(|_| "ledger unavailable")?
        .acquire_persisted_sink(home, &(agent.into(), turn.into()), id, own_read, Instant::now(), chrono::Utc::now().timestamp_millis())
}

pub(crate) fn release_sink_turn(agent: &str, turn: &str, id: &str) {
    if let Ok(mut ledger) = ledger().lock() {
        ledger.release_sink(&(agent.into(), turn.into()), id, Instant::now());
    }
}

pub struct RouteState {
    home: PathBuf,
    replay: ReplayGuard,
    rate: RateLimiter,
    hook_rate: RateLimiter,
    blocking: Arc<Semaphore>,
}

impl RouteState {
    pub fn new(home: PathBuf) -> Self {
        // Restore synchronously before the router is published. Direct callers
        // also ensure readiness under this mutex before admitting any read/sink;
        // no fixed startup window or timing assumption is needed.
        if let Ok(mut ledger) = ledger().lock() {
            ledger.ensure_store(&home, Instant::now());
        }
        Self {
            home,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
            hook_rate: RateLimiter::default(),
            blocking: Arc::new(Semaphore::new(MAX_BLOCKING_WORK)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: String,
    turn_id: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    reservation: Option<String>,
    #[serde(default)]
    payload: Option<bool>,
    #[serde(default)]
    own_read: Option<String>,
    #[serde(default)]
    connector_ref: Option<String>,
    #[serde(default)]
    remote_server: Option<String>,
    #[serde(default)]
    stamp_only: bool,
}

pub fn router(state: Arc<RouteState>) -> Router {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(STORE_RETRY);
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else { break; };
            let Ok(permit) = state.blocking.clone().try_acquire_owned() else { continue; };
            // Serialize with requests and direct callers without blocking a
            // Tokio worker on the mutex, SQLite busy timeout or FULL commit.
            if let Err(error) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                if let Ok(mut ledger) = ledger().lock() {
                    let now = Instant::now();
                    ledger.ensure_store(&state.home, now);
                    ledger.prune(now);
                }
            }).await {
                tracing::error!(%error, "egress lease maintenance task failed");
            }
        }
    });
    Router::new()
        .route(MARK_ROUTE, post(mark))
        .route(QUERY_ROUTE, post(query))
        .route(RECIPIENTS_ROUTE, post(recipients))
        .route(RESERVE_ROUTE, post(reserve))
        .route(FINISH_ROUTE, post(finish))
        .route(ACQUIRE_SINK_ROUTE, post(acquire_sink))
        .route(RELEASE_SINK_ROUTE, post(release_sink))
        .layer(DefaultBodyLimit::max(4096))
        .route(HOOK_CHECK_ROUTE, post(hook::check).layer(DefaultBodyLimit::max(128 * 1024)))
        .with_state(state)
}

async fn mark(
    State(state): State<Arc<RouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_async(state, peer, headers, body, "mark").await
}

async fn query(
    State(state): State<Arc<RouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_async(state, peer, headers, body, "query").await
}

async fn recipients(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    handle_async(state, peer, headers, body, "recipients").await
}

async fn reserve(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    handle_async(state, peer, headers, body, "reserve").await
}

async fn finish(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    handle_async(state, peer, headers, body, "finish").await
}

async fn acquire_sink(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    handle_async(state, peer, headers, body, "acquire_sink").await
}

async fn release_sink(State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Bytes) -> Response {
    handle_async(state, peer, headers, body, "release_sink").await
}

async fn blocking_response(blocking: Arc<Semaphore>, work: impl FnOnce() -> Response + Send + 'static) -> Response {
    let Ok(permit) = blocking.try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // A cancelled waiter cannot undo admission: an already running blocking
    // task completes and leaves its reservation/fence intact. A failed task
    // never yields an authenticated success response.
    tokio::task::spawn_blocking(move || {
        // Retain capacity even if the async waiter is cancelled while queued
        // or running. Only completion of the blocking work releases it.
        let _permit = permit;
        work()
    }).await
        .unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())
}

async fn handle_async(
    state: Arc<RouteState>,
    peer: SocketAddr,
    headers: HeaderMap,
    body: Bytes,
    op: &'static str,
) -> Response {
    if check_peer(peer).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    blocking_response(state.blocking.clone(), move || handle(&state, peer, &headers, &body, op)).await
}

fn handle(
    state: &RouteState,
    peer: SocketAddr,
    headers: &HeaderMap,
    body: &[u8],
    op: &str,
) -> Response {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs())
        .unwrap_or(0);
    let Ok(caller) = authenticate(&state.home, peer, headers, body, unix) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) || !state.replay.record(&caller.nonce, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(request) = serde_json::from_slice::<Request>(body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if request.op != op || !(crate::computer_use_sessions::turns::valid_turn_id(&request.turn_id)
        || (matches!(op, "recipients" | "reserve" | "acquire_sink" | "release_sink") && request.turn_id.is_empty())) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let valid = match op {
        "query" | "recipients" => request.source.is_none() && request.reservation.is_none() && request.payload.is_none(),
        "mark" => request.source.as_deref().is_some_and(valid_source) && request.reservation.is_none() && request.payload.is_none(),
        "reserve" => request.source.as_deref().is_some_and(valid_source) && request.payload.is_none(),
        "finish" => request.source.is_none() && request.payload.is_some(),
        "acquire_sink" | "release_sink" => request.source.is_none() && request.payload.is_none(),
        _ => false,
    };
    if !valid || ((request.stamp_only || request.connector_ref.is_some() || request.remote_server.is_some()) && op != "mark")
        || (request.connector_ref.is_some() && request.remote_server.is_some()) || (request.own_read.is_some() && (op != "acquire_sink" || !request.own_read.as_deref().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())))
        || (matches!(op, "reserve" | "finish" | "acquire_sink" | "release_sink") && !request.reservation.as_deref()
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    // Select the key that authenticated this request, including a valid
    // previous key during rotation. The response binds its fresh nonce.
    let Some(identity_key) = duduclaw_core::load_identity_key(&state.home) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let token = duduclaw_core::mint_identity_token(&identity_key, &caller.agent_id);
    let timestamp = headers
        .get(TIMESTAMP_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(key) = crate::mcp_internal_key::valid_internal_keys(&state.home)
        .into_iter()
        .find(|key| {
            duduclaw_core::verify_internal_request_signature(
                key.as_bytes(),
                &caller.agent_id,
                &token,
                timestamp,
                &caller.nonce,
                body,
                signature,
            )
        })
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let value = if op == "recipients" {
        match crate::recipient_guard::context(&state.home, &caller.agent_id, &request.turn_id) {
            Ok(context) => serde_json::json!(context),
            Err(error) => serde_json::json!({"error": error}),
        }
    } else {
        let remote_connector = request.remote_server.as_deref().filter(|_| op == "mark")
            .and_then(|server| connectors::remote_reference(&state.home, &caller.agent_id, server));
        let connector = request.connector_ref.as_deref().filter(|reference|
            op == "mark" && connectors::declared_allowed(&state.home, &caller.agent_id, reference)).or(remote_connector.as_deref());
        let rejected_connector = (request.connector_ref.is_some() || request.remote_server.is_some()) && connector.is_none();
        let turn_key = (caller.agent_id.clone(), request.turn_id.clone());
        let result = match ledger().lock() {
            Ok(mut ledger) => {
                let now = Instant::now();
                match op {
                    "reserve" => ledger.reserve_with_policy(&state.home, &turn_key, request.reservation.as_deref().unwrap(), request.source.as_deref().unwrap(), now),
                    "acquire_sink" => ledger.acquire_persisted_sink(&state.home, &turn_key, request.reservation.as_deref().unwrap(), request.own_read.as_deref(), now, chrono::Utc::now().timestamp_millis()),
                    "release_sink" => Ok(ledger.release_sink(&turn_key, request.reservation.as_deref().unwrap(), now)),
                    "finish" => Ok(ledger.finish(&turn_key, request.reservation.as_deref().unwrap(), request.payload.unwrap(), now)),
                    "mark" => {
                        if !request.stamp_only { ledger.mark(&turn_key, request.source.as_deref().unwrap(), now); }
                        ledger.stamp(&turn_key, connector, rejected_connector, now);
                        Ok(ledger.query(&turn_key, now))
                    },
                    _ => Ok(ledger.query(&turn_key, now)),
                }
            },
            Err(_) => Err("ledger unavailable".into()),
        };
        // Refusals are authenticated too: never trust an unsigned error message.
        match result {
            Ok(taint) => serde_json::json!(taint),
            Err(error) => serde_json::json!({"error": error}),
        }
    };
    let bytes = serde_json::to_vec(&value).expect("egress response serialization");
    let response_sig = duduclaw_core::internal_request_signature(
        key.as_bytes(),
        &caller.agent_id,
        &token,
        duduclaw_core::egress_guard::RESPONSE_DOMAIN,
        &caller.nonce,
        &bytes,
    );
    (
        [
            ("content-type", "application/json"),
            (
                duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER,
                response_sig.as_str(),
            ),
        ],
        bytes,
    )
        .into_response()
}

fn valid_source(source: &str) -> bool {
    !source.is_empty() && source.len() <= 256 && !source.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_stamps_do_not_change_taint_or_cross_employees() {
        let now = Instant::now();
        let mut ledger = Ledger::default();
        let alice = ("alice".into(), "turn".into());
        let bob = ("bob".into(), "turn".into());
        ledger.register(alice.clone(), now);
        ledger.register(bob.clone(), now);
        ledger.stamp(&alice, Some("google:person:owner"), false, now);
        let state = ledger.query(&alice, now);
        assert!(!state.tainted);
        assert!(state.sources.is_empty());
        assert_eq!(state.connector_refs, ["google:person:owner"]);
        assert!(ledger.query(&bob, now).connector_refs.is_empty());
        ledger.stamp(&alice, None, true, now);
        let state = ledger.query(&alice, now);
        assert_eq!(state.dropped_connector_refs, 1);
        assert!(state.connector_refs.iter().any(|r| r == "unknown"));
        assert!(!state.tainted);
        assert_eq!(ledger.query(&alice, now + TTL).connector_refs, ["unknown"]);
    }

    #[test]
    fn channel_stamps_keep_global_and_employee_bots_distinct() {
        assert_eq!(channel_connector("telegram:123:0"), Some("channel:telegram".into()));
        assert_eq!(channel_connector("telegram:123:0#agent:alice"), Some("channel:telegram:alice".into()));
        assert_eq!(channel_connector("googlechat:spaces/xyz#agent:alice"), Some("channel:googlechat:alice".into()));
        assert_eq!(channel_connector("telegram:123#agent:../bob"), None);
        assert_eq!(channel_connector("runtime:alice:turn"), None);
    }

    fn policy_home(mode: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        std::fs::write(home.path().join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
        home
    }

    #[test]
    fn egress_persisted_lease_survives_restart_and_old_release_cannot_clear_it() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let key = ("bot".into(), "before-crash".into());
        let mut old = Ledger::default();
        old.register(key.clone(), now);
        old.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap();
        drop(old);
        let mut restored = Ledger::default();
        assert!(restored.ensure_store(home.path(), now));
        for turn in ["", "forged", "before-crash"] {
            assert_eq!(restored.reserve_with_policy(home.path(), &("bot".into(), turn.into()), "read", "memory_read", now).unwrap_err(), READ_DURING_SINK);
        }
        restored.release_sink(&key, "lease", now);
        assert_eq!(restored.sink_count, 1);
        assert_eq!(restored.reserve_with_policy(home.path(), &("other".into(), "".into()), "read", "memory_read", now).unwrap_err(), UNTRACKED_TURN);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                assert_eq!(std::fs::metadata(home.path().join(format!("egress_leases.db{suffix}"))).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
    }

    #[test]
    fn egress_route_state_restores_before_first_request() {
        let home = policy_home("block");
        let agent = format!("startup-{}", uuid::Uuid::new_v4());
        let wall = chrono::Utc::now().timestamp_millis();
        let mut store = lease_store::Store::open(home.path()).unwrap();
        store.insert(&agent, "old-turn", "old-lease", wall).unwrap();
        drop(store);
        // No router task or request has run: constructing state must restore.
        let state = RouteState::new(home.path().into());
        {
            let ledger = ledger().lock().unwrap();
            assert!(ledger.stores[&state.home].store.is_some());
            assert!(ledger.sinks[&agent].values().all(|lease| lease.restored));
            assert_eq!(ledger.sinks[&agent].len(), 1);
        }
        assert_eq!(reserve_turn(home.path(), &agent, None, "memory_read", "read").unwrap_err(), READ_DURING_SINK);
    }

    #[test]
    fn egress_second_owner_fails_closed_then_restores_before_admission() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let key = ("bot".into(), "T".into());
        let mut first = Ledger::default();
        let mut second = Ledger::default();
        assert!(first.ensure_store(home.path(), now));
        assert!(!second.ensure_store(home.path(), now));
        first.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap();
        for agent in ["bot", "other"] {
            assert_eq!(second.reserve_with_policy(home.path(), &(agent.into(), "".into()), "r", "memory_read", now).unwrap_err(), LEASE_STORE_UNAVAILABLE);
        }
        drop(first);
        assert!(second.ensure_store(home.path(), now + STORE_RETRY));
        assert_eq!(second.reserve_with_policy(home.path(), &("bot".into(), "".into()), "r", "memory_read", now + STORE_RETRY).unwrap_err(), READ_DURING_SINK);
        assert_eq!(second.reserve_with_policy(home.path(), &("other".into(), "".into()), "r", "memory_read", now + STORE_RETRY).unwrap_err(), UNTRACKED_TURN);
    }

    #[test]
    fn egress_duplicate_lease_id_does_not_fence_other_agents() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let mut ledger = Ledger::default();
        let key = ("bot".into(), "T".into());
        ledger.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap();
        for key in [("bot".into(), "T2".into()), ("other".into(), "T".into())] {
            assert!(ledger.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).is_err());
            assert!(ledger.stores[home.path()].store.is_some());
        }
        assert_eq!(ledger.sink_count, 1);
        let other = ("other".into(), "read-turn".into());
        ledger.register(other.clone(), now);
        assert!(ledger.reserve_with_policy(home.path(), &other, "read", "memory_read", now).is_ok());
    }

    #[test]
    fn egress_restore_ignores_wall_clock_age_and_restarts_full_monotonic_fence() {
        // Ancient, current and future timestamps cover forward/backward jumps.
        // Restore deliberately takes no wall-clock input.
        for acquired_at in [0, chrono::Utc::now().timestamp_millis(), i64::MAX] {
            let home = policy_home("ask");
            let now = Instant::now();
            let key = ("bot".into(), "clock".into());
            {
                let mut store = lease_store::Store::open(home.path()).unwrap();
                store.insert("bot", "clock", "lease", acquired_at).unwrap();
            }
            let mut restored = Ledger::default();
            assert!(restored.ensure_store(home.path(), now));
            restored.prune(now + TTL - Duration::from_nanos(1));
            assert_eq!(restored.sink_count, 1);
            drop(restored);
            // Another restart renews the full fence, regardless of row age.
            let restart = now + TTL;
            let mut restored = Ledger::default();
            assert!(restored.ensure_store(home.path(), restart));
            restored.release_sink(&key, "lease", restart);
            restored.prune(restart + TTL - Duration::from_nanos(1));
            assert_eq!(restored.sink_count, 1);
            let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
            assert_eq!(conn.query_row("SELECT acquired_at FROM sink_leases", [], |r| r.get::<_, i64>(0)).unwrap(), acquired_at);
            restored.prune(restart + TTL);
            assert_eq!(restored.sink_count, 0);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM sink_leases", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
    }

    #[test]
    fn egress_unreadable_store_fences_all_turns_and_recovers() {
        for mode in ["ask", "block", "shadow"] {
            let home = policy_home(mode);
            let path = home.path().join("egress_leases.db");
            std::fs::create_dir(&path).unwrap();
            let now = Instant::now();
            let mut ledger = Ledger::default();
            for agent in ["bot", "other"] {
                let key = (agent.into(), "tracked".into());
                ledger.register(key.clone(), now);
                let result = ledger.reserve_with_policy(home.path(), &key, "r", "memory_read", now);
                if mode == "shadow" { assert!(result.is_ok()); }
                else { assert_eq!(result.unwrap_err(), LEASE_STORE_UNAVAILABLE); }
            }
            assert!(ledger.acquire_persisted_sink(home.path(), &("bot".into(), "".into()), "s", None, now, 100_000).is_err());
            std::fs::remove_dir(&path).unwrap();
            assert!(ledger.ensure_store(home.path(), now + STORE_RETRY));
            let key = ("bot".into(), "tracked".into());
            assert!(ledger.reserve_with_policy(home.path(), &key, "r2", "memory_read", now + STORE_RETRY).is_ok());
            if mode == "shadow" {
                let log = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
                assert!(log.contains("would_refuse_lease_store_unavailable"));
            }
        }
    }

    #[test]
    fn egress_unreadable_existing_store_restores_lease_before_reopening_reads() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let key = ("bot".into(), "old".into());
        let mut old = Ledger::default();
        old.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap();
        drop(old);
        let path = home.path().join("egress_leases.db");
        let saved = home.path().join("saved.db");
        std::fs::rename(&path, &saved).unwrap();
        std::fs::write(&path, "not a sqlite database").unwrap();
        let mut restored = Ledger::default();
        assert!(!restored.ensure_store(home.path(), now));
        for agent in ["bot", "other"] {
            assert_eq!(restored.reserve_with_policy(home.path(), &(agent.into(), "".into()), "r", "memory_read", now).unwrap_err(), LEASE_STORE_UNAVAILABLE);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&saved, &path).unwrap();
        assert!(restored.ensure_store(home.path(), now + STORE_RETRY));
        assert_eq!(restored.reserve_with_policy(home.path(), &("bot".into(), "".into()), "r", "memory_read", now + STORE_RETRY).unwrap_err(), READ_DURING_SINK);
        assert_eq!(restored.reserve_with_policy(home.path(), &("other".into(), "".into()), "r", "memory_read", now + STORE_RETRY).unwrap_err(), UNTRACKED_TURN);
    }

    #[test]
    fn egress_expiry_batch_failure_retains_all_fences_then_recovers() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let key = ("bot".into(), "T".into());
        let mut ledger = Ledger::default();
        for id in ["one", "two"] { ledger.acquire_persisted_sink(home.path(), &key, id, None, now, wall).unwrap(); }
        let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_delete BEFORE DELETE ON sink_leases WHEN OLD.lease_id='two' BEGIN SELECT RAISE(FAIL, 'disk write failed'); END;").unwrap();
        ledger.prune(now + TTL);
        assert_eq!(ledger.sink_count, 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM sink_leases", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        conn.execute_batch("DROP TRIGGER refuse_delete;").unwrap();
        assert!(ledger.ensure_store(home.path(), now + TTL + STORE_RETRY));
        ledger.prune(now + TTL + STORE_RETRY);
        assert_eq!(ledger.sink_count, 0);
    }

    #[test]
    fn egress_unreadable_schema_and_invalid_rows_fail_closed() {
        for setup in [
            "PRAGMA user_version=2;",
            "DROP TABLE sink_leases;",
            "INSERT INTO sink_leases VALUES ('lease', 'bot', 'T', 'Dispatched', 'invalid-time');",
        ] {
            let home = policy_home("block");
            drop(lease_store::Store::open(home.path()).unwrap());
            let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
            conn.execute_batch(setup).unwrap();
            let now = Instant::now();
            let mut ledger = Ledger::default();
            ledger.register(("bot".into(), "T".into()), now);
            assert_eq!(ledger.reserve_with_policy(home.path(), &("bot".into(), "T".into()), "r", "memory_read", now).unwrap_err(), LEASE_STORE_UNAVAILABLE);
        }
    }

    #[test]
    fn egress_insert_failure_refuses_block_sink_and_shadow_audits() {
        for mode in ["block", "shadow"] {
            let home = policy_home(mode);
            let now = Instant::now();
            let wall = chrono::Utc::now().timestamp_millis();
            let mut ledger = Ledger::default();
            assert!(ledger.ensure_store(home.path(), now));
            let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
            conn.execute_batch("CREATE TRIGGER refuse_insert BEFORE INSERT ON sink_leases BEGIN SELECT RAISE(FAIL, 'disk write failed'); END;").unwrap();
            let key = ("bot".into(), "T".into());
            ledger.register(key.clone(), now);
            let reason = ledger.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap_err();
            assert_eq!(reason, LEASE_STORE_UNAVAILABLE);
            let result = crate::egress_client::reservation_failure(home.path(), "bot", "web_fetch", &reason);
            assert_eq!(result.is_err(), mode == "block");
            assert_eq!(ledger.sink_count, 0);
            assert!(std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap().contains("reservation_failed"));
        }
    }

    #[test]
    fn egress_delete_failure_keeps_fence_and_recovery_preserves_live_release() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let key = ("bot".into(), "T".into());
        let mut ledger = Ledger::default();
        ledger.acquire_persisted_sink(home.path(), &key, "lease", None, now, wall).unwrap();
        let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_delete BEFORE DELETE ON sink_leases BEGIN SELECT RAISE(FAIL, 'disk write failed'); END;").unwrap();
        ledger.release_sink(&key, "lease", now);
        assert_eq!(ledger.sink_count, 1);
        assert_eq!(ledger.reserve_with_policy(home.path(), &key, "r", "memory_read", now).unwrap_err(), LEASE_STORE_UNAVAILABLE);
        conn.execute_batch("DROP TRIGGER refuse_delete;").unwrap();
        assert!(ledger.ensure_store(home.path(), now + STORE_RETRY));
        ledger.release_sink(&key, "lease", now + STORE_RETRY);
        assert_eq!(ledger.sink_count, 0);
    }

    #[test]
    fn egress_per_agent_quota_evicts_only_clean_turns_and_other_agent_is_unaffected() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        for i in 0..MAX_AGENT_TURNS {
            let key = ("bot".into(), i.to_string());
            ledger.note_registration(&key, now);
            ledger.turns.insert(key, Turn { created: now, started_at: duduclaw_memory::format_ts(chrono::Utc::now()), connector_refs: BTreeSet::new(), dropped_connector_refs: 0, sources: BTreeSet::from(["private".into()]), pending: HashMap::new() });
        }
        let refused = ("bot".into(), "over-quota".into());
        ledger.register(refused.clone(), now);
        assert!(!ledger.turns.contains_key(&refused));
        let other = ("other".into(), "T".into());
        ledger.register(other.clone(), now);
        assert!(!ledger.query(&other, now).tainted);
        assert!(ledger.acquire_sink(&other, "other-lease", None, now).is_ok());
        let clean = ("bot".into(), "0".into());
        ledger.turns.get_mut(&clean).unwrap().sources.clear();
        ledger.turns.get_mut(&clean).unwrap().pending.insert("pending".into(), "private".into());
        ledger.register(refused.clone(), now);
        assert!(!ledger.turns.contains_key(&refused));
        ledger.turns.get_mut(&clean).unwrap().pending.clear();
        ledger.acquire_sink(&clean, "lease", None, now).unwrap();
        ledger.register(refused.clone(), now);
        assert!(!ledger.turns.contains_key(&refused));
        ledger.release_sink(&clean, "lease", now);
        ledger.register(refused.clone(), now);
        assert!(!ledger.turns.contains_key(&clean));
        assert!(ledger.query(&clean, now).tainted);
        assert!(ledger.query(&refused, now).tainted);
        // Make room and retry the evicted ID: its tombstone forbids clean state.
        ledger.turns.remove(&refused);
        ledger.register(clean.clone(), now);
        assert!(ledger.query(&clean, now).tainted);
        assert_eq!(ledger.turns.keys().filter(|key| key.0 == "bot").count(), MAX_AGENT_TURNS);
    }

    #[test]
    fn egress_store_enforces_cap_and_restores_not_dispatched_rows_as_uncertain() {
        let home = policy_home("block");
        let now = Instant::now();
        let wall = chrono::Utc::now().timestamp_millis();
        let mut store = lease_store::Store::open(home.path()).unwrap();
        for i in 0..MAX_SINK_LEASES { store.insert("bot", "T", &i.to_string(), wall).unwrap(); }
        assert!(store.insert("bot", "T", "overflow", wall).is_err());
        assert!(store.insert("other", "T", "other", wall).is_ok());
        let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
        conn.execute("UPDATE sink_leases SET phase='NotDispatched' WHERE agent_id='bot'", []).unwrap();
        // Simulate a restart with no fixture connection or owner lock alive.
        // The independent connection also owns SQLite/WAL state.
        drop(conn);
        drop(store);
        let mut ledger = Ledger::default();
        assert!(ledger.ensure_store(home.path(), now));
        ledger.release_sink(&("bot".into(), "T".into()), "0", now);
        assert_eq!(ledger.sinks["bot"].len(), MAX_SINK_LEASES);
        assert!(ledger.acquire_persisted_sink(home.path(), &("bot".into(), "T".into()), "new", None, now, wall).is_err());
    }

    #[test]
    fn egress_sink_first_refuses_reads_until_all_leases_release() {
        for mode in ["ask", "block"] {
            let home = policy_home(mode);
            let mut ledger = Ledger::default();
            let now = Instant::now();
            let key = ("bot".into(), "sink-first".into());
            ledger.register(key.clone(), now);
            assert!(!ledger.acquire_sink(&key, "sink1", None, now).unwrap().tainted);
            assert!(!ledger.acquire_sink(&key, "sink2", None, now).unwrap().tainted);
            assert_eq!(ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", now).unwrap_err(), READ_DURING_SINK);
            assert!(ledger.turns[&key].pending.is_empty());
            ledger.release_sink(&key, "sink1", now);
            assert!(ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", now).is_err());
            ledger.release_sink(&key, "sink2", now);
            assert!(!ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", now).unwrap().tainted);
            assert_eq!(ledger.finish(&key, "read", true, now).sources, ["memory_search"]);
        }
    }

    #[test]
    fn egress_shadow_read_during_sink_is_audited_without_refusal() {
        let home = policy_home("shadow");
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("bot".into(), "shadow".into());
        ledger.register(key.clone(), now);
        ledger.acquire_sink(&key, "sink", None, now).unwrap();
        ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", now).unwrap();
        assert_eq!(ledger.finish(&key, "read", true, now).sources, ["memory_search"]);
        let log = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(row["egress_guard"], "would_refuse_read_during_sink");
    }

    #[test]
    fn egress_sink_rechecks_after_approval_and_excludes_only_own_read() {
        let home = policy_home("ask");
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("bot".into(), "approval-wait".into());
        ledger.register(key.clone(), now);
        let before = ledger.reserve_with_policy(home.path(), &key, "own", "crm.send", now).unwrap();
        assert!(!before.tainted);
        // No lease is held while approval is pending. Another process reads.
        ledger.reserve_with_policy(home.path(), &key, "other", "memory_search", now).unwrap();
        ledger.finish(&key, "other", true, now);
        let after = ledger.acquire_sink(&key, "sink", Some("own"), now).unwrap();
        assert_eq!(after.sources, ["memory_search"]);
        assert!(crate::egress_gate::confirm_dispatch(home.path(), "bot", "crm.send", true, after.clone(), None).is_err());
        let approved = serde_json::json!({"sources":["memory_search"]});
        assert!(crate::egress_gate::confirm_dispatch(home.path(), "bot", "crm.send", true, after.clone(), Some(&approved)).is_ok());
        let stale = serde_json::json!({"sources":["wiki_read"]});
        assert!(crate::egress_gate::confirm_dispatch(home.path(), "bot", "crm.send", true, after, Some(&stale)).is_err());
    }

    #[test]
    fn egress_sink_leases_are_bounded_and_expire_from_acquisition() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("bot".into(), "bounded".into());
        ledger.register(key.clone(), now);
        for id in 0..MAX_SINK_LEASES { ledger.acquire_sink(&key, &id.to_string(), None, now).unwrap(); }
        assert!(ledger.acquire_sink(&key, "0", None, now).is_err());
        assert!(ledger.acquire_sink(&key, "extra", None, now).is_err());
        ledger.release_sink(&key, "0", now);
        ledger.acquire_sink(&key, "extra", None, now).unwrap();
        ledger.prune(now + TTL);
        assert!(!ledger.turns.contains_key(&key));
        assert!(ledger.sinks.is_empty());
        assert_eq!(ledger.sink_count, 0);
        assert!(ledger.acquire_sink(&key, "expired", None, now + TTL).unwrap().tainted);
    }

    #[tokio::test]
    async fn egress_two_phase_lease_retains_uncertain_dispatch_until_ttl() {
        let home = policy_home("block");
        let agent = format!("phase-{}", uuid::Uuid::new_v4());
        for dispatched in [false, true] {
            for outcome in ["terminal", "error", "timeout", "cancel", "panic"] {
                let turn = uuid::Uuid::new_v4().to_string();
                register_turn(&agent, &turn);
                let (lease, _) = crate::egress_client::SinkLease::direct(home.path(), &agent, Some(&turn), None).unwrap();
                if dispatched { lease.dispatched(); }
                assert!(reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "read").is_err());
                match outcome {
                    "timeout" => { let _ = tokio::time::timeout(Duration::from_millis(1), async move { let _held = lease; std::future::pending::<()>().await }).await; }
                    "cancel" | "panic" => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        let task = tokio::spawn(async move {
                            let _held = lease;
                            let _ = tx.send(());
                            if outcome == "panic" { panic!("simulated sink panic"); }
                            std::future::pending::<()>().await;
                        });
                        rx.await.unwrap();
                        if outcome == "cancel" { task.abort(); }
                        assert!(task.await.is_err());
                    }
                    "error" => { let _: Result<(), ()> = async move { let _held = lease; Err(()) }.await; }
                    _ => { lease.terminal_response(); lease.release().await; }
                }
                let refused = reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "read").is_err();
                assert_eq!(refused, dispatched && outcome != "terminal", "{dispatched}/{outcome}");
                if refused {
                    let mut ledger = ledger().lock().unwrap();
                    let key = (agent.clone(), turn);
                    assert_eq!(ledger.sinks[&agent].len(), 1);
                    ledger.turns.get_mut(&key).unwrap().created = Instant::now() - TTL;
                    ledger.prune(Instant::now());
                    assert!(!ledger.turns.contains_key(&key));
                    assert_eq!(ledger.sinks[&agent].len(), 1);
                    for acquired in ledger.sinks.get_mut(&agent).unwrap().values_mut() {
                        acquired.acquired = Instant::now() - TTL;
                    }
                    ledger.prune(Instant::now());
                    assert!(!ledger.sinks.contains_key(&agent));
                }
            }
        }
    }

    #[test]
    fn egress_agent_fence_blocks_forged_missing_and_other_turns_only_for_owner() {
        for mode in ["ask", "block"] {
            let home = policy_home(mode);
            let mut ledger = Ledger::default();
            let now = Instant::now();
            let key = ("bot".into(), "T".into());
            let other_turn = ("bot".into(), "T2".into());
            let other_agent = ("another".into(), "T".into());
            ledger.register(key.clone(), now);
            ledger.register(other_turn.clone(), now);
            ledger.register(other_agent.clone(), now);
            ledger.acquire_sink(&key, "sink", None, now).unwrap();
            for turn in ["T", "F", "", "T2"] {
                assert_eq!(ledger.reserve_with_policy(home.path(), &("bot".into(), turn.into()), "r", "memory_search", now).unwrap_err(), READ_DURING_SINK);
            }
            assert!(ledger.reserve_with_policy(home.path(), &other_agent, "r", "memory_search", now).is_ok());
            assert_eq!(ledger.reserve_with_policy(home.path(), &("another".into(), "".into()), "r", "memory_search", now).unwrap_err(), UNTRACKED_TURN);
            // Wrong turn or agent cannot release the owner's lease.
            ledger.release_sink(&other_turn, "sink", now);
            ledger.release_sink(&other_agent, "sink", now);
            assert!(ledger.sinks.contains_key("bot"));
            ledger.release_sink(&key, "sink", now);
            for turn in ["F", ""] {
                let key = ("bot".into(), turn.into());
                let reason = ledger.reserve_with_policy(home.path(), &key, "r", "memory_search", now).unwrap_err();
                assert_eq!(reason, UNTRACKED_TURN);
                crate::egress_client::reservation_failure(home.path(), "bot", "memory_search", &reason).unwrap();
                assert_eq!(ledger.query(&key, now), Taint::unknown());
            }
        }
    }

    #[test]
    fn egress_lease_ttl_is_independent_of_turn_pruning_and_registration() {
        let home = policy_home("block");
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("bot".into(), "old-turn".into());
        ledger.register(key.clone(), now);
        let acquired = now + TTL - Duration::from_secs(1);
        ledger.acquire_sink(&key, "late-sink", None, acquired).unwrap();
        for at in [now + TTL, acquired + TTL - Duration::from_nanos(1)] {
            assert_eq!(ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", at).unwrap_err(), READ_DURING_SINK);
            assert!(!ledger.turns.contains_key(&key));
            assert_eq!(ledger.sink_count, 1);
        }
        assert_eq!(ledger.reserve_with_policy(home.path(), &key, "read", "memory_search", acquired + TTL).unwrap_err(), UNTRACKED_TURN);
        assert_eq!(ledger.sink_count, 0);
    }

    fn assert_unknown_turn(ledger: &mut Ledger, key: &Key, now: Instant) {
        let actual = ledger.query(key, now);
        let expected = Taint {
            turn_started_at: ledger.turns.get(key).map(|turn| turn.started_at.clone()),
            ..Taint::unknown()
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn egress_full_turn_ledger_preserves_fences_without_refusing_other_sinks() {
        let home = policy_home("ask");
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("bot".into(), "held".into());
        ledger.register(key.clone(), now);
        ledger.acquire_sink(&key, "sink", None, now).unwrap();
        // Fill directly so the test does not repeatedly prune a growing ledger.
        for i in 1..MAX_TURNS {
            ledger.turns.insert(("filler".into(), i.to_string()), Turn {
                created: now,
                started_at: duduclaw_memory::format_ts(chrono::Utc::now()),
                connector_refs: BTreeSet::new(), dropped_connector_refs: 0,
                sources: BTreeSet::from(["memory_read".into()]), pending: HashMap::new(),
            });
        }
        let overflow = ("bot".into(), "overflow".into());
        ledger.register(overflow.clone(), now);
        assert!(!ledger.turns.contains_key(&overflow));
        assert_unknown_turn(&mut ledger, &overflow, now);
        for sink_key in [&key, &overflow, &("bot".into(), "".into())] {
            assert!(ledger.acquire_sink(sink_key, "new-sink", None, now).is_ok());
            ledger.release_sink(sink_key, "new-sink", now);
        }
        assert_eq!(ledger.sink_count, 1);
        assert_eq!(ledger.reserve_with_policy(home.path(), &overflow, "r", "memory_search", now).unwrap_err(), READ_DURING_SINK);
        // If capacity returns, retrying the failed registration is still tainted.
        ledger.turns.remove(&("filler".into(), "1".into()));
        ledger.register(overflow.clone(), now);
        assert_unknown_turn(&mut ledger, &overflow, now);
        ledger.release_sink(&key, "sink", now);
        assert_eq!(ledger.sink_count, 0);
    }

    #[test]
    fn egress_untracked_sink_is_tainted_and_still_fences_the_agent() {
        let home = policy_home("ask");
        let mut ledger = Ledger::default();
        let now = Instant::now();
        for turn in ["", "forged"] {
            let key = ("bot".into(), turn.into());
            assert_eq!(ledger.acquire_sink(&key, "sink", None, now).unwrap(), Taint::unknown());
            assert_eq!(ledger.reserve_with_policy(home.path(), &key, "r", "memory_search", now).unwrap_err(), READ_DURING_SINK);
            ledger.release_sink(&key, "sink", now);
            assert_eq!(ledger.reserve_with_policy(home.path(), &key, "r", "memory_search", now).unwrap_err(), UNTRACKED_TURN);
        }
    }

    #[test]
    fn egress_held_reservation_fences_interleaved_sink_and_payload_errors() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("held-agent".into(), "held-turn".into());
        ledger.register(key.clone(), now);
        // Atomic admission returns the pre-call view, excluding itself.
        assert!(!ledger.reserve(&key, "read-1", "memory_read", now).tainted);
        assert!(ledger.query(&key, now).tainted, "another process's sink sees the held read");
        assert!(ledger.reserve(&key, "read-2", "db_query", now).tainted);
        assert!(ledger.finish(&key, "read-2", false, now).tainted);
        assert!(!ledger.finish(&key, "read-1", false, now).tainted);
        ledger.reserve(&key, "read-3", "memory_read", now);
        assert_eq!(ledger.finish(&key, "read-3", true, now).sources, ["memory_read"]);
        assert!(ledger.finish(&key, "read-3", false, now).tainted, "replay cannot clear a source");
        assert!(ledger.query(&key, now + TTL).tainted);
    }

    #[test]
    fn egress_registration_tombstones_never_expire_or_cross_agents() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let early = ("alice".into(), "early".into());
        ledger.register(early.clone(), now);
        ledger.turns.remove(&early); // Simulate clean-turn capacity eviction.
        for at in [now, now + TTL * 2, now + TTL * 10] {
            assert_unknown_turn(&mut ledger, &early, at);
            ledger.register(early.clone(), at);
            assert_unknown_turn(&mut ledger, &early, at);
            ledger.turns.remove(&early);
        }
        let other = ("bob".into(), "early".into());
        ledger.register(other.clone(), now + TTL * 10);
        assert!(!ledger.query(&other, now + TTL * 10).tainted);
    }

    #[test]
    fn egress_tombstone_saturation_only_taints_its_agent_and_keeps_live_sources() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("alice".into(), "collision".into());
        ledger.note_registration(&("alice".into(), "seed".into()), now);
        // Force a collision independently of the hasher's implementation.
        ledger.seen.get_mut("alice").unwrap().fill(u64::MAX);
        let registered = now + TTL - Duration::from_secs(1);
        ledger.register(key.clone(), registered);
        assert_unknown_turn(&mut ledger, &key, registered);
        ledger.mark(&key, "memory_search", registered);
        ledger.register(key.clone(), now + TTL);
        assert_eq!(
            ledger.query(&key, now + TTL).sources,
            ["memory_search", "turn_state_unavailable"]
        );
        let other = ("bob".into(), "collision".into());
        ledger.register(other.clone(), now + TTL);
        assert!(!ledger.query(&other, now + TTL).tainted);
        // Expiring and re-registering the entry still fails closed.
        assert_unknown_turn(&mut ledger, &key, now + TTL * 3);
        ledger.register(key.clone(), now + TTL * 3);
        assert_unknown_turn(&mut ledger, &key, now + TTL * 3);
    }

    #[test]
    fn egress_registration_history_is_bounded_and_overflow_is_tainted() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        for i in 0..MAX_REGISTRATION_AGENTS {
            assert!(!ledger.note_registration(&(format!("agent-{i}"), "T".into()), now));
        }
        let overflow = ("overflow".into(), "T".into());
        ledger.register(overflow.clone(), now);
        assert_unknown_turn(&mut ledger, &overflow, now);
        ledger.register(overflow.clone(), now + TTL * 10);
        assert_unknown_turn(&mut ledger, &overflow, now + TTL * 10);
        assert_eq!(ledger.seen.len(), MAX_REGISTRATION_AGENTS);
        let known = ("agent-0".into(), "fresh".into());
        ledger.register(known.clone(), now + TTL * 10);
        assert!(!ledger.query(&known, now + TTL * 10).tainted);
    }

    #[test]
    fn ledger_is_monotonic_isolated_and_expires_closed() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let key = ("alice".into(), "turn-a".into());
        assert!(ledger.query(&key, now).tainted);
        ledger.register(key.clone(), now);
        assert!(!ledger.query(&key, now).tainted);
        ledger.mark(&key, "memory_search", now);
        ledger.mark(&key, "db_query", now);
        ledger.mark(&key, "memory_search", now);
        ledger.register(key.clone(), now);
        assert_eq!(
            ledger.query(&key, now).sources,
            ["db_query", "memory_search"]
        );
        assert_eq!(
            ledger.query(&("bob".into(), "turn-a".into()), now),
            Taint::unknown()
        );
        assert_unknown_turn(&mut ledger, &key, now + TTL);
        ledger.register(key.clone(), now + TTL);
        assert_unknown_turn(&mut ledger, &key, now + TTL);
    }

    #[test]
    fn sources_are_bounded() {
        assert!(!valid_source(""));
        assert!(!valid_source("tool\nname"));
        assert!(!valid_source(&"a".repeat(257)));
        assert!(valid_source("mcp__third-party__read"));
    }

    #[test]
    fn route_refuses_unsigned_and_nonloopback_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let state = RouteState::new(tmp.path().into());
        let headers = HeaderMap::new();
        for peer in ["127.0.0.1:1234", "192.0.2.1:1234"] {
            assert_eq!(
                handle(&state, peer.parse().unwrap(), &headers, b"{}", "query").status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }

    #[test]
    fn egress_nonloopback_is_rejected_without_spawning() {
        let home = tempfile::tempdir().unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        // No Tokio runtime: reaching spawn_blocking would panic. Even a full
        // semaphore must not turn an unauthenticated remote request into 503.
        let _permits = state.blocking.clone().try_acquire_many_owned(MAX_BLOCKING_WORK as u32).unwrap();
        for peer in ["192.0.2.1:1234", "[2001:db8::1]:1234", "[::ffff:192.0.2.1]:1234"] {
            let mut future = Box::pin(handle_async(state.clone(), peer.parse().unwrap(), HeaderMap::new(), Bytes::from_static(b"{}"), "query"));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let std::task::Poll::Ready(response) = std::future::Future::poll(future.as_mut(), &mut cx) else {
                panic!("remote rejection must be immediate");
            };
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn egress_blocking_capacity_returns_503_and_recovers() {
        let home = policy_home("block");
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        let permits = state.blocking.clone().try_acquire_many_owned(MAX_BLOCKING_WORK as u32).unwrap();
        for peer in ["127.0.0.1:1234", "[::1]:1234", "[::ffff:127.0.0.1]:1234"] {
            for op in ["mark", "query", "reserve", "finish", "acquire_sink", "release_sink"] {
                let response = tokio::time::timeout(Duration::from_millis(100), handle_async(
                    state.clone(), peer.parse().unwrap(), HeaderMap::new(), Bytes::from_static(b"{}"), op,
                )).await.expect("saturation must not queue a waiter");
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{peer} {op}");
            }
        }
        drop(permits);
        let agent = format!("capacity-{}", uuid::Uuid::new_v4());
        register_turn(&agent, "T");
        let id = uuid::Uuid::new_v4().to_string();
        let (headers, body) = signed_request(home.path(), &agent, "T", "acquire_sink", &id);
        let value = response_value(handle_async(state.clone(), "127.0.0.1:1234".parse().unwrap(), headers, body, "acquire_sink").await).await;
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["tainted"], false);
        assert_eq!(state.blocking.available_permits(), MAX_BLOCKING_WORK);
        release_sink_turn(&agent, "T", &id);
    }

    #[tokio::test]
    async fn egress_saturated_gateway_clients_fail_closed() {
        const CHILD: &str = "DUDUCLAW_EGRESS_SATURATED_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let home = PathBuf::from(std::env::var_os("DUDUCLAW_HOME").unwrap());
            let taint = crate::egress_client::query(&home, "bot").await;
            assert!(taint.tainted);
            assert!(taint.sources.iter().any(|s| s == "turn_state_unavailable"));
            assert_eq!(crate::egress_client::mark(&home, "bot", "memory_search").await.unwrap_err(), "gateway refused taint request");
            let reason = crate::egress_client::reserve(&home, "bot", "memory_search").await.err().unwrap();
            assert_eq!(reason, "gateway refused taint request");
            assert_eq!(crate::egress_client::SinkLease::acquire(&home, "bot", None).await.err().unwrap(), reason);
            for mode in ["ask", "block", "shadow"] {
                std::fs::write(home.join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
                assert_eq!(crate::egress_client::reservation_failure(&home, "bot", "memory_search", &reason).is_err(), mode != "shadow");
            }
            return;
        }
        let home = policy_home("block");
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let key = crate::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        let _permits = state.blocking.clone().try_acquire_many_owned(MAX_BLOCKING_WORK as u32).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        // Credentials and turn identity stay in a child process, isolated from
        // the parallel suite. The parent serves a genuinely saturated router.
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "egress_guard::tests::egress_saturated_gateway_clients_fail_closed", "--nocapture"])
            .env(CHILD, "1").env("DUDUCLAW_HOME", home.path())
            .env("DUDUCLAW_PORT", port.to_string())
            .env(duduclaw_core::ENV_MCP_API_KEY, key)
            .env(duduclaw_core::ENV_AGENT_TOKEN, duduclaw_core::mint_identity_token(&identity, "bot"))
            .env(duduclaw_core::ENV_TRUST_TURN_ID, "saturated-turn")
            .env_remove(duduclaw_core::egress_guard::ENV_TURN_ALIAS)
            .kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(30), command.output()).await;
        server.abort();
        let output = result.expect("saturated clients must promptly refuse").unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    fn signed_request(home: &Path, agent: &str, turn: &str, op: &str, id: &str) -> (HeaderMap, Bytes) {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        let identity = duduclaw_core::load_identity_key(home).unwrap();
        let api_key = crate::mcp_internal_key::ensure_internal_mcp_key(home).unwrap();
        let token = duduclaw_core::mint_identity_token(&identity, agent);
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
        let body = serde_json::to_vec(&serde_json::json!({
            "op": op, "turn_id": turn, "reservation": id,
            "source": (op == "reserve").then_some("memory_search"),
        })).unwrap();
        let signature = duduclaw_core::internal_request_signature(api_key.as_bytes(), agent, &token, &timestamp, &nonce, &body);
        let mut headers = HeaderMap::new();
        for (name, value) in [(AGENT_ID_HEADER, agent), (TIMESTAMP_HEADER, timestamp.as_str()),
            (NONCE_HEADER, nonce.as_str()), (SIGNATURE_HEADER, signature.as_str())] {
            headers.insert(name, value.parse().unwrap());
        }
        (headers, body.into())
    }

    async fn response_value(response: Response) -> serde_json::Value {
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn egress_async_handlers_serialize_read_and_sink_admission() {
        let home = policy_home("block");
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        let peer = ConnectInfo("127.0.0.1:50001".parse().unwrap());
        for _ in 0..8 {
            let agent = format!("parallel-{}", uuid::Uuid::new_v4());
            register_turn(&agent, "T");
            let sink_id = uuid::Uuid::new_v4().to_string();
            let read_id = uuid::Uuid::new_v4().to_string();
            let (sink_headers, sink_body) = signed_request(home.path(), &agent, "T", "acquire_sink", &sink_id);
            let (read_headers, read_body) = signed_request(home.path(), &agent, "T", "reserve", &read_id);
            let (sink, read) = tokio::join!(
                acquire_sink(State(state.clone()), peer, sink_headers, sink_body),
                reserve(State(state.clone()), peer, read_headers, read_body),
            );
            let sink = response_value(sink).await;
            let read = response_value(read).await;
            assert!(sink.get("error").is_none(), "{sink}");
            // Either the read reserves first and taints the sink, or the sink
            // commits first and fences the read. Both can never be clean.
            if read.get("error").is_none() {
                assert_eq!(sink["tainted"], true);
                assert_eq!(sink["sources"], serde_json::json!(["pending_private_read"]));
                finish_turn(&agent, Some("T"), &read_id, false).unwrap();
            } else {
                assert_eq!(read["error"], READ_DURING_SINK);
                assert_eq!(sink["tainted"], false);
            }
            release_sink_turn(&agent, "T", &sink_id);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn egress_async_handler_sqlite_wait_does_not_block_tokio_worker() {
        let home = policy_home("block");
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        let agent = format!("sqlite-wait-{}", uuid::Uuid::new_v4());
        register_turn(&agent, "T");
        let id = uuid::Uuid::new_v4().to_string();
        let (headers, body) = signed_request(home.path(), &agent, "T", "acquire_sink", &id);
        let conn = rusqlite::Connection::open(home.path().join("egress_leases.db")).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let request = tokio::spawn(acquire_sink(State(state), ConnectInfo("127.0.0.1:50001".parse().unwrap()), headers, body));
        // With synchronous admission this worker cannot release SQLite until
        // the one-second busy timeout has already refused the request.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        conn.execute_batch("ROLLBACK;").unwrap();
        let value = response_value(request.await.unwrap()).await;
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["tainted"], false);
        release_sink_turn(&agent, "T", &id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn egress_blocking_panic_refuses_and_cancelled_waiter_keeps_fence() {
        assert_eq!(blocking_response(Arc::new(Semaphore::new(MAX_BLOCKING_WORK)), || panic!("simulated blocking task panic")).await.status(), StatusCode::SERVICE_UNAVAILABLE);
        let home = policy_home("block");
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        let agent = format!("cancelled-{}", uuid::Uuid::new_v4());
        register_turn(&agent, "T");
        let id = uuid::Uuid::new_v4().to_string();
        let (headers, body) = signed_request(home.path(), &agent, "T", "acquire_sink", &id);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let blocking = state.blocking.clone();
        let waiter = tokio::spawn(blocking_response(blocking.clone(), move || {
            let response = handle(&state, "127.0.0.1:50001".parse().unwrap(), &headers, &body, "acquire_sink");
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let _ = done_tx.send(());
            response
        }));
        // This single-worker runtime must remain responsive while the blocking
        // task waits; the timeout also prevents a broken test from hanging.
        tokio::time::timeout(Duration::from_secs(5), started_rx).await.unwrap().unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(blocking.available_permits(), MAX_BLOCKING_WORK - 1);
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), done_rx).await.unwrap().unwrap();
        assert_eq!(reserve_turn(home.path(), &agent, Some("T"), "memory_search", "read").unwrap_err(), READ_DURING_SINK);
        release_sink_turn(&agent, "T", &id);
        assert!(reserve_turn(home.path(), &agent, Some("T"), "memory_search", "read").is_ok());
        finish_turn(&agent, Some("T"), "read", false).unwrap();
    }

    #[tokio::test]
    async fn egress_signed_route_fences_missing_forged_and_other_turns() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        let home = policy_home("block");
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let api_key = crate::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
        let state = RouteState::new(home.path().into());
        let agent = format!("route-{}", uuid::Uuid::new_v4());
        let other = format!("route-{}", uuid::Uuid::new_v4());
        register_turn(&agent, "T");
        register_turn(&agent, "T2");
        register_turn(&other, "T2");
        let lease_id = uuid::Uuid::new_v4().to_string();
        for (caller, turn, op, expected_error) in [
            (&agent, "T", "acquire_sink", None),
            (&agent, "forged", "reserve", Some(READ_DURING_SINK)),
            (&agent, "", "reserve", Some(READ_DURING_SINK)),
            (&agent, "T2", "reserve", Some(READ_DURING_SINK)),
            (&other, "T2", "reserve", None),
            (&agent, "T", "release_sink", None),
            // Missing-turn sinks are admitted as tainted, then release by their
            // exact empty-turn key. Neither operation needs a synthetic turn.
            (&agent, "", "acquire_sink", None),
            (&agent, "T2", "reserve", Some(READ_DURING_SINK)),
            (&agent, "", "release_sink", None),
            (&agent, "T2", "reserve", None),
        ] {
            let token = duduclaw_core::mint_identity_token(&identity, caller);
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
            let body = serde_json::to_vec(&serde_json::json!({
                "op": op, "turn_id": turn, "reservation": lease_id,
                "source": (op == "reserve").then_some("memory_search"),
            })).unwrap();
            let signature = duduclaw_core::internal_request_signature(api_key.as_bytes(), caller, &token, &timestamp, &nonce, &body);
            let mut headers = HeaderMap::new();
            for (name, value) in [(AGENT_ID_HEADER, caller.as_str()), (TIMESTAMP_HEADER, timestamp.as_str()),
                (NONCE_HEADER, nonce.as_str()), (SIGNATURE_HEADER, signature.as_str())] {
                headers.insert(name, value.parse().unwrap());
            }
            let response = handle(&state, "127.0.0.1:50001".parse().unwrap(), &headers, &body, op);
            assert_eq!(response.status(), StatusCode::OK);
            let sig = response.headers()[duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER].to_str().unwrap().to_string();
            let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(duduclaw_core::verify_internal_request_signature(api_key.as_bytes(), caller, &token,
                duduclaw_core::egress_guard::RESPONSE_DOMAIN, &nonce, &bytes, &sig));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value.get("error").and_then(|v| v.as_str()), expected_error, "{op}/{turn}");
            if turn.is_empty() && op == "acquire_sink" { assert_eq!(value["tainted"], true); }
        }
    }

    #[tokio::test]
    async fn signed_route_marks_queries_and_rejects_replay() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='block'\n").unwrap();
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let key = crate::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
        let agent = "egress-test-agent";
        let token = duduclaw_core::mint_identity_token(&identity, agent);
        let turn = uuid::Uuid::new_v4().to_string();
        register_turn(agent, &turn);
        let state = RouteState::new(home.path().into());
        let peer = "127.0.0.1:50000".parse().unwrap();
        let reservation = uuid::Uuid::new_v4().to_string();
        for (op, source, expected, pending, own_read, refused) in [
            ("query", None, false, false, false, false),
            ("acquire_sink", None, false, false, false, false),
            ("reserve", Some("memory_search"), false, false, false, true),
            ("release_sink", None, false, false, false, false),
            ("reserve", Some("memory_search"), false, false, false, false),
            ("acquire_sink", None, false, false, true, false),
            ("query", None, true, true, false, false),
            ("release_sink", None, true, true, false, false),
            ("finish", None, false, false, false, false),
            ("mark", Some("memory_search"), true, false, false, false),
            ("query", None, true, false, false, false),
        ] {
            let body = serde_json::to_vec(
                &serde_json::json!({
                    "op": op, "turn_id": turn, "source": source,
                    "reservation": matches!(op, "reserve" | "finish" | "acquire_sink" | "release_sink").then_some(&reservation),
                    "payload": (op == "finish").then_some(false),
                    "own_read": own_read.then_some(&reservation),
                }),
            )
            .unwrap();
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string();
            let signature = duduclaw_core::internal_request_signature(
                key.as_bytes(),
                agent,
                &token,
                &timestamp,
                &nonce,
                &body,
            );
            let mut headers = HeaderMap::new();
            for (name, value) in [
                (AGENT_ID_HEADER, agent),
                (TIMESTAMP_HEADER, timestamp.as_str()),
                (NONCE_HEADER, nonce.as_str()),
                (SIGNATURE_HEADER, signature.as_str()),
            ] {
                headers.insert(name, value.parse().unwrap());
            }
            let response = handle(&state, peer, &headers, &body, op);
            assert_eq!(response.status(), StatusCode::OK);
            let sig = response
                .headers()
                .get(duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .unwrap();
            assert!(duduclaw_core::verify_internal_request_signature(
                key.as_bytes(),
                agent,
                &token,
                duduclaw_core::egress_guard::RESPONSE_DOMAIN,
                &nonce,
                &bytes,
                &sig
            ));
            if refused {
                let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(response["error"], READ_DURING_SINK);
            } else {
                let taint: Taint = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(taint.tainted, expected);
                if expected {
                    assert_eq!(taint.sources, [if pending { "pending_private_read" } else { "memory_search" }]);
                }
            }
            assert_eq!(
                handle(&state, peer, &headers, &body, op).status(),
                StatusCode::TOO_MANY_REQUESTS
            );
        }
    }
    #[tokio::test]
    async fn recipient_route_authenticates_context_and_rejects_caller_source_fields() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        use duduclaw_knowledge::requester::RequesterIdentity;
        let home = policy_home("shadow");
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let api_key = crate::mcp_internal_key::ensure_internal_mcp_key(home.path()).unwrap();
        let state = RouteState::new(home.path().into());
        let agent = "recipient-route-bot";
        let token = duduclaw_core::mint_identity_token(&identity, agent);
        let _turn = crate::turn_context::register(agent, "live", "line:source#agent:recipient-route-bot").unwrap();
        let _requester = crate::knowledge::turn_requesters::register(agent, "live",
            RequesterIdentity::ChannelUser {channel:"line".into(),chat:"source".into(),sender:"person".into(),user_id:Some("verified-person".into())}).unwrap();
        let _guest_turn = crate::turn_context::register(agent, "guest", "line:guest").unwrap();
        let _guest = crate::knowledge::turn_requesters::register(agent, "guest",
            RequesterIdentity::ChannelUser { channel: "line".into(), chat: "guest".into(),
                sender: "guest".into(), user_id: None }).unwrap();
        for (turn, forged_source, expected) in [("live", false, Some("line:source")), ("guest", false, None), ("unknown", false, None), ("", false, None), ("live", true, None)] {
            let mut request = serde_json::json!({"op":"recipients", "turn_id":turn});
            if forged_source { request["source"] = serde_json::json!("line:forged"); }
            let body = serde_json::to_vec(&request).unwrap();
            let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let signature = duduclaw_core::internal_request_signature(api_key.as_bytes(), agent, &token, &timestamp, &nonce, &body);
            let mut headers = HeaderMap::new();
            for (name, value) in [(AGENT_ID_HEADER, agent), (TIMESTAMP_HEADER, timestamp.as_str()),
                (NONCE_HEADER, nonce.as_str()), (SIGNATURE_HEADER, signature.as_str())] {
                headers.insert(name, value.parse().unwrap());
            }
            let response = handle(&state, "127.0.0.1:50001".parse().unwrap(), &headers, &body, "recipients");
            if forged_source {
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
                continue;
            }
            assert_eq!(response.status(), StatusCode::OK);
            let signature = response.headers()[duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER].to_str().unwrap().to_string();
            let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(duduclaw_core::verify_internal_request_signature(api_key.as_bytes(), agent, &token,
                duduclaw_core::egress_guard::RESPONSE_DOMAIN, &nonce, &bytes, &signature));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["source"].as_str(), expected);
            assert_eq!(value["owners"], serde_json::json!([]));
            assert_eq!(handle(&state, "127.0.0.1:50001".parse().unwrap(), &headers, &body, "recipients").status(), StatusCode::TOO_MANY_REQUESTS);
        }
    }

}
