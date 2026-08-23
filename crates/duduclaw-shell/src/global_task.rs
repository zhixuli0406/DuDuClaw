// A1 (2026-08-23) — the global 交辦欄 trigger feed.
//
// ## Why a poll at all
//
// The task bar must open on Super+K *no matter what has keyboard focus* —
// including a third-party application window, which is the whole point of
// "全域". At that moment this shell's own `cmd-k` binding cannot possibly
// fire: the key events are going to the focused client, not to us. So the
// compositor has to own the hotkey. That is not a workaround, it is the
// standard split in the wlroots ecosystem — panels do not register global
// hotkeys, compositors do.
//
// Which leaves comp needing to tell us. Comp's shell-control socket is a
// one-shot connect/request/response/close channel served by a SINGLE
// sequential listener thread (see comp's own `shell_control/listener.rs`
// module doc: it handles each connection to completion before `accept()`ing
// the next). A long-poll or a persistent event stream would therefore wedge
// every other caller — the dock's `list_windows` included — behind one idle
// connection. So: comp queues, we drain.
//
// The cost is polling latency on the ⌘K path. It is a real, recorded
// tradeoff, not an oversight — see `POLL_INTERVAL` for the number and
// `BACKOFF_INTERVAL` for what stops it from being a busy-loop against a
// compositor that isn't there.
//
// ## Shape
//
// Same discipline as `home/running_windows.rs`'s `RunningWindowsFeed` (read
// that file's header first — this one deliberately mirrors it): pure
// `&mut self` mutation, no gpui types anywhere, no I/O performed here. The
// caller does the `std::thread::spawn` + `cx.spawn` bridge and hands
// already-settled results back through `apply_ok`/`apply_err`. That is what
// makes the whole state machine testable in a crate with no headless
// UI-click harness.

use std::time::{Duration, Instant};

use crate::comp_client::ShellIntent;

/// Poll cadence while comp is answering.
///
/// Much tighter than `running_windows::POLL_INTERVAL` (3s) because this one
/// sits directly in front of a **human keypress**: the operator hits Super+K
/// and waits for a bar to appear. 3s would feel broken; 200ms reads as
/// "instant enough" while still being only five local socket round trips a
/// second, each one a few hundred microseconds of work on the same host.
///
/// This is the honest upper bound on ⌘K latency, and the one number to
/// revisit if the live VM run says it still feels sluggish.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Poll cadence after comp has refused or failed to answer.
///
/// Without this, a shell running where no compositor exists — a dev Mac, a
/// `weston` headless container, or simply a comp build predating
/// `take_shell_intents` — would hammer a missing socket five times a second
/// for the entire session. Backing off to 5s keeps the recovery path alive
/// (comp restarting mid-session is picked up within 5s) at ~1/25th the idle
/// cost.
pub(crate) const BACKOFF_INTERVAL: Duration = Duration::from_secs(5);

/// How many consecutive failures before the backoff cadence applies.
///
/// `1` — i.e. immediately. There is no "transient blip" worth burning 5Hz
/// on: the *successful* path re-arms the fast cadence on its very next
/// answer, so a single stray error costs at most one 5s window, while a
/// permanently-absent comp costs nothing.
const FAILURES_BEFORE_BACKOFF: u32 = 1;

/// Drives the Super+K poll and holds whatever comp has handed over that the
/// shell has not acted on yet.
///
/// `Default` is the state the shell boots into: never polled, nothing
/// pending, fast cadence armed (so the first poll happens immediately rather
/// than one interval in).
#[derive(Debug, Clone, Default)]
pub(crate) struct GlobalTaskIntentFeed {
    /// Intents received but not yet acted on. Bounded implicitly by the fact
    /// that `take_pending` is called every render pass; comp's own queue is
    /// explicitly bounded on its side.
    pending: Vec<ShellIntent>,
    busy: bool,
    last_polled_at: Option<Instant>,
    consecutive_failures: u32,
    /// Set once comp answers `{"ok":false,…}` to this op, which is how a
    /// comp build that predates it identifies itself. See [`Self::give_up`].
    unsupported: bool,
}

impl GlobalTaskIntentFeed {
    /// The cadence currently in force — fast while comp is answering, backed
    /// off once it isn't.
    pub(crate) fn interval(&self) -> Duration {
        if self.consecutive_failures >= FAILURES_BEFORE_BACKOFF {
            BACKOFF_INTERVAL
        } else {
            POLL_INTERVAL
        }
    }

    /// Whether a poll is due. Always true before the first one.
    ///
    /// Permanently false once [`Self::give_up`] has been called — there is
    /// nothing to recover from a comp that does not implement the op, and
    /// continuing to ask would be a busy-loop with a guaranteed answer.
    pub(crate) fn is_stale(&self) -> bool {
        if self.unsupported {
            return false;
        }
        match self.last_polled_at {
            None => true,
            Some(t) => t.elapsed() >= self.interval(),
        }
    }

    /// `true` if a poll was actually started and the caller must now dispatch
    /// one; `false` if one is already in flight or none is due.
    ///
    /// Single-flight, same contract `RunningWindowsFeed::begin_refresh`
    /// establishes: a slow comp must not accumulate a pile of overlapping
    /// requests against a socket that serves them one at a time anyway.
    pub(crate) fn begin_poll(&mut self) -> bool {
        if self.busy || !self.is_stale() {
            return false;
        }
        self.busy = true;
        true
    }

    /// A poll came back with comp's drained queue (possibly empty).
    ///
    /// Success re-arms the fast cadence, so one transient failure never
    /// leaves the shell stuck at the slow one.
    pub(crate) fn apply_ok(&mut self, intents: Vec<ShellIntent>) {
        self.busy = false;
        self.last_polled_at = Some(Instant::now());
        self.consecutive_failures = 0;
        self.pending.extend(intents);
    }

    /// A poll failed (comp not running, I/O error, timeout, protocol error).
    ///
    /// Deliberately does NOT clear `pending`: a failure to ask about NEW
    /// intents says nothing about ones already received and not yet acted on.
    pub(crate) fn apply_err(&mut self) {
        self.busy = false;
        self.last_polled_at = Some(Instant::now());
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// Comp explicitly refused the op — it is running a build that does not
    /// have it. Stops polling for the rest of the session.
    ///
    /// Separate from [`Self::apply_err`] on purpose: "comp isn't there" is
    /// recoverable (it may start later) and stays on the backoff cadence;
    /// "comp is there and has never heard of this op" is not, because the
    /// answer cannot change without comp restarting — and a comp restart
    /// takes this shell's whole Wayland connection with it, so the process
    /// would not survive to benefit.
    pub(crate) fn give_up(&mut self) {
        self.busy = false;
        self.last_polled_at = Some(Instant::now());
        self.unsupported = true;
    }

    /// Removes and returns every intent received since the last call.
    ///
    /// Take-once semantics: an intent represents an action to perform, so
    /// leaving it in place after acting would re-trigger it on the next
    /// render pass.
    pub(crate) fn take_pending(&mut self) -> Vec<ShellIntent> {
        std::mem::take(&mut self.pending)
    }

    /// Convenience for the one intent that exists today: did the operator
    /// ask for the global task bar since we last looked?
    ///
    /// Collapses a burst (several Super+K presses inside one poll window)
    /// into a single open — pressing it three times quickly must not queue
    /// three openings of a surface that can only be open once.
    pub(crate) fn take_task_bar_request(&mut self) -> bool {
        self.take_pending().into_iter().any(|i| i == ShellIntent::GlobalTaskBar)
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_feed_is_stale_so_the_first_poll_happens_immediately() {
        let f = GlobalTaskIntentFeed::default();
        assert!(f.is_stale());
        assert!(!f.is_busy());
        assert_eq!(f.interval(), POLL_INTERVAL);
    }

    #[test]
    fn begin_poll_marks_busy_and_is_single_flight() {
        let mut f = GlobalTaskIntentFeed::default();
        assert!(f.begin_poll(), "the first poll must start");
        assert!(f.is_busy());
        assert!(!f.begin_poll(), "a second poll must not start while one is in flight");
    }

    #[test]
    fn a_successful_poll_clears_busy_and_keeps_the_fast_cadence() {
        let mut f = GlobalTaskIntentFeed::default();
        f.begin_poll();
        f.apply_ok(vec![]);
        assert!(!f.is_busy());
        assert_eq!(f.interval(), POLL_INTERVAL);
        // Just polled, so not due again yet.
        assert!(!f.is_stale());
    }

    #[test]
    fn a_failed_poll_backs_the_cadence_off() {
        let mut f = GlobalTaskIntentFeed::default();
        f.begin_poll();
        f.apply_err();
        assert!(!f.is_busy());
        assert_eq!(f.interval(), BACKOFF_INTERVAL);
    }

    #[test]
    fn one_success_re_arms_the_fast_cadence_after_failures() {
        let mut f = GlobalTaskIntentFeed::default();
        f.begin_poll();
        f.apply_err();
        f.apply_err();
        assert_eq!(f.interval(), BACKOFF_INTERVAL);
        f.apply_ok(vec![]);
        assert_eq!(f.interval(), POLL_INTERVAL, "a good answer must restore responsiveness");
    }

    #[test]
    fn intents_accumulate_until_taken() {
        let mut f = GlobalTaskIntentFeed::default();
        f.apply_ok(vec![ShellIntent::GlobalTaskBar]);
        f.apply_ok(vec![ShellIntent::GlobalTaskBar]);
        assert_eq!(f.pending_len(), 2);
        assert_eq!(f.take_pending(), vec![ShellIntent::GlobalTaskBar; 2]);
        assert_eq!(f.pending_len(), 0, "taking must clear");
    }

    #[test]
    fn take_task_bar_request_collapses_a_burst_into_one_open() {
        let mut f = GlobalTaskIntentFeed::default();
        f.apply_ok(vec![ShellIntent::GlobalTaskBar; 3]);
        assert!(f.take_task_bar_request());
        assert!(!f.take_task_bar_request(), "a taken request must not re-fire");
    }

    #[test]
    fn take_task_bar_request_is_false_when_nothing_arrived() {
        let mut f = GlobalTaskIntentFeed::default();
        assert!(!f.take_task_bar_request());
        f.apply_ok(vec![]);
        assert!(!f.take_task_bar_request());
    }

    #[test]
    fn a_failed_poll_does_not_discard_already_received_intents() {
        let mut f = GlobalTaskIntentFeed::default();
        f.apply_ok(vec![ShellIntent::GlobalTaskBar]);
        f.begin_poll();
        f.apply_err();
        assert!(f.take_task_bar_request(), "a later failure must not swallow an earlier request");
    }

    #[test]
    fn giving_up_stops_polling_permanently() {
        let mut f = GlobalTaskIntentFeed::default();
        f.begin_poll();
        f.give_up();
        assert!(!f.is_stale(), "an unsupported op must never be polled again");
        assert!(!f.begin_poll(), "and begin_poll must refuse to start one");
    }

    #[test]
    fn giving_up_still_lets_previously_received_intents_be_acted_on() {
        let mut f = GlobalTaskIntentFeed::default();
        f.apply_ok(vec![ShellIntent::GlobalTaskBar]);
        f.give_up();
        assert!(f.take_task_bar_request());
    }

    #[test]
    fn failure_counter_saturates_rather_than_overflowing() {
        let mut f = GlobalTaskIntentFeed::default();
        f.consecutive_failures = u32::MAX;
        f.apply_err();
        assert_eq!(f.consecutive_failures, u32::MAX);
        assert_eq!(f.interval(), BACKOFF_INTERVAL);
    }
}
