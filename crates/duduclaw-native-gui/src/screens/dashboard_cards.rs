// S4b — card/prompt-bar/activity-shelf rendering helpers for `dashboard.rs`.
// Split out purely to keep `dashboard.rs` (state + fetch orchestration +
// top-level composition) under this crate's own file-size convention — same
// reason `shell.rs` is split across `shell_sidebar.rs`/`shell_content_list.rs`/
// `shell_row.rs`. No behavior differs from an unsplit version.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::chat_ws::ConnState;
use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, ButtonVariant};
use crate::screens::dashboard::{
    error_row, loading_row, ActivityItem, AgentsCard, BudgetCard, ChannelsCard, DashboardState, GoalCard,
    Loadable, TasksCard,
};
use crate::theme;
use crate::RootView;

// ── Shared chrome ──────────────────────────────────────────────────────

/// One card's frame: status dot + label row, then whatever body the caller
/// built (skeleton / error / real content), then an optional single action
/// button — the "每卡狀態點＋單一動作鈕" shape the task brief specifies.
fn card_frame(dot_color: u32, title: SharedString, body: Div, action: Option<Stateful<Div>>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(8.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(title),
                ),
        )
        .child(body)
        .children(action)
}

/// A card's single action button, navigating to an existing `nav.rs` page.
/// `Secondary` variant for every non-primary card (see `dashboard.rs`'s
/// module doc comment on why — no canvas-matching "outline" variant exists
/// in `mds_gpui::ButtonVariant` yet).
fn nav_button(
    id: &'static str,
    label: SharedString,
    target: &'static str,
    variant: ButtonVariant,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    button(
        id,
        label,
        variant,
        false,
        None,
        cx.listener(move |this, _ev, _window, cx| {
            this.active_page = target;
            cx.notify();
        }),
    )
}

pub(super) fn refresh_button(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    button(
        "home-refresh",
        i18n::t(locale, "native.home.refresh"),
        ButtonVariant::Ghost,
        false,
        None,
        cx.listener(|_this, _ev, _window, cx| {
            cx.default_global::<DashboardState>().request_refresh();
            cx.notify();
        }),
    )
}

// ── Prompt bar + chips ────────────────────────────────────────────────

/// Reuses `state.chat`'s own composer `Entity<ImeTextInput>` — the SAME
/// IME-capable entity `screens/chat.rs`'s composer renders, not a second
/// input widget. Submitting from here calls the exact `ChatState::submit`
/// the chat page's own send button calls, then navigates to `newChat` so
/// the user sees the exchange land — this crate's honest middle ground
/// between the task brief's two suggested options ("接線成本高就只導頁帶
/// 入文字" vs. wiring it for real): reusing the existing entity + existing
/// `submit()` method made "for real" the cheaper option, not the harder
/// one. Enter-to-submit gets the same navigation via a one-line addition to
/// `main.rs`'s existing `ImeTextInputEvent::Submit` subscription (that
/// subscription is global — one listener on one entity — so extending it
/// there covers both this page's Enter key and the chat page's own,
/// without duplicating the emitter wiring).
///
/// Deliberate deviation from the canvas: because the composer entity is
/// SHARED with the chat page, its placeholder text is whatever `main.rs`
/// set at creation time (`native.chat.inputPlaceholder`) — `element.rs`'s
/// paint pass already draws that placeholder itself whenever the engine's
/// content is empty (with its own muted color), so this bar does NOT lay a
/// second, dashboard-specific placeholder string on top (that would double-
/// render two overlapping placeholders). `native.home.prompt.placeholder`
/// is therefore unused by this function; kept in the catalogs for whichever
/// future pass gives the composer a per-page placeholder override.
pub(super) fn prompt_bar(state: &RootView, _locale: Locale, cx: &mut Context<RootView>) -> Div {
    let can_send = state.chat.conn_state == ConnState::Authenticated;
    div()
        .w(px(620.))
        .flex()
        .items_center()
        .gap_2p5()
        .h(px(56.))
        .px_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::input_border())
        .shadow(theme::surface_shadow())
        .child(div().flex_1().text_size(px(theme::TEXT_BASE)).child(state.chat.input.clone()))
        .child(
            div()
                .id("home-prompt-send")
                .size(px(30.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_MD))
                .when(can_send, |el| {
                    el.bg(theme::alpha(theme::BRAND, 1.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            let content = this.chat.input.update(cx, |input, cx| {
                                let taken = input.engine.take_content();
                                cx.notify();
                                taken
                            });
                            if !content.trim().is_empty() {
                                this.chat.submit(content);
                                this.active_page = "newChat";
                            }
                            cx.notify();
                        }))
                })
                .when(!can_send, |el| el.bg(theme::alpha(theme::MUTED, 0.4)))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(theme::alpha(
                            if can_send { theme::BRAND_FOREGROUND } else { theme::MUTED_FOREGROUND },
                            1.0,
                        ))
                        .child("↑"),
                ),
        )
}

pub(super) fn chip(id: &'static str, label: SharedString, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(label.clone())
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            // `ChatState::submit` already no-ops (with its own system-message
            // guard) when the chat socket isn't authenticated yet — reusing
            // it here means this click needs no connection check of its own.
            this.chat.submit(label.to_string());
            this.active_page = "newChat";
            cx.notify();
        }))
}

// ── Cards ──────────────────────────────────────────────────────────────

pub(super) fn approvals_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().approvals {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(0) => {
            (theme::SUCCESS, div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.home.card.approvals.empty")))
        }
        Loadable::Ready(n) => (
            theme::WARNING,
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.home.card.approvals.detail", "count", &n.to_string())),
        ),
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.approvals.title"),
        body,
        // No `inbox` page exists in this crate's nav subset yet (see
        // `nav.rs`'s own header comment on the still-unported `manageNav`
        // surface) — the closest existing page approvals actually surface
        // on is the task board's `needs_human` column, so the button
        // navigates there and is LABELED accordingly (not "前往收件匣",
        // which would point somewhere this button can't actually take you).
        Some(nav_button(
            "home-card-approvals-action",
            i18n::t(locale, "native.home.card.approvals.action"),
            "tasks",
            ButtonVariant::Primary,
            cx,
        )),
    )
}

fn status_label(locale: Locale, status: &str) -> SharedString {
    let key = match status {
        "todo" | "pending" => "native.home.status.todo",
        "in_progress" => "native.home.status.inProgress",
        "revising" => "native.home.status.revising",
        "review" => "native.home.status.review",
        "blocked" | "failed" => "native.home.status.blocked",
        "needs_human" => "native.home.card.goal.needsHuman",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

pub(super) fn goal_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().goal {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(GoalCard { highlight: None, .. }) => (
            theme::SUCCESS,
            div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.home.card.goal.empty")),
        ),
        Loadable::Ready(GoalCard { highlight: Some(h), needs_human }) => (
            if *needs_human > 0 { theme::WARNING } else { theme::SUCCESS },
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(format!("「{}」", h.title)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(format!(
                            "{}・{}",
                            i18n::t1(locale, "native.home.card.goal.round", "n", &(h.round + 1).to_string()),
                            status_label(locale, &h.status),
                        )),
                ),
        ),
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.goal.title"),
        body,
        Some(nav_button("home-card-goal-action", i18n::t(locale, "native.home.card.goal.action"), "goals", ButtonVariant::Secondary, cx)),
    )
}

pub(super) fn tasks_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().tasks {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(TasksCard { active_count: 0, .. }) => (
            theme::SUCCESS,
            div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.home.card.tasks.empty")),
        ),
        Loadable::Ready(TasksCard { active_count, needs_human }) => (
            if *needs_human > 0 { theme::WARNING } else { theme::SUCCESS },
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.home.card.tasks.detail", "count", &active_count.to_string())),
        ),
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.tasks.title"),
        body,
        Some(nav_button("home-card-tasks-action", i18n::t(locale, "native.home.card.tasks.action"), "tasks", ButtonVariant::Secondary, cx)),
    )
}

/// Integer cents → "N,NNN" (no decimals — matches the canvas's own
/// "NT$3,720 / 6,000" formatting, which drops cents too).
fn format_dollars(cents: i64) -> String {
    let dollars = cents / 100;
    let neg = dollars < 0;
    let digits = dollars.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let mut out: String = grouped.chars().rev().collect();
    if neg {
        out.insert(0, '-');
    }
    out
}

pub(super) fn budget_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().budget {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(BudgetCard { spent_cents, total_cents }) if *total_cents <= 0 => (
            theme::BRAND,
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.home.card.budget.noLimit", "spent", &format_dollars(*spent_cents))),
        ),
        Loadable::Ready(BudgetCard { spent_cents, total_cents }) => {
            let percent = ((*spent_cents as f64 / *total_cents as f64) * 100.0).max(0.0);
            let dot = if percent >= 100.0 {
                theme::DESTRUCTIVE
            } else if percent >= 90.0 {
                theme::WARNING
            } else {
                theme::BRAND
            };
            let body = div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::tn(
                            locale,
                            "native.home.card.budget.detail",
                            &[
                                ("percent", &format!("{:.0}", percent)),
                                ("spent", &format_dollars(*spent_cents)),
                                ("total", &format_dollars(*total_cents)),
                            ],
                        )),
                )
                .child(
                    div()
                        .h(px(6.))
                        .rounded(px(theme::RADIUS_4XL))
                        .bg(theme::alpha(theme::MUTED, 1.0))
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .rounded(px(theme::RADIUS_4XL))
                                .w(gpui::relative((percent.min(100.0) / 100.0) as f32))
                                .bg(theme::alpha(dot, 1.0)),
                        ),
                );
            (dot, body)
        }
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.budget.title"),
        body,
        Some(nav_button("home-card-budget-action", i18n::t(locale, "native.home.card.budget.action"), "reports", ButtonVariant::Secondary, cx)),
    )
}

pub(super) fn channels_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().channels {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(ChannelsCard { total: 0, .. }) => (
            theme::MUTED_FOREGROUND,
            div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.home.card.channels.empty")),
        ),
        Loadable::Ready(ChannelsCard { connected, total }) => (
            if connected == total {
                theme::SUCCESS
            } else if *connected > 0 {
                theme::WARNING
            } else {
                theme::DESTRUCTIVE
            },
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::tn(
                    locale,
                    "native.home.card.channels.detail",
                    &[("connected", &connected.to_string()), ("total", &total.to_string())],
                )),
        ),
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.channels.title"),
        body,
        // No dedicated `channels` nav page exists in this crate's subset —
        // `manage` ("整合、帳務、安全、設定") is the closest existing surface.
        Some(nav_button("home-card-channels-action", i18n::t(locale, "native.home.card.channels.action"), "manage", ButtonVariant::Secondary, cx)),
    )
}

pub(super) fn agents_card(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (dot, body) = match &cx.default_global::<DashboardState>().agents {
        Loadable::Loading => (theme::MUTED_FOREGROUND, loading_row()),
        Loadable::Failed(msg) => (theme::DESTRUCTIVE, error_row(locale, msg)),
        Loadable::Ready(AgentsCard { total: 0, .. }) => (
            theme::MUTED_FOREGROUND,
            div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.home.card.agents.empty")),
        ),
        Loadable::Ready(AgentsCard { active, total }) => (
            if active == total {
                theme::SUCCESS
            } else if *active > 0 {
                theme::WARNING
            } else {
                theme::DESTRUCTIVE
            },
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::tn(
                    locale,
                    "native.home.card.agents.detail",
                    &[("active", &active.to_string()), ("offline", &(total - active).to_string())],
                )),
        ),
    };
    card_frame(
        dot,
        i18n::t(locale, "native.home.card.agents.title"),
        body,
        Some(nav_button("home-card-agents-action", i18n::t(locale, "native.home.card.agents.action"), "agents", ButtonVariant::Secondary, cx)),
    )
}

// ── Activity shelf ────────────────────────────────────────────────────

fn activity_row(item: &ActivityItem) -> Div {
    div()
        .w(px(220.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(if item.agent.is_empty() { "—".to_string() } else { item.agent.clone() }),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(item.summary.clone()),
        )
        .child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(item.when.clone()))
}

pub(super) fn activity_shelf(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let title = div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "native.home.activity.title"));

    let body = match &cx.default_global::<DashboardState>().activity {
        Loadable::Loading => div().flex().gap_3().children([loading_row(), loading_row(), loading_row()]),
        Loadable::Failed(msg) => error_row(locale, msg),
        Loadable::Ready(items) if items.is_empty() => {
            empty_state("📭", i18n::t(locale, "native.home.activity.empty"), None, None::<Div>)
        }
        Loadable::Ready(items) => {
            let rows: Vec<Div> = items.iter().map(activity_row).collect();
            div().flex().gap_3().children(rows)
        }
    };

    div().w_full().max_w(px(920.)).flex().flex_col().gap_2().child(title).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_dollars_groups_thousands_and_drops_cents() {
        assert_eq!(format_dollars(0), "0");
        assert_eq!(format_dollars(372_000), "3,720"); // NT$3,720 — the canvas's own example
        assert_eq!(format_dollars(600_000), "6,000");
        assert_eq!(format_dollars(123_456_789), "1,234,567");
    }

    #[test]
    fn format_dollars_handles_negative_and_sub_dollar_cents() {
        assert_eq!(format_dollars(-50), "0"); // integer division truncates toward zero, not "-1"
        assert_eq!(format_dollars(-250_000), "-2,500");
    }
}
