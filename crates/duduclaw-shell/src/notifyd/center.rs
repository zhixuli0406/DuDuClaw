// D6 (2026-08-23) — the notification-center store the 通知中心 panel renders.
//
// Pure `&mut self` state machine: no gpui types, no I/O, no clock read (every
// entry point that needs `now` takes it as a parameter). Same discipline
// `global_task.rs` and `home/running_windows.rs` state in their own headers,
// and the reason ~90% of D6 is unit-tested on a macOS host that cannot run a
// single line of `dbus.rs`.
//
// ## Who owns what
//
// The D-Bus side (`inbox.rs`) owns id allocation and the flood decision —
// both have to happen inside the handler, synchronously, before a reply goes
// out. THIS side owns everything with a lifetime: what is on screen, in what
// order, for how long, and which signals the operator's clicks owe the bus.
// Nothing here can block a caller, so it is free to hold the whole list.
//
// ## Signals are queued, never sent from here
//
// Every transition that the spec says must produce a `NotificationClosed` or
// an `ActionInvoked` pushes an `EmitCommand` into `outbox`. The caller drains
// it (`take_emits`) and hands the batch to the notifyd thread. Two reasons
// this indirection is worth it: emitting is I/O (a socket write) and must not
// happen on gpui's main thread, and keeping it a value makes "did dismissing
// actually tell the sender?" a plain assertion in a test rather than
// something only a live bus could answer.

use std::time::{Duration, Instant};

use super::inbox::Drained;
use super::{CloseReason, DaemonEvent, EmitCommand, NotificationAction, PostedNotification, Urgency, DEFAULT_ACTION_KEY};

/// Hard cap on cards held at once.
///
/// The flood guard bounds any single app; this bounds the sum of all of them.
/// Eviction takes the OLDEST and tells its sender (`CloseReason::Undefined` —
/// the spec has no "the center was full" code, and saying nothing would leave
/// a client waiting forever for a close that never comes).
const MAX_ITEMS: usize = 50;

/// How often the shell drains the inbox and retires expired cards.
///
/// One self-re-arming task at this cadence, NOT a fresh timer per render pass
/// — see `overlay::notifications::schedule_stale_check`'s own doc comment for
/// the WP-A4-4 pile-up this crate already had to fix once. 250ms is chosen to
/// sit under human perception for "a message just arrived" while costing one
/// uncontended mutex acquire per tick when nothing is happening; no repaint
/// is requested unless the data actually moved.
pub(crate) const DRAIN_INTERVAL: Duration = Duration::from_millis(250);

/// What happened to the shell's attempt to own `org.freedesktop.Notifications`.
///
/// Surfaced to the operator rather than kept as a debug detail: "third-party
/// apps cannot notify you" is exactly the class of silent failure E1a spent a
/// VM session root-causing, and it must never be invisible again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum DaemonState {
    /// Not attempted yet (macOS, or before the first drain tick).
    #[default]
    NotStarted,
    /// Connecting to the session bus / claiming the name is in flight.
    Starting,
    /// Owning the name, serving calls.
    Running,
    /// Another notification daemon already owns the name. Honest, and
    /// arguably fine — that daemon will show the notifications, just not in
    /// this panel — so it is reported as its own state, not as a failure.
    NameTaken,
    /// No session bus, or the connection failed. Third-party notifications
    /// are going nowhere; the message says so.
    Failed(String),
    /// This build has no D-Bus support compiled in (macOS dev loop).
    ///
    /// Constructed only by `NotifyRuntime`'s `#[cfg(not(target_os =
    /// "linux"))]` arms, but MATCHED on every platform (the panel's status
    /// banner is one `match` for all of them) — so on Linux the dead-code
    /// lint sees a variant nothing constructs. Allowed there specifically,
    /// rather than blanket-allowed, so a genuinely orphaned variant added
    /// later still warns.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unsupported,
}

/// Relative age of a card, resolved by the store so the UI does not have to
/// carry clock arithmetic into a render body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelativeAge {
    JustNow,
    Minutes(u32),
    Hours(u32),
    Days(u32),
}

impl RelativeAge {
    pub(crate) fn of(received_at: Instant, now: Instant) -> Self {
        let secs = now.saturating_duration_since(received_at).as_secs();
        match secs {
            0..=59 => RelativeAge::JustNow,
            60..=3599 => RelativeAge::Minutes((secs / 60) as u32),
            3600..=86_399 => RelativeAge::Hours((secs / 3600) as u32),
            _ => RelativeAge::Days((secs / 86_400) as u32),
        }
    }
}

/// One card in the center.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CenterNotification {
    pub(crate) id: u32,
    pub(crate) app_name: String,
    pub(crate) summary: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<NotificationAction>,
    pub(crate) urgency: Urgency,
    pub(crate) received_at: Instant,
    /// `None` == never expires (see `ExpirePolicy::resolve`).
    pub(crate) expires_at: Option<Instant>,
    /// How many further notifications the flood guard folded onto this card.
    /// `0` for an ordinary card; the UI renders "+N" only when it is not.
    pub(crate) merged: u32,
    /// A1 result-loopback (2026-08-24): `Some(task_id)` when THIS shell
    /// posted the card itself (`post_system`, below) about one of its own
    /// goal-task delegations — `None` for every third-party D-Bus card,
    /// which is the overwhelming majority. Two things key off this:
    /// `NotificationCenter::invoke` must not queue a D-Bus `ActionInvoked`
    /// signal for an id no real client ever sent (see that fn's own note),
    /// and `overlay/notifications_apps.rs`'s button click handler uses it to
    /// route a `sysact_retry`/`sysact_abort` click to `tasks.goal_decide`
    /// instead of the generic D-Bus emit path.
    pub(crate) system_task: Option<String>,
}

impl CenterNotification {
    fn from_posted(posted: PostedNotification, now: Instant) -> Self {
        let expires_at = match posted.expire {
            super::ExpirePolicy::Never => None,
            super::ExpirePolicy::After(d) => now.checked_add(d),
        };
        Self {
            id: posted.id,
            app_name: posted.app_name,
            summary: posted.summary,
            body: posted.body,
            actions: posted.actions,
            urgency: posted.urgency,
            received_at: now,
            expires_at,
            merged: 0,
            system_task: None,
        }
    }

    /// The action a click on the card body invokes, if the sender declared
    /// one. See `PostedNotification::default_action` — a card with no
    /// `default` action is display-only and must not fabricate one.
    pub(crate) fn default_action(&self) -> Option<&NotificationAction> {
        self.actions.iter().find(|a| a.key == DEFAULT_ACTION_KEY)
    }

    /// Buttons to draw: every action EXCEPT `default`, which is the card
    /// click itself and would otherwise be offered twice.
    pub(crate) fn button_actions(&self) -> impl Iterator<Item = &NotificationAction> {
        self.actions.iter().filter(|a| a.key != DEFAULT_ACTION_KEY)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NotificationCenter {
    /// Newest first — the order the panel draws them in.
    items: Vec<CenterNotification>,
    outbox: Vec<EmitCommand>,
    daemon: DaemonState,
    /// Single-slot claim for the one drain task. Same shape as
    /// `NotificationsFeed::try_arm_stale_timer`, and for the same reason.
    drain_armed: bool,
    /// Cumulative events lost to a full inbox queue. Reported once per
    /// increase, never silently.
    lost: u64,
    /// A1 result-loopback (2026-08-24): id allocator for `post_system`, kept
    /// entirely SEPARATE from `inbox::Inbox::allocate_id` (which is
    /// Linux-only — D-Bus does not exist on the macOS dev loop, but this
    /// shell's own result-loopback cards must still post there). Handed out
    /// with the top bit set (`SYSTEM_ID_BASE | n`) so a system card's id can
    /// never collide with a D-Bus one even in the same session, and vice
    /// versa — the two counters share nothing else, so keeping their ranges
    /// disjoint is the only thing standing between them.
    next_system_id: u32,
}

/// The top bit — see `next_system_id`'s own doc comment. `inbox::Inbox::
/// allocate_id` counts up from 1 and would need over two billion D-Bus
/// notifications in one session to ever reach this range.
const SYSTEM_ID_BASE: u32 = 0x8000_0000;

impl NotificationCenter {
    // ── the one drain task ────────────────────────────────────────────────

    /// Claims the single drain-task slot. `false` means one is already
    /// running and the caller must return immediately.
    ///
    /// There is deliberately no `disarm`: unlike
    /// `NotificationsFeed::try_arm_stale_timer` (whose task releases its slot
    /// during OOBE, because polling the gateway before there is a session is
    /// pointless), this task must run for the process's entire life. It is
    /// what starts the daemon in the first place, and a notification that
    /// only arrives while some particular surface happens to be on screen is
    /// not a notification. The loop ends exactly once — when the view is
    /// dropped, which takes the slot with it.
    pub(crate) fn try_arm_drain(&mut self) -> bool {
        if self.drain_armed {
            return false;
        }
        self.drain_armed = true;
        true
    }

    // ── daemon status ─────────────────────────────────────────────────────

    pub(crate) fn daemon(&self) -> &DaemonState {
        &self.daemon
    }

    /// Returns whether anything a surface draws changed — a status the panel
    /// shows, so a transition is a real repaint reason.
    pub(crate) fn set_daemon(&mut self, state: DaemonState) -> bool {
        if self.daemon == state {
            return false;
        }
        self.daemon = state;
        true
    }

    // ── inbound ───────────────────────────────────────────────────────────

    /// Applies one drained batch. Returns whether anything drawn changed.
    pub(crate) fn apply(&mut self, drained: Drained, now: Instant) -> bool {
        let mut changed = false;
        if drained.dropped > 0 {
            self.lost = self.lost.saturating_add(drained.dropped);
            eprintln!(
                "[notifyd] inbox overflowed: {} notification(s) discarded before the UI could read them (total {} this session)",
                drained.dropped, self.lost
            );
        }
        for event in drained.events {
            match event {
                DaemonEvent::Posted(posted) => {
                    self.insert_or_replace(*posted, now);
                    changed = true;
                }
                DaemonEvent::Merged { onto, posted } => {
                    self.merge_onto(onto, *posted, now);
                    changed = true;
                }
                DaemonEvent::CloseRequested { id } => {
                    changed |= self.close(id, CloseReason::Requested);
                }
            }
        }
        changed
    }

    fn insert_or_replace(&mut self, posted: PostedNotification, now: Instant) {
        let fresh = CenterNotification::from_posted(posted, now);
        if let Some(slot) = self.items.iter_mut().find(|c| c.id == fresh.id) {
            // `replaces_id` semantics: the card keeps its POSITION (so a
            // progress notification does not jump to the top on every
            // update) and its merge counter, but takes the new content and
            // the new expiry clock. Deliberately NO `NotificationClosed` is
            // emitted — the spec treats a replace as an update of a live
            // notification, not a close followed by a new one, and senders
            // that watch for the close would wrongly conclude the operator
            // dismissed it.
            let merged = slot.merged;
            *slot = fresh;
            slot.merged = merged;
            return;
        }
        self.items.insert(0, fresh);
        self.enforce_capacity();
    }

    fn merge_onto(&mut self, onto: u32, posted: PostedNotification, now: Instant) {
        if let Some(slot) = self.items.iter_mut().find(|c| c.id == onto) {
            let merged = slot.merged.saturating_add(1);
            let mut fresh = CenterNotification::from_posted(posted, now);
            fresh.merged = merged;
            *slot = fresh;
            return;
        }
        // The merge target is gone (the operator dismissed it in the window
        // between the handler's decision and this drain). Falling back to a
        // plain insert is the honest choice: the alternative is dropping a
        // message because of a race the sender had no part in.
        self.items.insert(0, CenterNotification::from_posted(posted, now));
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        while self.items.len() > MAX_ITEMS {
            // Oldest is last — `items` is newest-first.
            if let Some(evicted) = self.items.pop() {
                // A1 result-loopback: same "no real D-Bus sender to tell"
                // skip `close`'s own doc comment gives.
                if evicted.system_task.is_none() {
                    self.outbox.push(EmitCommand::Closed { id: evicted.id, reason: CloseReason::Undefined });
                }
            }
        }
    }

    // ── shell-originated cards (A1 result-loopback, 2026-08-24) ────────────

    /// Posts a card this shell wrote itself — a goal task this shell
    /// delegated (`crate::task_result`) reaching `done`/`failed`/
    /// `needs_human`. Deliberately a SEPARATE entry point from `apply`
    /// (which only ever consumes `DaemonEvent`s drained from the D-Bus
    /// inbox): this shell is not a D-Bus client of itself, there is no
    /// `NotifyRequest` to sanitize, and the id has to come from
    /// `next_system_id` (see that field's own doc comment), not `inbox::
    /// Inbox`. The SAME boundary rules still apply to the text, though — a
    /// task's `result_summary`/`judge_feedback` is agent-generated free
    /// text this shell did not author either, so `summary`/`body` are run
    /// through the identical `sanitize_line`/`sanitize_block` + length caps
    /// `NotifyRequest::sanitized` applies to a third-party D-Bus call.
    ///
    /// Lands at the FRONT of the list (newest-first, same as `insert_or_
    /// replace`) and persists until the operator dismisses or acts on it —
    /// no `expires_at` is ever set here, matching this shell's own policy
    /// for `-1`/server-default D-Bus cards (`ExpirePolicy::resolve`'s own
    /// doc comment: there is no banner layer, so an unattended timeout would
    /// be exactly the silent-loss failure D6 exists to prevent — doubly true
    /// for a card the operator explicitly delegated work through).
    ///
    /// Returns the id, so a caller that wants to reference the card later
    /// (none does yet) can.
    pub(crate) fn post_system(
        &mut self,
        app_name: &str,
        summary: &str,
        body: &str,
        urgency: super::Urgency,
        actions: Vec<NotificationAction>,
        system_task: Option<String>,
    ) -> u32 {
        self.next_system_id = self.next_system_id.wrapping_add(1);
        let id = SYSTEM_ID_BASE | self.next_system_id;
        let card = CenterNotification {
            id,
            app_name: super::sanitize_line(app_name, super::MAX_APP_NAME_CHARS),
            summary: super::sanitize_line(summary, super::MAX_SUMMARY_CHARS),
            body: super::sanitize_block(body, super::MAX_BODY_CHARS),
            actions,
            urgency,
            received_at: Instant::now(),
            expires_at: None,
            merged: 0,
            system_task,
        };
        self.items.insert(0, card);
        self.enforce_capacity();
        id
    }

    // ── time ──────────────────────────────────────────────────────────────

    /// Retires every card whose `expire_timeout` has elapsed, telling each
    /// sender why (`CloseReason::Expired`). Returns whether anything changed.
    pub(crate) fn expire_due(&mut self, now: Instant) -> bool {
        let mut expired = Vec::new();
        self.items.retain(|card| match card.expires_at {
            Some(at) if at <= now => {
                expired.push(card.id);
                false
            }
            _ => true,
        });
        if expired.is_empty() {
            return false;
        }
        for id in expired {
            self.outbox.push(EmitCommand::Closed { id, reason: CloseReason::Expired });
        }
        true
    }

    // ── operator actions ──────────────────────────────────────────────────

    /// Operator dismissed one card.
    pub(crate) fn dismiss(&mut self, id: u32) -> bool {
        self.close(id, CloseReason::Dismissed)
    }

    /// Operator cleared the whole list.
    pub(crate) fn dismiss_all(&mut self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        for card in self.items.drain(..) {
            self.outbox.push(EmitCommand::Closed { id: card.id, reason: CloseReason::Dismissed });
        }
        true
    }

    /// Operator activated one of the card's action buttons, or the card
    /// itself (`DEFAULT_ACTION_KEY`).
    ///
    /// Emits `ActionInvoked` and then closes the card with
    /// `CloseReason::Dismissed`. The spec is deliberately silent on whether a
    /// server should close after an action ("clients should not assume"),
    /// and every mainstream daemon closes — which is also the behavior that
    /// matches what the operator just did: they answered the notification, so
    /// it should stop asking.
    ///
    /// Refuses an action key the card never declared: the key goes straight
    /// out on the bus, and a UI bug must not be able to invent one.
    ///
    /// A1 result-loopback (2026-08-24): a card this shell posted itself
    /// (`card.system_task.is_some()`, see that field's own doc comment) has
    /// no real D-Bus sender waiting on `ActionInvoked` — queuing the signal
    /// anyway would be harmless (nothing holds that id) but is still a
    /// pointless wakeup of the notifyd thread for a broadcast nobody can
    /// hear, so it is skipped for system cards specifically.
    pub(crate) fn invoke(&mut self, id: u32, action_key: &str) -> bool {
        let Some(card) = self.items.iter().find(|c| c.id == id) else {
            return false;
        };
        if !card.actions.iter().any(|a| a.key == action_key) {
            return false;
        }
        if card.system_task.is_none() {
            self.outbox.push(EmitCommand::ActionInvoked { id, action_key: action_key.to_string() });
        }
        self.close(id, CloseReason::Dismissed)
    }

    /// A click on the card body. A no-op (not a dismissal) when the sender
    /// declared no `default` action — clicking must never silently throw a
    /// message away.
    pub(crate) fn invoke_default(&mut self, id: u32) -> bool {
        let has_default = self.items.iter().find(|c| c.id == id).is_some_and(|c| c.default_action().is_some());
        if !has_default {
            return false;
        }
        self.invoke(id, DEFAULT_ACTION_KEY)
    }

    /// A1 result-loopback (2026-08-24): skips the `Closed` emit for a
    /// `system_task` card, same reasoning `invoke`'s own doc comment gives
    /// for skipping `ActionInvoked` — there is no real D-Bus sender on the
    /// other end of that signal either way, for ANY reason a system card
    /// closes (an explicit dismiss, `invoke`, capacity eviction, or —
    /// though `post_system` never sets an expiry — `expire_due`).
    fn close(&mut self, id: u32, reason: CloseReason) -> bool {
        let Some(pos) = self.items.iter().position(|c| c.id == id) else {
            return false;
        };
        let removed = self.items.remove(pos);
        if removed.system_task.is_none() {
            self.outbox.push(EmitCommand::Closed { id, reason });
        }
        true
    }

    // ── outbound ──────────────────────────────────────────────────────────

    pub(crate) fn take_emits(&mut self) -> Vec<EmitCommand> {
        std::mem::take(&mut self.outbox)
    }

    // ── reads ─────────────────────────────────────────────────────────────

    pub(crate) fn items(&self) -> &[CenterNotification] {
        &self.items
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ExpirePolicy, NotifyRequest};
    use super::*;

    fn posted(id: u32, app: &str, summary: &str) -> PostedNotification {
        NotifyRequest { app_name: app.into(), summary: summary.into(), expire_timeout: -1, ..Default::default() }.sanitized(id)
    }

    fn batch(events: Vec<DaemonEvent>) -> Drained {
        Drained { events, dropped: 0 }
    }

    #[test]
    fn a_posted_notification_lands_newest_first() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        assert!(c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(1, "a", "first")))]), t0));
        assert!(c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(2, "a", "second")))]), t0));
        assert_eq!(c.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 1]);
    }

    #[test]
    fn a_replace_updates_in_place_and_emits_no_close() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(1, "a", "old")))]), t0);
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(2, "a", "other")))]), t0);
        c.take_emits();

        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(1, "a", "new")))]), t0);
        assert_eq!(c.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 1], "a replace must not reorder the list");
        assert_eq!(c.items()[1].summary, "new");
        assert!(c.take_emits().is_empty(), "a replace is an update, not a close");
    }

    #[test]
    fn a_merge_bumps_the_counter_and_shows_the_newest_text() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(1, "chat", "msg 1")))]), t0);
        c.apply(batch(vec![DaemonEvent::Merged { onto: 1, posted: Box::new(posted(1, "chat", "msg 2")) }]), t0);
        c.apply(batch(vec![DaemonEvent::Merged { onto: 1, posted: Box::new(posted(1, "chat", "msg 3")) }]), t0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.items()[0].merged, 2);
        assert_eq!(c.items()[0].summary, "msg 3");
    }

    #[test]
    fn a_merge_onto_a_dismissed_card_still_shows_the_message() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Merged { onto: 77, posted: Box::new(posted(77, "chat", "raced")) }]), t0);
        assert_eq!(c.len(), 1, "losing a race must not lose a message");
    }

    #[test]
    fn close_notification_removes_the_card_and_answers_with_reason_three() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(9, "a", "x")))]), t0);
        c.take_emits();
        assert!(c.apply(batch(vec![DaemonEvent::CloseRequested { id: 9 }]), t0));
        assert!(c.is_empty());
        assert_eq!(c.take_emits(), vec![EmitCommand::Closed { id: 9, reason: CloseReason::Requested }]);
    }

    #[test]
    fn closing_an_unknown_id_changes_nothing_and_emits_nothing() {
        let mut c = NotificationCenter::default();
        assert!(!c.apply(batch(vec![DaemonEvent::CloseRequested { id: 1234 }]), Instant::now()));
        assert!(c.take_emits().is_empty());
    }

    #[test]
    fn dismissing_tells_the_sender_it_was_the_user() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(3, "a", "x")))]), t0);
        c.take_emits();
        assert!(c.dismiss(3));
        assert_eq!(c.take_emits(), vec![EmitCommand::Closed { id: 3, reason: CloseReason::Dismissed }]);
        assert!(!c.dismiss(3), "dismissing twice must be a no-op");
    }

    #[test]
    fn expiry_only_retires_cards_that_asked_for_it() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        let mut timed = posted(1, "a", "timed");
        timed.expire = ExpirePolicy::After(Duration::from_millis(500));
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(timed)), DaemonEvent::Posted(Box::new(posted(2, "a", "persistent")))]), t0);
        c.take_emits();

        assert!(!c.expire_due(t0 + Duration::from_millis(400)), "nothing is due yet");
        assert!(c.expire_due(t0 + Duration::from_millis(600)));
        assert_eq!(c.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(c.take_emits(), vec![EmitCommand::Closed { id: 1, reason: CloseReason::Expired }]);
    }

    #[test]
    fn a_default_action_click_emits_action_invoked_then_closes() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        let p = NotifyRequest {
            app_name: "chat".into(),
            summary: "hi".into(),
            actions: vec!["default".into(), "Open".into()],
            expire_timeout: -1,
            ..Default::default()
        }
        .sanitized(5);
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(p))]), t0);
        c.take_emits();

        assert!(c.invoke_default(5));
        assert_eq!(
            c.take_emits(),
            vec![
                EmitCommand::ActionInvoked { id: 5, action_key: "default".into() },
                EmitCommand::Closed { id: 5, reason: CloseReason::Dismissed },
            ]
        );
        assert!(c.is_empty());
    }

    #[test]
    fn clicking_a_card_with_no_default_action_does_nothing_at_all() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(6, "a", "plain")))]), t0);
        c.take_emits();
        assert!(!c.invoke_default(6), "no default action was declared");
        assert!(c.take_emits().is_empty(), "a click must not invent an action the app never offered");
        assert_eq!(c.len(), 1, "and must not throw the message away either");
    }

    #[test]
    fn an_action_key_the_card_never_declared_is_refused() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(7, "a", "plain")))]), t0);
        assert!(!c.invoke(7, "delete-everything"));
        assert!(c.take_emits().is_empty());
    }

    #[test]
    fn button_actions_exclude_the_default_one() {
        let p = NotifyRequest {
            actions: vec!["default".into(), "Open".into(), "reply".into(), "Reply".into()],
            expire_timeout: -1,
            ..Default::default()
        }
        .sanitized(1);
        let card = CenterNotification::from_posted(p, Instant::now());
        let keys: Vec<&str> = card.button_actions().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["reply"], "the default action is the card click, not a second button");
    }

    #[test]
    fn the_list_is_capped_and_evictions_are_announced() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        for id in 1..=(MAX_ITEMS as u32 + 3) {
            c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(id, "a", "x")))]), t0);
        }
        assert_eq!(c.len(), MAX_ITEMS);
        let closed: Vec<u32> = c
            .take_emits()
            .into_iter()
            .filter_map(|e| match e {
                EmitCommand::Closed { id, reason: CloseReason::Undefined } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(closed, vec![1, 2, 3], "the three oldest were evicted and their senders were told");
    }

    #[test]
    fn dismiss_all_clears_and_answers_every_sender() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        for id in 1..=3 {
            c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(id, "a", "x")))]), t0);
        }
        c.take_emits();
        assert!(c.dismiss_all());
        assert!(c.is_empty());
        assert_eq!(c.take_emits().len(), 3);
        assert!(!c.dismiss_all(), "clearing an empty list is a no-op");
    }

    /// The WP-A4-4 pile-up guard: N render passes spawning N drain tasks must
    /// leave exactly ONE of them running.
    #[test]
    fn the_drain_slot_admits_exactly_one_claimant_and_never_reopens() {
        let mut c = NotificationCenter::default();
        assert!(c.try_arm_drain());
        for _ in 0..10 {
            assert!(!c.try_arm_drain(), "a second drain task must never start");
        }
    }

    #[test]
    fn a_daemon_state_change_is_a_repaint_reason_but_a_repeat_is_not() {
        let mut c = NotificationCenter::default();
        assert!(c.set_daemon(DaemonState::Running));
        assert!(!c.set_daemon(DaemonState::Running));
        assert!(c.set_daemon(DaemonState::NameTaken));
    }

    #[test]
    fn relative_age_buckets_the_way_the_panel_reads_it() {
        let t0 = Instant::now();
        assert_eq!(RelativeAge::of(t0, t0 + Duration::from_secs(5)), RelativeAge::JustNow);
        assert_eq!(RelativeAge::of(t0, t0 + Duration::from_secs(125)), RelativeAge::Minutes(2));
        assert_eq!(RelativeAge::of(t0, t0 + Duration::from_secs(7300)), RelativeAge::Hours(2));
        assert_eq!(RelativeAge::of(t0, t0 + Duration::from_secs(200_000)), RelativeAge::Days(2));
    }

    /// A card whose sender asked for a timeout and one that did not must age
    /// independently — the mixed case is what a real desktop looks like.
    #[test]
    fn a_persistent_card_survives_an_arbitrarily_later_tick() {
        let mut c = NotificationCenter::default();
        let t0 = Instant::now();
        let mut soon = posted(1, "a", "soon");
        soon.expire = ExpirePolicy::After(Duration::from_secs(5));
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(soon)), DaemonEvent::Posted(Box::new(posted(2, "a", "forever")))]), t0);
        c.expire_due(t0 + Duration::from_secs(6));
        c.expire_due(t0 + Duration::from_secs(60 * 60 * 24 * 7));
        assert_eq!(c.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
    }

    // ── A1 result-loopback: post_system ────────────────────────────────────

    #[test]
    fn post_system_lands_newest_first_and_never_expires() {
        let mut c = NotificationCenter::default();
        let a = c.post_system("DuDuClaw", "「寄出報價單」已完成", "已寄出，對方已回覆收到", Urgency::Normal, vec![], Some("t1".to_string()));
        let b = c.post_system("DuDuClaw", "「整理報告」失敗", "缺少附件", Urgency::Normal, vec![], Some("t2".to_string()));
        assert_eq!(c.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![b, a]);
        assert!(c.items().iter().all(|i| i.expires_at.is_none()), "a task-result card must persist until acted on");
    }

    #[test]
    fn post_system_ids_never_collide_with_dbus_ids_even_after_many_of_each() {
        let mut c = NotificationCenter::default();
        // A run of ordinary D-Bus posts (small ids, starting at 1).
        for id in 1..=5 {
            c.apply(batch(vec![DaemonEvent::Posted(Box::new(posted(id, "chromium", "x")))]), Instant::now());
        }
        let dbus_ids: std::collections::HashSet<u32> = c.items().iter().map(|i| i.id).collect();
        let sys_id = c.post_system("DuDuClaw", "s", "b", Urgency::Normal, vec![], None);
        assert!(!dbus_ids.contains(&sys_id), "a system id must never land in the D-Bus id range");
        assert!(sys_id >= SYSTEM_ID_BASE, "system ids must live in the reserved high range");
    }

    #[test]
    fn post_system_sanitizes_the_same_way_a_dbus_notify_call_does() {
        let mut c = NotificationCenter::default();
        c.post_system("DuDuClaw", "hello\nthere", "line\u{0007}one", Urgency::Normal, vec![], None);
        let card = &c.items()[0];
        assert_eq!(card.summary, "hello there", "the summary must be forced onto one line, same as a D-Bus card");
        assert_eq!(card.body, "lineone", "control characters must be stripped, same as a D-Bus card");
    }

    #[test]
    fn invoking_a_system_cards_action_closes_it_without_queuing_a_phantom_dbus_signal() {
        let mut c = NotificationCenter::default();
        let action = NotificationAction { key: "sysact_retry".to_string(), label: "重試".to_string() };
        let id = c.post_system("DuDuClaw", "s", "b", Urgency::Normal, vec![action], Some("t1".to_string()));
        assert!(c.invoke(id, "sysact_retry"));
        assert!(!c.items().iter().any(|i| i.id == id), "the card must close on invoke, same as a D-Bus card");
        assert!(c.take_emits().is_empty(), "a system card has no real D-Bus sender — nothing should be queued for the bus");
    }

    #[test]
    fn invoking_a_dbus_cards_action_still_queues_the_real_signal() {
        // Regression guard for the branch added alongside `post_system`:
        // ordinary D-Bus cards (`system_task: None`) must be byte-identical
        // to before this round.
        let mut c = NotificationCenter::default();
        let mut p = posted(1, "chromium", "x");
        p.actions = vec![NotificationAction { key: "reply".to_string(), label: "Reply".to_string() }];
        c.apply(batch(vec![DaemonEvent::Posted(Box::new(p))]), Instant::now());
        assert!(c.invoke(1, "reply"));
        assert_eq!(c.take_emits(), vec![EmitCommand::ActionInvoked { id: 1, action_key: "reply".to_string() }, EmitCommand::Closed { id: 1, reason: CloseReason::Dismissed }]);
    }
}
