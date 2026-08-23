// D6 (2026-08-23) — per-app flood guard for `org.freedesktop.Notifications`.
//
// ## The threat, stated plainly
//
// `Notify` is callable by any process that can reach the session bus, with no
// authentication and no cost. A buggy web app in a loop — or a hostile one —
// can call it thousands of times a second. Without a guard the 通知中心 grows
// without bound, the gpui repaint budget goes with it, and the operator's
// real approvals are pushed off the screen by whatever shouted loudest. This
// is the notification equivalent of the WP-A4-4 timer pile-up incident, and
// it deserves the same treatment: bound it structurally, do not hope.
//
// ## The policy, and what it trades away
//
// A fixed window per `app_name`: up to `MAX_PER_WINDOW` notifications may
// take a card of their own inside any `WINDOW`; beyond that, further calls
// from the same app are **merged** onto the most recent card that app owns —
// its text is replaced with the newest content and a "+N" counter goes up.
//
// Merging rather than dropping is the deliberate part. A dropped notification
// is a message the operator never learns existed, which is the exact failure
// D6 exists to end; a merged one keeps the newest content visible and says
// out loud how many it stands for. The sender is not lied to either: `Notify`
// still returns a valid id (the id of the card it merged onto), so a later
// `CloseNotification` on it still resolves.
//
// What it trades away: **inside a burst, older messages are not individually
// readable.** For a chat app that is a real loss — five rapid messages become
// "the newest one, +4". It is judged the better half of the trade against an
// unbounded list, and it is the "簡單版" the D6 brief explicitly asked for.
// A per-app grouped/expandable card (the mainstream answer) is a listed
// follow-up, not this round.
//
// A fixed window, not a sliding one: a token bucket or sliding log costs a
// per-app allocation that grows with the burst, which is the thing being
// defended against. A fixed window's worst case is 2×`MAX_PER_WINDOW` cards
// across a window boundary — a bound, which is all that is being asked for.
//
// Clock-injected (`now: Instant` is a parameter, never read internally) so
// every rule below is deterministically testable, same discipline
// `overlay/notifications_backoff.rs` already established in this crate.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Length of the fixed window.
pub(crate) const WINDOW: Duration = Duration::from_secs(10);

/// Cards a single app may open inside one window before merging starts.
pub(crate) const MAX_PER_WINDOW: usize = 5;

/// Hard cap on how many distinct `app_name`s are tracked at once.
///
/// The map is keyed by an attacker-chosen string, so it needs its own bound:
/// an app that sends every notification under a fresh random name would
/// otherwise grow this map without limit while never tripping the per-app
/// rule. When full, the least-recently-used entry is evicted — which at worst
/// grants one extra card to whoever gets evicted, and never fails open into
/// unbounded memory.
const MAX_TRACKED_APPS: usize = 64;

#[derive(Debug, Clone)]
struct AppWindow {
    started_at: Instant,
    last_seen: Instant,
    count: usize,
    /// Id of the newest card this app owns in the current window — the merge
    /// target once the budget is spent.
    representative: u32,
}

/// What the caller should do with a `Notify` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Give it a card of its own.
    Post,
    /// Fold it onto this already-visible card.
    Merge { onto: u32 },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FloodGuard {
    apps: HashMap<String, AppWindow>,
}

impl FloodGuard {
    /// Decides what happens to one incoming notification, and records it.
    ///
    /// `fresh_id` is the id the caller has already allocated for this call —
    /// it becomes the window's new merge target on a `Post`, and is simply
    /// discarded by the caller on a `Merge`.
    pub(crate) fn admit(&mut self, app_name: &str, now: Instant, fresh_id: u32) -> Verdict {
        self.evict_if_needed(now);

        match self.apps.get_mut(app_name) {
            Some(win) if now.duration_since(win.started_at) < WINDOW => {
                win.last_seen = now;
                // Saturating: a sustained flood must not overflow the counter
                // into a fresh budget.
                win.count = win.count.saturating_add(1);
                if win.count <= MAX_PER_WINDOW {
                    win.representative = fresh_id;
                    Verdict::Post
                } else {
                    Verdict::Merge { onto: win.representative }
                }
            }
            // No window, or the previous one has aged out — start a fresh one.
            _ => {
                self.apps.insert(
                    app_name.to_string(),
                    AppWindow { started_at: now, last_seen: now, count: 1, representative: fresh_id },
                );
                Verdict::Post
            }
        }
    }

    /// A `replaces_id` call bypasses the merge decision (it can only ever
    /// occupy the one card it is replacing, so it cannot grow the list) but
    /// still **consumes budget**, so an app cannot use a replace stream to
    /// keep its window counter from ever filling.
    pub(crate) fn note_replace(&mut self, app_name: &str, now: Instant, replaced_id: u32) {
        self.evict_if_needed(now);
        match self.apps.get_mut(app_name) {
            Some(win) if now.duration_since(win.started_at) < WINDOW => {
                win.last_seen = now;
                win.count = win.count.saturating_add(1);
            }
            _ => {
                self.apps.insert(
                    app_name.to_string(),
                    AppWindow { started_at: now, last_seen: now, count: 1, representative: replaced_id },
                );
            }
        }
    }

    /// Drops windows that have aged out, and — only if that was not enough —
    /// the least-recently-used entry.
    fn evict_if_needed(&mut self, now: Instant) {
        if self.apps.len() < MAX_TRACKED_APPS {
            return;
        }
        self.apps.retain(|_, win| now.duration_since(win.started_at) < WINDOW);
        while self.apps.len() >= MAX_TRACKED_APPS {
            let Some(victim) = self.apps.iter().min_by_key(|(_, w)| w.last_seen).map(|(k, _)| k.clone()) else {
                break;
            };
            self.apps.remove(&victim);
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked_apps(&self) -> usize {
        self.apps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_notifications_from_an_app_each_get_their_own_card() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for i in 0..MAX_PER_WINDOW {
            assert_eq!(g.admit("chat", t0, 100 + i as u32), Verdict::Post, "call {i} must get its own card");
        }
    }

    #[test]
    fn past_the_budget_calls_merge_onto_the_newest_card() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for i in 0..MAX_PER_WINDOW {
            g.admit("chat", t0, 100 + i as u32);
        }
        let last_posted = 100 + MAX_PER_WINDOW as u32 - 1;
        assert_eq!(g.admit("chat", t0, 999), Verdict::Merge { onto: last_posted });
        assert_eq!(g.admit("chat", t0, 1000), Verdict::Merge { onto: last_posted }, "the merge target must stay stable inside the window");
    }

    #[test]
    fn a_new_window_restores_the_full_budget() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for i in 0..=MAX_PER_WINDOW {
            g.admit("chat", t0, 100 + i as u32);
        }
        let later = t0 + WINDOW + Duration::from_millis(1);
        assert_eq!(g.admit("chat", later, 200), Verdict::Post);
    }

    #[test]
    fn one_noisy_app_never_spends_another_apps_budget() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for i in 0..50 {
            g.admit("noisy", t0, i);
        }
        assert_eq!(g.admit("quiet", t0, 900), Verdict::Post, "a quiet app must be unaffected by a loud neighbour");
    }

    #[test]
    fn a_replace_stream_consumes_budget_instead_of_resetting_it() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for _ in 0..MAX_PER_WINDOW {
            g.note_replace("progress", t0, 7);
        }
        // The budget is spent purely on replaces, so a genuinely NEW
        // notification from the same app in the same window merges.
        assert!(matches!(g.admit("progress", t0, 8), Verdict::Merge { .. }));
    }

    #[test]
    fn the_tracked_app_table_is_bounded_against_random_app_names() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        for i in 0..(MAX_TRACKED_APPS * 4) {
            g.admit(&format!("app-{i}"), t0, i as u32);
        }
        assert!(g.tracked_apps() <= MAX_TRACKED_APPS, "tracked apps must stay bounded, got {}", g.tracked_apps());
    }

    #[test]
    fn the_counter_saturates_rather_than_wrapping_into_a_fresh_budget() {
        let mut g = FloodGuard::default();
        let t0 = Instant::now();
        g.admit("chat", t0, 1);
        if let Some(win) = g.apps.get_mut("chat") {
            win.count = usize::MAX;
        }
        assert!(matches!(g.admit("chat", t0, 2), Verdict::Merge { .. }));
    }
}
