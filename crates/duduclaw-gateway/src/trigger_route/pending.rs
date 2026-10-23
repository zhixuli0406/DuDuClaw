//! One-shot addressee: after `@name` with no task (or tapping a name in the
//! `@` list), only the sender's **next** message in that conversation goes to
//! that employee, then routing returns to normal (owner decision D6: this is
//! "the next one message", not stickiness).
//!
//! Kept in memory with a short TTL. A gateway restart forgets every pending
//! addressee, so the next message goes to the default employee (or, for a
//! LINE event already accepted under the pending addressee, its route
//! snapshot no longer matches and the durable ingress quarantines it with an
//! alert rather than answering from the wrong employee).
//!
//! Durable LINE ingress recomputes an event's route several times (on
//! acceptance, before running, before each send), so every operation is
//! idempotent per event: `claim` by the same event keeps returning the
//! addressee, and the `seq` order key (LINE event timestamp, or a process
//! counter elsewhere) makes sure an older event recomputed later never
//! consumes or cancels an addressee armed after it.
//!
//! Two kinds share the store (one entry per sender and conversation, the
//! newest wins): [`PendingKind::Chat`] after `@name` alone (the next message
//! is a conversation turn with that employee) and [`PendingKind::Assign`]
//! after `/交辦 @name` alone (the next message becomes a task for them). A
//! claim only takes an entry of the kind it asks for.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long an armed addressee waits for the next message.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long a used or cancelled entry is remembered so recomputing the same
/// event's route gives the same answer.
const TOMBSTONE_TTL: Duration = Duration::from_secs(60 * 60);

/// Upper bound on remembered entries (memory guard).
const MAX_ENTRIES: usize = 10_000;

/// One sender in one conversation on one channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingKey {
    pub channel: String,
    pub conversation: String,
    pub sender: String,
}

impl PendingKey {
    pub fn new(channel: &str, conversation: &str, sender: &str) -> Self {
        Self {
            channel: channel.to_string(),
            conversation: conversation.to_string(),
            sender: sender.to_string(),
        }
    }
}

/// What the next message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingKind {
    /// A conversation turn with the employee.
    Chat,
    /// A task handed to the employee (`/交辦 @name`, then the task).
    Assign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Open,
    Claimed(String),
    Cancelled(String),
}

#[derive(Debug, Clone)]
struct Entry {
    agent: String,
    kind: PendingKind,
    origin: String,
    seq: i64,
    armed: Instant,
    state: State,
}

fn store() -> &'static Mutex<HashMap<PendingKey, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<PendingKey, Entry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<PendingKey, Entry>> {
    store().lock().unwrap_or_else(|e| e.into_inner())
}

/// A strictly increasing order key for channels without an event timestamp
/// (messages handled one after another in arrival order).
pub fn next_seq() -> i64 {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = chrono::Utc::now().timestamp_millis();
    let mut prev = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(prev + 1);
        match LAST.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(p) => prev = p,
        }
    }
}

fn expired(e: &Entry, now: Instant) -> bool {
    let age = now.saturating_duration_since(e.armed);
    match e.state {
        State::Open => age >= PENDING_TTL,
        _ => age >= PENDING_TTL + TOMBSTONE_TTL,
    }
}

/// Below this size the store is not scanned at all on `arm`.
const PURGE_THRESHOLD: usize = 1_024;

fn purge(map: &mut HashMap<PendingKey, Entry>, now: Instant) {
    if map.len() < PURGE_THRESHOLD {
        return;
    }
    map.retain(|_, e| {
        let age = now.saturating_duration_since(e.armed);
        age < PENDING_TTL + TOMBSTONE_TTL
    });
    if map.len() >= MAX_ENTRIES {
        // Under pressure, drop used/cancelled entries first, then the oldest.
        map.retain(|_, e| e.state == State::Open);
    }
    if map.len() >= MAX_ENTRIES {
        let mut keys: Vec<(Instant, PendingKey)> =
            map.iter().map(|(k, e)| (e.armed, k.clone())).collect();
        keys.sort_by_key(|(t, _)| *t);
        for (_, k) in keys.into_iter().take(map.len() + 1 - MAX_ENTRIES) {
            map.remove(&k);
        }
    }
}

/// Arm `agent` as the addressee of `key`'s next message. `origin` names the
/// event that armed it; re-arming from the same event, or from an event older
/// than the current entry, changes nothing.
pub fn arm(key: &PendingKey, agent: &str, origin: &str, seq: i64) {
    arm_kind(key, agent, PendingKind::Chat, origin, seq);
}

/// [`arm`] for a given [`PendingKind`].
pub fn arm_kind(key: &PendingKey, agent: &str, kind: PendingKind, origin: &str, seq: i64) {
    let now = Instant::now();
    let mut map = lock();
    purge(&mut map, now);
    if let Some(e) = map.get(key) {
        if e.origin == origin || e.seq > seq {
            return;
        }
    }
    map.insert(
        key.clone(),
        Entry {
            agent: agent.to_string(),
            kind,
            origin: origin.to_string(),
            seq,
            armed: now,
            state: State::Open,
        },
    );
}

/// How long the result of a claim is remembered for its event, so an event
/// whose route is recomputed while it runs keeps its addressee even when the
/// sender armed someone else in the meantime.
const CLAIM_TTL: Duration = Duration::from_secs(75 * 60);
/// Upper bound on remembered claim results (memory guard).
const MAX_CLAIMS: usize = 10_000;

type ClaimMap = HashMap<(PendingKey, String), (String, PendingKind, Instant)>;

fn claims() -> std::sync::MutexGuard<'static, ClaimMap> {
    static CLAIMS: OnceLock<Mutex<ClaimMap>> = OnceLock::new();
    CLAIMS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn remember_claim(key: &PendingKey, event: &str, agent: &str, kind: PendingKind, now: Instant) {
    let mut map = claims();
    if map.len() >= MAX_CLAIMS {
        map.retain(|_, (_, _, at)| now.saturating_duration_since(*at) < CLAIM_TTL);
        if map.len() >= MAX_CLAIMS {
            let mut by_age: Vec<((PendingKey, String), Instant)> =
                map.iter().map(|(k, (_, _, at))| (k.clone(), *at)).collect();
            by_age.sort_by_key(|(_, at)| *at);
            for (k, _) in by_age.into_iter().take(map.len() + 1 - MAX_CLAIMS) {
                map.remove(&k);
            }
        }
    }
    map.insert(
        (key.clone(), event.to_string()),
        (agent.to_string(), kind, now),
    );
}

/// The addressee for `event`, consuming it on first use. Returns the same
/// addressee again for the same event (kept for [`CLAIM_TTL`], independent
/// of later arms). `None` when nothing is armed, it expired, or it belongs
/// to an earlier/other event. Only takes a [`PendingKind::Chat`] entry.
pub fn claim(key: &PendingKey, event: &str, seq: i64) -> Option<String> {
    claim_kind(key, event, seq, PendingKind::Chat)
}

/// [`claim`] for a given [`PendingKind`]: an entry of another kind is left
/// alone (it is still waiting for its own next message).
pub fn claim_kind(key: &PendingKey, event: &str, seq: i64, kind: PendingKind) -> Option<String> {
    let now = Instant::now();
    if let Some(found) = claimed(key, event, kind) {
        return Some(found);
    }
    let agent = {
        let mut map = lock();
        let e = map.get_mut(key)?;
        if !(e.state == State::Open && e.kind == kind && seq > e.seq && !expired(e, now)) {
            return None;
        }
        e.state = State::Claimed(event.to_string());
        e.agent.clone()
    };
    remember_claim(key, event, &agent, kind, now);
    Some(agent)
}

/// The addressee `event` already claimed with `kind`, if it did (for an
/// event whose handling is recomputed after its first pass).
pub fn claimed(key: &PendingKey, event: &str, kind: PendingKind) -> Option<String> {
    let now = Instant::now();
    claims()
        .get(&(key.clone(), event.to_string()))
        .filter(|(_, k, at)| *k == kind && now.saturating_duration_since(*at) < CLAIM_TTL)
        .map(|(agent, _, _)| agent.clone())
}

/// Whether `claim` would succeed for a new event at `seq`, without using it
/// (for "mention only" gates that run before the message is accepted).
pub fn peek(key: &PendingKey, seq: i64) -> Option<String> {
    peek_kind(key, seq).map(|(agent, _)| agent)
}

/// [`peek`] with the entry's kind.
pub fn peek_kind(key: &PendingKey, seq: i64) -> Option<(String, PendingKind)> {
    let now = Instant::now();
    let map = lock();
    let e = map.get(key)?;
    (e.state == State::Open && seq > e.seq && !expired(e, now)).then(|| (e.agent.clone(), e.kind))
}

/// A newer message that calls someone explicitly cancels the addressee.
pub fn cancel(key: &PendingKey, event: &str, seq: i64) {
    let mut map = lock();
    if let Some(e) = map.get_mut(key) {
        if e.state == State::Open && seq > e.seq && e.origin != event {
            e.state = State::Cancelled(event.to_string());
        }
    }
}

#[cfg(test)]
pub(crate) fn age_for_test(key: &PendingKey, by: Duration) {
    let mut map = lock();
    if let Some(e) = map.get_mut(key) {
        e.armed = e.armed.checked_sub(by).unwrap_or(e.armed);
    }
}
