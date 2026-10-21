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

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Open,
    Claimed(String),
    Cancelled(String),
}

#[derive(Debug, Clone)]
struct Entry {
    agent: String,
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

fn purge(map: &mut HashMap<PendingKey, Entry>, now: Instant) {
    map.retain(|_, e| {
        let age = now.saturating_duration_since(e.armed);
        age < PENDING_TTL + TOMBSTONE_TTL
    });
    if map.len() >= MAX_ENTRIES {
        // Oldest first.
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
            origin: origin.to_string(),
            seq,
            armed: now,
            state: State::Open,
        },
    );
}

/// The addressee for `event`, consuming it on first use. Returns the same
/// addressee again when `event` already claimed it. `None` when nothing is
/// armed, it expired, or it belongs to an earlier/other event.
pub fn claim(key: &PendingKey, event: &str, seq: i64) -> Option<String> {
    let now = Instant::now();
    let mut map = lock();
    let e = map.get_mut(key)?;
    match &e.state {
        State::Claimed(by) if by == event => Some(e.agent.clone()),
        State::Open if seq > e.seq && !expired(e, now) => {
            e.state = State::Claimed(event.to_string());
            Some(e.agent.clone())
        }
        _ => None,
    }
}

/// Whether `claim` would succeed for a new event at `seq`, without using it
/// (for "mention only" gates that run before the message is accepted).
pub fn peek(key: &PendingKey, seq: i64) -> Option<String> {
    let now = Instant::now();
    let map = lock();
    let e = map.get(key)?;
    (e.state == State::Open && seq > e.seq && !expired(e, now)).then(|| e.agent.clone())
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
