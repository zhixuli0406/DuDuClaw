// Full detail page for one task (p10, `TaskDetail.dc.html`). Reached via
// `tasks_quickview.rs`'s "打開完整詳情" link — an in-page mode flip
// (`TasksState::mode = Detail(id)`), not a nav-tree navigation; see
// `tasks.rs`'s module doc comment for why.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/TaskDetail.dc.html`
// — NO tabs (unlike `web/src/components/task/TaskBottomTabs.tsx`'s 產物／
// 檔案／變更／過程 tab strip): breadcrumb + header (status dot + title +
// 標記完成/催一下進度 buttons) + one continuously-scrolling body (描述 /
// 產物 / 過程), plus a fixed 320px right column (屬性 / 權限與稽核 / 相關).
//
// The data model, RPC shapes, fetch orchestration, and 標記完成/催一下進度
// write dispatch all live in the sibling `tasks_detail_data.rs` (same
// file-size-driven split `tasks.rs`/`tasks_data.rs` establish, keeping this
// file under this crate's own <800-line convention) — this file is pure
// rendering, plus the small pure helpers below (`channel_link_for`/
// `find_title`/`subtasks_of`/`related_info`) that only need `&TasksState`,
// not `cx`.

use chrono::{DateTime, Utc};
use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::screens::goals::{relative_time, short_date, status_label};
use crate::screens::tasks::{self, TasksState};
use crate::screens::tasks_data::TaskItem;
use crate::screens::tasks_detail_data::{
    dispatch_mark_done, dispatch_nudge, format_bytes, is_terminal, maybe_fetch_artifacts, maybe_fetch_detail, ActivityItem,
    ArtifactsData, TaskArtifactItem, TimelineData,
};
use crate::theme;
use crate::RootView;

// ── Shared small helpers ─────────────────────────────────────────────

fn section_label(locale: Locale, key: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, key))
}

/// Local duplicate of `tasks_quickview.rs::box_frame` — see that file's own
/// comment on why a small styling helper is duplicated rather than shared
/// across pages.
fn box_frame(title: SharedString, body: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(title),
        )
        .child(body)
}

fn attr_row(label: SharedString, value: Div) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(value)
}

fn attr_text(text: SharedString, color: gpui::Rgba) -> Div {
    div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::MEDIUM).text_color(color).child(text)
}

/// `tasks.timeline`'s bare `task_row_to_json` carries no `channel_link` —
/// that augmentation only happens in `handle_tasks_list`/
/// `handle_tasks_list_page` (see `tasks_detail_data.rs`'s module doc
/// comment). Recovered here from whichever of `tasks.rs`'s own list caches
/// already has this row (it was reached BY opening this page from that
/// list), honestly `None` when neither does (e.g. this page were ever
/// deep-linked directly).
fn channel_link_for<'a>(g: &'a TasksState, task_id: &str) -> Option<&'a str> {
    let from = |l: &'a Loadable<Vec<TaskItem>>| match l {
        Loadable::Ready(list) => list.iter().find(|t| t.id == task_id).and_then(|t| t.channel_link.as_deref()),
        _ => None,
    };
    from(&g.tasks).or_else(|| from(&g.archived_tasks))
}

fn find_title<'a>(g: &'a TasksState, task_id: &str) -> Option<&'a str> {
    let from = |l: &'a Loadable<Vec<TaskItem>>| match l {
        Loadable::Ready(list) => list.iter().find(|t| t.id == task_id).map(|t| t.title.as_str()),
        _ => None,
    };
    from(&g.tasks).or_else(|| from(&g.archived_tasks))
}

fn subtasks_of(g: &TasksState, parent_id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in [&g.tasks, &g.archived_tasks] {
        if let Loadable::Ready(list) = l {
            for t in list {
                if t.parent_task_id.as_deref() == Some(parent_id) {
                    out.push((t.id.clone(), t.title.clone()));
                }
            }
        }
    }
    out.truncate(5);
    out
}

// ── Header ────────────────────────────────────────────────────────────

fn breadcrumb(locale: Locale, task: &TaskItem, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("tasks-detail-breadcrumb")
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("tasks-detail-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.tasks.detail.breadcrumbRoot"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<TasksState>().back_to_list();
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(tasks::truncate_chars(&task.title, 30)))
}

fn header_row(state: &RootView, task: &TaskItem, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let busy = cx.default_global::<TasksState>().action_busy;
    let done = task.status == "done";
    let task_id_done = task.id.clone();
    let task_id_nudge = task.id.clone();
    let nudge_body = i18n::t(locale, "native.tasks.detail.nudgeMessage").to_string();

    let mut row = div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(tasks::status_dot(&task.status, px(14.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(task.title.clone())),
        );

    if !done {
        row = row.child(button(
            "tasks-detail-mark-done",
            i18n::t(locale, "native.tasks.detail.markDone"),
            ButtonVariant::Secondary,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                let tx = this.session_tx.clone();
                dispatch_mark_done(cx, tx, task_id_done.clone());
                cx.notify();
            }),
        ));
    }
    if !is_terminal(&task.status) {
        row = row.child(button(
            "tasks-detail-nudge",
            i18n::t(locale, "native.tasks.detail.nudge"),
            ButtonVariant::Primary,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                let tx = this.session_tx.clone();
                dispatch_nudge(cx, tx, task_id_nudge.clone(), nudge_body.clone());
                cx.notify();
            }),
        ));
    }
    row
}

fn action_result_line(locale: Locale, result: &Option<Result<&'static str, String>>) -> Option<Div> {
    let r = result.as_ref()?;
    let (color, text) = match r {
        Ok(key) => (theme::SUCCESS, i18n::t(locale, key).to_string()),
        Err(msg) => (theme::DESTRUCTIVE, i18n::t1(locale, "native.tasks.detail.actionErrorPrefix", "message", msg).to_string()),
    };
    Some(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text))
}

fn meta_line(locale: Locale, task: &TaskItem) -> SharedString {
    let agent = if task.agent_id.is_empty() {
        i18n::t(locale, "native.tasks.agent.unassigned").to_string()
    } else {
        task.agent_id.clone()
    };
    i18n::tn(locale, "native.tasks.detail.meta", &[("agent", &agent), ("created", &short_date(&task.created_at))])
}

// ── Body: description / artifacts / process ──────────────────────────

fn description_section(locale: Locale, task: &TaskItem) -> Div {
    let body = if task.description.trim().is_empty() {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.tasks.detail.descriptionEmpty"))
    } else {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(task.description.clone())
    };
    div().flex().flex_col().gap_1p5().child(section_label(locale, "native.tasks.detail.description")).child(body)
}

fn artifact_card(locale: Locale, a: &TaskArtifactItem) -> Div {
    let meta = match a.size {
        Some(n) => format!("{} · {}", format_bytes(n), short_date(&a.produced_at)),
        None => short_date(&a.produced_at),
    };
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .w(px(170.))
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .overflow_hidden()
                .child(a.name.clone()),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta))
        .when(a.attribution == "inferred", |el| {
            el.child(badge(i18n::t(locale, "native.tasks.detail.artifactInferred"), BadgeVariant::Outline))
        })
}

fn artifacts_section(locale: Locale, data: &Loadable<ArtifactsData>) -> Div {
    let body: Div = match data {
        Loadable::Loading => {
            let mut row = div().flex().gap_2();
            for _ in 0..2 {
                row = row.child(skeleton(px(170.), px(48.)).rounded(px(theme::RADIUS_LG)));
            }
            row
        }
        Loadable::Failed(msg) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(d) if d.items.is_empty() => div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.tasks.detail.artifactsEmpty")),
        Loadable::Ready(d) => {
            let mut row = div().flex().flex_wrap().gap_2();
            for a in &d.items {
                row = row.child(artifact_card(locale, a));
            }
            row
        }
    };
    div().flex().flex_col().gap_1p5().child(section_label(locale, "native.tasks.detail.artifacts")).child(body)
}

/// Event-type→text fallback when `summary` is blank — deliberately just the
/// raw type token rather than a full translation table for all 30+ event
/// types this system emits (`autopilot_engine.rs`, `goal_notify.rs`, ...);
/// most rows DO carry a human summary (`ActivityRow::summary` is populated
/// at every real call site) so this is a rare, honest fallback, not the
/// common case.
fn activity_text(e: &ActivityItem) -> SharedString {
    if !e.summary.trim().is_empty() {
        e.summary.clone().into()
    } else {
        e.event_type.clone().into()
    }
}

fn timeline_row(locale: Locale, e: &ActivityItem, now: DateTime<Utc>) -> Div {
    div()
        .flex()
        .gap_2p5()
        .items_start()
        .child(div().mt(px(5.)).size(px(8.)).rounded_full().bg(theme::alpha(theme::SUCCESS, 1.0)).flex_shrink_0())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(activity_text(e)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(relative_time(locale, &e.at, now))),
        )
}

fn process_section(locale: Locale, data: &Loadable<TimelineData>, now: DateTime<Utc>) -> Div {
    let body: Div = match data {
        Loadable::Loading => {
            let mut col = div().flex().flex_col().gap_2();
            for _ in 0..3 {
                col = col.child(skeleton(px(360.), px(14.)));
            }
            col
        }
        Loadable::Failed(msg) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(d) if d.activity.is_empty() => div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.tasks.detail.processEmpty")),
        Loadable::Ready(d) => {
            let mut col = div().flex().flex_col().gap_2p5();
            for e in &d.activity {
                col = col.child(timeline_row(locale, e, now));
            }
            col
        }
    };
    div().flex().flex_col().gap_1p5().child(section_label(locale, "native.tasks.detail.process")).child(body)
}

// ── Right column: attributes / audit / related ────────────────────────

fn attributes_card(locale: Locale, task: &TaskItem) -> Div {
    let agent: SharedString = if task.agent_id.is_empty() {
        i18n::t(locale, "native.tasks.agent.unassigned")
    } else {
        task.agent_id.clone().into()
    };
    let mut body = div().flex().flex_col().gap_2().child(attr_row(
        i18n::t(locale, "native.tasks.quickview.status"),
        attr_text(status_label(locale, &task.status), theme::alpha(theme::BRAND, 1.0)),
    ));
    body = body.child(attr_row(
        i18n::t(locale, "native.tasks.detail.assignee"),
        attr_text(agent, theme::alpha(theme::FOREGROUND, 1.0)),
    ));
    body = body.child(attr_row(
        i18n::t(locale, "native.tasks.quickview.priority"),
        attr_text(tasks::priority_label(locale, &task.priority), theme::alpha(theme::FOREGROUND, 1.0)),
    ));
    if let Some(deadline) = &task.deadline_at {
        body = body.child(attr_row(
            i18n::t(locale, "native.tasks.quickview.deadline"),
            attr_text(short_date(deadline).into(), theme::alpha(theme::FOREGROUND, 1.0)),
        ));
    }
    if !task.tags.is_empty() {
        body = body.child(attr_row(
            i18n::t(locale, "native.tasks.quickview.tags"),
            attr_text(task.tags.join(" · ").into(), theme::alpha(theme::FOREGROUND, 1.0)),
        ));
    }
    box_frame(i18n::t(locale, "native.tasks.quickview.attributes"), body)
}

fn audit_card(locale: Locale, task: &TaskItem, tool_call_count: u64) -> Div {
    let calls_line = if tool_call_count > 0 {
        i18n::t1(locale, "native.tasks.detail.auditToolCalls", "n", &tool_call_count.to_string())
    } else {
        i18n::t(locale, "native.tasks.detail.auditNone")
    };
    let source_line = match &task.channel {
        Some(ch) => i18n::t1(locale, "native.tasks.detail.auditSourceChannel", "channel", ch),
        None => i18n::t(locale, "native.tasks.detail.auditSourceManual"),
    };
    let body = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(calls_line))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(source_line));
    box_frame(i18n::t(locale, "native.tasks.detail.auditTitle"), body)
}

/// Owned snapshot of everything `related_card` needs — split out from
/// rendering (see `render`'s own comment) so gathering it only needs `&
/// TasksState` (no `cx`), letting that borrow end well before the caller
/// needs `cx` mutably again for `header_row`/`breadcrumb`'s own listeners.
struct RelatedInfo {
    /// `(task_id, resolved title — falls back to the raw id when the
    /// parent isn't in either loaded list cache)`.
    parent: Option<(String, String)>,
    /// `(task_id, title)`, capped at 5 by `subtasks_of`.
    subtasks: Vec<(String, String)>,
    channel_link: Option<String>,
}

fn related_info(g: &TasksState, task: &TaskItem) -> Option<RelatedInfo> {
    let parent = task.parent_task_id.as_ref().map(|pid| {
        let title = find_title(g, pid).map(str::to_string).unwrap_or_else(|| pid.clone());
        (pid.clone(), title)
    });
    let subtasks = subtasks_of(g, &task.id);
    let channel_link = channel_link_for(g, &task.id).map(str::to_string);
    if parent.is_none() && subtasks.is_empty() && channel_link.is_none() {
        return None;
    }
    Some(RelatedInfo { parent, subtasks, channel_link })
}

fn related_card(locale: Locale, info: Option<RelatedInfo>, cx: &mut Context<RootView>) -> Option<Div> {
    let info = info?;
    let mut body = div().flex().flex_col().gap_2();

    if let Some((pid, title)) = info.parent {
        body = body.child(link_row(
            format!("tasks-detail-related-parent-{pid}"),
            i18n::t1(locale, "native.tasks.detail.relatedParent", "title", &title),
            cx.listener(move |_this, _ev, _window, cx| {
                cx.global_mut::<TasksState>().open_detail(pid.clone());
                cx.notify();
            }),
        ));
    }
    for (sid, title) in info.subtasks {
        body = body.child(link_row(
            format!("tasks-detail-related-sub-{sid}"),
            title.clone().into(),
            cx.listener(move |_this, _ev, _window, cx| {
                cx.global_mut::<TasksState>().open_detail(sid.clone());
                cx.notify();
            }),
        ));
    }
    if let Some(link) = info.channel_link {
        body = body.child(link_row(
            "tasks-detail-related-channel",
            i18n::t(locale, "native.tasks.detail.relatedChannel"),
            move |_ev: &gpui::ClickEvent, _window: &mut gpui::Window, cx: &mut gpui::App| cx.open_url(&link),
        ));
    }

    Some(box_frame(i18n::t(locale, "native.tasks.detail.related"), body))
}

fn link_row(
    id: impl Into<gpui::ElementId>,
    label: SharedString,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::BRAND, 1.0))
        .cursor_pointer()
        .hover(|s| s.underline())
        .child(label)
        .on_click(on_click)
}

// ── Top-level render ───────────────────────────────────────────────────

pub(super) fn render(state: &RootView, cx: &mut Context<RootView>, task_id: &str) -> Stateful<Div> {
    maybe_fetch_detail(state, cx, task_id);
    maybe_fetch_artifacts(state, cx, task_id);

    let locale = state.locale;

    // Snapshot everything this render needs as OWNED values in one pass
    // over `g` before doing anything else. `cx.default_global::<TasksState>()`
    // hands back `&mut TasksState` borrowed from `cx`; gpui's `cx.listener`
    // (used by `breadcrumb`/`header_row`/`related_card` below) also needs
    // `cx` mutably, and — because `g` is used one more time at the very end
    // for `related_info` — NLL would otherwise extend `g`'s borrow across
    // every one of those calls in between and refuse to compile. Taking
    // everything out up front (including the related-links lookup, which
    // only needs `&TasksState`, not `cx`) means `g`'s borrow ends right
    // here, before `cx` is ever borrowed mutably again.
    let g = cx.default_global::<TasksState>();
    // Prefer the fresher `tasks.timeline` row; fall back to whichever list
    // cache already has it while the timeline fetch is still in flight, so
    // the header doesn't flash empty on every open.
    let task: Option<TaskItem> = match &g.detail {
        Loadable::Ready(d) if d.task.is_some() => d.task.clone(),
        _ => {
            let from = |l: &Loadable<Vec<TaskItem>>| match l {
                Loadable::Ready(list) => list.iter().find(|t| t.id == task_id).cloned(),
                _ => None,
            };
            from(&g.tasks).or_else(|| from(&g.archived_tasks))
        }
    };
    let detail_failed: Option<String> = match &g.detail {
        Loadable::Failed(msg) => Some(msg.clone()),
        _ => None,
    };
    let tool_call_count = match &g.detail {
        Loadable::Ready(d) => d.tool_call_count,
        _ => 0,
    };
    let artifacts = g.artifacts.clone();
    let detail = g.detail.clone();
    let action_result = g.action_result.clone();
    let related = task.as_ref().and_then(|t| related_info(g, t));

    let Some(task) = task else {
        return div()
            .id("tasks-detail-loading")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(breadcrumb_placeholder(locale, cx))
            .child(
                div().flex_1().flex().items_center().justify_center().child(match detail_failed {
                    Some(msg) => empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", &msg), None, None::<Div>),
                    None => empty_state("📋", i18n::t(locale, "native.tasks.detail.loading"), None, None::<Div>),
                }),
            );
    };

    let now = Utc::now();

    let left = div()
        .id("tasks-detail-left")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(breadcrumb(locale, &task, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(header_row(state, &task, cx))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta_line(locale, &task)))
                .children(action_result_line(locale, &action_result)),
        )
        .child(
            div()
                .id("tasks-detail-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .child(description_section(locale, &task))
                .child(artifacts_section(locale, &artifacts))
                .child(process_section(locale, &detail, now)),
        );

    let right = div()
        .id("tasks-detail-right")
        .w(px(320.))
        .h_full()
        .flex_shrink_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SIDEBAR, 1.0))
        .border_1()
        .border_color(theme::sidebar_border())
        .shadow(theme::surface_shadow())
        .child(attributes_card(locale, &task))
        .child(audit_card(locale, &task, tool_call_count))
        .children(related_card(locale, related, cx));

    div().id("tasks-detail-page").size_full().flex().gap_3().child(left).child(right)
}

fn breadcrumb_placeholder(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("tasks-detail-breadcrumb-loading")
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .cursor_pointer()
        .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        .child(i18n::t(locale, "native.tasks.detail.breadcrumbRoot"))
        .on_click(cx.listener(|_this, _ev, _window, cx| {
            cx.global_mut::<TasksState>().back_to_list();
            cx.notify();
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_text_falls_back_to_event_type_when_summary_blank() {
        let e = ActivityItem { at: "x".into(), event_type: "task_completed".into(), summary: String::new() };
        assert_eq!(activity_text(&e), "task_completed");
        let e2 = ActivityItem { at: "x".into(), event_type: "task_completed".into(), summary: "完成了".into() };
        assert_eq!(activity_text(&e2), "完成了");
    }
}
