// S4b (console, second wave) — right column: the selected item's detail
// pane (approval card / goal intervention card) + the bottom reply composer.
// Split out of the original `console_cards.rs` (which grew past this
// crate's own <800-line file convention once this half was added) —
// `console_queue.rs` is this file's sibling for the left column. No
// behavior differs from the unsplit version.
//
// Every click handler below mutates `super::ConsoleState` through
// `cx.global_mut::<ConsoleState>()` (see `console.rs`'s module doc comment
// for why state lives behind `Global` instead of a `RootView` field this
// pass). Decision buttons (approve/reject, 核准繼續/附指示重試/中止) call
// `console.rs`'s `submit_approval_decision`/`submit_goal_decision`, which
// dispatch the real `approvals.decide`/`tasks.goal_decide` RPCs — wired for
// real, but never clicked by this pass's own live-gateway smoke test (see
// this crate's S4b-console report: decision-RPC correctness is covered by
// `console.rs`'s `build_*_params` unit tests instead, per the task brief).

use gpui::{
    div, prelude::*, px, Context, CursorStyle, Div, KeyDownEvent, MouseButton, SharedString, Stateful,
};

use crate::chat_ws::ConnState;
use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::theme;
use crate::RootView;

use super::{agent_label, kind_label, queue_load_state, resolve_selection, short_time, truncate_chars};
use super::{ApprovalItem, ConsoleSnapshot, ConsoleState, GoalItem, QueueEntry, QueueLoad};

/// `pause_reason` is already a resolved token (`task_row_to_json`'s own doc
/// comment) — six closed values, `_` catches only a hypothetical future
/// addition this client doesn't know about yet (never a raw column value).
fn pause_reason_label(locale: Locale, token: &str) -> SharedString {
    let key = match token {
        "no_progress" => "native.console.pauseReason.noProgress",
        "budget_exhausted" => "native.console.pauseReason.budgetExhausted",
        "blocked_needs_decision" => "native.console.pauseReason.blockedNeedsDecision",
        "infra" => "native.console.pauseReason.infra",
        "restart" => "native.console.pauseReason.restart",
        _ => "native.console.pauseReason.unknown",
    };
    i18n::t(locale, key)
}

fn plain_header(title: SharedString) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(title),
        )
}

fn detail_header(title: String, subtitle: String, badge_key: &'static str, locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(div().size(px(9.)).flex_shrink_0().rounded_full().border_2().border_color(theme::alpha(theme::WARNING, 1.0)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(title),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(subtitle),
                ),
        )
        .child(badge(i18n::t(locale, badge_key), BadgeVariant::Warning))
}

fn assistant_bubble(text: String) -> Div {
    div().w_full().flex().justify_start().child(
        div()
            .max_w(px(480.))
            .px_3p5()
            .py_2p5()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .text_size(px(theme::TEXT_SM))
            .child(text),
    )
}

fn error_line(locale: Locale, msg: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.console.decision.errorPrefix", "message", msg))
}

fn open_link_button(locale: Locale, link: String, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("console-approval-link")
        .px_1()
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::BRAND, 1.0))
        .hover(|s| s.text_color(theme::alpha(theme::BRAND, 0.8)))
        .child(i18n::t(locale, "native.console.approval.viewLink"))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            // Read-only navigation, not a decision RPC — safe to wire for
            // real (unlike `markdown.rs`'s deferred inline-link case, this
            // is a single, non-repeating button, not N arbitrary markdown
            // links). `App::open_url` — see `crates/gpui/src/app.rs`.
            cx.open_url(&link);
        }))
}

fn approval_pane(
    state: &RootView,
    item: &ApprovalItem,
    snap: &ConsoleSnapshot,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let header = detail_header(
        if item.summary.is_empty() { kind_label(locale, &item.kind).to_string() } else { item.summary.clone() },
        format!(
            "{} {} · {}",
            agent_label(&item.agent_id),
            i18n::t(locale, "native.console.approval.raisedBy"),
            short_time(&item.created_at),
        ),
        "native.console.approval.badge",
        locale,
    );

    let busy = snap.decision_busy.as_deref() == Some(item.id.as_str());
    let payload_preview = truncate_chars(&item.payload.to_string(), 220);
    let id_for_approve = item.id.clone();
    let id_for_reject = item.id.clone();

    let card = div()
        .w(px(440.))
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::alpha(theme::WARNING, 0.4))
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::WARNING, 1.0))
                        .child(i18n::t(locale, "native.console.approval.needApproval")),
                )
                .child(badge(kind_label(locale, &item.kind), BadgeVariant::Outline)),
        )
        .child(
            div()
                .px_2p5()
                .py_2()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(payload_preview),
        )
        .children(snap.decision_error.as_deref().map(|e| error_line(locale, e)))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(button(
                    "console-approve",
                    i18n::t(locale, "native.console.approval.approve"),
                    ButtonVariant::Primary,
                    busy,
                    None,
                    cx.listener(move |this, _ev, _window, cx| {
                        super::submit_approval_decision(this, cx, id_for_approve.clone(), true);
                    }),
                ))
                .child(button(
                    "console-reject",
                    i18n::t(locale, "native.console.approval.reject"),
                    ButtonVariant::Secondary,
                    busy,
                    None,
                    cx.listener(move |this, _ev, _window, cx| {
                        super::submit_approval_decision(this, cx, id_for_reject.clone(), false);
                    }),
                ))
                .children(item.channel_link.clone().map(|link| open_link_button(locale, link, cx))),
        );

    let body = div()
        .id("console-detail-body")
        .flex_1()
        .flex()
        .flex_col()
        .gap_3()
        .px_5()
        .py_4()
        .overflow_y_scroll()
        .child(assistant_bubble(if item.summary.is_empty() {
            i18n::t(locale, "native.console.approval.noSummary").to_string()
        } else {
            item.summary.clone()
        }))
        .child(div().flex().justify_start().child(card));

    detail_shell(state, header, body, locale, cx)
}

/// The 附指示重試 inline note field — reuses `conversations.rs::search_box`'s
/// exact manual key-capture pattern (a cached `FocusHandle` + `on_key_down`
/// backspace/char handling), the crate's established substitute for real IME
/// composition outside the dedicated `ImeTextInput` entity (see that
/// file's own doc comment on the same documented CJK-composition gap).
fn goal_note_field(task_id: String, note_value: String, busy: bool, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let handle = super::ensure_focus_handle(cx);
    let handle_for_click = handle.clone();
    let is_empty = note_value.is_empty();

    let input_box = div()
        .id("console-goal-note-input")
        .track_focus(&handle)
        .key_context("ConsoleGoalNote")
        .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
            let ks = &event.keystroke;
            if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
                return;
            }
            match ks.key.as_str() {
                "backspace" => {
                    cx.global_mut::<ConsoleState>().goal_note.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_deref() {
                        if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                            cx.global_mut::<ConsoleState>().goal_note.push_str(ch);
                            cx.notify();
                        }
                    }
                }
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_this, _ev, window, cx| {
                window.focus(&handle_for_click, cx);
            }),
        )
        .flex_1()
        .h(px(32.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::input_bg())
        .border_1()
        .border_color(theme::input_border())
        .cursor(CursorStyle::IBeam)
        .text_size(px(theme::TEXT_XS))
        .text_color(if is_empty { theme::alpha(theme::MUTED_FOREGROUND, 1.0) } else { theme::alpha(theme::FOREGROUND, 1.0) })
        .child(SharedString::from(if is_empty {
            i18n::t(locale, "native.console.goal.notePlaceholder").to_string()
        } else {
            note_value
        }));

    // Disabled while empty — 附指示重試's whole point is the instruction; an
    // empty send would be indistinguishable from 核准繼續 (see console.rs's
    // module doc comment on the 3-button mapping).
    let go_button = button(
        "console-goal-retry-go",
        i18n::t(locale, "native.console.goal.retryGo"),
        ButtonVariant::Primary,
        busy || is_empty,
        None,
        cx.listener(move |this, _ev, _window, cx| {
            let note = cx.global::<ConsoleState>().goal_note.clone();
            super::submit_goal_decision(this, cx, task_id.clone(), "retry", Some(note));
        }),
    );

    div().flex().gap_2().child(input_box).child(go_button)
}

fn goal_pane(
    state: &RootView,
    item: &GoalItem,
    snap: &ConsoleSnapshot,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let header = detail_header(
        item.title.clone(),
        format!(
            "{} · {}",
            i18n::t1(locale, "native.console.goal.assignedTo", "agent", &agent_label(&item.assigned_to)),
            i18n::t1(locale, "native.home.card.goal.round", "n", &(item.revision_round + 1).to_string()),
        ),
        "native.console.goal.badge",
        locale,
    );

    let busy = snap.decision_busy.as_deref() == Some(item.id.as_str());
    let id_continue = item.id.clone();
    let id_abort = item.id.clone();
    let id_note = item.id.clone();

    let buttons_row = div()
        .flex()
        .flex_wrap()
        .gap_2()
        .child(button(
            "console-goal-continue",
            i18n::t(locale, "native.console.goal.continueAction"),
            ButtonVariant::Primary,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                super::submit_goal_decision(this, cx, id_continue.clone(), "retry", None);
            }),
        ))
        .child(button(
            "console-goal-retry-toggle",
            i18n::t(locale, "native.console.goal.retryWithNote"),
            ButtonVariant::Secondary,
            busy,
            None,
            cx.listener(|_this, _ev, _window, cx| {
                let g = cx.global_mut::<ConsoleState>();
                g.goal_note_open = !g.goal_note_open;
                cx.notify();
            }),
        ))
        .child(button(
            "console-goal-abort",
            i18n::t(locale, "native.console.goal.abort"),
            ButtonVariant::Destructive,
            busy,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                super::submit_goal_decision(this, cx, id_abort.clone(), "abort", None);
            }),
        ));

    let note_field =
        snap.goal_note_open.then(|| goal_note_field(id_note, snap.goal_note.clone(), busy, locale, cx));

    let card = div()
        .w(px(440.))
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::alpha(theme::WARNING, 0.4))
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::WARNING, 1.0))
                        .child(i18n::t(locale, "native.console.goal.needDecision")),
                )
                .children(item.pause_reason.as_deref().map(|token| badge(pause_reason_label(locale, token), BadgeVariant::Warning))),
        )
        .child(buttons_row)
        .children(note_field)
        .children(snap.decision_error.as_deref().map(|e| error_line(locale, e)));

    let body = div()
        .id("console-detail-body")
        .flex_1()
        .flex()
        .flex_col()
        .gap_3()
        .px_5()
        .py_4()
        .overflow_y_scroll()
        .children(item.judge_feedback.clone().map(assistant_bubble))
        .child(div().flex().justify_start().child(card));

    detail_shell(state, header, body, locale, cx)
}

fn detail_shell(
    state: &RootView,
    header: Div,
    body: Stateful<Div>,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    div()
        .id("console-detail")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .child(header)
        .child(body)
        .child(composer(state, locale, cx))
}

/// Bottom reply composer — reuses `state.chat`'s own composer `Entity<
/// ImeTextInput>` + `ChatState::submit`, the exact same "shared entity,
/// navigate to `newChat` on send" pattern `dashboard_cards.rs::prompt_bar`
/// already established (see that function's doc comment for the full
/// rationale). Deliberately does NOT try to route the reply "back to the
/// approving agent's own thread" — that would need a session/agent
/// resolution this page's read-only RPC scope doesn't have a clean source
/// for; sending here starts (or continues) the shared default chat instead,
/// same honest scope as the dashboard's identical reuse.
fn composer(state: &RootView, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let can_send = state.chat.conn_state == ConnState::Authenticated;
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex_1()
                .h(px(38.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::input_bg())
                .border_1()
                .border_color(theme::input_border())
                .child(state.chat.input.clone()),
        )
        .child(
            div()
                .id("console-composer-send")
                .h(px(38.))
                .px_3p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_LG))
                .when(can_send, |el| {
                    el.bg(theme::alpha(theme::BRAND, 1.0)).cursor_pointer().hover(|s| s.bg(theme::alpha(theme::BRAND, 0.90))).on_click(
                        cx.listener(|this, _ev, _window, cx| {
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
                        }),
                    )
                })
                .when(!can_send, |el| el.bg(theme::alpha(theme::MUTED, 0.4)))
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(if can_send { theme::BRAND_FOREGROUND } else { theme::MUTED_FOREGROUND }, 1.0))
                        .child(i18n::t(locale, "native.chat.send")),
                ),
        )
}

pub(super) fn detail_column(state: &RootView, snap: &ConsoleSnapshot, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    match queue_load_state(snap) {
        QueueLoad::Loading => {
            let header = plain_header(i18n::t(locale, "native.console.title"));
            let body =
                div().id("console-detail-body").flex_1().flex().items_center().justify_center().child(skeleton(px(220.), px(28.)));
            detail_shell(state, header, body, locale, cx)
        }
        QueueLoad::Ready { entries, .. } => {
            let selected = resolve_selection(&snap.selected, &entries);
            let found = selected.and_then(|sel| entries.iter().find(|e| e.selection() == sel).cloned());
            match found {
                None => {
                    let header = plain_header(i18n::t(locale, "native.console.title"));
                    let body = div().id("console-detail-body").flex_1().flex().items_center().justify_center().child(empty_state(
                        "✅",
                        i18n::t(locale, "native.console.queue.empty"),
                        Some(i18n::t(locale, "native.console.queue.emptyDesc")),
                        None::<Div>,
                    ));
                    detail_shell(state, header, body, locale, cx)
                }
                Some(QueueEntry::Approval(item)) => approval_pane(state, &item, snap, locale, cx),
                Some(QueueEntry::Goal(item)) => goal_pane(state, &item, snap, locale, cx),
            }
        }
    }
}
