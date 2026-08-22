// Real "執行中視窗" (running windows) feed for the dock — WP-comp-shell-ipc
// (2026-08-22). Replaces A3's blank gap: `home_dock.rs`'s dock icons used
// to only ever LAUNCH an app (`crate::apps::launch`), with no concept of
// "is this app already running" at all (A3's own header comment: "其他
// dock icon 維持靜態"). This struct holds the polled `comp_client::
// list_windows` result and exposes a small query surface
// (`is_app_running`) that `home_dock.rs` uses to (a) show a running-
// indicator dot on a dock icon and (b) decide whether a click should FOCUS
// the existing window (`comp_client::focus_window`) instead of launching a
// new instance.
//
// Same "pure &mut self mutation, no gpui types" discipline `overlay::
// notifications_feed::NotificationsFeed`'s own header comment establishes
// — the actual comp I/O is dispatched from `home_dock.rs` via a
// `std::thread::spawn` + `cx.spawn` poll bridge, the same pattern
// `overlay/notifications.rs` already established for the gateway. This
// struct only ever receives already-settled results through
// `apply_list_ok`/`apply_list_err`, never performs I/O itself.
//
// ── Matching key (APP-1, 2026-08-22) ────────────────────────────────────
// The query is `apps::installed::InstalledApp::xdg_app_id()` — the flatpak
// application id for a flatpak app, the desktop-file id for a native one.
// Both rest on the SAME freedesktop convention (a well-behaved app sets its
// xdg-shell app_id to its desktop-file basename, and flatpak's own window
// integration relies on that basename being the application id), and it is
// a convention, NOT a guarantee: an app that sets something else simply
// never matches, which shows up as a missing running-indicator dot and a
// click that launches a second instance — never as a dot on the wrong app.
//
// Before APP-1 this read `fake_data::DockApp::flatpak_id` — an `Option` on
// a canned six-entry array where exactly one entry was non-`None`, so in
// practice only Chromium could ever match. That array is gone; every dock
// tile is now a real installed app carrying a real id.

use std::time::{Duration, Instant};

use crate::comp_client::CompWindow;

/// Poll cadence — comp round trips are a local socket call (comp's own
/// `MAIN_THREAD_REPLY_TIMEOUT`/`REQUEST_READ_TIMEOUT` are 3s WORST case,
/// not the common case), so this can run much tighter than the
/// Notifications feed's 30s network-bound cadence — a running-indicator dot
/// that lags several seconds behind reality would make for a worse dock
/// than one that costs a few extra idle local socket round trips.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FeedStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    Offline,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunningWindowsFeed {
    windows: Vec<CompWindow>,
    status: FeedStatus,
    busy: bool,
    last_refreshed_at: Option<Instant>,
}

impl RunningWindowsFeed {
    pub(crate) fn is_stale(&self) -> bool {
        match self.last_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= POLL_INTERVAL,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    /// `true` if a fetch was actually started (the caller must now dispatch
    /// one) — `false` if one is already in flight, same single-flight
    /// contract `NotificationsFeed::begin_refresh`'s own doc comment
    /// establishes.
    pub(crate) fn begin_refresh(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        if self.status == FeedStatus::Idle {
            self.status = FeedStatus::Loading;
        }
        true
    }

    /// Returns whether anything the DOCK actually draws changed. The dock's
    /// only reader of this feed is `is_app_running` (a running-indicator
    /// dot per icon), so the answer is exactly "did the window list move" —
    /// `status`/`busy` have no on-screen representation here at all
    /// (`status()` is `#[cfg(test)]`).
    ///
    /// WP-A4-4 (2026-08-22): before this returned anything, `home_dock.rs`
    /// notified unconditionally on every settle, so a machine whose
    /// `duduclaw-comp` is not running at all — which is exactly the
    /// appliance's `cage` fallback session, where this shell is cage's only
    /// client and there is no comp to ask — repainted the whole window
    /// twice every 3 seconds forever to redraw an unchanged dock. See
    /// `overlay/notifications_backoff.rs`'s header comment for the wider
    /// post-mortem this belongs to.
    pub(crate) fn apply_list_ok(&mut self, windows: Vec<CompWindow>) -> bool {
        let changed = windows != self.windows;
        self.windows = windows;
        self.status = FeedStatus::Ready;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
        changed
    }

    /// Deliberately does NOT clear `windows`/downgrade to an empty list on
    /// a transient failure — same "keep showing the last known state
    /// rather than blanking everything" reasoning `NotificationsFeed`'s own
    /// decided-history section documents for a settled row staying
    /// visible; a dock whose running-indicator dots all flicker off on one
    /// missed poll (e.g. comp briefly restarting) would be worse than
    /// briefly-stale dots. `last_refreshed_at` still advances so the next
    /// poll tick retries on the normal cadence rather than backing off
    /// forever — see `is_stale`.
    pub(crate) fn apply_list_err(&mut self) {
        self.status = FeedStatus::Offline;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> FeedStatus {
        self.status
    }

    /// True iff any currently known window's `app_id` exactly equals
    /// `app_id` — see this file's module doc for the `flatpak_id`-as-query
    /// assumption. Empty `app_id` never matches anything (a dock entry
    /// with no real identity — `flatpak_id: None` — passes `""` here, and
    /// an honest app_id-less window can never be "found running" either).
    pub(crate) fn is_app_running(&self, app_id: &str) -> bool {
        if app_id.is_empty() {
            return false;
        }
        self.windows.iter().any(|w| w.app_id.as_deref() == Some(app_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(app_id: &str) -> CompWindow {
        CompWindow { app_id: Some(app_id.to_string()), title: Some(app_id.to_string()), focused: false }
    }

    #[test]
    fn fresh_feed_is_stale_and_idle() {
        let feed = RunningWindowsFeed::default();
        assert!(feed.is_stale());
        assert_eq!(feed.status(), FeedStatus::Idle);
        assert!(!feed.is_busy());
    }

    #[test]
    fn begin_refresh_flips_busy_and_idle_to_loading_once() {
        let mut feed = RunningWindowsFeed::default();
        assert!(feed.begin_refresh());
        assert!(feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Loading);
        // Already busy — a second concurrent caller must not start another.
        assert!(!feed.begin_refresh());
    }

    /// WP-A4-4: the dock must not repaint the whole window for a poll that
    /// found exactly what the previous poll found — which, on a session
    /// with no `duduclaw-comp` running, is every poll forever.
    #[test]
    fn an_unchanged_window_list_reports_no_repaint_needed() {
        let mut feed = RunningWindowsFeed::default();
        feed.begin_refresh();
        assert!(feed.apply_list_ok(vec![window("foot-A")]), "first result is a real change");
        feed.begin_refresh();
        assert!(!feed.apply_list_ok(vec![window("foot-A")]), "an identical poll result must cost nothing");
        feed.begin_refresh();
        assert!(feed.apply_list_ok(vec![window("foot-A"), window("foot-B")]), "a genuinely new window must still repaint");
        feed.begin_refresh();
        assert!(feed.apply_list_ok(vec![]), "and so must one going away");
        feed.begin_refresh();
        assert!(!feed.apply_list_ok(vec![]), "empty -> empty is the no-comp appliance case: zero repaints");
    }

    #[test]
    fn apply_list_ok_clears_busy_and_becomes_fresh() {
        let mut feed = RunningWindowsFeed::default();
        feed.begin_refresh();
        feed.apply_list_ok(vec![window("foot-A")]);
        assert!(!feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Ready);
        assert!(!feed.is_stale());
        assert!(feed.is_app_running("foot-A"));
        assert!(!feed.is_app_running("foot-B"));
    }

    #[test]
    fn apply_list_err_marks_offline_but_keeps_prior_windows() {
        let mut feed = RunningWindowsFeed::default();
        feed.begin_refresh();
        feed.apply_list_ok(vec![window("foot-A")]);
        feed.begin_refresh();
        feed.apply_list_err();
        assert_eq!(feed.status(), FeedStatus::Offline);
        assert!(!feed.is_busy());
        // Last known state survives a transient failure — see
        // `apply_list_err`'s own doc comment.
        assert!(feed.is_app_running("foot-A"));
    }

    #[test]
    fn is_app_running_rejects_empty_query() {
        let mut feed = RunningWindowsFeed::default();
        feed.apply_list_ok(vec![CompWindow { app_id: None, title: Some("untitled".into()), focused: false }]);
        assert!(!feed.is_app_running(""));
    }

    #[test]
    fn is_app_running_is_exact_not_substring() {
        let mut feed = RunningWindowsFeed::default();
        feed.apply_list_ok(vec![window("org.example.App")]);
        assert!(!feed.is_app_running("org.example"));
        assert!(feed.is_app_running("org.example.App"));
    }

    #[test]
    fn is_stale_becomes_true_again_after_the_poll_interval() {
        let mut feed = RunningWindowsFeed::default();
        feed.apply_list_ok(vec![]);
        assert!(!feed.is_stale());
        // Directly back-date the clock rather than sleeping the test for
        // real seconds — same trick this crate would use anywhere a
        // wall-clock interval needs a fast, deterministic test.
        feed.last_refreshed_at = Some(Instant::now() - POLL_INTERVAL - Duration::from_millis(1));
        assert!(feed.is_stale());
    }
}
