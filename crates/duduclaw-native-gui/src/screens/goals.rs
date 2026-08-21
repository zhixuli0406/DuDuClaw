// S4b second wave — Screen "目標" (p08, `nav.rs` id `goals`, already listed
// under the 任務與目標 area's Column-2 page list — this pass wires that
// existing id to a real page instead of `shell.rs`'s generic placeholder).
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Goals.dc.html` —
// an OmniFocus-style THREE-pane layout: a smart-view rail (需要你/進行中/
// 今天/已完成), a list of goal tasks for the selected view, and a right
// inspector (acceptance criteria / judge feedback / round history / three
// decision buttons). No Kanban board anywhere in this page — that's `tasks`
// (a separate existing nav id), not `goals`.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ─────────────────────────────────────────
//   `tasks.list {"goal_mode": true}` → `{"tasks": [...]}`, each row shaped by
//   `task_row_to_json` (~L33422) plus a per-row `"channel"` field the
//   `handle_tasks_list` wrapper adds on top (~L28741). Fields this page
//   reads: id/title/status/assigned_to/claimed_by/revision_round/
//   judge_feedback/acceptance_criteria/acceptance_criteria_baseline/
//   pause_reason/updated_at/created_at/channel.
//   `tasks.iterations {"task_id"}` → `{"iterations": [...]}`, `TaskIterationRow`
//   shaped by `task_iteration_to_json` (~L33487): round/verdict(accepted|
//   rejected|escalated|null)/judge_feedback/dispatched_at/submitted_at/
//   judged_at.
//   `tasks.goal_decide {"task_id","action","note"?}` → `handle_tasks_goal_decide`
//   (~L29784): `action` one of retry|done|abort|takeover|continue.
//
// ── Three buttons, not four (deliberate deviation from `web/src/pages/
// GoalsPage.tsx`) ──────────────────────────────────────────────────────
// The web dashboard's `InterventionButtons` renders FOUR buttons (重試 /
// 標記完成 / 放棄 / 我來接手). The visual authority canvas shows only THREE:
// 核准繼續 / 附指示重試 / 中止 — and the task brief names exactly those three.
// Rather than inventing a new server verb for "核准繼續", this page reuses
// the existing `retry` action twice, differing only in whether a note is
// attached (the server already treats an empty `note` as "no extra
// guidance" — see `handle_tasks_goal_decide`'s generic branch): 核准繼續 =
// `retry` with no note (approve the current state, let the loop try again
// unguided), 附指示重試 = `retry` with a typed note (same action, extra
// guidance attached). 中止 = `abort`. `done` ("標記完成") and `takeover`
// ("我來接手") are NOT exposed here — the canvas never shows them and the
// task brief names only three buttons; a human who wants either can still
// use the web dashboard. See `goals_inspector.rs` for the button wiring and
// `build_goal_decide_params`'s unit tests for the exact assembled payloads.
//
// ── Why state lives in a `gpui::Global`, not a `RootView` field ─────────
// The task brief explicitly forbids touching `main.rs` this pass (a
// concurrent pass owns its wiring this round — same constraint `chat.rs`
// documents for why ITS boot-fetch runs from `render` instead of `main.rs`'s
// poll loop). `RootView`'s fields are all private and declared/constructed
// in `main.rs`, so a new `goals: GoalsState` field is off the table without
// editing that file. `GoalsState` is therefore a `gpui::Global` — the
// documented gpui idiom for state that isn't naturally owned by one
// `Entity` (`crates/gpui/src/global.rs`'s own doc comment: "you may need to
// store some global state... implement this on types you want to store in
// the context as a global"). `Context<RootView>` derefs to `&mut App`, which
// carries `global`/`global_mut`/`default_global` — no `RootView` field
// needed at all. Every mutation below happens either from `render`'s own
// `cx: &mut Context<RootView>` (fetch-latch bookkeeping, default selection —
// safe because it happens BEFORE this same pass reads the state back out,
// not across a re-render boundary) or from a `cx.listener` click handler /
// an async `spawn_goal_call` callback, both of which also carry a live
// `&mut Context<RootView>`.

use chrono::{DateTime, Local, Utc};
use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// `goals_inspector`/`goals_data` are SIBLING modules (declared in
// `screens/mod.rs`, not `mod ...;` here) — same file-layout precedent
// `dashboard.rs`/`dashboard_cards.rs` already establish: a `foo.rs` +
// `foo_cards.rs` pair both living directly under `screens/`, rather than
// `foo.rs` + `foo/cards.rs` (which Rust's 2018+ module resolution would
// otherwise require for a `mod cards;` declared INSIDE `foo.rs`). Kept
// consistent with that existing convention rather than introducing a new
// nested-directory shape for just this one page. `goals_data` (types +
// pure parsing/filtering: `SmartView`/`GoalTask`/`IterationRow`) is a
// second split of what was originally this same file — see that module's
// own doc comment for why.
use crate::screens::goals_data::{filtered_sorted, local_date, parse_goal_tasks, parse_iterations, GoalTask, IterationRow, SmartView};
use crate::screens::goals_inspector as inspector;

// ── Global state ───────────────────────────────────────────────────────

pub struct GoalsState {
    tasks_requested: bool,
    pub tasks: Loadable<Vec<GoalTask>>,
    pub smart_view: SmartView,
    pub selected_task_id: Option<String>,
    pub iterations: Loadable<Vec<IterationRow>>,
    /// Latch: the task id whose iterations are already loaded or in flight
    /// — `None` means "fetch on next render". Distinct from
    /// `selected_task_id` itself so switching away and back to the same
    /// task doesn't re-fetch. `pub(super)`: `goals_inspector.rs`'s own
    /// `dispatch_decide` clears this after a decision lands, to force a
    /// round-history reload (a new round may have just been appended).
    pub(super) iterations_loaded_for: Option<String>,
    pub retry_note_open: bool,
    pub retry_note: Option<gpui::Entity<crate::text_field::TextField>>,
    pub decision_busy: bool,
    /// Last `tasks.goal_decide` outcome, shown as a one-line status under
    /// the decision buttons. Cleared whenever the selection changes or a new
    /// decision starts.
    pub decision_result: Option<Result<String, String>>,
}

impl Default for GoalsState {
    fn default() -> Self {
        Self {
            tasks_requested: false,
            tasks: Loadable::Loading,
            smart_view: SmartView::default(),
            selected_task_id: None,
            iterations: Loadable::Loading,
            iterations_loaded_for: None,
            retry_note_open: false,
            retry_note: None,
            decision_busy: false,
            decision_result: None,
        }
    }
}

impl Global for GoalsState {}

impl GoalsState {
    /// The list header's manual refresh — re-fetches the task list and
    /// forces the currently-open task's iteration history to reload too.
    /// Deliberately does NOT reset `smart_view`/`selected_task_id`: unlike
    /// `dashboard.rs`'s full-page refresh, jumping the user back to the
    /// default view on every refresh would be disorienting for a page whose
    /// whole point is "keep watching this one goal".
    pub fn request_refresh(&mut self) {
        self.tasks_requested = false;
        self.tasks = Loadable::Loading;
        self.iterations_loaded_for = None;
        self.iterations = Loadable::Loading;
        self.decision_result = None;
    }

    pub fn select_task(&mut self, id: String) {
        if self.selected_task_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.selected_task_id = Some(id);
        self.retry_note_open = false;
        self.decision_result = None;
        self.iterations = Loadable::Loading;
        self.iterations_loaded_for = None;
    }

    pub fn select_view(&mut self, view: SmartView) {
        if self.smart_view == view {
            return;
        }
        self.smart_view = view;
        self.selected_task_id = None;
        self.retry_note_open = false;
        self.decision_result = None;
    }
}

// ── Fetch orchestration (mirrors `dashboard.rs`'s `spawn_call` shape, but
// operating on the `GoalsState` global instead of a `RootView` field — see
// this file's module doc comment for why `dashboard.rs`'s own private
// `spawn_call` isn't reused: it's scoped `fn` not `pub(crate) fn`, and
// touching `dashboard.rs` is out of this pass's assigned file set) ──────

pub(super) fn spawn_goal_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

pub(super) fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

/// Fires `tasks.list {"goal_mode": true}` exactly once per app run, the
/// moment the main `/ws` is authenticated — same "wait for Authenticated,
/// latch, only a manual refresh re-arms it" shape as `dashboard.rs::
/// maybe_fetch`, just entered from `render` (see module doc comment) instead
/// of `main.rs`'s poll loop.
fn maybe_fetch_tasks(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<GoalsState>().tasks_requested {
        return;
    }
    cx.global_mut::<GoalsState>().tasks_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "tasks.list", json!({"goal_mode": true}), |cx, result| {
        cx.default_global::<GoalsState>().tasks = result.map(|v| parse_goal_tasks(&v)).into();
    });
}

/// If nothing is selected yet, auto-select the first task of the current
/// smart view (matching OmniFocus's own "opening a perspective focuses its
/// first item" convention) — only once tasks have actually loaded.
fn maybe_autoselect(cx: &mut Context<RootView>) {
    let g = cx.default_global::<GoalsState>();
    if g.selected_task_id.is_some() {
        return;
    }
    let Loadable::Ready(tasks) = &g.tasks else { return };
    let today = Local::now().date_naive();
    let Some(first) = filtered_sorted(tasks, g.smart_view, today).into_iter().next() else { return };
    let id = first.id.clone();
    cx.global_mut::<GoalsState>().select_task(id);
}

/// Re-fetches `tasks.iterations` whenever the selected task id changes.
fn maybe_fetch_iterations(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<GoalsState>();
    let Some(task_id) = g.selected_task_id.clone() else { return };
    if g.iterations_loaded_for.as_deref() == Some(task_id.as_str()) {
        return;
    }
    cx.global_mut::<GoalsState>().iterations_loaded_for = Some(task_id.clone());
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "tasks.iterations", json!({"task_id": task_id}), |cx, result| {
        cx.default_global::<GoalsState>().iterations = result.map(|v| parse_iterations(&v)).into();
    });
}

// ── Shared labels ─────────────────────────────────────────────────────

pub(super) fn status_label(locale: Locale, status: &str) -> SharedString {
    let key = match status {
        "todo" | "pending" => "native.goals.status.todo",
        "in_progress" => "native.goals.status.inProgress",
        "review" => "native.goals.status.review",
        "revising" => "native.goals.status.revising",
        "needs_human" => "native.goals.status.needsHuman",
        "done" => "native.goals.status.done",
        "cancelled" => "native.goals.status.cancelled",
        "failed" => "native.goals.status.failed",
        "blocked" => "native.goals.status.blocked",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

/// Ring vs. filled + hue — "狀態圈語意著色：琥珀=需要你/藍=進行中/綠=完成"
/// per the task brief, extended to the statuses the canvas's 4 example rows
/// don't cover using `web/src/pages/GoalsPage.tsx`'s own `statusTone`
/// mapping (cancelled/failed → rose, review/revising → the same blue as
/// in_progress) rather than inventing a fifth hue.
fn status_dot_color(status: &str) -> u32 {
    match status {
        "needs_human" => theme::WARNING,
        "done" => theme::SUCCESS,
        "cancelled" | "failed" => theme::DESTRUCTIVE,
        "blocked" => theme::WARNING,
        _ => theme::BRAND, // todo/pending/in_progress/review/revising
    }
}

fn status_dot_filled(status: &str) -> bool {
    matches!(status, "done" | "cancelled" | "failed" | "blocked")
}

fn status_dot(status: &str, size: gpui::Pixels) -> Div {
    let color = status_dot_color(status);
    let dot = div().size(size).rounded_full().flex_shrink_0();
    if status_dot_filled(status) {
        dot.bg(theme::alpha(color, 1.0))
    } else {
        dot.border_2().border_color(theme::alpha(color, 1.0))
    }
}

/// "剛剛"/"N 分鐘前"/"N 小時前"/"N 天前" — `now` is a parameter (not
/// `Utc::now()` read internally) purely so this stays unit-testable with
/// fixed instants, same reasoning `chat/conversation_row.rs`'s
/// `classify_group` documents for taking `today` as a parameter.
pub(super) fn relative_time(locale: Locale, iso: &str, now: DateTime<Utc>) -> SharedString {
    let Some(dt) = DateTime::parse_from_rfc3339(iso).ok().map(|d| d.with_timezone(&Utc)) else {
        return SharedString::default();
    };
    let mins = (now - dt).num_minutes().max(0);
    if mins < 1 {
        i18n::t(locale, "native.goals.time.justNow")
    } else if mins < 60 {
        i18n::t1(locale, "native.goals.time.minutesAgo", "n", &mins.to_string())
    } else if mins < 60 * 24 {
        i18n::t1(locale, "native.goals.time.hoursAgo", "n", &(mins / 60).to_string())
    } else {
        i18n::t1(locale, "native.goals.time.daysAgo", "n", &(mins / 60 / 24).to_string())
    }
}

/// `"2026-08-20T10:00:00Z"` → `"08/20"`, local calendar date. Falls back to
/// the raw string when unparseable, same defensive shape `dashboard.rs::
/// short_time` uses.
pub(super) fn short_date(iso: &str) -> String {
    match local_date(iso) {
        Some(d) => d.format("%m/%d").to_string(),
        None => iso.to_string(),
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

const RAIL_WIDTH: f32 = 168.0;
const LIST_WIDTH: f32 = 320.0;

fn smart_view_row(view: SmartView, active: SmartView, count: usize, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selected = view == active;
    let id: SharedString = format!("goals-smartview-{view:?}").into();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h_8()
        .px_2p5()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.14)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_SM))
                .font_weight(if selected { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
                .text_color(if selected { theme::alpha(theme::BRAND, 1.0) } else { theme::alpha(theme::FOREGROUND, 1.0) })
                .child(i18n::t(locale, view.label_key())),
        )
        .when(count > 0, |el| el.child(badge(count.to_string(), BadgeVariant::Warning)))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<GoalsState>().select_view(view);
            cx.notify();
        }))
}

fn smart_view_rail(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<GoalsState>();
    let active = g.smart_view;
    let needs_human_count = match &g.tasks {
        Loadable::Ready(tasks) => tasks.iter().filter(|t| t.status == "needs_human").count(),
        _ => 0,
    };

    let mut rows = Vec::with_capacity(4);
    for view in SmartView::ALL {
        let count = if view == SmartView::NeedsHuman { needs_human_count } else { 0 };
        rows.push(smart_view_row(view, active, count, locale, cx));
    }

    div()
        .id("goals-rail")
        .w(px(RAIL_WIDTH))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SIDEBAR, 1.0))
        .border_1()
        .border_color(theme::sidebar_border())
        .shadow(theme::surface_shadow())
        .children(rows)
}

fn task_list_row(task: &GoalTask, selected: bool, locale: Locale, now: DateTime<Utc>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id: SharedString = format!("goals-task-{}", task.id).into();
    let time = relative_time(locale, &task.updated_at, now);
    let round_label = i18n::t1(locale, "native.home.card.goal.round", "n", &(task.revision_round + 1).to_string());
    let meta = if task.agent_id.is_empty() {
        round_label.to_string()
    } else {
        format!("{round_label}・{}", task.agent_id)
    };
    let task_id = task.id.clone();

    div()
        .id(id)
        .px_2p5()
        .py_2()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.14)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .flex()
        .gap_2p5()
        .child(div().mt(px(3.)).child(status_dot(&task.status, px(10.))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .overflow_hidden()
                                .child(SharedString::from(task.title.clone())),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(time),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(meta),
                        )
                        .when(task.status == "needs_human", |el| {
                            el.child(badge(i18n::t(locale, "native.goals.status.needsHuman"), BadgeVariant::Warning))
                        }),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<GoalsState>().select_task(task_id.clone());
            cx.notify();
        }))
}

fn task_list(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<GoalsState>();
    let view = g.smart_view;
    let selected_id = g.selected_task_id.clone();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px_1()
        .pb_1()
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, view.label_key())),
        )
        .child(
            div()
                .id("goals-refresh")
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.home.refresh"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<GoalsState>().request_refresh();
                    cx.notify();
                })),
        );

    let list = div().id("goals-list").w(px(LIST_WIDTH)).h_full().flex_shrink_0().flex().flex_col().gap_2().p_3().overflow_hidden().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SIDEBAR, 1.0)).border_1().border_color(theme::sidebar_border()).shadow(theme::surface_shadow()).child(header);

    match &cx.default_global::<GoalsState>().tasks {
        Loadable::Loading => {
            let mut body = div().id("goals-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_1p5().px_1();
            for _ in 0..4 {
                body = body.child(skeleton(px(LIST_WIDTH - 40.), px(44.)).rounded(px(theme::RADIUS_MD)));
            }
            return list.child(body);
        }
        Loadable::Failed(msg) => {
            return list.child(
                div()
                    .id("goals-list-error")
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>)),
            );
        }
        _ => {}
    }

    let today = Local::now().date_naive();
    let now = Utc::now();
    let tasks_snapshot = match &cx.default_global::<GoalsState>().tasks {
        Loadable::Ready(t) => t.clone(),
        _ => Vec::new(),
    };
    let filtered = filtered_sorted(&tasks_snapshot, view, today);

    if filtered.is_empty() {
        return list.child(
            div()
                .id("goals-list-empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_state("🎯", i18n::t(locale, "native.goals.list.empty"), None, None::<Div>)),
        );
    }

    let mut body = div().id("goals-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_0p5();
    for task in filtered {
        let selected = selected_id.as_deref() == Some(task.id.as_str());
        body = body.child(task_list_row(task, selected, locale, now, cx));
    }
    list.child(body)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch_tasks(state, cx);
    maybe_autoselect(cx);
    maybe_fetch_iterations(state, cx);

    div()
        .id("goals-page")
        .size_full()
        .flex()
        .gap_3()
        .child(smart_view_rail(state, cx))
        .child(task_list(state, cx))
        .child(inspector::render(state, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_goal_tasks`/`parse_iterations`/`SmartView::matches`/
    // `filtered_sorted` unit tests now live in `goals_data.rs` (the module
    // that owns them, per this file's 2nd file-size split — see that
    // file's own header comment).

    #[test]
    fn relative_time_buckets() {
        let now = DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(relative_time(Locale::ZhTw, "2026-08-21T12:00:00Z", now), "剛剛");
        assert_eq!(relative_time(Locale::ZhTw, "2026-08-21T11:30:00Z", now), "30 分鐘前");
        assert_eq!(relative_time(Locale::ZhTw, "2026-08-21T10:00:00Z", now), "2 小時前");
        assert_eq!(relative_time(Locale::ZhTw, "2026-08-18T12:00:00Z", now), "3 天前");
    }

    #[test]
    fn relative_time_clamps_future_timestamps_to_just_now() {
        let now = DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(relative_time(Locale::ZhTw, "2026-08-21T12:05:00Z", now), "剛剛");
    }

    #[test]
    fn relative_time_unparseable_is_empty_not_panicking() {
        let now = Utc::now();
        assert_eq!(relative_time(Locale::ZhTw, "not-a-date", now), "");
    }

    #[test]
    fn goals_state_select_task_resets_per_task_transient_fields() {
        let mut g = GoalsState {
            decision_result: Some(Ok("done".into())),
            retry_note_open: true,
            iterations: Loadable::Ready(vec![]),
            ..GoalsState::default()
        };
        g.select_task("t2".to_string());
        assert_eq!(g.selected_task_id.as_deref(), Some("t2"));
        assert!(!g.retry_note_open);
        assert!(g.decision_result.is_none());
        assert!(matches!(g.iterations, Loadable::Loading));
    }

    #[test]
    fn goals_state_select_task_is_a_noop_for_the_same_id() {
        let mut g = GoalsState::default();
        g.select_task("t1".to_string());
        g.iterations = Loadable::Ready(vec![]);
        g.select_task("t1".to_string());
        // Re-selecting the SAME id must not clobber already-loaded iterations.
        assert!(matches!(g.iterations, Loadable::Ready(_)));
    }

    #[test]
    fn goals_state_select_view_clears_selection() {
        let mut g = GoalsState::default();
        g.select_task("t1".to_string());
        g.select_view(SmartView::Done);
        assert_eq!(g.smart_view, SmartView::Done);
        assert!(g.selected_task_id.is_none());
    }

    #[test]
    fn status_dot_shape_matches_the_canvas_ring_vs_filled_convention() {
        assert!(!status_dot_filled("needs_human"));
        assert!(!status_dot_filled("in_progress"));
        assert!(status_dot_filled("done"));
        assert_eq!(status_dot_color("needs_human"), theme::WARNING);
        assert_eq!(status_dot_color("in_progress"), theme::BRAND);
        assert_eq!(status_dot_color("done"), theme::SUCCESS);
    }

    #[test]
    fn short_date_formats_local_calendar_date() {
        assert_eq!(short_date("2026-08-21T10:00:00Z"), "08/21");
        assert_eq!(short_date("not-a-date"), "not-a-date");
    }
}
