// Left column: header ("例行工作 · N 項" + assembled "新增例行工作" button) +
// the scrollable routine list. Split out of `routines.rs` for the same
// file-size reason `console.rs`/`console_queue.rs` are split — see
// `routines.rs`'s module doc comment for the overall page design.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::RootView;

use super::{last_run_label, resolve_selection, RoutineRow, RoutinesSnapshot, RoutinesState};

/// Status-dot color for a routine row: disabled routines always render the
/// neutral "off" dot regardless of their last run outcome (matching the
/// canvas's dimmed 停用 row, which still shows a gray dot even though its
/// `last_status` could theoretically be anything); enabled routines map
/// `last_status` — `"failure"` → destructive, `"partial"` → warning,
/// `"success"`/anything else (including `None`, never run yet) → success/
/// muted respectively. Never guesses beyond the three known values
/// (`cron_scheduler.rs` tests) — an unrecognized non-empty string still
/// degrades to the same "muted, unknown" dot a `None` gets, not a silent
/// success claim.
pub(super) fn status_dot_color(row: &RoutineRow) -> u32 {
    if !row.enabled {
        return theme::MUTED_FOREGROUND;
    }
    match row.last_status.as_deref() {
        Some("failure") => theme::DESTRUCTIVE,
        Some("partial") => theme::WARNING,
        Some("success") => theme::SUCCESS,
        _ => theme::MUTED_FOREGROUND,
    }
}

/// Manual "重新整理" control — this page has no server-push event for cron
/// task changes, so a stuck `Failed` state (e.g. a transient RPC timeout)
/// has no other recovery path than a fresh `maybe_fetch`, which
/// `request_refresh` re-arms. Mirrors `console_queue.rs::refresh_button`'s
/// exact shape.
fn refresh_button(cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("routines-refresh")
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
            cx.global_mut::<RoutinesState>().request_refresh();
            cx.notify();
        }))
}

fn skeleton_row() -> Div {
    div().p_2p5().flex().flex_col().gap_1p5().child(skeleton(px(170.), px(13.))).child(skeleton(px(120.), px(11.)))
}

fn error_banner(locale: Locale, message: &str) -> Div {
    div()
        .mx_2()
        .mb_1()
        .px_2p5()
        .py_1p5()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::DESTRUCTIVE, 0.10))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.routines.loadError", "message", message))
}

fn routine_row(row: &RoutineRow, is_selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let subtitle = format!("{} · {}", row.agent_id, super::describe_cron(locale, &row.cron));
    let when = last_run_label(locale, &row.last_run_at);
    let row_id: SharedString = format!("routines-row-{}", row.id).into();
    let dot_color = status_dot_color(row);
    let dimmed = !row.enabled;
    let id_for_click = row.id.clone();

    div()
        .id(row_id)
        .flex()
        .gap_2p5()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(dimmed, |el| el.opacity(0.55))
        .when(is_selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .when(!is_selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(div().mt_1p5().size(px(8.)).flex_shrink_0().rounded_full().bg(theme::alpha(dot_color, 1.0)))
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
                                .text_color(if is_selected {
                                    theme::alpha(theme::SIDEBAR_ACCENT_FOREGROUND, 1.0)
                                } else {
                                    theme::alpha(theme::FOREGROUND, 1.0)
                                })
                                .child(row.name.clone()),
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
            cx.global_mut::<RoutinesState>().selected = Some(id_for_click.clone());
            cx.notify();
        }))
}

pub(super) fn list_column(snap: &RoutinesSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let count = match &snap.routines {
        Loadable::Ready(v) => Some(v.len()),
        _ => None,
    };
    let title = match count {
        Some(n) => i18n::t1(locale, "native.routines.titleCount", "n", &n.to_string()),
        None => i18n::t(locale, "native.routines.title"),
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .pt_3()
        .pb_2()
        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(refresh_button(cx))
                // Assembled, not wired — see `routines.rs`'s module doc comment.
                .child(button("routines-add", i18n::t(locale, "native.routines.add"), ButtonVariant::Primary, false, None, |_ev, _window, _app| {})),
        );

    let body = match &snap.routines {
        Loadable::Loading => div()
            .id("routines-list-loading")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .children((0..4).map(|_| skeleton_row())),
        Loadable::Failed(e) => div().id("routines-list-error").flex_1().overflow_y_scroll().flex().flex_col().gap_1().p_2().child(error_banner(locale, e)),
        Loadable::Ready(rows) => {
            let col = div().id("routines-list").flex_1().overflow_y_scroll().flex().flex_col().gap_1().p_2();
            if rows.is_empty() {
                col.child(empty_state(
                    "🗓️",
                    i18n::t(locale, "native.routines.empty"),
                    Some(i18n::t(locale, "native.routines.emptyDesc")),
                    None::<Div>,
                ))
            } else {
                let effective = resolve_selection(&snap.selected, rows).map(|r| r.id.clone());
                let row_els: Vec<Stateful<Div>> =
                    rows.iter().map(|r| routine_row(r, effective.as_deref() == Some(r.id.as_str()), locale, cx)).collect();
                col.children(row_els)
            }
        }
    };

    div()
        .id("routines-list-column")
        .w(px(380.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(header)
        .child(body)
}
