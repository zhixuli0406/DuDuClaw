// S4b third wave — Screen "任務" (p09, `nav.rs` id `tasks`, already listed
// under the 任務與目標 area's Column-2 page list, same as `goals` was before
// the second wave wired it — this pass wires that existing id to a real
// page instead of `shell.rs`'s generic placeholder).
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Tasks.dc.html` —
// the SAME OmniFocus-style three-pane skeleton `goals.rs` already
// establishes (smart-view rail / list / right quick-view inspector), with a
// different smart-view set (今天/指派給我/全部/封存 — see `tasks_data.rs`'s
// module doc comment for exactly what each means and why) and a richer
// right panel (attributes / description / latest progress + a link to the
// full detail page). `TaskDetail.dc.html` is the full detail page reached
// via that link — see `tasks_detail.rs`.
//
// ── Why state lives in a `gpui::Global`, not a `RootView` field ─────────
// Identical reasoning to `goals.rs`'s own header comment: this pass's task
// brief also forbids touching `main.rs`, so `TasksState` is a
// `gpui::Global` rather than a new `RootView` field. See that file's doc
// comment for the fuller argument (gpui's own documented idiom for state no
// single `Entity` naturally owns).
//
// ── In-page navigation instead of a nav-tree page ────────────────────────
// "打開完整詳情" (quick-view → full detail) does NOT change `nav.rs`'s
// `active_page` or touch `shell.rs`'s Column 1/2 selection — it flips
// `TasksState::mode` and re-renders the SAME `active_page == "tasks"` slot,
// the same "select within the page, not through the nav tree" pattern
// `chat.rs` already uses for picking a conversation. A breadcrumb inside
// `tasks_detail.rs` flips `mode` back to `List`.
//
// ── RPC shapes — see `tasks_data.rs`'s module doc comment for `tasks.list`/
// `tasks.list_page`/`users.me`. This file additionally uses:
//   `tasks.update {"task_id","status":"done"}` → 標記完成
//     (`handle_tasks_update`, ~L28846) — same verb `web/src/pages/
//     TaskDetailPage.tsx`'s `applyStatus('done')` uses.
//   `tasks.comment {"task_id","body"}` → 催一下進度
//     (`handle_tasks_comment`, ~L29237) — a generic, already-existing verb
//     reused rather than inventing a new "nudge" RPC (same discipline
//     `goals_inspector.rs`'s header comment documents for its own
//     three-button mapping onto `tasks.goal_decide`'s existing verbs): no
//     dedicated "ask the agent to report progress" endpoint exists, and a
//     comment is genuinely visible to the agent (broadcast as `task.
//     comment`, and any dispatch round can read `tasks.comments`).

use chrono::{DateTime, Local, Utc};
use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::screens::dashboard::Loadable;
// Reused rather than duplicated — both are generic RPC-dispatch helpers
// with no goal-specific behavior (see this file's own header comment).
use crate::screens::goals::{relative_time, spawn_goal_call as spawn_call};
use crate::screens::tasks_data::{filtered, parse_my_agent_ids, parse_tasks, SmartView, TaskItem};
use crate::screens::{tasks_detail, tasks_detail_data, tasks_quickview};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── In-page navigation state ──────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TasksMode {
    #[default]
    List,
    /// Task id currently shown by `tasks_detail::render`.
    Detail(String),
}

// ── Global state ───────────────────────────────────────────────────────

pub struct TasksState {
    list_requested: bool,
    pub tasks: Loadable<Vec<TaskItem>>,
    archived_requested: bool,
    pub archived_tasks: Loadable<Vec<TaskItem>>,
    me_requested: bool,
    /// `None` ⇒ no restriction (admin fallback / no explicit bindings —
    /// see `tasks_data.rs`'s module doc comment). `Some(ids)` ⇒ the real
    /// agent-name allow-list for 指派給我.
    pub my_agent_ids: Option<Vec<String>>,
    pub smart_view: SmartView,
    /// Quick-view selection (List mode only).
    pub selected_task_id: Option<String>,
    pub mode: TasksMode,
    /// Latch: the task id whose `tasks.timeline` is already loaded or in
    /// flight — `pub(super)` so `tasks_detail.rs`'s own fetch orchestration
    /// can read/clear it (same split `goals.rs`/`goals_inspector.rs`
    /// establish for `iterations_loaded_for`).
    pub(super) detail_loaded_for: Option<String>,
    pub detail: Loadable<tasks_detail_data::TimelineData>,
    pub(super) artifacts_loaded_for: Option<String>,
    pub artifacts: Loadable<tasks_detail_data::ArtifactsData>,
    pub action_busy: bool,
    /// Last 標記完成/催一下進度 outcome. `Ok` holds a **stable i18n key**
    /// (not pre-localized text) — the async RPC-response closure that sets
    /// this has no `Locale` in scope (see `tasks_detail.rs::
    /// dispatch_mark_done`), so localization happens at render time instead,
    /// same "format at render time, not at dispatch time" split
    /// `goals_inspector.rs::decision_row` already uses for its own error
    /// half. Cleared on navigation.
    pub action_result: Option<Result<&'static str, String>>,
}

impl Default for TasksState {
    fn default() -> Self {
        Self {
            list_requested: false,
            tasks: Loadable::Loading,
            archived_requested: false,
            archived_tasks: Loadable::Loading,
            me_requested: false,
            my_agent_ids: None,
            smart_view: SmartView::default(),
            selected_task_id: None,
            mode: TasksMode::List,
            detail_loaded_for: None,
            detail: Loadable::Loading,
            artifacts_loaded_for: None,
            artifacts: Loadable::Loading,
            action_busy: false,
            action_result: None,
        }
    }
}

impl Global for TasksState {}

impl TasksState {
    pub fn request_refresh(&mut self) {
        self.list_requested = false;
        self.tasks = Loadable::Loading;
        if self.smart_view == SmartView::Archived {
            self.archived_requested = false;
            self.archived_tasks = Loadable::Loading;
        }
    }

    pub fn select_task(&mut self, id: String) {
        if self.selected_task_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.selected_task_id = Some(id);
    }

    pub fn select_view(&mut self, view: SmartView) {
        if self.smart_view == view {
            return;
        }
        self.smart_view = view;
        self.selected_task_id = None;
    }

    /// Quick-view "打開完整詳情" → full detail page, in place.
    pub fn open_detail(&mut self, id: String) {
        self.detail_loaded_for = None;
        self.detail = Loadable::Loading;
        self.artifacts_loaded_for = None;
        self.artifacts = Loadable::Loading;
        self.action_result = None;
        self.mode = TasksMode::Detail(id);
    }

    /// Detail page breadcrumb → back to the list, same selection as before.
    pub fn back_to_list(&mut self) {
        self.mode = TasksMode::List;
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch_tasks(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<TasksState>().list_requested {
        return;
    }
    cx.global_mut::<TasksState>().list_requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "tasks.list", json!({"goal_mode": false}), |cx, result| {
        cx.default_global::<TasksState>().tasks = result.map(|v| parse_tasks(&v)).into();
    });
}

/// Only fires once the 封存 smart view is actually selected — archived rows
/// are a genuinely separate query (see `tasks_data.rs`'s module doc
/// comment), so there is no reason to pull them for a user who never opens
/// that view. `limit: 200` is `list_tasks_paginated`'s own hard clamp
/// ceiling (`crates/duduclaw-gateway/src/task_store.rs`) — a first-pass cap,
/// not a real pager; a board with more than 200 archived tasks would silently
/// show only the 200 most-recently-updated ones. No pagination UI exists in
/// this pass (honest limitation, not attempted).
const ARCHIVED_FETCH_LIMIT: i64 = 200;

fn maybe_fetch_archived(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<TasksState>();
    if g.smart_view != SmartView::Archived || g.archived_requested {
        return;
    }
    cx.global_mut::<TasksState>().archived_requested = true;
    let tx = state.session_tx.clone();
    let params = json!({"goal_mode": false, "archived": true, "limit": ARCHIVED_FETCH_LIMIT, "offset": 0});
    spawn_call(cx, tx, "tasks.list_page", params, |cx, result| {
        cx.default_global::<TasksState>().archived_tasks = result.map(|v| parse_tasks(&v)).into();
    });
}

fn maybe_fetch_me(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<TasksState>().me_requested {
        return;
    }
    cx.global_mut::<TasksState>().me_requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "users.me", json!({}), |cx, result| {
        // Fail-open to "no restriction" on any error — a transient failure
        // to resolve identity must never silently hide tasks that are
        // genuinely 指派給我 (same fail-open discipline the coding
        // conventions require for security-relevant gates; this is not a
        // security gate, but the failure direction is the same: showing too
        // much is recoverable by the user re-narrowing, showing too little
        // looks like data loss).
        cx.default_global::<TasksState>().my_agent_ids = result.ok().and_then(|v| parse_my_agent_ids(&v));
    });
}

/// Mirrors `goals.rs::maybe_autoselect` — focuses the current smart view's
/// first row once loaded, only in List mode and only when nothing is picked
/// yet.
fn maybe_autoselect(cx: &mut Context<RootView>) {
    let g = cx.default_global::<TasksState>();
    if g.mode != TasksMode::List || g.selected_task_id.is_some() {
        return;
    }
    let today = Local::now().date_naive();
    let view = g.smart_view;
    let my_agent_ids = g.my_agent_ids.clone();
    let backing: Option<&Vec<TaskItem>> = match view {
        SmartView::Archived => match &g.archived_tasks {
            Loadable::Ready(list) => Some(list),
            _ => None,
        },
        _ => match &g.tasks {
            Loadable::Ready(list) => Some(list),
            _ => None,
        },
    };
    let Some(list) = backing else { return };
    let Some(first) = filtered(list, view, today, my_agent_ids.as_deref()).into_iter().next() else { return };
    let id = first.id.clone();
    cx.global_mut::<TasksState>().select_task(id);
}

// ── Shared label/formatting helpers ───────────────────────────────────

/// low→grey / medium→amber / high→blue / urgent→red — matches
/// `web/src/components/ui/PriorityIcon.tsx`'s own `COLORS` table (status-
/// icon hues, not the one "高"=amber example the visual-authority canvas
/// happens to show for its single sample row — that single data point is
/// insufficient to reverse an already-shipped, four-value semantic mapping
/// used identically on the web dashboard).
fn priority_badge_variant(priority: &str) -> BadgeVariant {
    match priority {
        "urgent" => BadgeVariant::Destructive,
        "high" => BadgeVariant::Info,
        "low" => BadgeVariant::Secondary,
        _ => BadgeVariant::Warning, // medium, and any unrecognized value
    }
}

pub(super) fn priority_label(locale: Locale, priority: &str) -> SharedString {
    let key = match priority {
        "low" => "native.tasks.priority.low",
        "medium" => "native.tasks.priority.medium",
        "high" => "native.tasks.priority.high",
        "urgent" => "native.tasks.priority.urgent",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

/// Ring vs. filled + hue — same 3-state convention `goals.rs::status_dot`
/// establishes, duplicated here (not widened to `pub(super)`) for the same
/// "thin intentional 6-line duplication over reaching into a sibling
/// page's private styling helper" reasoning `goals_inspector.rs::
/// goals_status_dot` already documents for this crate.
fn status_dot_color(status: &str) -> u32 {
    match status {
        "needs_human" => theme::WARNING,
        "done" => theme::SUCCESS,
        "cancelled" | "failed" => theme::DESTRUCTIVE,
        "blocked" => theme::WARNING,
        _ => theme::BRAND,
    }
}

fn status_dot_filled(status: &str) -> bool {
    matches!(status, "done" | "cancelled" | "failed" | "blocked")
}

pub(super) fn status_dot(status: &str, size: gpui::Pixels) -> Div {
    let color = status_dot_color(status);
    let dot = div().size(size).rounded_full().flex_shrink_0();
    if status_dot_filled(status) {
        dot.bg(theme::alpha(color, 1.0))
    } else {
        dot.border_2().border_color(theme::alpha(color, 1.0))
    }
}

/// Codepoint-count truncation with an ellipsis — CJK-safe, same reasoning
/// `goals_inspector.rs::truncate_chars`'s own doc comment gives (this crate
/// has no `duduclaw-core` dependency to reuse `truncate_chars` from).
pub(super) fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// The list row's one-line subtitle: prefer the agent's own progress report
/// (`result_summary`) over the task's static `description` — a status
/// update is more useful at a glance than the original ask, matching the
/// canvas's own example rows (all four show progress text, not the
/// original description).
pub(super) fn row_subtitle(task: &TaskItem) -> Option<String> {
    let text = task.result_summary.as_deref().or_else(|| {
        let d = task.description.trim();
        (!d.is_empty()).then_some(d)
    })?;
    Some(truncate_chars(text.trim(), 42))
}

// ── Rendering ──────────────────────────────────────────────────────────

const RAIL_WIDTH: f32 = 168.0;
const LIST_WIDTH: f32 = 320.0;

/// How many rows `view` currently holds, or `None` while its backing data
/// hasn't loaded yet (never shows a misleading "0" badge before the fetch
/// resolves).
fn view_count(g: &TasksState, view: SmartView, today: chrono::NaiveDate) -> Option<usize> {
    match view {
        SmartView::Archived => match &g.archived_tasks {
            Loadable::Ready(list) => Some(list.len()),
            _ => None,
        },
        _ => match &g.tasks {
            Loadable::Ready(list) => Some(filtered(list, view, today, g.my_agent_ids.as_deref()).len()),
            _ => None,
        },
    }
}

fn smart_view_row(
    view: SmartView,
    active: SmartView,
    count: Option<usize>,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let selected = view == active;
    let id: SharedString = format!("tasks-smartview-{view:?}").into();
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
        .when_some(count.filter(|n| *n > 0), |el, n| el.child(badge(n.to_string(), BadgeVariant::Warning)))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<TasksState>().select_view(view);
            cx.notify();
        }))
}

fn smart_view_rail(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let today = Local::now().date_naive();
    let g = cx.default_global::<TasksState>();
    let active = g.smart_view;

    let mut rows = Vec::with_capacity(SmartView::ALL.len());
    for view in SmartView::ALL {
        let count = view_count(cx.default_global::<TasksState>(), view, today);
        rows.push(smart_view_row(view, active, count, locale, cx));
    }

    div()
        .id("tasks-rail")
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

fn task_list_row(task: &TaskItem, selected: bool, locale: Locale, now: DateTime<Utc>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id: SharedString = format!("tasks-row-{}", task.id).into();
    let time = relative_time(locale, &task.updated_at, now);
    let subtitle = row_subtitle(task);
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
                        .when(task.priority == "high" || task.priority == "urgent", |el| {
                            el.child(badge(priority_label(locale, &task.priority), priority_badge_variant(&task.priority)))
                        })
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
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(match subtitle {
                            Some(s) if !task.agent_id.is_empty() => format!("{}・{s}", task.agent_id),
                            Some(s) => s,
                            None => task.agent_id.clone(),
                        }),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<TasksState>().select_task(task_id.clone());
            cx.notify();
        }))
}

fn task_list(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    // Copied/cloned out (not borrowed) up front — this `g`'s borrow of `cx`
    // must end before the header's `cx.listener(...)` below, which needs
    // `cx` mutably again. `source` is fetched via a SECOND, fresh
    // `cx.default_global` call further down (same "re-borrow rather than
    // hold one long-lived reference across a listener" shape
    // `goals.rs::task_list` already establishes).
    let g = cx.default_global::<TasksState>();
    let view = g.smart_view;
    let selected_id = g.selected_task_id.clone();
    let my_agent_ids = g.my_agent_ids.clone();

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
                .id("tasks-refresh")
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.home.refresh"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<TasksState>().request_refresh();
                    cx.notify();
                })),
        );

    let list = div().id("tasks-list").w(px(LIST_WIDTH)).h_full().flex_shrink_0().flex().flex_col().gap_2().p_3().overflow_hidden().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SIDEBAR, 1.0)).border_1().border_color(theme::sidebar_border()).shadow(theme::surface_shadow()).child(header);

    let g = cx.default_global::<TasksState>();
    let source: &Loadable<Vec<TaskItem>> = if view == SmartView::Archived { &g.archived_tasks } else { &g.tasks };
    match source {
        Loadable::Loading => {
            let mut body = div().id("tasks-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_1p5().px_1();
            for _ in 0..4 {
                body = body.child(skeleton(px(LIST_WIDTH - 40.), px(44.)).rounded(px(theme::RADIUS_MD)));
            }
            return list.child(body);
        }
        Loadable::Failed(msg) => {
            return list.child(
                div()
                    .id("tasks-list-error")
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
    let snapshot: Vec<TaskItem> = match source {
        Loadable::Ready(t) => t.clone(),
        _ => Vec::new(),
    };
    let rows = filtered(&snapshot, view, today, my_agent_ids.as_deref());

    if rows.is_empty() {
        return list.child(
            div()
                .id("tasks-list-empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_state("📋", i18n::t(locale, "native.tasks.list.empty"), None, None::<Div>)),
        );
    }

    let mut body = div().id("tasks-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_0p5();
    for task in rows {
        let selected = selected_id.as_deref() == Some(task.id.as_str());
        body = body.child(task_list_row(task, selected, locale, now, cx));
    }
    list.child(body)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch_tasks(state, cx);
    maybe_fetch_me(state, cx);

    let mode = cx.default_global::<TasksState>().mode.clone();
    match mode {
        TasksMode::List => {
            maybe_fetch_archived(state, cx);
            maybe_autoselect(cx);
            div()
                .id("tasks-page")
                .size_full()
                .flex()
                .gap_3()
                .child(smart_view_rail(state, cx))
                .child(task_list(state, cx))
                .child(tasks_quickview::render(state, cx))
        }
        TasksMode::Detail(id) => tasks_detail::render(state, cx, &id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_state_select_task_is_a_noop_for_the_same_id() {
        let mut g = TasksState::default();
        g.select_task("t1".to_string());
        g.action_result = Some(Ok("x"));
        g.select_task("t1".to_string());
        // Re-selecting the SAME id must not clobber transient state.
        assert!(g.action_result.is_some());
    }

    #[test]
    fn tasks_state_select_view_clears_selection() {
        let mut g = TasksState::default();
        g.select_task("t1".to_string());
        g.select_view(SmartView::All);
        assert_eq!(g.smart_view, SmartView::All);
        assert!(g.selected_task_id.is_none());
    }

    #[test]
    fn tasks_state_open_detail_resets_detail_fetch_latches() {
        let mut g = TasksState {
            detail_loaded_for: Some("old".to_string()),
            detail: Loadable::Ready(tasks_detail_data::TimelineData::default()),
            ..TasksState::default()
        };
        g.open_detail("t2".to_string());
        assert_eq!(g.mode, TasksMode::Detail("t2".to_string()));
        assert!(g.detail_loaded_for.is_none());
        assert!(matches!(g.detail, Loadable::Loading));
    }

    #[test]
    fn tasks_state_back_to_list_restores_list_mode() {
        let mut g = TasksState::default();
        g.open_detail("t2".to_string());
        g.back_to_list();
        assert_eq!(g.mode, TasksMode::List);
    }

    #[test]
    fn status_dot_shape_matches_the_shared_convention() {
        assert!(!status_dot_filled("in_progress"));
        assert!(status_dot_filled("done"));
        assert_eq!(status_dot_color("done"), theme::SUCCESS);
    }

    #[test]
    fn priority_badge_variant_maps_all_four_values() {
        assert!(matches!(priority_badge_variant("urgent"), BadgeVariant::Destructive));
        assert!(matches!(priority_badge_variant("high"), BadgeVariant::Info));
        assert!(matches!(priority_badge_variant("medium"), BadgeVariant::Warning));
        assert!(matches!(priority_badge_variant("low"), BadgeVariant::Secondary));
    }

    #[test]
    fn truncate_chars_is_codepoint_safe_on_cjk() {
        assert_eq!(truncate_chars("一二三四五", 3), "一二三…");
        assert_eq!(truncate_chars("短", 10), "短");
    }

    #[test]
    fn row_subtitle_prefers_result_summary_over_description() {
        let mut t = sample_task();
        t.description = "原始描述".to_string();
        t.result_summary = Some("最新進度".to_string());
        assert_eq!(row_subtitle(&t).as_deref(), Some("最新進度"));
        t.result_summary = None;
        assert_eq!(row_subtitle(&t).as_deref(), Some("原始描述"));
        t.description = "  ".to_string();
        assert_eq!(row_subtitle(&t), None);
    }

    fn sample_task() -> TaskItem {
        TaskItem {
            id: "t1".into(),
            title: "x".into(),
            description: String::new(),
            status: "todo".into(),
            priority: "medium".into(),
            assigned_to: "cs-lead".into(),
            agent_id: "cs-lead".into(),
            created_by: "system".into(),
            created_at: "2026-08-01T00:00:00Z".into(),
            updated_at: "2026-08-21T00:00:00Z".into(),
            judge_feedback: None,
            blocked_reason: None,
            parent_task_id: None,
            tags: Vec::new(),
            result_summary: None,
            deadline_at: None,
            archived: false,
            pinned: false,
            channel: None,
            channel_link: None,
        }
    }
}
