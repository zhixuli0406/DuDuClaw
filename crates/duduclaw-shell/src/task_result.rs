// A1 result-loopback (2026-08-24) — the pure state machine behind "Super+K
// 交辦一個任務 → 結果推回殼".
//
// ## What was missing before this round
//
// `global_task.rs` (A1's first half, 2026-08-23) gets the Super+K KEYPRESS
// from comp to the Launcher overlay. Once open, the Launcher's delegate
// card (`overlay::launcher::delegate_section`) was still `fake_data`-only —
// pressing Enter did nothing (`ShellView::on_oobe_next`'s own doc comment:
// "A no-op outside OOBE (Home has no Enter binding of its own this
// round)"), and even a task submitted some other way had no path back to
// the operator: no poll watched it, nothing surfaced when it finished. The
// TODO line for A1 called this out by name: "「結果回流」（交辦結果推回通
// 道）整塊未動".
//
// This file is the "回流" half: it tracks every goal task THIS shell itself
// submitted (`crate::gateway_client::create_goal`), polls
// `crate::gateway_client::list_tasks` for the agent it delegated to, and
// tells its caller exactly once per task per terminal status
// (`done`/`failed`/`needs_human`) that something needs to reach the
// operator. `main.rs`'s poll loop turns those events into cards on
// `ShellView::notify_center` (`notifyd::center::NotificationCenter::
// post_system` — see that fn's own doc comment for why THIS shell's own
// notifications ride the existing D6 notification centre instead of a new
// surface).
//
// ## Shape
//
// Same discipline `global_task.rs`/`home/running_windows.rs`/
// `overlay/notifications_feed.rs` all state in their own headers: pure
// `&mut self` mutation, no gpui types, no I/O performed here. The caller
// does the `std::thread::spawn` + `cx.spawn` bridge (`main.rs`) and hands
// already-settled results back through `apply_*`.
//
// ## Why polling is scoped to one agent, and where that agent comes from
//
// `list_tasks` (`gateway_client::tasks`) takes a required `agent_id` — this
// tracker only ever watches tasks it itself created via `create_goal`
// (never an arbitrary pre-existing task on the board), and every one of
// those is a delegation to the SAME resolved default agent
// (`gateway_client::pick_default_agent` — the Launcher card has no agent
// picker yet, see `overlay/launcher.rs`'s own header comment). So
// `watched` and `agent_id` are set together, by `apply_submit_ok`, and stay
// in lock-step: `has_watches()` is true if and only if `agent_id` is
// `Some`.
//
// ## Dedup — exactly one card per (task, terminal status)
//
// A poll that lands while a `needs_human` card is still sitting unread in
// the centre must not post a second, identical one — the operator has
// already been told. `WatchedTask::notified_for` remembers the LAST status
// this tracker already turned into an event for that task, so a repeat poll
// answering with the same status is silently absorbed. It is deliberately
// NOT "notify once ever, ever again" though: a task that goes
// `needs_human` -> (operator retries) -> `in_progress` -> `done` gets TWO
// events, one per genuinely new terminal state — see
// `apply_poll_ok_reports_a_second_event_for_a_later_different_terminal_
// status` below.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::gateway_client::TaskSnapshot;

/// Poll cadence while the gateway is answering. Slower than `global_task.rs`
/// ::POLL_INTERVAL` (200ms, sits directly in front of a human keypress) —
/// this one sits behind a goal task that is realistically going to take at
/// minimum several seconds of LLM/tool time, so there is nothing to lose by
/// checking less than five times a second, and a lot of idle gateway load
/// to save by not doing so.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Poll cadence after the gateway has failed to answer — same "back off
/// after exactly one failure, re-arm the fast cadence on the very next
/// success" policy `global_task.rs::GlobalTaskIntentFeed` documents and
/// implements for the identical reason.
pub(crate) const BACKOFF_INTERVAL: Duration = Duration::from_secs(15);

/// The two decision buttons a `needs_human` card offers
/// (`main.rs::post_task_result_card` declares them,
/// `overlay/notifications_apps.rs`'s click handler matches on them to route
/// to `gateway_client::decide_goal_task` instead of the generic D-Bus
/// `invoke` path) — the same `retry`/`abort` verbs
/// `handle_tasks_goal_decide` accepts, so no translation layer sits between
/// this constant and the RPC's own `action` param.
pub(crate) const ACTION_RETRY: &str = "sysact_retry";
pub(crate) const ACTION_ABORT: &str = "sysact_abort";

/// `app_name` every card this tracker's events turn into
/// (`notifyd::center::NotificationCenter::post_system`) carries — this
/// shell's own delegation/result flow, not a third-party app, so the same
/// literal `overlay/launcher.rs::post_submit_failure_card` uses for its own
/// pre-task submit-failure card too (both are "this shell talking to the
/// operator about a delegation").
pub(crate) const NOTIFY_APP_NAME: &str = "DuDuClaw";

/// How many watched tasks this tracker keeps at once. Bounded so a machine
/// left running for days, occasionally delegating, can never grow this
/// unboundedly — same "bounded implicitly/explicitly" discipline every
/// other feed in this crate applies to its own list (`notifications_feed::
/// MAX_DECIDED_HISTORY`, `notifyd::center::MAX_ITEMS`). The OLDEST watch is
/// dropped to make room — it is also the one most likely to have already
/// settled and been acted on.
const MAX_WATCHED: usize = 20;

/// The three task-board statuses this tracker treats as "the operator must
/// be told" — every other status (`todo`/`in_progress`) is silently
/// tracked but produces no event. Matches `task_row_to_json`'s own
/// vocabulary (`duduclaw-gateway/src/handlers.rs`) verbatim, not guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalOutcome {
    Done,
    Failed,
    NeedsHuman,
}

impl GoalOutcome {
    fn from_status(status: &str) -> Option<Self> {
        match status {
            "done" => Some(GoalOutcome::Done),
            "failed" => Some(GoalOutcome::Failed),
            "needs_human" => Some(GoalOutcome::NeedsHuman),
            _ => None,
        }
    }
}

/// Resolves a `TaskSnapshot::pause_reason` wire token into the operator
/// sentence — the exact six strings `web/src/i18n/zh-TW.json`'s
/// `goals.pauseReason.*` keys already show on the dashboard (copied
/// verbatim from that file, not re-translated, so the two surfaces can
/// never drift apart in wording). `None` and any unrecognised token both
/// resolve to the SAME fail-safe "需要人工確認" the gateway's own
/// `PauseReason::from_stored` degrades unknown/legacy values to (its own
/// doc comment: "an unclassifiable pause degrades toward MORE human
/// attention, never less") — this module mirrors that direction rather
/// than inventing a different unknown-state message.
fn pause_reason_label(token: Option<&str>) -> &'static str {
    match token {
        Some("no_progress") => "卡住沒進展",
        Some("budget_exhausted") => "次數或時限用盡",
        Some("blocked_needs_decision") => "等你決策",
        Some("infra") => "系統問題",
        Some("restart") => "系統重啟後暫停",
        _ => "需要人工確認",
    }
}

#[derive(Debug, Clone, PartialEq)]
struct WatchedTask {
    id: String,
    title: String,
    /// The status this tracker already turned into an event for this task,
    /// if any — see this file's header comment on dedup.
    notified_for: Option<String>,
}

/// One terminal transition the caller must turn into a notification card.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaskResultEvent {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) outcome: GoalOutcome,
    /// `result_summary` for `Done`, `judge_feedback` (falling back to
    /// `result_summary`) for `Failed`, the resolved `pause_reason` label for
    /// `NeedsHuman`. `None` when the gateway itself had nothing to say —
    /// left as `None` rather than an invented placeholder string so the
    /// caller can render its own honest "沒有附上摘要" line exactly once,
    /// in one place, rather than this module deciding operator-facing
    /// copy.
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TaskResultTracker {
    watched: Vec<WatchedTask>,
    /// The single agent every watched task in this tracker was delegated
    /// to — see this file's header comment on why one agent is enough.
    agent_id: Option<String>,
    session_jwt: Option<String>,
    submit_busy: bool,
    poll_busy: bool,
    last_polled_at: Option<Instant>,
    consecutive_failures: u32,
    /// Task ids with an in-flight `tasks.goal_decide` call — see
    /// `begin_decide`'s own doc comment.
    deciding: HashSet<String>,
    /// Single-slot claim for `main.rs::schedule_task_result_poll`'s one
    /// self-re-arming outer loop — same shape (and reason)
    /// `notifyd::center::NotificationCenter::try_arm_drain`'s own doc
    /// comment gives for its identical guard: N render passes must start
    /// at most ONE loop, never one per pass.
    poll_loop_armed: bool,
}

impl TaskResultTracker {
    /// Claims the single poll-loop slot — `true` means the caller now OWNS
    /// it and must arm exactly one self-re-arming loop; `false` means one
    /// already exists and the caller must do nothing.
    pub(crate) fn try_arm_poll(&mut self) -> bool {
        if self.poll_loop_armed {
            return false;
        }
        self.poll_loop_armed = true;
        true
    }

    // ── session (shared by submit and poll — one bootstrap for both) ──────

    pub(crate) fn session_jwt(&self) -> Option<&str> {
        self.session_jwt.as_deref()
    }

    pub(crate) fn apply_session(&mut self, jwt: String) {
        self.session_jwt = Some(jwt);
    }

    // ── submit (Launcher's "Enter 交辦") ────────────────────────────────

    /// No production caller yet — `try_submit_delegate` reads `begin_submit`'s
    /// own return value instead of polling this separately. Kept as the
    /// natural public accessor for a future "送出中" affordance on the
    /// delegate card (out of THIS round's scope — see `overlay/launcher.rs`'s
    /// own header comment on the card staying demo content beyond the plain
    /// text field), same allowance `overlay/notifications_feed.rs::
    /// NotificationsFeed::has_session`'s own doc comment gives for the
    /// identical situation; exercised by this module's own tests.
    #[allow(dead_code)]
    pub(crate) fn is_submitting(&self) -> bool {
        self.submit_busy
    }

    /// `false` (no-op) if a submit is already in flight — the same
    /// single-flight guard every poll/refresh primitive in this crate
    /// applies to itself.
    pub(crate) fn begin_submit(&mut self) -> bool {
        if self.submit_busy {
            return false;
        }
        self.submit_busy = true;
        true
    }

    /// A `create_goal` call landed. Starts (or continues) watching this task
    /// under `agent_id` — see this file's header comment on why `watched`
    /// and `agent_id` move together.
    pub(crate) fn apply_submit_ok(&mut self, agent_id: String, task_id: String, title: String) {
        self.submit_busy = false;
        self.agent_id = Some(agent_id);
        if self.watched.iter().any(|w| w.id == task_id) {
            return;
        }
        self.watched.push(WatchedTask { id: task_id, title, notified_for: None });
        if self.watched.len() > MAX_WATCHED {
            self.watched.remove(0);
        }
    }

    pub(crate) fn apply_submit_err(&mut self) {
        self.submit_busy = false;
    }

    // ── poll (terminal-state watch) ────────────────────────────────────

    pub(crate) fn has_watches(&self) -> bool {
        !self.watched.is_empty()
    }

    /// The agent every watched task belongs to — `None` until at least one
    /// `apply_submit_ok` has landed, which is also exactly when
    /// `has_watches()` first becomes true.
    pub(crate) fn watch_agent_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }

    fn interval(&self) -> Duration {
        if self.consecutive_failures >= 1 {
            BACKOFF_INTERVAL
        } else {
            POLL_INTERVAL
        }
    }

    pub(crate) fn is_stale(&self) -> bool {
        match self.last_polled_at {
            None => true,
            Some(t) => t.elapsed() >= self.interval(),
        }
    }

    /// How long the poll loop's self-re-arming sleep (`main.rs::
    /// schedule_task_result_poll`) should wait before its next `is_stale`
    /// check — the current cadence when something is watched, or the fast
    /// cadence when nothing is (so a first delegation right after boot is
    /// picked up within `POLL_INTERVAL`, not stuck behind a stale backoff
    /// window left over from nothing).
    pub(crate) fn next_check_delay(&self) -> Duration {
        if self.has_watches() {
            self.interval()
        } else {
            POLL_INTERVAL
        }
    }

    /// `true` if a poll was actually started (the caller must now dispatch
    /// one). Refuses when there is nothing to watch — an idle machine that
    /// has never delegated anything must never open a socket on a timer,
    /// same "no work, no network" rule `notifyd`'s own daemon status states
    /// applies to a healthy-and-quiet bus.
    pub(crate) fn begin_poll(&mut self) -> bool {
        if self.poll_busy || !self.has_watches() || !self.is_stale() {
            return false;
        }
        self.poll_busy = true;
        true
    }

    pub(crate) fn apply_poll_err(&mut self) {
        self.poll_busy = false;
        self.last_polled_at = Some(Instant::now());
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// Applies one fresh `list_tasks` batch, returning every terminal
    /// transition the caller has not already been told about (dedup by
    /// `(task_id, status)` — see this file's header comment). A watched
    /// task absent from the batch (removed from the board, or the agent_id
    /// changed underneath it) is left as-is rather than dropped: a single
    /// missed poll must not silently stop watching something the operator
    /// is still waiting on.
    pub(crate) fn apply_poll_ok(&mut self, snapshots: Vec<TaskSnapshot>) -> Vec<TaskResultEvent> {
        self.poll_busy = false;
        self.last_polled_at = Some(Instant::now());
        self.consecutive_failures = 0;

        let mut events = Vec::new();
        for snap in &snapshots {
            let Some(watched) = self.watched.iter_mut().find(|w| w.id == snap.id) else {
                continue;
            };
            let Some(outcome) = GoalOutcome::from_status(&snap.status) else {
                continue;
            };
            if watched.notified_for.as_deref() == Some(snap.status.as_str()) {
                continue;
            }
            watched.notified_for = Some(snap.status.clone());
            let detail = match outcome {
                GoalOutcome::Done => snap.result_summary.clone(),
                GoalOutcome::Failed => snap.judge_feedback.clone().or_else(|| snap.result_summary.clone()),
                // `pause_reason` on the wire is a STABLE STORAGE TOKEN
                // (`duduclaw-gateway/src/pause_reason.rs::PauseReason::
                // as_str`'s own doc comment: "Never localise this — it is
                // persisted in SQLite and shipped over the dashboard RPC as
                // a key"), not display text — showing `"no_progress"`
                // verbatim in a notification would be exactly the internal
                // vocabulary leak 5.誠實回報/task brief ("結果文字使用者
                // 視角，零內部術語") forbids. `pause_reason_label` resolves
                // it through the SAME six sentences `web/src/i18n/
                // zh-TW.json`'s `goals.pauseReason.*` keys already show on
                // the dashboard, so an operator sees identical wording in
                // both places.
                GoalOutcome::NeedsHuman => Some(pause_reason_label(snap.pause_reason.as_deref()).to_string()),
            };
            events.push(TaskResultEvent { task_id: snap.id.clone(), title: watched.title.clone(), outcome, detail });
        }
        events
    }

    // ── decide (retry/abort dispatched from a notification card) ─────────

    /// Claims the single in-flight slot for a `tasks.goal_decide` call
    /// against this task id. `false` means one is already running (a
    /// double-click on the same button, or a click while a stale render
    /// pass is still catching up) and the caller must do nothing.
    pub(crate) fn begin_decide(&mut self, task_id: &str) -> bool {
        self.deciding.insert(task_id.to_string())
    }

    pub(crate) fn end_decide(&mut self, task_id: &str) {
        self.deciding.remove(task_id);
    }

    #[cfg(test)]
    pub(crate) fn is_deciding(&self, task_id: &str) -> bool {
        self.deciding.contains(task_id)
    }

    #[cfg(test)]
    pub(crate) fn watched_len(&self) -> usize {
        self.watched.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, status: &str) -> TaskSnapshot {
        TaskSnapshot { id: id.to_string(), title: "t".to_string(), status: status.to_string(), result_summary: None, judge_feedback: None, pause_reason: None }
    }

    #[test]
    fn a_fresh_tracker_has_nothing_to_watch_and_never_polls() {
        let t = TaskResultTracker::default();
        assert!(!t.has_watches());
        assert!(t.watch_agent_id().is_none());
        // is_stale() answers true (nothing ever polled), but begin_poll
        // must still refuse — the "no work, no network" rule.
        assert!(t.is_stale());
    }

    #[test]
    fn begin_poll_refuses_with_nothing_watched() {
        let mut t = TaskResultTracker::default();
        assert!(!t.begin_poll());
    }

    #[test]
    fn submitting_arms_the_watch_and_records_the_agent() {
        let mut t = TaskResultTracker::default();
        assert!(t.begin_submit());
        assert!(!t.begin_submit(), "a second submit while one is in flight must refuse");
        t.apply_submit_ok("finance".to_string(), "task-1".to_string(), "寄出報價單".to_string());
        assert!(!t.is_submitting());
        assert!(t.has_watches());
        assert_eq!(t.watch_agent_id(), Some("finance"));
        assert_eq!(t.watched_len(), 1);
    }

    #[test]
    fn submit_err_clears_busy_without_watching_anything() {
        let mut t = TaskResultTracker::default();
        t.begin_submit();
        t.apply_submit_err();
        assert!(!t.is_submitting());
        assert!(!t.has_watches());
    }

    #[test]
    fn begin_poll_is_single_flight_and_respects_the_watch_gate() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        assert!(t.begin_poll());
        assert!(!t.begin_poll(), "already in flight — must refuse");
    }

    #[test]
    fn a_poll_failure_backs_off_and_a_success_re_arms_the_fast_cadence() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        t.apply_poll_err();
        assert_eq!(t.interval(), BACKOFF_INTERVAL);
        t.begin_poll();
        t.apply_poll_ok(vec![snap("t1", "in_progress")]);
        assert_eq!(t.interval(), POLL_INTERVAL);
    }

    #[test]
    fn an_in_progress_status_produces_no_event() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let events = t.apply_poll_ok(vec![snap("t1", "in_progress")]);
        assert!(events.is_empty());
    }

    #[test]
    fn a_done_status_produces_exactly_one_event_and_carries_the_summary() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "寄出報價單".to_string());
        t.begin_poll();
        let mut s = snap("t1", "done");
        s.result_summary = Some("已寄出，對方已回覆收到".to_string());
        let events = t.apply_poll_ok(vec![s]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].task_id, "t1");
        assert_eq!(events[0].title, "寄出報價單");
        assert_eq!(events[0].outcome, GoalOutcome::Done);
        assert_eq!(events[0].detail.as_deref(), Some("已寄出，對方已回覆收到"));
    }

    #[test]
    fn a_repeat_poll_with_the_same_terminal_status_is_not_re_notified() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let first = t.apply_poll_ok(vec![snap("t1", "done")]);
        assert_eq!(first.len(), 1);
        t.begin_poll();
        let second = t.apply_poll_ok(vec![snap("t1", "done")]);
        assert!(second.is_empty(), "an already-notified terminal status must not fire twice");
    }

    #[test]
    fn a_later_different_terminal_status_still_fires_a_second_event() {
        // needs_human -> (operator retries) -> done: two distinct terminal
        // states the operator genuinely needs to hear about twice.
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let first = t.apply_poll_ok(vec![snap("t1", "needs_human")]);
        assert_eq!(first[0].outcome, GoalOutcome::NeedsHuman);
        t.begin_poll();
        let second = t.apply_poll_ok(vec![snap("t1", "done")]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].outcome, GoalOutcome::Done);
    }

    #[test]
    fn failed_prefers_judge_feedback_and_falls_back_to_result_summary() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let mut s = snap("t1", "failed");
        s.judge_feedback = Some("驗收沒過：缺少附件".to_string());
        s.result_summary = Some("不會用到的摘要".to_string());
        let events = t.apply_poll_ok(vec![s]);
        assert_eq!(events[0].detail.as_deref(), Some("驗收沒過：缺少附件"));

        let mut t2 = TaskResultTracker::default();
        t2.apply_submit_ok("a".to_string(), "t2".to_string(), "title".to_string());
        t2.begin_poll();
        let mut s2 = snap("t2", "failed");
        s2.result_summary = Some("只有摘要，沒有 judge_feedback".to_string());
        let events2 = t2.apply_poll_ok(vec![s2]);
        assert_eq!(events2[0].detail.as_deref(), Some("只有摘要，沒有 judge_feedback"));
    }

    #[test]
    fn needs_human_carries_the_translated_pause_reason_as_its_detail() {
        // The raw wire token ("no_progress") must NEVER reach the operator
        // verbatim — see `pause_reason_label`'s own doc comment.
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let mut s = snap("t1", "needs_human");
        s.pause_reason = Some("no_progress".to_string());
        let events = t.apply_poll_ok(vec![s]);
        assert_eq!(events[0].outcome, GoalOutcome::NeedsHuman);
        assert_eq!(events[0].detail.as_deref(), Some("卡住沒進展"));
    }

    #[test]
    fn pause_reason_label_covers_every_wire_token_the_gateway_sends() {
        assert_eq!(pause_reason_label(Some("no_progress")), "卡住沒進展");
        assert_eq!(pause_reason_label(Some("budget_exhausted")), "次數或時限用盡");
        assert_eq!(pause_reason_label(Some("blocked_needs_decision")), "等你決策");
        assert_eq!(pause_reason_label(Some("infra")), "系統問題");
        assert_eq!(pause_reason_label(Some("restart")), "系統重啟後暫停");
        assert_eq!(pause_reason_label(Some("unknown")), "需要人工確認");
    }

    #[test]
    fn pause_reason_label_fails_safe_toward_more_human_attention() {
        // An absent or unrecognised token (legacy row, future gateway
        // adding a class this build doesn't know about yet) must resolve to
        // the SAME "needs a human" message the gateway's own
        // `PauseReason::from_stored` degrades to — never silently blank.
        assert_eq!(pause_reason_label(None), "需要人工確認");
        assert_eq!(pause_reason_label(Some("some_future_class_this_build_never_heard_of")), "需要人工確認");
        assert_eq!(pause_reason_label(Some("")), "需要人工確認");
    }

    #[test]
    fn a_snapshot_for_an_unwatched_task_is_ignored() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.begin_poll();
        let events = t.apply_poll_ok(vec![snap("some-other-task", "done")]);
        assert!(events.is_empty());
    }

    #[test]
    fn watching_the_same_task_id_twice_does_not_duplicate_it() {
        let mut t = TaskResultTracker::default();
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title".to_string());
        t.apply_submit_ok("a".to_string(), "t1".to_string(), "title again".to_string());
        assert_eq!(t.watched_len(), 1);
    }

    #[test]
    fn watched_tasks_are_bounded() {
        let mut t = TaskResultTracker::default();
        for n in 0..(MAX_WATCHED + 5) {
            t.apply_submit_ok("a".to_string(), format!("t{n}"), "title".to_string());
        }
        assert_eq!(t.watched_len(), MAX_WATCHED);
    }

    #[test]
    fn decide_is_single_flight_per_task_id() {
        let mut t = TaskResultTracker::default();
        assert!(t.begin_decide("t1"));
        assert!(t.is_deciding("t1"));
        assert!(!t.begin_decide("t1"), "a second decide on the same task must refuse while one is in flight");
        // A DIFFERENT task id must not be blocked by the first one's guard.
        assert!(t.begin_decide("t2"));
        t.end_decide("t1");
        assert!(!t.is_deciding("t1"));
        assert!(t.begin_decide("t1"), "released after end_decide");
    }
}
