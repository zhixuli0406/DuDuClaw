// Right column: the selected routine's full detail — status badge + header,
// "測試執行"/"編輯"/"刪除" assembled action row, 任務內容/排程/上次執行/從模板帶入
// cards. Split out of `routines.rs` for the same file-size reason
// `console.rs`/`console_detail.rs` are split — see `routines.rs`'s module
// doc comment for the overall page design and RPC shapes.

use gpui::{div, prelude::*, px, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::theme;

use super::routines_rows::status_dot_color;
use super::{last_run_label, resolve_selection, RoutineRow, RoutinesSnapshot};

fn field_card(label: impl Into<SharedString>, body: impl IntoElement) -> Div {
    div()
        .flex_1()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .rounded(px(theme::RADIUS_XL))
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(body)
}

fn last_run_card(row: &RoutineRow, locale: Locale) -> Div {
    let (dot_color, label) = match row.last_status.as_deref() {
        _ if row.last_run_at.is_none() => (theme::MUTED_FOREGROUND, i18n::t(locale, "native.routines.status.never")),
        Some("failure") => (theme::DESTRUCTIVE, i18n::t(locale, "native.routines.status.failed")),
        Some("partial") => (theme::WARNING, i18n::t(locale, "native.routines.status.partial")),
        _ => (theme::SUCCESS, i18n::t(locale, "native.routines.status.success")),
    };
    let when = last_run_label(locale, &row.last_run_at);
    field_card(
        i18n::t(locale, "native.routines.field.lastRun"),
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(theme::alpha(dot_color, 1.0)))
            .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(format!("{label} · {when}"))),
    )
}

fn schedule_card(row: &RoutineRow, locale: Locale) -> Div {
    field_card(
        i18n::t(locale, "native.routines.field.schedule"),
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .child(format!("Cron: {}", row.cron)),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(super::describe_cron(locale, &row.cron)),
            ),
    )
}

fn content_card(row: &RoutineRow, locale: Locale) -> Div {
    let body_text = if row.task.is_empty() { i18n::t(locale, "native.routines.field.content.empty") } else { row.task.clone().into() };
    field_card(
        i18n::t(locale, "native.routines.field.content"),
        div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(body_text),
    )
}

fn detail_header(row: &RoutineRow, locale: Locale) -> Div {
    let status_badge = if row.enabled {
        badge(i18n::t(locale, "native.routines.status.enabled"), BadgeVariant::Success)
    } else {
        badge(i18n::t(locale, "native.routines.status.disabled"), BadgeVariant::Secondary)
    };
    let dot_color = status_dot_color(row);

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(12.)).flex_shrink_0().rounded_full().border_3().border_color(theme::alpha(dot_color, 1.0)))
                .child(div().flex_1().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(row.name.clone()))
                .child(status_badge),
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
                        .child(format!("{} · {}", row.agent_id, super::describe_cron(locale, &row.cron))),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        // Assembled, not wired — see `routines.rs`'s module doc comment.
                        .child(button("routines-run-now", i18n::t(locale, "native.routines.testRun"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
                        .child(button("routines-edit", i18n::t(locale, "native.routines.edit"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
                        .child(button("routines-delete", i18n::t(locale, "native.routines.delete"), ButtonVariant::Destructive, false, None, |_ev, _window, _app| {})),
                ),
        )
}

pub(super) fn detail_column(snap: &RoutinesSnapshot, locale: Locale) -> Stateful<Div> {
    let rows: &[RoutineRow] = match &snap.routines {
        Loadable::Ready(v) => v.as_slice(),
        _ => &[],
    };
    let selected = resolve_selection(&snap.selected, rows);

    let body: Stateful<Div> = match selected {
        None => div().id("routines-detail-empty").flex_1().flex().items_center().justify_center().child(empty_state(
            "🗓️",
            i18n::t(locale, "native.routines.selectHint"),
            None,
            None::<Div>,
        )),
        Some(row) => div()
            .id("routines-detail-body")
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .overflow_y_scroll()
            .child(detail_header(row, locale))
            .child(content_card(row, locale))
            .child(div().flex().gap_3().child(schedule_card(row, locale)).child(last_run_card(row, locale))),
    };

    div().id("routines-detail-column").flex_1().h_full().flex().flex_col().bg(theme::alpha(theme::PAGE_CANVAS, 0.4)).child(body)
}
