// Installed-app feed — APP-1 (2026-08-22).
//
// Holds the result of `apps::installed::scan()` and decides when another
// one is due. Same "pure `&mut self` mutation, no gpui types, never
// performs I/O itself" discipline `home::running_windows::RunningWindowsFeed`
// and `overlay::notifications_feed::NotificationsFeed` already establish —
// the blocking scan is dispatched from `home/home_dock.rs` through this
// crate's standard `std::thread::spawn` + `mpsc` + `cx.spawn` bridge, and
// this struct only ever receives already-settled results.
//
// ── Why a cached feed and not a scan per render ─────────────────────────
// A scan runs two subprocesses and walks several directories. gpui
// re-renders on every notify, every hover, every keystroke into the
// Launcher's search box — running that on the render thread would stall the
// shell, and running it per keystroke would fork `flatpak` dozens of times
// a second. So the list is scanned on a cadence into this struct and SEARCH
// filters the already-scanned list in memory (`apps::search`), which is a
// substring test over pre-lowercased text and costs nothing.
//
// ── The repaint discipline this crate learned the hard way ──────────────
// `apply_scan` returns whether anything RENDERED actually changed, and the
// poll bridge only calls `cx.notify()` when it did. `home::running_windows`
// documents the incident behind that rule (a poll that failed identically
// forever still repainted the whole window twice every three seconds on the
// appliance's cage session). The same applies doubly here: an appliance
// with a stable app list would otherwise repaint on every single refresh
// for the rest of its uptime. And `schedule_installed_apps_poll` is a
// one-shot `cx.spawn`, never a self-re-arming timer — see that fn's own doc
// comment in `home/home_dock.rs`.

use std::time::{Duration, Instant};

use super::installed::{InstalledApp, ScanOutcome, SourceStatus};

/// How long a scan result stays fresh. Deliberately far longer than
/// `running_windows::POLL_INTERVAL` (3s): a window opens and closes many
/// times a minute, but a machine's installed-app set changes when somebody
/// installs something — minutes to months apart. One minute is short enough
/// that an app installed through this shell's own confirmation gate shows
/// up without anyone restarting anything, and long enough that the two
/// subprocesses per scan are irrelevant.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FeedStatus {
    /// Nothing has been asked yet.
    #[default]
    Idle,
    /// A first scan is in flight — the list is not "empty", it is "unknown".
    Loading,
    /// At least one scan has settled. The list is now an answer, including
    /// when it is empty.
    Ready,
}

/// What an EMPTY app list means right now. Three distinct states because
/// they are three distinct facts, and the Launcher says which one it is
/// instead of showing the same shrug for all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppsEmptyState {
    /// No scan has settled yet.
    Scanning,
    /// Scanned; this machine genuinely has nothing installed that this
    /// shell can launch.
    NoneInstalled,
    /// Scanned; every source that IS present failed to enumerate. The list
    /// is empty because we could not find out, not because there is
    /// nothing — and the copy says so.
    ReadFailed,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InstalledAppsFeed {
    apps: Vec<InstalledApp>,
    flatpak: Option<SourceStatus>,
    desktop: Option<SourceStatus>,
    status: FeedStatus,
    busy: bool,
    last_refreshed_at: Option<Instant>,
}

/// What the caller has to do after `apply_scan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplyOutcome {
    /// Something the UI draws moved — the caller should `cx.notify()`.
    pub repaint: bool,
    /// A source's status changed (e.g. started failing, or recovered) — the
    /// caller should log it ONCE, rather than every refresh forever.
    pub status_changed: bool,
}

impl InstalledAppsFeed {
    pub(crate) fn is_stale(&self) -> bool {
        match self.last_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_INTERVAL,
        }
    }

    /// `true` if a scan was actually started (the caller must now dispatch
    /// one); `false` if one is already in flight — same single-flight
    /// contract `RunningWindowsFeed::begin_refresh` establishes.
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

    pub(crate) fn apply_scan(&mut self, outcome: ScanOutcome) -> ApplyOutcome {
        let repaint = outcome.apps != self.apps;
        let status_changed = self.flatpak.as_ref() != Some(&outcome.flatpak) || self.desktop.as_ref() != Some(&outcome.desktop);
        self.apps = outcome.apps;
        self.flatpak = Some(outcome.flatpak);
        self.desktop = Some(outcome.desktop);
        self.status = FeedStatus::Ready;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
        // A status flip changes the EMPTY-state copy (`empty_state` below),
        // which is on screen whenever the list is empty — so it is a real
        // repaint reason even when the (empty) list itself did not move.
        ApplyOutcome { repaint: repaint || (status_changed && self.apps.is_empty()), status_changed }
    }

    pub(crate) fn apps(&self) -> &[InstalledApp] {
        &self.apps
    }

    /// Whether `id` is already installed — exact, case-insensitive match on
    /// the app id, never a substring (this crate's coding convention 2).
    /// Used to keep an app that is ALREADY here off the installable
    /// catalog's list.
    pub(crate) fn contains_id(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        self.apps.iter().any(|a| a.id.eq_ignore_ascii_case(id) || a.flatpak_id.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(id)))
    }

    /// What an empty list means. Only meaningful when `apps()` is empty;
    /// the Launcher is the only caller and only calls it then.
    pub(crate) fn empty_state(&self) -> AppsEmptyState {
        if self.status != FeedStatus::Ready {
            return AppsEmptyState::Scanning;
        }
        let statuses = [self.flatpak.as_ref(), self.desktop.as_ref()];
        let present: Vec<&SourceStatus> = statuses.into_iter().flatten().filter(|s| **s != SourceStatus::Absent).collect();
        if !present.is_empty() && present.iter().all(|s| matches!(s, SourceStatus::Failed(_))) {
            return AppsEmptyState::ReadFailed;
        }
        AppsEmptyState::NoneInstalled
    }

    /// The two source statuses, for the poll bridge's change-triggered log
    /// line. `None` until the first scan settles.
    pub(crate) fn source_statuses(&self) -> (Option<&SourceStatus>, Option<&SourceStatus>) {
        (self.flatpak.as_ref(), self.desktop.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> FeedStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::installed::AppSource;

    fn app(id: &str) -> InstalledApp {
        InstalledApp {
            id: id.to_string(),
            name: id.to_string(),
            source: AppSource::Desktop,
            icon: None,
            resolved_icon: None,
            exec_argv: Some(vec!["/bin/true".to_string()]),
            flatpak_id: None,
            terminal: false,
            search_key: id.to_lowercase(),
            origin: "test".to_string(),
        }
    }

    fn outcome(apps: Vec<InstalledApp>, flatpak: SourceStatus, desktop: SourceStatus) -> ScanOutcome {
        ScanOutcome { apps, flatpak, desktop }
    }

    #[test]
    fn a_fresh_feed_is_stale_idle_and_reports_scanning_not_empty() {
        let feed = InstalledAppsFeed::default();
        assert!(feed.is_stale());
        assert_eq!(feed.status(), FeedStatus::Idle);
        assert!(!feed.is_busy());
        assert!(feed.apps().is_empty());
        assert_eq!(feed.empty_state(), AppsEmptyState::Scanning, "before the first scan settles the list is UNKNOWN, never 'this machine has nothing'");
    }

    #[test]
    fn begin_refresh_is_single_flight() {
        let mut feed = InstalledAppsFeed::default();
        assert!(feed.begin_refresh());
        assert!(feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Loading);
        assert!(!feed.begin_refresh(), "a second concurrent caller must not start another scan");
    }

    #[test]
    fn an_unchanged_scan_result_costs_no_repaint() {
        let mut feed = InstalledAppsFeed::default();
        feed.begin_refresh();
        assert!(feed.apply_scan(outcome(vec![app("a")], SourceStatus::Absent, SourceStatus::Ok(1))).repaint, "the first result is a real change");
        feed.begin_refresh();
        let second = feed.apply_scan(outcome(vec![app("a")], SourceStatus::Absent, SourceStatus::Ok(1)));
        assert!(!second.repaint, "an identical scan must not repaint the whole window");
        assert!(!second.status_changed);
        feed.begin_refresh();
        assert!(feed.apply_scan(outcome(vec![app("a"), app("b")], SourceStatus::Absent, SourceStatus::Ok(2))).repaint, "a newly installed app must repaint");
        feed.begin_refresh();
        assert!(feed.apply_scan(outcome(vec![app("b")], SourceStatus::Absent, SourceStatus::Ok(1))).repaint, "and so must one going away");
    }

    #[test]
    fn a_machine_with_nothing_installed_repaints_once_then_never_again() {
        // The appliance/dev-Mac shape: both sources absent, empty list, over
        // and over. This is the exact case `running_windows` was fixed for
        // — with one deliberate difference: the FIRST settle does repaint,
        // because the empty-state copy really does change (「正在尋找…」 ->
        // 「還沒有可以開啟的應用程式」). Every subsequent identical scan is
        // free, forever, which is the property that matters.
        let mut feed = InstalledAppsFeed::default();
        feed.begin_refresh();
        let first = feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Absent));
        assert!(first.repaint, "the message stopped saying 'still looking', which is on screen");
        assert!(first.status_changed, "and the statuses settled, which the caller logs once");
        assert_eq!(feed.empty_state(), AppsEmptyState::NoneInstalled);
        for _ in 0..5 {
            feed.begin_refresh();
            let again = feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Absent));
            assert!(!again.repaint, "an identical scan on an unchanged machine must cost zero repaints");
            assert!(!again.status_changed, "and zero log lines");
        }
    }

    #[test]
    fn a_status_flip_on_an_empty_list_repaints_because_the_message_itself_changed() {
        let mut feed = InstalledAppsFeed::default();
        feed.begin_refresh();
        feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Ok(0)));
        assert_eq!(feed.empty_state(), AppsEmptyState::NoneInstalled);
        feed.begin_refresh();
        let applied = feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Failed("boom".into())));
        assert!(applied.repaint, "the empty-state copy changed, so the panel must redraw");
        assert!(applied.status_changed);
        assert_eq!(feed.empty_state(), AppsEmptyState::ReadFailed);
    }

    #[test]
    fn empty_states_distinguish_absent_from_broken() {
        let mut feed = InstalledAppsFeed::default();
        feed.begin_refresh();
        // Nothing here at all — the honest dev-Mac answer.
        feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Absent));
        assert_eq!(feed.empty_state(), AppsEmptyState::NoneInstalled);

        // One source present and empty, the other broken — still an honest
        // "nothing installed", because a source DID answer.
        feed.begin_refresh();
        feed.apply_scan(outcome(Vec::new(), SourceStatus::Failed("x".into()), SourceStatus::Ok(0)));
        assert_eq!(feed.empty_state(), AppsEmptyState::NoneInstalled);

        // Every present source failed — we do not know, and we say so.
        feed.begin_refresh();
        feed.apply_scan(outcome(Vec::new(), SourceStatus::Failed("x".into()), SourceStatus::Absent));
        assert_eq!(feed.empty_state(), AppsEmptyState::ReadFailed);
    }

    #[test]
    fn apply_scan_clears_busy_and_becomes_fresh() {
        let mut feed = InstalledAppsFeed::default();
        feed.begin_refresh();
        feed.apply_scan(outcome(vec![app("a")], SourceStatus::Ok(0), SourceStatus::Ok(1)));
        assert!(!feed.is_busy());
        assert_eq!(feed.status(), FeedStatus::Ready);
        assert!(!feed.is_stale());
        assert_eq!(feed.source_statuses(), (Some(&SourceStatus::Ok(0)), Some(&SourceStatus::Ok(1))));
    }

    #[test]
    fn is_stale_becomes_true_again_after_the_refresh_interval() {
        let mut feed = InstalledAppsFeed::default();
        feed.apply_scan(outcome(Vec::new(), SourceStatus::Absent, SourceStatus::Absent));
        assert!(!feed.is_stale());
        // Back-date rather than sleeping, same deterministic trick
        // `running_windows`'s equivalent test uses.
        feed.last_refreshed_at = Some(Instant::now() - REFRESH_INTERVAL - Duration::from_millis(1));
        assert!(feed.is_stale());
    }

    #[test]
    fn contains_id_is_exact_and_case_insensitive_never_substring() {
        let mut feed = InstalledAppsFeed::default();
        let mut chromium = app("org.chromium.Chromium");
        chromium.flatpak_id = Some("org.chromium.Chromium".to_string());
        feed.apply_scan(outcome(vec![chromium], SourceStatus::Ok(1), SourceStatus::Absent));
        assert!(feed.contains_id("org.chromium.Chromium"));
        assert!(feed.contains_id("ORG.CHROMIUM.CHROMIUM"));
        assert!(!feed.contains_id("org.chromium"), "a prefix is a different app");
        assert!(!feed.contains_id(""), "an empty id matches nothing");
    }
}
