//! Poll mode and gap catch-up (2026-10-08).
//!
//! The draft's poll delivery is request/response: the client sends
//! `events/poll { name, arguments, cursor, maxAgeMs, maxEvents }` and gets
//! `{ events[], cursor, truncated, hasMore, nextPollMs }`; the first poll
//! with a `null` cursor bootstraps (no events, a fresh cursor) and the server
//! keeps no subscription state. Here the gateway is the client:
//!
//! - one loop, started only in the process that holds
//!   `duduclaw_core::gateway_instance` (a second gateway on the same data
//!   directory never polls), ticking once a second;
//! - at most [`super::service::MAX_POLL_SUBSCRIPTIONS`] poll subscriptions,
//!   each event name polled no more often than `[mcp_events] poll_floor_ms`
//!   (default 5 s, never below the draft's 1 s floor) and no less often than
//!   every [`MAX_WAIT_MS`]; at most [`MAX_CONCURRENT`] polls in flight; a
//!   `hasMore` backlog is drained for at most [`MAX_DRAIN_BATCHES`] batches
//!   per pass, then the next pass continues;
//! - a failing name backs off (double per failure, capped at 15 minutes);
//! - the cursor is stored only after the batch's events were recorded in
//!   `events.db`, so a crash re-reads the batch and `eventId` de-duplication
//!   drops the repeats;
//! - `truncated: true` is recorded (audit `gap`) and the fresh cursor kept.
//!
//! Events take exactly the path of webhook deliveries
//! ([`super::receiver::ingest_occurrence`]): scanned, capped, marked with the
//! subscription's lane (explore by default).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::receiver::{Ingested, ingest_occurrence};
use super::service::{max_age_ms, poll_floor_ms};
use super::store::{self, DeliveryKind, EventSubscription, SubStatus};
use super::upstream::{self, Session, UpstreamError};

/// Poll wait when the server names none.
pub const DEFAULT_WAIT_MS: u64 = 60_000;
/// Longest wait between polls, whatever the server asks for.
pub const MAX_WAIT_MS: u64 = 3_600_000;
/// Longest backoff after failures.
const MAX_BACKOFF_MS: u64 = 900_000;
/// Polls in flight at once.
pub const MAX_CONCURRENT: usize = 4;
/// Batches taken from one name in one pass (`hasMore` drain).
pub const MAX_DRAIN_BATCHES: usize = 10;

fn fallback_id(name: &str, ev: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update(b"\0");
    h.update(ev.to_string().as_bytes());
    format!("poll_{}", &hex::encode(h.finalize())[..32])
}

fn record_gap(home: &Path, rec: &EventSubscription, name: &str, kind: &str) {
    super::audit(home, super::AUDIT_CONTROL, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "name": name, "type": kind }));
}

/// Ingest the events of one answer. `Err` = events.db unavailable.
async fn ingest_all(home: &Path, rec: &EventSubscription, name: &str, events: &[Value]) -> Result<usize, ()> {
    let mut n = 0;
    let now = chrono::Utc::now().timestamp();
    for ev in events {
        let Some(obj) = ev.as_object() else { continue };
        // A poll entry names its event type; one without a name belongs to
        // the type that was polled.
        let mut obj = obj.clone();
        obj.entry("name").or_insert_with(|| Value::String(name.to_string()));
        if obj.get("name").and_then(|v| v.as_str()) != Some(name) {
            continue;
        }
        match ingest_occurrence(home, rec, &obj, &fallback_id(name, ev), now).await? {
            Ingested::Recorded => n += 1,
            Ingested::Duplicate | Ingested::NotSubscribed => {}
        }
    }
    Ok(n)
}

/// First poll of every event name of a new poll subscription (`cursor:
/// null`): learns the cursor and proves the name and arguments to the server.
pub(crate) async fn bootstrap(home: &Path, rec: &EventSubscription) -> Result<(), UpstreamError> {
    let mut session = match Session::open(home, &rec.agent_id, &rec.server).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let _ = store::update(home, &rec.id, |r| {
                r.status = SubStatus::Failed;
                for u in r.upstream.iter_mut() {
                    u.last_error = Some(msg.clone());
                }
                Ok(true)
            });
            return Err(e);
        }
    };
    let mut errors: Vec<(String, String)> = Vec::new();
    let mut cursors: Vec<(String, Option<String>)> = Vec::new();
    for name in &rec.event_types {
        let args = rec.arguments.get(name).cloned().unwrap_or_else(|| json!({}));
        match upstream::poll(&mut session, name, &args, None, None).await {
            Ok(r) => cursors.push((name.clone(), r.cursor)),
            Err(e) => errors.push((name.clone(), duduclaw_core::truncate_chars(&e.to_string(), 200).to_string())),
        }
    }
    session.close().await;
    let ok = errors.is_empty();
    let _ = store::update(home, &rec.id, |r| {
        r.status = if ok { SubStatus::Active } else { SubStatus::Failed };
        for u in r.upstream.iter_mut() {
            u.last_error = errors.iter().find(|(n, _)| n == &u.name).map(|(_, e)| e.clone());
            if let Some((_, c)) = cursors.iter().find(|(n, _)| n == &u.name) {
                u.cursor = c.clone();
                u.last_polled_at = Some(store::now_rfc3339());
            }
        }
        Ok(true)
    });
    if !ok {
        super::audit(home, super::AUDIT_REFRESH_FAILED, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server }));
    }
    Ok(())
}

/// Poll one event name of one poll subscription once (draining `hasMore`
/// backlog up to [`MAX_DRAIN_BATCHES`]). Returns the wait in milliseconds
/// until the next poll.
pub async fn poll_once(home: &Path, sub_id: &str, name: &str) -> Result<u64, String> {
    let rec = store::get(home, sub_id)?.ok_or_else(|| "subscription not found".to_string())?;
    if rec.delivery != DeliveryKind::Poll || rec.status == SubStatus::Terminated {
        return Err("not an active poll subscription".into());
    }
    if !rec.event_types.iter().any(|t| t == name) {
        return Err("unknown event type".into());
    }
    let args = rec.arguments.get(name).cloned().unwrap_or_else(|| json!({}));
    let max_age = max_age_ms(home);
    let floor = poll_floor_ms(home);
    let fail = |home: &Path, msg: String| {
        let m = duduclaw_core::truncate_chars(&msg, 200).to_string();
        let _ = store::update(home, sub_id, |r| {
            if r.status != SubStatus::Terminated {
                r.status = SubStatus::Failed;
            }
            if let Some(u) = r.upstream.iter_mut().find(|u| u.name == name) {
                u.last_error = Some(m.clone());
            }
            Ok(true)
        });
        super::audit(home, super::AUDIT_REFRESH_FAILED, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "name": name, "error": m }));
        msg
    };
    let mut session = Session::open(home, &rec.agent_id, &rec.server).await.map_err(|e| fail(home, e.to_string()))?;
    let mut wait = DEFAULT_WAIT_MS;
    let mut result: Result<(), String> = Ok(());
    for _ in 0..MAX_DRAIN_BATCHES {
        let cursor = store::get(home, sub_id)?
            .and_then(|r| r.upstream.into_iter().find(|u| u.name == name))
            .and_then(|u| u.cursor);
        let reply = match upstream::poll(&mut session, name, &args, cursor.as_deref(), Some(max_age)).await {
            Ok(r) => r,
            Err(e) => {
                result = Err(fail(home, e.to_string()));
                break;
            }
        };
        if ingest_all(home, &rec, name, &reply.events).await.is_err() {
            result = Err(fail(home, "could not record the events".into()));
            break;
        }
        if reply.truncated {
            record_gap(home, &rec, name, "truncated");
        }
        let (cursor_new, trunc) = (reply.cursor.clone(), reply.truncated);
        let _ = store::update(home, sub_id, |r| {
            if r.status != SubStatus::Terminated {
                r.status = SubStatus::Active;
            }
            if let Some(u) = r.upstream.iter_mut().find(|u| u.name == name) {
                u.last_error = None;
                u.last_polled_at = Some(store::now_rfc3339());
                // `null` from a replay-less event type is what we keep.
                u.cursor = cursor_new.clone();
                if trunc {
                    u.truncated = true;
                    u.gap_at = Some(store::now_rfc3339());
                }
            }
            Ok(true)
        });
        if let Some(ms) = reply.next_poll_ms {
            wait = ms;
        }
        if !reply.has_more {
            break;
        }
        // Drain at once; a cursor that did not move would loop for nothing.
        if reply.cursor == cursor {
            break;
        }
        wait = 0;
    }
    session.close().await;
    result?;
    Ok(wait.clamp(floor, MAX_WAIT_MS))
}

/// One best-effort catch-up per recorded gap: poll from the cursor held
/// before the `gap` (bounded by `maxAgeMs`) and record whatever the server
/// can still serve; the attempt is made once whatever the outcome.
pub async fn catch_up_gaps(home: &Path, sub_id: &str) {
    let Ok(Some(rec)) = store::get(home, sub_id) else { return };
    if rec.status == SubStatus::Terminated {
        return;
    }
    let pending: Vec<(String, String)> = rec
        .upstream
        .iter()
        .filter_map(|u| u.gap_cursor.clone().map(|c| (u.name.clone(), c)))
        .collect();
    if pending.is_empty() {
        return;
    }
    let max_age = max_age_ms(home);
    let mut session = match Session::open(home, &rec.agent_id, &rec.server).await {
        Ok(s) => s,
        Err(_) => return, // gap_cursor stays; the next sweep tries again
    };
    for (name, start) in pending {
        let args = rec.arguments.get(&name).cloned().unwrap_or_else(|| json!({}));
        let mut cursor = Some(start);
        let mut recovered = 0usize;
        let mut outcome = "ok";
        for _ in 0..MAX_DRAIN_BATCHES {
            match upstream::poll(&mut session, &name, &args, cursor.as_deref(), Some(max_age)).await {
                Ok(r) => {
                    match ingest_all(home, &rec, &name, &r.events).await {
                        Ok(n) => recovered += n,
                        Err(()) => {
                            outcome = "store_unavailable";
                            break;
                        }
                    }
                    if r.truncated {
                        outcome = "truncated";
                    }
                    if !r.has_more || r.cursor == cursor || r.cursor.is_none() {
                        break;
                    }
                    cursor = r.cursor;
                }
                Err(_) => {
                    // The event type may not offer poll at all.
                    outcome = "unavailable";
                    break;
                }
            }
        }
        if outcome != "store_unavailable" {
            let n2 = name.clone();
            let _ = store::update(home, sub_id, |r| {
                if let Some(u) = r.upstream.iter_mut().find(|u| u.name == n2) {
                    u.gap_cursor = None;
                }
                Ok(true)
            });
        }
        super::audit(
            home,
            super::AUDIT_CONTROL,
            &rec.agent_id,
            json!({ "subscription": rec.id, "server": rec.server, "name": name, "type": "gap_catch_up", "recovered": recovered, "outcome": outcome }),
        );
    }
    session.close().await;
}

/// Subscriptions with a gap waiting for a catch-up.
pub async fn catch_up_all(home: &Path) {
    let Ok(all) = store::load_all(home) else { return };
    for rec in all {
        if rec.upstream.iter().any(|u| u.gap_cursor.is_some()) {
            catch_up_gaps(home, &rec.id).await;
        }
    }
}

/// When each `(subscription, event name)` is next polled.
#[derive(Default)]
pub struct Schedule {
    next: HashMap<(String, String), Instant>,
    failures: HashMap<(String, String), u32>,
}

/// Poll every name that is due at `now`, at most [`MAX_CONCURRENT`] at a
/// time. Returns how many polls ran. (The caller decides whether this
/// process may poll at all.)
pub async fn run_due(home: &Path, sched: &mut Schedule, now: Instant) -> usize {
    let home_o = home.to_path_buf();
    let subs = match tokio::task::spawn_blocking(move || store::load_all(&home_o)).await {
        Ok(Ok(s)) => s,
        _ => return 0,
    };
    let mut live: Vec<(String, String)> = Vec::new();
    let mut due: Vec<(String, String)> = Vec::new();
    for s in subs.iter().filter(|s| s.delivery == DeliveryKind::Poll && s.status != SubStatus::Terminated) {
        for n in &s.event_types {
            let key = (s.id.clone(), n.clone());
            live.push(key.clone());
            if sched.next.get(&key).is_none_or(|t| *t <= now) {
                due.push(key);
            }
        }
    }
    sched.next.retain(|k, _| live.contains(k));
    sched.failures.retain(|k, _| live.contains(k));
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    let ran = due.len();
    for key in due {
        let (home, sem) = (home.to_path_buf(), sem.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            let r = poll_once(&home, &key.0, &key.1).await;
            (key, r)
        });
    }
    let floor = poll_floor_ms(home);
    while let Some(Ok((key, r))) = set.join_next().await {
        let wait_ms = match r {
            Ok(ms) => {
                sched.failures.remove(&key);
                ms
            }
            Err(_) => {
                let n = sched.failures.entry(key.clone()).or_insert(0);
                *n = n.saturating_add(1);
                let backoff = floor.saturating_mul(1u64 << (*n).min(10));
                backoff.min(MAX_BACKOFF_MS).max(floor)
            }
        };
        sched.next.insert(key, Instant::now() + Duration::from_millis(wait_ms));
    }
    ran
}

/// The poll loop. Does nothing in a process that does not hold the gateway
/// lock, and nothing on an install with no subscriptions file.
pub fn spawn_poll_loop(home: PathBuf) {
    tokio::spawn(async move {
        let mut sched = Schedule::default();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !duduclaw_core::gateway_instance::held(&home) || !store::store_path(&home).exists() {
                continue;
            }
            run_due(&home, &mut sched, Instant::now()).await;
        }
    });
}
