// In-progress task feed — A4 (2026-08-24, real-gap fill). The dock never
// showed anything about the task board at all (only `running_windows`'
// comp-side window dots and two STATIC `fake_data::DOCK_AGENTS` entries),
// and the Notifications panel had no task-board section either — see
// `overlay::notifications_tasks`'s own header comment for the render side.
//
// Deliberately a MUCH simpler state machine than `notifications_feed::
// NotificationsFeed`: no per-row decide/confirm (this is a read-only list,
// nothing here writes to the task board), no independent backoff policy —
// it is only ever driven from `overlay::notifications::schedule_stale_check`
//'s already-existing single-arm 30s timer (see `notifications_tasks::
// trigger_task_refresh_if_stale`'s own doc comment for why riding that timer
// rather than arming a second one matters: WP-A4-4's own header comment in
// `notifications.rs` is the post-mortem for exactly what a second
// independently-armed timer against this same gateway caused). A transient
// failure therefore can't retry faster than that shared 30s cadence either,
// which is spacing enough for a plain read-only poll — no exponential
// backoff needed on top.
//
// Same "pure &mut self mutation, no gpui types" discipline `notifications_
// feed`/`home::running_windows::RunningWindowsFeed` both establish — the
// actual gateway I/O is dispatched from `overlay/notifications_tasks.rs`.

use std::time::Instant;

use crate::gateway_client::TaskProgressItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    Offline,
}

/// Same 30s cadence `notifications_feed::REFRESH_STALE_AFTER` uses — kept
/// as its own constant (not re-exported from that module) since this feed
/// has no other coupling to `NotificationsFeed` and a shared constant would
/// be a stranger import for what it buys.
pub const REFRESH_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct TaskProgressFeed {
    status: FeedStatus,
    busy: bool,
    last_refreshed_at: Option<Instant>,
    rows: Vec<TaskProgressItem>,
}

impl Default for TaskProgressFeed {
    fn default() -> Self {
        Self { status: FeedStatus::Idle, busy: false, last_refreshed_at: None, rows: Vec::new() }
    }
}

impl TaskProgressFeed {
    /// How many in-progress tasks the last successful fetch found — the
    /// dock badge's count. `0` before the first fetch ever lands, same
    /// honest "no data yet" default `NotificationsFeed::pending_count()`
    /// starts at.
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn rows(&self) -> &[TaskProgressItem] {
        &self.rows
    }

    /// Not read from production code yet this round (the dock badge only
    /// needs `count()`, and the panel section only renders when `rows()` is
    /// non-empty — see `notifications_tasks::task_progress_section`'s own
    /// doc comment) but a natural public accessor to have alongside `rows()`
    /// /`count()`, same allowance `NotificationsFeed::has_session`'s own doc
    /// comment gives for the identical situation. Exercised by this module's
    /// own tests below.
    #[allow(dead_code)]
    pub fn status(&self) -> FeedStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn is_stale(&self) -> bool {
        match self.last_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_STALE_AFTER,
        }
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

    /// Returns whether the dock badge's number (or the panel's row list)
    /// actually changed — same "無變化不 notify" discipline WP-A4-4
    /// established for `NotificationsFeed::apply_list_ok`/`RunningWindowsFeed
    /// ::apply_list_ok`, extended here rather than reinvented.
    pub(crate) fn apply_list_ok(&mut self, items: Vec<TaskProgressItem>) -> bool {
        let changed = items != self.rows || self.status != FeedStatus::Ready;
        self.rows = items;
        self.status = FeedStatus::Ready;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
        changed
    }

    /// Deliberately does NOT clear `rows` on a transient failure — same
    /// "keep showing the last known state" reasoning `RunningWindowsFeed::
    /// apply_list_err`'s own doc comment gives; a badge that blinks to zero
    /// on one missed poll would be actively misleading (it reads as "nothing
    /// is running" rather than "couldn't check"). `last_refreshed_at` still
    /// advances so the next attempt waits out the normal 30s cadence rather
    /// than retrying instantly — see that field's own doc comment on this
    /// struct.
    pub(crate) fn apply_list_err(&mut self) {
        self.status = FeedStatus::Offline;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str) -> TaskProgressItem {
        TaskProgressItem { id: id.to_string(), title: title.to_string(), status: "in_progress".to_string(), assigned_to: "agent-a".to_string() }
    }

    #[test]
    fn fresh_feed_is_stale_idle_and_empty() {
        let feed = TaskProgressFeed::default();
        assert!(feed.is_stale());
        assert_eq!(feed.status(), FeedStatus::Idle);
        assert!(!feed.is_busy());
        assert_eq!(feed.count(), 0);
    }

    #[test]
    fn begin_refresh_flips_busy_and_idle_to_loading_once() {
        let mut feed = TaskProgressFeed::default();
        assert!(feed.begin_refresh());
        assert!(feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Loading);
        assert!(!feed.begin_refresh(), "already busy — must not double-fire");
    }

    #[test]
    fn refresh_round_trip_populates_rows_and_clears_staleness() {
        let mut feed = TaskProgressFeed::default();
        feed.begin_refresh();
        let outcome = feed.apply_list_ok(vec![item("t1", "寄出報價單")]);
        assert!(outcome, "Idle -> Ready with a new row is a real change");
        assert!(!feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Ready);
        assert_eq!(feed.count(), 1);
        assert!(!feed.is_stale());
    }

    #[test]
    fn an_identical_poll_result_reports_no_repaint_needed() {
        let mut feed = TaskProgressFeed::default();
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("t1", "寄出報價單")]);
        feed.begin_refresh();
        let outcome = feed.apply_list_ok(vec![item("t1", "寄出報價單")]);
        assert!(!outcome, "an unchanged poll result must not cost a repaint");
    }

    #[test]
    fn list_err_goes_offline_but_keeps_existing_rows() {
        let mut feed = TaskProgressFeed::default();
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("t1", "寄出報價單")]);
        feed.begin_refresh();
        feed.apply_list_err();
        assert_eq!(feed.status(), FeedStatus::Offline);
        assert_eq!(feed.count(), 1, "a transient failure must not blank out an already-shown count");
        assert!(!feed.is_stale(), "a failure still advances the retry clock — no instant hammering");
    }
}
