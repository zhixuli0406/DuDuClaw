// S4b (console, second wave) — left column: filter chips + the merged
// approval/goal queue list. Split out of the original `console_cards.rs`
// (which grew past this crate's own <800-line file convention once the
// right-hand detail pane was added) — `console_detail.rs` is this file's
// sibling for the right column. No behavior differs from the unsplit
// version; see `console.rs`'s module doc comment for the overall page
// design and `console_detail.rs`'s header comment for the right column.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::theme;
use crate::RootView;

use super::{agent_label, kind_label, queue_load_state, resolve_selection, short_time};
use super::{ConsoleSnapshot, ConsoleState, Filter, QueueEntry, QueueLoad};

/// Manual "重新整理" control — the queue header's own equivalent of
/// `dashboard_cards.rs::refresh_button`, needed for the same reason: this
/// page has no server-push event for approvals/needs_human-goal aggregates,
/// so a stuck `Loading`/`Failed` state (e.g. a transient RPC timeout) has no
/// other recovery path than a fresh `maybe_fetch` — which `request_refresh`
/// re-arms by resetting `requested` back to `false`.
fn refresh_button(cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("console-refresh")
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child("↻")
        .on_click(cx.listener(|_this, _ev, _window, cx| {
            cx.global_mut::<ConsoleState>().request_refresh();
            cx.notify();
        }))
}

fn filter_chip(
    id: &'static str,
    label: SharedString,
    active: bool,
    target: Filter,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .when(active, |el| {
            el.bg(theme::alpha(theme::FOREGROUND, 1.0))
                .text_color(theme::alpha(theme::SURFACE, 1.0))
                .font_weight(gpui::FontWeight::MEDIUM)
        })
        .when(!active, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<ConsoleState>().filter = target;
            cx.notify();
        }))
}

fn filter_chips(snap: &ConsoleSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let approvals_n = match &snap.approvals {
        super::Loadable::Ready(v) => Some(v.len()),
        _ => None,
    };
    let goals_n = match &snap.goals {
        super::Loadable::Ready(v) => Some(v.len()),
        _ => None,
    };
    let waiting_label = match approvals_n {
        Some(n) => i18n::t1(locale, "native.console.filter.waitingApprovalCount", "n", &n.to_string()),
        None => i18n::t(locale, "native.console.filter.waitingApproval"),
    };
    let stuck_label = match goals_n {
        Some(n) => i18n::t1(locale, "native.console.filter.stuckCount", "n", &n.to_string()),
        None => i18n::t(locale, "native.console.filter.stuck"),
    };
    div()
        .flex()
        .gap_1p5()
        .px_3()
        .pb_2()
        .child(filter_chip("console-filter-all", i18n::t(locale, "native.console.filter.all"), snap.filter == Filter::All, Filter::All, cx))
        .child(filter_chip(
            "console-filter-waiting",
            waiting_label,
            snap.filter == Filter::WaitingApproval,
            Filter::WaitingApproval,
            cx,
        ))
        .child(filter_chip("console-filter-stuck", stuck_label, snap.filter == Filter::Stuck, Filter::Stuck, cx))
}

fn queue_skeleton_row() -> Div {
    div()
        .p_2p5()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(skeleton(px(170.), px(13.)))
        .child(skeleton(px(110.), px(11.)))
}

fn queue_error_banner(locale: Locale, message: &str) -> Div {
    div()
        .mx_2()
        .mb_1()
        .px_2p5()
        .py_1p5()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::DESTRUCTIVE, 0.10))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.console.decision.errorPrefix", "message", message))
}

fn queue_row(entry: &QueueEntry, is_selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selection = entry.selection();
    let (title, subtitle, when) = match entry {
        QueueEntry::Approval(a) => (
            if a.summary.is_empty() { kind_label(locale, &a.kind).to_string() } else { a.summary.clone() },
            format!("{} · {}", agent_label(&a.agent_id), i18n::t(locale, "native.console.queue.waitingApproval")),
            short_time(&a.created_at),
        ),
        QueueEntry::Goal(g) => (
            g.title.clone(),
            format!(
                "{} · {}",
                i18n::t1(locale, "native.home.card.goal.round", "n", &(g.revision_round + 1).to_string()),
                g.judge_feedback.clone().unwrap_or_else(|| i18n::t(locale, "native.console.queue.stuck").to_string()),
            ),
            short_time(&g.updated_at),
        ),
    };
    let row_id: SharedString = match &selection {
        super::Selection::Approval(id) => format!("console-row-approval-{id}"),
        super::Selection::Goal(id) => format!("console-row-goal-{id}"),
    }
    .into();

    div()
        .id(row_id)
        .flex()
        .gap_2()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(is_selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .when(!is_selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(
            div()
                .mt_1()
                .size(px(8.))
                .flex_shrink_0()
                .rounded_full()
                .border_2()
                .border_color(theme::alpha(theme::WARNING, 1.0)),
        )
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
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(when),
                        ),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(subtitle),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<ConsoleState>().selected = Some(selection.clone());
            cx.notify();
        }))
}

pub(super) fn queue_column(snap: &ConsoleSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .pt_3()
        .pb_1()
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.console.title")),
        )
        .child(refresh_button(cx));
    let chips = filter_chips(snap, locale, cx);

    let body = match queue_load_state(snap) {
        QueueLoad::Loading => div()
            .id("console-queue-list-loading")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .children((0..4).map(|_| queue_skeleton_row())),
        QueueLoad::Ready { entries, approvals_error, goals_error } => {
            let mut col = div().id("console-queue-list").flex_1().overflow_y_scroll().flex().flex_col().gap_1().p_2();
            if let Some(e) = approvals_error {
                col = col.child(queue_error_banner(locale, e));
            }
            if let Some(e) = goals_error {
                col = col.child(queue_error_banner(locale, e));
            }
            if entries.is_empty() {
                col.child(empty_state(
                    "✅",
                    i18n::t(locale, "native.console.queue.empty"),
                    Some(i18n::t(locale, "native.console.queue.emptyDesc")),
                    None::<Div>,
                ))
            } else {
                let effective = resolve_selection(&snap.selected, &entries);
                let rows: Vec<Stateful<Div>> = entries
                    .iter()
                    .map(|entry| {
                        let selected = effective.as_ref() == Some(&entry.selection());
                        queue_row(entry, selected, locale, cx)
                    })
                    .collect();
                col.children(rows)
            }
        }
    };

    div()
        .id("console-queue")
        .w(px(264.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(header)
        .child(chips)
        .child(body)
}
