// Left column: header ("共同計畫 · N 件" + assembled "新增計畫" button) + the
// scrollable plan list. Split out of `plans.rs` for the same file-size
// reason `routines.rs`/`routines_rows.rs` are split — see `plans.rs`'s
// module doc comment for the overall page design.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::screens::goals::relative_time;
use crate::theme;
use crate::RootView;

use super::{resolve_selected_id, PlanSummary, PlansSnapshot, PlansState};

/// Manual "重新整理" control — mirrors `console_queue.rs::refresh_button` /
/// `routines_rows.rs::refresh_button`'s exact shape (this page's own list
/// has no server-push event either).
fn refresh_button(cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("plans-refresh")
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
            cx.global_mut::<PlansState>().request_refresh();
            cx.notify();
        }))
}

fn skeleton_row() -> Div {
    div().p_2p5().flex().flex_col().gap_1p5().child(skeleton(px(170.), px(13.))).child(skeleton(px(110.), px(11.)))
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
        .child(i18n::t1(locale, "native.plans.loadError", "message", message))
}

fn plan_row(row: &PlanSummary, is_selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let when = relative_time(locale, &row.updated_at, chrono::Utc::now());
    let progress = i18n::tn(
        locale,
        "native.plans.progress",
        &[("done", &row.steps_done.to_string()), ("total", &row.steps_total.to_string())],
    );
    let row_id: SharedString = format!("plans-row-{}", row.id).into();
    let id_for_click = row.id.clone();
    let done_dimmed = row.steps_total > 0 && row.steps_done == row.steps_total;

    div()
        .id(row_id)
        .flex()
        .flex_col()
        .gap_1p5()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(done_dimmed, |el| el.opacity(0.6))
        .when(is_selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .when(!is_selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
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
                        .child(row.title.clone()),
                )
                .child(div().flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(when)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(if row.agent_id.is_empty() { i18n::t(locale, "native.plans.step.unassigned") } else { row.agent_id.clone().into() }),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(progress),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<PlansState>().selected = Some(id_for_click.clone());
            cx.notify();
        }))
}

pub(super) fn list_column(snap: &PlansSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let count = match &snap.plans {
        Loadable::Ready(v) => Some(v.len()),
        _ => None,
    };
    let title = match count {
        Some(n) => i18n::t1(locale, "native.plans.titleCount", "n", &n.to_string()),
        None => i18n::t(locale, "native.plans.title"),
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
                // Assembled, not wired — see `plans.rs`'s module doc comment.
                .child(button("plans-add", i18n::t(locale, "native.plans.add"), ButtonVariant::Primary, false, None, |_ev, _window, _app| {})),
        );

    let body = match &snap.plans {
        Loadable::Loading => div()
            .id("plans-list-loading")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .children((0..3).map(|_| skeleton_row())),
        Loadable::Failed(e) => div().id("plans-list-error").flex_1().overflow_y_scroll().flex().flex_col().gap_1().p_2().child(error_banner(locale, e)),
        Loadable::Ready(rows) => {
            let col = div().id("plans-list").flex_1().overflow_y_scroll().flex().flex_col().gap_1p5().p_2();
            if rows.is_empty() {
                col.child(empty_state("📋", i18n::t(locale, "native.plans.empty"), Some(i18n::t(locale, "native.plans.emptyDesc")), None::<Div>))
            } else {
                let effective = resolve_selected_id(&snap.selected, rows);
                let row_els: Vec<Stateful<Div>> =
                    rows.iter().map(|r| plan_row(r, effective.as_deref() == Some(r.id.as_str()), locale, cx)).collect();
                col.children(row_els)
            }
        }
    };

    div()
        .id("plans-list-column")
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
