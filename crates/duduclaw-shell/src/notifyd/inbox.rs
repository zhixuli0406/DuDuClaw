// D6 (2026-08-23) — the hand-off point between the D-Bus service and gpui.
//
// ## Why a mutex-guarded queue and not a channel
//
// The two sides have opposite constraints. The D-Bus handler runs on zbus's
// own executor thread and must return a `u32` id **synchronously and fast** —
// a caller is blocked on the reply, and a slow reply is indistinguishable to
// them from a broken daemon. The gpui side, meanwhile, cannot be pushed to:
// `Context<ShellView>` is not `Send`, so nothing off-thread may touch the
// view. So the handler needs somewhere to put a record and leave, and the UI
// needs somewhere to collect from on its own schedule.
//
// A `std::sync::mpsc` would do the transport, but not the two things this
// type also owns: the **id allocator** (the handler must know the id before
// the UI has seen anything) and the **flood guard** (the decision has to be
// made in the handler, because it changes what id is returned). Both are
// shared mutable state that the handler reads and writes under one lock; the
// queue rides along in the same lock rather than adding a second
// synchronisation primitive with its own ordering questions.
//
// The lock is held for a few field writes and a `push_back` — never across
// I/O, never across an `await`, never while any gpui code runs.
//
// ## Poisoning
//
// Every lock is taken with `unwrap_or_else(PoisonError::into_inner)`, never
// `.unwrap()`. A panic inside a D-Bus handler that poisoned this mutex would
// otherwise take down *every subsequent notification*, turning one bad call
// into the exact silent-total-failure D6 exists to end. The data behind the
// lock is a queue and two counters — there is no invariant a panic could
// leave half-broken that reading it again makes worse.

// On a non-Linux host there is no D-Bus transport, so `post`/`close` have no
// production caller and the dead-code lint fires on the macOS dev loop for
// code that is very much alive on the ship target. Suppressed only there —
// on Linux a genuinely unused item still warns.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use super::rate_limit::{FloodGuard, Verdict};
use super::{DaemonEvent, NotifyRequest};

/// How many undelivered events the queue holds before the OLDEST are dropped.
///
/// Reached only if the UI stops draining entirely (a wedged main thread) while
/// an app keeps sending — the flood guard already bounds the normal case. The
/// oldest go first because the newest are the ones still worth showing, and
/// every drop is counted so the UI can say so out loud instead of quietly
/// losing messages.
const INBOX_CAPACITY: usize = 256;

#[derive(Debug, Default)]
struct Inbox {
    /// Next id to hand out. Ids are per-session and never reused while a
    /// notification is live; `0` is reserved by the spec (it is what a client
    /// passes as `replaces_id` to mean "this is a new one"), so the allocator
    /// skips it on wrap.
    next_id: u32,
    events: VecDeque<DaemonEvent>,
    guard: FloodGuard,
    /// Events discarded because the queue was full. Never reset silently —
    /// `drain` reports the count so exactly one honest line can be logged.
    dropped: u64,
}

/// Cloneable handle to the shared queue. Cheap to clone (one `Arc` bump); the
/// D-Bus interface object holds one, `ShellView` holds another.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedInbox(Arc<Mutex<Inbox>>);

/// One drained batch, plus whatever honest bad news came with it.
#[derive(Debug, Default)]
pub(crate) struct Drained {
    pub(crate) events: Vec<DaemonEvent>,
    /// Events lost to a full queue since the previous drain. Non-zero means
    /// the UI stopped draining, not that an app misbehaved.
    pub(crate) dropped: u64,
}

impl SharedInbox {
    /// Handles one `Notify` call: allocates the id, applies the flood guard,
    /// queues the event, and returns the id the caller must reply with.
    ///
    /// `now` is injected so the flood guard stays deterministic in tests; the
    /// production call site passes `Instant::now()`.
    pub(crate) fn post(&self, req: &NotifyRequest, now: Instant) -> u32 {
        let mut inbox = self.lock();

        if req.replaces_id != 0 {
            // A replace targets an id the client already holds. Do NOT
            // allocate a new one — the whole point is that it lands on the
            // same card — but still charge it against the app's budget so a
            // replace stream cannot be used to dodge the guard.
            let id = req.replaces_id;
            inbox.guard.note_replace(&req.app_name, now, id);
            let posted = req.sanitized(id);
            inbox.push(DaemonEvent::Posted(Box::new(posted)));
            return id;
        }

        let fresh = inbox.allocate_id();
        match inbox.guard.admit(&req.app_name, now, fresh) {
            Verdict::Post => {
                let posted = req.sanitized(fresh);
                inbox.push(DaemonEvent::Posted(Box::new(posted)));
                fresh
            }
            Verdict::Merge { onto } => {
                // The sender gets the id of the card its message actually
                // landed on, so a later `CloseNotification` on it still
                // resolves to something real. Returning `fresh` (an id no
                // card will ever carry) would be a small, quiet lie.
                let posted = req.sanitized(onto);
                inbox.push(DaemonEvent::Merged { onto, posted: Box::new(posted) });
                onto
            }
        }
    }

    /// Handles one `CloseNotification(id)` call.
    ///
    /// Unknown ids are accepted rather than refused. The spec allows an error
    /// reply for a notification that no longer exists, but this daemon does
    /// not hold the authoritative list on this side of the boundary (the
    /// center does), so answering would mean either a second lock-step copy
    /// of the list here or blocking the caller on the UI thread. Accepting is
    /// also what mainstream daemons do, and a close for something already
    /// gone is a harmless no-op downstream.
    pub(crate) fn close(&self, id: u32) {
        let mut inbox = self.lock();
        inbox.push(DaemonEvent::CloseRequested { id });
    }

    /// Takes everything queued since the last call. Called from the gpui
    /// drain timer only.
    pub(crate) fn drain(&self) -> Drained {
        let mut inbox = self.lock();
        let events = inbox.events.drain(..).collect();
        let dropped = std::mem::take(&mut inbox.dropped);
        Drained { events, dropped }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        // See this module's header: a poisoned lock must never become a
        // permanent notification blackout.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inbox {
    fn allocate_id(&mut self) -> u32 {
        // `wrapping_add` then skip 0: 4 billion notifications into a session
        // the counter wraps, and `0` is not a legal notification id.
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        self.next_id
    }

    fn push(&mut self, event: DaemonEvent) {
        if self.events.len() >= INBOX_CAPACITY {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::super::rate_limit::MAX_PER_WINDOW;
    use super::*;

    fn req(app: &str, summary: &str) -> NotifyRequest {
        NotifyRequest { app_name: app.into(), summary: summary.into(), expire_timeout: -1, ..Default::default() }
    }

    #[test]
    fn ids_start_at_one_and_are_never_zero() {
        let inbox = SharedInbox::default();
        let t0 = Instant::now();
        assert_eq!(inbox.post(&req("a", "one"), t0), 1);
        assert_eq!(inbox.post(&req("a", "two"), t0), 2);
    }

    #[test]
    fn the_id_allocator_skips_zero_on_wrap() {
        let inbox = SharedInbox::default();
        {
            let mut guard = inbox.lock();
            guard.next_id = u32::MAX;
        }
        let t0 = Instant::now();
        assert_eq!(inbox.post(&req("a", "wrapped"), t0), 1, "0 is reserved by the spec and must never be handed out");
    }

    #[test]
    fn a_posted_notification_arrives_sanitized() {
        let inbox = SharedInbox::default();
        let mut r = req("chat", "hello\nthere");
        r.body = "line\u{0007}one".into();
        let id = inbox.post(&r, Instant::now());
        let drained = inbox.drain();
        assert_eq!(drained.events.len(), 1);
        let DaemonEvent::Posted(p) = &drained.events[0] else { panic!("expected Posted, got {:?}", drained.events[0]) };
        assert_eq!(p.id, id);
        assert_eq!(p.summary, "hello there");
        assert_eq!(p.body, "lineone");
    }

    #[test]
    fn a_replace_keeps_the_client_supplied_id_and_does_not_allocate() {
        let inbox = SharedInbox::default();
        let t0 = Instant::now();
        let first = inbox.post(&req("app", "one"), t0);
        let mut second = req("app", "two");
        second.replaces_id = first;
        assert_eq!(inbox.post(&second, t0), first);

        // The next genuinely-new notification must still get a fresh id, not
        // reuse the replaced one.
        assert_ne!(inbox.post(&req("app", "three"), t0), first);
    }

    #[test]
    fn a_flood_merges_and_still_returns_a_real_id() {
        let inbox = SharedInbox::default();
        let t0 = Instant::now();
        let mut last_posted = 0;
        for i in 0..MAX_PER_WINDOW {
            last_posted = inbox.post(&req("noisy", &format!("m{i}")), t0);
        }
        let merged_id = inbox.post(&req("noisy", "overflow"), t0);
        assert_eq!(merged_id, last_posted, "the sender must get an id that resolves to a real card");

        let drained = inbox.drain();
        let merged = drained.events.iter().filter(|e| matches!(e, DaemonEvent::Merged { .. })).count();
        assert_eq!(merged, 1);
    }

    #[test]
    fn draining_twice_yields_nothing_the_second_time() {
        let inbox = SharedInbox::default();
        inbox.post(&req("a", "one"), Instant::now());
        assert_eq!(inbox.drain().events.len(), 1);
        assert!(inbox.drain().events.is_empty());
    }

    #[test]
    fn an_undrained_queue_is_bounded_and_reports_what_it_lost() {
        let inbox = SharedInbox::default();
        let t0 = Instant::now();
        // Distinct app names so the flood guard is not the thing bounding
        // this — the queue cap itself is under test.
        for i in 0..(INBOX_CAPACITY + 40) {
            inbox.post(&req(&format!("app{i}"), "x"), t0);
        }
        let drained = inbox.drain();
        assert_eq!(drained.events.len(), INBOX_CAPACITY);
        assert_eq!(drained.dropped, 40, "drops must be counted, never silent");
        assert_eq!(inbox.drain().dropped, 0, "the drop counter resets once reported");
    }

    #[test]
    fn close_queues_an_event_for_any_id() {
        let inbox = SharedInbox::default();
        inbox.close(4242);
        let drained = inbox.drain();
        assert_eq!(drained.events, vec![DaemonEvent::CloseRequested { id: 4242 }]);
    }

    #[test]
    fn a_poisoned_lock_does_not_become_a_permanent_blackout() {
        let inbox = SharedInbox::default();
        let poisoner = inbox.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("deliberate panic to poison the mutex");
        })
        .join();

        // The whole point: the daemon keeps working after a handler panic.
        let id = inbox.post(&req("after", "still working"), Instant::now());
        assert_eq!(id, 1);
        assert_eq!(inbox.drain().events.len(), 1);
    }
}
