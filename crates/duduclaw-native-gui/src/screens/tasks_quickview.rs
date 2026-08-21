// Right-hand quick-view panel for the "任務" list page (Column 3 of
// `tasks.rs`'s internal 3-pane layout). Split out purely to keep `tasks.rs`
// under this crate's own <800-line convention — same reasoning
// `goals.rs`/`goals_inspector.rs` document for their own split.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Tasks.dc.html`'s
// right column — a FIXED 348px panel (not a flex-grow max-width column like
// `goals_inspector.rs`'s: the quick-view is deliberately a summary, not a
// second full-height reading surface) with 屬性 / 描述 / 最新進度 boxes and a
// "打開完整詳情" link that switches this page into `tasks_detail.rs`'s full
// page (see `tasks.rs`'s module doc comment on why that's an in-page mode
// flip, not a nav-tree navigation).

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::screens::dashboard::Loadable;
use crate::screens::goals::{short_date, status_label};
use crate::screens::tasks::{self, TasksState};
use crate::screens::tasks_data::TaskItem;
use crate::theme;
use crate::RootView;

const PANEL_WIDTH: f32 = 348.0;

/// Neutral card frame shared by every box in this panel — same shape
/// `goals_inspector.rs::box_frame` establishes, duplicated rather than
/// shared across pages (a 15-line styling helper, not worth widening
/// visibility for).
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

fn attributes_box(locale: Locale, task: &TaskItem) -> Div {
    let mut body = div().flex().flex_col().gap_1p5().child(attr_row(
        i18n::t(locale, "native.tasks.quickview.status"),
        attr_text(status_label(locale, &task.status), theme::alpha(theme::BRAND, 1.0)),
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

fn description_box(locale: Locale, task: &TaskItem) -> Div {
    let body = if task.description.trim().is_empty() {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.tasks.quickview.descriptionEmpty"))
    } else {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(task.description.clone())
    };
    box_frame(i18n::t(locale, "native.tasks.quickview.description"), body)
}

/// Only rendered when the agent has actually reported progress
/// (`result_summary` non-empty) — matches the canvas (all four example rows
/// happen to have one, but a freshly-created task never does).
fn progress_box(locale: Locale, task: &TaskItem) -> Option<Div> {
    let text = task.result_summary.as_ref()?;
    Some(box_frame(
        i18n::t(locale, "native.tasks.quickview.progress"),
        div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(text.clone()),
    ))
}

fn header(locale: Locale, task: &TaskItem) -> Div {
    let agent: String = if task.agent_id.is_empty() {
        i18n::t(locale, "native.tasks.agent.unassigned").to_string()
    } else {
        task.agent_id.clone()
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(tasks::status_dot(&task.status, px(12.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(SharedString::from(task.title.clone())),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::tn(locale, "native.tasks.quickview.meta", &[("agent", &agent), ("created", &short_date(&task.created_at))])),
        )
}

pub(super) fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<TasksState>();

    let view = g.smart_view;
    let selected: Option<TaskItem> = match &g.selected_task_id {
        Some(id) => {
            let source: &Loadable<Vec<TaskItem>> =
                if view == crate::screens::tasks_data::SmartView::Archived { &g.archived_tasks } else { &g.tasks };
            match source {
                Loadable::Ready(list) => list.iter().find(|t| &t.id == id).cloned(),
                _ => None,
            }
        }
        None => None,
    };

    let panel = div()
        .id("tasks-quickview")
        .w(px(PANEL_WIDTH))
        .h_full()
        .flex_shrink_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow());

    let Some(task) = selected else {
        return panel.child(
            div().flex_1().flex().items_center().justify_center().child(empty_state(
                "📋",
                i18n::t(locale, "native.tasks.quickview.emptyTitle"),
                Some(i18n::t(locale, "native.tasks.quickview.emptyDesc")),
                None::<Div>,
            )),
        );
    };

    let task_id_for_link = task.id.clone();

    panel
        .child(header(locale, &task))
        .child(attributes_box(locale, &task))
        .child(description_box(locale, &task))
        .children(progress_box(locale, &task))
        .child(
            div()
                .id("tasks-quickview-open-detail")
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::BRAND, 0.8)))
                .child(i18n::t(locale, "native.tasks.quickview.openDetail"))
                .on_click(cx.listener(move |_this, _ev, _window, cx| {
                    cx.global_mut::<TasksState>().open_detail(task_id_for_link.clone());
                    cx.notify();
                })),
        )
}
